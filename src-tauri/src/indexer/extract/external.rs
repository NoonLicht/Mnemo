//! Opt-in external text extractors.
//!
//! A plugin is an executable configured by the user in `config.json`. Mnemo
//! never shells out: arguments are passed as separate OS strings, the child's
//! environment is cleared, stdout is bounded, and the whole process group is
//! killed on timeout. The default registry is empty.

use crate::config::ExternalExtractor;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use super::DocumentExtract;

/// Upper bound chosen below the extraction layer's 8s watchdog so a plugin
/// timeout is always handled by this module before its worker is detached.
const MAX_TIMEOUT_MS: u64 = 7_000;
const MIN_TIMEOUT_MS: u64 = 100;
const MAX_OUTPUT_BYTES: u64 = 4 * 1024 * 1024;

static REGISTRY: OnceLock<RwLock<Vec<ExternalExtractor>>> = OnceLock::new();
static FAILURES: OnceLock<Mutex<VecDeque<(PathBuf, String)>>> = OnceLock::new();

fn registry() -> &'static RwLock<Vec<ExternalExtractor>> {
    REGISTRY.get_or_init(|| RwLock::new(Vec::new()))
}

/// Replaces the process-wide registry. Called when config is loaded or saved.
pub fn set_extractors(extractors: Vec<ExternalExtractor>) {
    if let Ok(mut guard) = registry().write() {
        *guard = extractors;
    }
    if let Ok(mut failures) = failures().lock() {
        failures.clear();
    }
}

fn failures() -> &'static Mutex<VecDeque<(PathBuf, String)>> {
    FAILURES.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// Records the most recent failure reason for Doctor. Bounded so a broken
/// plugin cannot grow memory during a large crawl.
pub fn record_failure(path: &Path, reason: &str) {
    let Ok(mut failures) = failures().lock() else {
        return;
    };
    failures.retain(|(failed, _)| failed != path);
    if failures.len() >= 64 {
        failures.pop_front();
    }
    failures.push_back((path.to_path_buf(), reason.to_string()));
}

pub fn clear_failure(path: &Path) {
    if let Ok(mut failures) = failures().lock() {
        failures.retain(|(failed, _)| failed != path);
    }
}

/// Recent runtime failures, newest last, for Doctor.
pub fn recent_failures() -> Vec<(PathBuf, String)> {
    failures()
        .lock()
        .map(|failures| failures.iter().cloned().collect())
        .unwrap_or_default()
}

/// Returns the first enabled extractor matching this path.
pub fn find(path: &Path) -> Option<ExternalExtractor> {
    let extension = normalized_extension(path);
    let mime = mime_for_extension(extension.as_deref());
    let guard = registry().read().ok()?;
    guard
        .iter()
        .find(|extractor| extractor.matches(extension.as_deref(), mime))
        .cloned()
}

/// True when a configured extractor handles this path. Used by the crawler and
/// watcher to decide whether an otherwise unknown extension needs content work.
pub fn handles(path: &Path) -> bool {
    let extension = normalized_extension(path);
    let mime = mime_for_extension(extension.as_deref());
    registry().read().is_ok_and(|guard| {
        guard
            .iter()
            .any(|extractor| extractor.matches(extension.as_deref(), mime))
    })
}

impl ExternalExtractor {
    /// Validates everything except the `enabled` opt-in. Doctor uses this to
    /// show the command and catch broken configurations before they run.
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("external extractor has an empty name".to_string());
        }
        if self.extensions.is_empty() && self.mime.is_empty() {
            return Err(format!(
                "external extractor '{}' needs an extension or MIME selector",
                self.name
            ));
        }
        if self
            .extensions
            .iter()
            .any(|selector| selector.trim().trim_matches('.').is_empty())
        {
            return Err(format!(
                "external extractor '{}' has an empty extension selector",
                self.name
            ));
        }
        if self.mime.iter().any(|selector| selector.trim().is_empty()) {
            return Err(format!(
                "external extractor '{}' has an empty MIME selector",
                self.name
            ));
        }
        if self.args.len() > 128 {
            return Err(format!(
                "external extractor '{}' has too many arguments",
                self.name
            ));
        }
        validate_command(&self.command).map(|_| ())
    }

    fn matches(&self, extension: Option<&str>, mime: Option<&str>) -> bool {
        if !self.enabled {
            return false;
        }

        let extension_match = self.extensions.iter().any(|selector| {
            let selector = selector.trim().trim_start_matches('.').to_ascii_lowercase();
            selector == "*" || extension.is_some_and(|extension| selector == extension)
        });

        let mime_match = self.mime.iter().any(|selector| {
            let selector = selector.trim().to_ascii_lowercase();
            if selector == "*/*" {
                return true;
            }
            let Some(mime) = mime else {
                return false;
            };
            match selector.split_once('/') {
                Some((kind, "*")) => mime
                    .split_once('/')
                    .is_some_and(|(actual, _)| actual == kind),
                _ => selector == mime,
            }
        });

        extension_match || mime_match
    }

    /// Runs the plugin and returns extracted text.
    ///
    /// The file path is appended as the last argument unless an argument
    /// contains `{path}`. No shell is involved in either case.
    pub fn run(&self, path: &Path, max_bytes: usize) -> Result<DocumentExtract, String> {
        if !self.enabled {
            return Err(format!("external extractor '{}' is disabled", self.name));
        }
        self.validate()
            .map_err(|error| format!("external extractor '{}': {}", self.name, error))?;

        let command_path = validate_command(&self.command)?;
        let args = self.expanded_args(path)?;
        if args.len() > 128 {
            return Err(format!(
                "external extractor '{}' has too many arguments",
                self.name
            ));
        }

        let timeout_ms = self.timeout_ms.clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS);
        let output_cap =
            (self.max_output_bytes.clamp(1024, MAX_OUTPUT_BYTES) as usize).min(max_bytes.max(1024));

        let mut command = Command::new(&command_path);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env_clear();

        // Keep only what a non-interactive program needs to start. User
        // secrets (daemon tokens, API keys) never reach the plugin.
        #[cfg(unix)]
        command.env("PATH", "/usr/local/bin:/usr/bin:/bin");
        #[cfg(windows)]
        {
            if let Some(path) = std::env::var_os("PATH") {
                command.env("PATH", path);
            }
            for key in ["SYSTEMROOT", "WINDIR", "TEMP", "TMP"] {
                if let Some(value) = std::env::var_os(key) {
                    command.env(key, value);
                }
            }
        }

        if let Some(parent) = path.parent() {
            command.current_dir(parent);
        }

        // Hide console windows on Windows (external extractors are console apps).
        crate::util::process::hide_console(&mut command);

        // A new process group lets the timeout path kill the plugin *and* any
        // helpers it spawned, instead of leaving them behind.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }

        let mut child = command.spawn().map_err(|error| {
            format!(
                "external extractor '{}' could not start {}: {}",
                self.name,
                command_path.display(),
                error
            )
        })?;

        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                kill_process_group(&mut child);
                let _ = child.wait();
                return Err(format!("external extractor '{}' has no stdout", self.name));
            }
        };
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let _ = result_tx.send(read_capped(stdout, output_cap));
        });

        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let mut timed_out = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() >= deadline => {
                    timed_out = true;
                    kill_process_group(&mut child);
                    let _ = child.wait();
                    break None;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(error) => {
                    kill_process_group(&mut child);
                    let _ = child.wait();
                    let _ = result_rx.recv_timeout(Duration::from_millis(250));
                    return Err(format!(
                        "external extractor '{}' failed while waiting: {}",
                        self.name, error
                    ));
                }
            }
        };

        // A plugin that exits but leaves a helper holding stdout open must not
        // pin this extraction slot forever. Wait for the reader only within the
        // same deadline, then kill the remaining process group.
        let reader_timeout = if timed_out {
            Duration::from_millis(250)
        } else {
            deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(250))
        };
        let reader_result = match result_rx.recv_timeout(reader_timeout) {
            Ok(result) => {
                let _ = reader.join();
                result
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if !timed_out {
                    kill_process_group(&mut child);
                }
                return Err(format!(
                    "external extractor '{}' timed out after {} ms waiting for output",
                    self.name, timeout_ms
                ));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                if !timed_out {
                    kill_process_group(&mut child);
                }
                return Err(format!(
                    "external extractor '{}' reader stopped unexpectedly",
                    self.name
                ));
            }
        };

        if timed_out {
            return Err(format!(
                "external extractor '{}' timed out after {} ms",
                self.name, timeout_ms
            ));
        }
        let Some(status) = status else {
            return Err(format!(
                "external extractor '{}' timed out after {} ms",
                self.name, timeout_ms
            ));
        };
        let (output, overflow) = reader_result;
        if overflow {
            return Err(format!(
                "external extractor '{}' exceeded the {} byte output cap",
                self.name, output_cap
            ));
        }
        if !status.success() {
            return Err(match status.code() {
                Some(code) => format!("external extractor '{}' exited with {}", self.name, code),
                None => format!("external extractor '{}' was terminated", self.name),
            });
        }

        let text = String::from_utf8(output).map_err(|_| {
            format!(
                "external extractor '{}' returned non-UTF-8 output",
                self.name
            )
        })?;
        let text = truncate_utf8(text, max_bytes);
        if text.trim().is_empty() {
            return Err(format!(
                "external extractor '{}' returned no text",
                self.name
            ));
        }

        clear_failure(path);
        Ok(DocumentExtract {
            text,
            page_count: None,
            is_encrypted: false,
            is_scanned: false,
            format_name: self.name.trim().to_string(),
        })
    }

    fn expanded_args(&self, path: &Path) -> Result<Vec<OsString>, String> {
        let mut args = Vec::with_capacity(self.args.len() + 1);
        let mut used_placeholder = false;

        for arg in &self.args {
            if arg.contains("{path}") {
                let path_str = path.to_str().ok_or_else(|| {
                    format!(
                        "external extractor '{}' cannot pass a non-UTF-8 path through '{{path}}'",
                        self.name
                    )
                })?;
                args.push(arg.replace("{path}", path_str).into());
                used_placeholder = true;
            } else {
                args.push(arg.into());
            }
        }

        if !used_placeholder {
            args.push(path.as_os_str().to_owned());
        }
        Ok(args)
    }
}

fn validate_command(command: &str) -> Result<PathBuf, String> {
    let command = command.trim();
    if command.is_empty() {
        return Err("external extractor command is empty".to_string());
    }
    let path = PathBuf::from(command);
    if !path.is_absolute() {
        return Err(format!(
            "external extractor command must be an absolute path, got '{}'",
            command
        ));
    }
    let metadata = fs::metadata(&path)
        .map_err(|error| format!("external extractor command '{}': {}", path.display(), error))?;
    if !metadata.is_file() {
        return Err(format!(
            "external extractor command '{}' is not a file",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(format!(
                "external extractor command '{}' is not executable",
                path.display()
            ));
        }
    }
    #[cfg(windows)]
    {
        // Rust can implicitly route .bat/.cmd through cmd.exe. Reject them so
        // the no-shell guarantee holds on every platform.
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if extension == "bat" || extension == "cmd" {
            return Err(format!(
                "external extractor command '{}' is a batch script; configure an .exe instead",
                path.display()
            ));
        }
    }
    Ok(path)
}

fn normalized_extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.trim_start_matches('.').to_ascii_lowercase())
        .filter(|extension| !extension.is_empty())
}

fn mime_for_extension(extension: Option<&str>) -> Option<&'static str> {
    Some(match extension? {
        "txt" | "text" | "log" => "text/plain",
        "md" | "markdown" => "text/markdown",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "xml" => "application/xml",
        "yaml" | "yml" => "application/yaml",
        "toml" => "application/toml",
        "pdf" => "application/pdf",
        "rtf" => "application/rtf",
        "epub" => "application/epub+zip",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "stl" => "model/stl",
        "obj" => "model/obj",
        "gltf" => "model/gltf+json",
        "glb" => "model/gltf-binary",
        _ => return None,
    })
}

fn read_capped(mut stream: impl Read, cap: usize) -> (Vec<u8>, bool) {
    let mut output = Vec::with_capacity(cap.min(64 * 1024));
    let mut buffer = [0u8; 8192];
    let mut overflow = false;
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let remaining = cap.saturating_sub(output.len());
                if read > remaining {
                    output.extend_from_slice(&buffer[..remaining]);
                    overflow = true;
                    break;
                }
                output.extend_from_slice(&buffer[..read]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    (output, overflow)
}

fn kill_process_group(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        // The child was spawned into its own process group, so the negative
        // pid reaches helpers too. Fall back to the direct kill below.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

fn truncate_utf8(mut text: String, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text;
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    text.truncate(boundary);
    text
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    static REGISTRY_LOCK: Mutex<()> = Mutex::new(());

    struct TempDir {
        dir: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let id = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "mnemo-ext-{}-{}-{}",
                tag,
                std::process::id(),
                id
            ));
            fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }

        fn file(&self, name: &str, content: &str) -> PathBuf {
            let path = self.dir.join(name);
            fs::write(&path, content).unwrap();
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn extractor(command: &Path, args: Vec<&str>) -> ExternalExtractor {
        ExternalExtractor {
            name: "test-plugin".to_string(),
            extensions: vec!["k10ext".to_string()],
            mime: Vec::new(),
            command: command.to_string_lossy().to_string(),
            args: args.into_iter().map(str::to_string).collect(),
            timeout_ms: 2_000,
            max_output_bytes: 1_000_000,
            enabled: true,
        }
    }

    #[test]
    fn disabled_extractor_never_runs() {
        let temp = TempDir::new("disabled");
        let path = temp.file("input.k10ext", "text\n");
        let mut extractor = extractor(Path::new("/bin/cat"), Vec::new());
        extractor.enabled = false;
        assert!(!extractor.matches(Some("k10ext"), None));
        let error = extractor.run(&path, 1_000_000).unwrap_err();
        assert!(error.contains("disabled"), "got: {error}");
    }

    #[test]
    fn matches_extensions_and_mime_selectors() {
        let extractor = ExternalExtractor {
            name: "matcher".to_string(),
            extensions: vec![".K10EXT".to_string()],
            mime: vec!["text/*".to_string()],
            command: "/bin/true".to_string(),
            args: Vec::new(),
            timeout_ms: 1_000,
            max_output_bytes: 1_000,
            enabled: true,
        };

        assert!(extractor.matches(Some("k10ext"), None));
        assert!(extractor.matches(Some("txt"), Some("text/plain")));
        assert!(extractor.matches(None, Some("text/markdown")));
        assert!(!extractor.matches(Some("bin"), Some("application/octet-stream")));
        assert!(!extractor.matches(Some("txt"), None));

        let disabled = ExternalExtractor {
            enabled: false,
            ..extractor
        };
        assert!(!disabled.matches(Some("k10ext"), None));
    }

    #[test]
    fn validate_requires_a_selector_and_an_executable_command() {
        let mut extractor = extractor(Path::new("/bin/cat"), Vec::new());
        extractor.extensions.clear();
        let error = extractor.validate().unwrap_err();
        assert!(error.contains("selector"), "got: {error}");

        extractor.extensions.push("  ".to_string());
        let error = extractor.validate().unwrap_err();
        assert!(error.contains("empty extension"), "got: {error}");

        extractor.extensions = vec!["k10ext".to_string()];
        extractor.enabled = false;
        assert!(
            extractor.validate().is_ok(),
            "validation is separate from the enable opt-in"
        );
    }

    #[test]
    fn relative_command_is_rejected_before_spawn() {
        let extractor = ExternalExtractor {
            name: "relative".to_string(),
            extensions: vec!["k10ext".to_string()],
            mime: Vec::new(),
            command: "plugin".to_string(),
            args: Vec::new(),
            timeout_ms: 1_000,
            max_output_bytes: 1_000,
            enabled: true,
        };
        let error = extractor
            .run(Path::new("/tmp/file.k10ext"), 1_000)
            .unwrap_err();
        assert!(error.contains("absolute"), "got: {error}");
    }

    #[test]
    fn runs_without_a_shell_and_passes_odd_filenames_as_one_argument() {
        let temp = TempDir::new("shell");
        let payload = "plugin text: needle\n";
        let path = temp.file("note $(printf pwned) ; `echo x`.k10ext", payload);
        let pwned = Path::new("/tmp/mnemo-k10-shell-injection");
        let _ = fs::remove_file(pwned);

        let extractor = extractor(Path::new("/bin/cat"), Vec::new());
        let document = extractor.run(&path, 1_000_000).unwrap();
        assert_eq!(document.text, payload);
        assert!(
            !pwned.exists(),
            "filename must never be interpreted as shell code"
        );
    }

    #[test]
    fn expands_path_placeholder_as_one_argument() {
        let temp = TempDir::new("placeholder");
        let path = temp.file("space name.k10ext", "");
        let extractor = extractor(Path::new("/bin/echo"), vec!["{path}"]);
        let document = extractor.run(&path, 1_000_000).unwrap();
        assert_eq!(document.text.trim_end(), path.to_string_lossy());
    }

    #[test]
    fn hung_plugin_is_killed_by_the_timeout() {
        let temp = TempDir::new("timeout");
        let pid_file = temp.dir.join("pid");
        let command = format!("echo $$ > '{}'; sleep 30", pid_file.display());
        let path = temp.file("input.k10ext", "");
        let mut extractor = extractor(
            Path::new("/bin/sh"),
            vec!["-c", command.as_str(), "mnemo-plugin"],
        );
        extractor.timeout_ms = 150;

        let started = Instant::now();
        let error = extractor.run(&path, 1_000_000).unwrap_err();
        assert!(error.contains("timed out"), "got: {error}");
        assert!(started.elapsed() < Duration::from_secs(2));

        let pid = fs::read_to_string(&pid_file)
            .expect("plugin should write its pid")
            .trim()
            .parse::<i32>()
            .unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        assert!(!alive, "plugin process {pid} should have been killed");
    }

    #[test]
    fn background_helper_holding_stdout_is_killed() {
        let temp = TempDir::new("helper");
        let pid_file = temp.dir.join("helper.pid");
        let command = format!("sleep 30 & echo $! > '{}'", pid_file.display());
        let path = temp.file("input.k10ext", "");
        let mut extractor = extractor(
            Path::new("/bin/sh"),
            vec!["-c", command.as_str(), "mnemo-plugin"],
        );
        extractor.timeout_ms = 200;

        let started = Instant::now();
        let error = extractor.run(&path, 1_000_000).unwrap_err();
        assert!(error.contains("timed out"), "got: {error}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "a helper holding stdout must not pin the extraction slot"
        );

        // macOS may briefly report a reparented helper as a zombie; the
        // timeout/no-hang assertions above carry the cross-platform check.
        #[cfg(target_os = "linux")]
        {
            let pid = fs::read_to_string(&pid_file)
                .expect("helper should write its pid")
                .trim()
                .parse::<i32>()
                .unwrap();
            std::thread::sleep(Duration::from_millis(50));
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            assert!(!alive, "helper process {pid} should have been killed");
        }
        #[cfg(not(target_os = "linux"))]
        let _ = &pid_file;
    }

    #[test]
    fn output_cap_is_enforced() {
        let temp = TempDir::new("cap");
        let command = "i=0; while [ $i -lt 200 ]; do printf '0123456789abcdef'; i=$((i+1)); done";
        let path = temp.file("input.k10ext", "");
        let mut extractor = extractor(Path::new("/bin/sh"), vec!["-c", command, "mnemo-plugin"]);
        extractor.max_output_bytes = 1_024;
        let error = extractor.run(&path, 1_000_000).unwrap_err();
        assert!(error.contains("output cap"), "got: {error}");
    }

    #[test]
    fn nonzero_exit_is_reported() {
        let temp = TempDir::new("exit");
        let path = temp.file("input.k10ext", "");
        let extractor = extractor(Path::new("/bin/sh"), vec!["-c", "exit 7", "mnemo-plugin"]);
        let error = extractor.run(&path, 1_000_000).unwrap_err();
        assert!(error.contains("exited with 7"), "got: {error}");
    }

    #[test]
    fn runtime_failures_are_recorded_and_cleared() {
        let _lock = REGISTRY_LOCK.lock().unwrap();
        let path = Path::new("/tmp/mnemo-k10-failure.k10ext");
        record_failure(path, "test failure");
        assert!(
            recent_failures()
                .iter()
                .any(|(failed, reason)| { failed == path && reason == "test failure" })
        );
        clear_failure(path);
        assert!(!recent_failures().iter().any(|(failed, _)| failed == path));
    }

    #[test]
    fn extract_document_uses_the_registry() {
        let _lock = REGISTRY_LOCK.lock().unwrap();
        let temp = TempDir::new("registry");
        let path = temp.file("input.k10ext", "text from registry\n");
        let config = crate::config::MnemoConfig {
            extractors: vec![extractor(Path::new("/bin/cat"), Vec::new())],
            ..crate::config::MnemoConfig::default()
        };
        // Round-trip through the same config format users edit, proving a new
        // format needs no recompile.
        let raw = serde_json::to_string(&config).unwrap();
        let parsed = crate::config::MnemoConfig::load_from_str(&raw);
        assert_eq!(parsed.extractors, config.extractors);
        set_extractors(parsed.extractors);

        let result = super::super::extract_document(&path, 1_000_000);
        set_extractors(Vec::new());

        let document = result.expect("external extraction should run");
        assert_eq!(document.text, "text from registry\n");
        assert_eq!(document.format_name, "test-plugin");
    }
}
