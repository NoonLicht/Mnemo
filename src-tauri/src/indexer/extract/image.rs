use image::{DynamicImage, ImageReader, Limits};
use serde::Serialize;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Upper bound for a single decodable image (50 megapixels).
///
/// Anything larger is rejected before the decoder allocates the pixel buffer,
/// so a malformed or malicious image cannot exhaust memory.
pub const MAX_IMAGE_PIXELS: u64 = 50_000_000;

/// Value used to disambiguate temporary thumbnail files.
static THUMB_TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Process-wide cap on image size for OCR (0 = no limit).
static OCR_MAX_IMAGE_BYTES: AtomicU64 = AtomicU64::new(0);

pub fn set_ocr_max_image_bytes(bytes: u64) {
    OCR_MAX_IMAGE_BYTES.store(bytes, Ordering::SeqCst);
}

pub fn ocr_max_image_bytes() -> u64 {
    OCR_MAX_IMAGE_BYTES.load(Ordering::SeqCst)
}

/// Whether the extension is one of the raster formats we can decode cheaply.
///
/// HEIC/HEIF and SVG are deliberately excluded: the former is not supported by
/// the `image` crate without a platform decoder, the latter is not a raster
/// format.
/// Longest side of every generated thumbnail and preview.
pub const THUMB_SIZE: u32 = 240;

pub fn is_supported_image_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "tif"
    )
}

/// Snapshot of the local OCR engine and its language data.
#[derive(Debug, Clone, Serialize)]
pub struct OcrStatus {
    /// Whether `tesseract --version` exits successfully.
    pub engine_present: bool,
    /// Languages reported by `tesseract --list-langs`.
    pub languages: Vec<String>,
    /// `engine_present && !languages.is_empty()`.
    pub available: bool,
    /// Where Mnemo stores downloaded language data.
    pub tessdata_dir: String,
    /// Last error line when the engine is missing or failing.
    pub error: Option<String>,
    /// Currently selected backend (`tesseract`, `accurate` or `indra`).
    pub engine: String,
    /// Whether PP-OCR can actually read: compiled in, pack present, runtime
    /// loaded, and its self-test read known text correctly.
    pub accurate_available: bool,
    /// Whether this binary was compiled with the accurate engine.
    pub accurate_compiled: bool,
    /// Why PP-OCR cannot read right now, in plain words. `None` when it can.
    pub accurate_problem: Option<String>,
    /// Whether Indra can actually read: compiled in, pack present, runtime
    /// loaded, and its self-test read known text correctly.
    pub indra_available: bool,
    /// Why Indra cannot read right now, in plain words. `None` when it can.
    pub indra_problem: Option<String>,
    /// The engine that is really reading images right now (`accurate`,
    /// `indra`, `tesseract`, or `none`), which can differ from the selected one.
    pub active_engine: String,
}

/// Directory holding language data downloaded by Mnemo.
///
/// The directory is created lazily by [`download_eng_tessdata`]; read-only
/// callers such as [`ocr_status`] never create it.
pub fn tessdata_dir() -> PathBuf {
    crate::config::MnemoConfig::config_dir().join("tessdata")
}

fn build_status(
    engine_present: bool,
    languages: Vec<String>,
    tessdata_dir: PathBuf,
    error: Option<String>,
) -> OcrStatus {
    OcrStatus {
        available: engine_present && !languages.is_empty(),
        engine_present,
        languages,
        tessdata_dir: tessdata_dir.to_string_lossy().to_string(),
        error,
        engine: crate::ocr::engine_name().to_string(),
        accurate_available: crate::ocr::accurate_available(),
        accurate_compiled: crate::ocr::accurate_compiled(),
        accurate_problem: crate::ocr::accurate_problem(),
        indra_available: crate::ocr_indra::available(),
        indra_problem: crate::ocr_indra::problem(),
        active_engine: crate::ocr::usable_engine().to_string(),
    }
}

fn last_non_empty_line(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .map(str::to_string)
}

/// Parses the output of `tesseract --list-langs`.
///
/// Handles the `List of available languages` header, blank lines, CRLF line
/// endings and duplicate entries while preserving the reported order.
fn parse_langs(raw: &str) -> Vec<String> {
    let mut languages: Vec<String> = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty()
            || line
                .to_ascii_lowercase()
                .starts_with("list of available languages")
        {
            continue;
        }
        if !languages.iter().any(|existing| existing.as_str() == line) {
            languages.push(line.to_string());
        }
    }
    languages
}

/// Returns `dir` when it contains at least one `*.traineddata` file.
///
/// This is the switch that lets Mnemo prefer its private language data over
/// a system-wide `tessdata` directory without overriding a working install.
fn private_tessdata(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("traineddata") && path.is_file() {
            return Some(dir.to_path_buf());
        }
    }
    None
}

/// Resolves the tesseract binary to run.
///
/// A desktop-launched app (notably an AppImage) can inherit a shorter `PATH`
/// than a login shell, so a perfectly good system install can look missing.
/// After walking `PATH` we probe the usual install locations. Setting
/// `MNEMO_TESSERACT` overrides both, which keeps Nix/Flatpak/portable
/// installs workable without patching the app.
fn tesseract_program() -> PathBuf {
    if let Some(path) = std::env::var_os("MNEMO_TESSERACT") {
        return PathBuf::from(path);
    }

    let name = if cfg!(windows) {
        "tesseract.exe"
    } else {
        "tesseract"
    };

    // A copy shipped inside the app wins over whatever the user installed,
    // so OCR behaves the same on every machine the app is installed on.
    for candidate in bundled_tesseract_paths(name) {
        if candidate.is_file() {
            return candidate;
        }
    }

    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return candidate;
            }
        }
    }

    for candidate in fallback_tesseract_paths() {
        if candidate.is_file() {
            return candidate;
        }
    }

    // Nothing found: the OS will now produce the canonical "not found" error.
    PathBuf::from(name)
}

/// Places an app-bundled Tesseract can live: next to the executable (Windows
/// installs, Linux packages) and in the macOS bundle's Resources folder.
fn bundled_tesseract_paths(name: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let Ok(exe) = std::env::current_exe() else {
        return paths;
    };
    if let Some(dir) = exe.parent() {
        paths.push(dir.join(name));
        paths.push(dir.join("tesseract").join(name));
        // macOS: Mnemo.app/Contents/MacOS/mnemo -> Contents/Resources
        if let Some(contents) = dir.parent() {
            paths.push(contents.join("Resources").join("tesseract").join(name));
        }
    }
    paths
}

/// Well-known install prefixes used when `tesseract` is not on `PATH`.
fn fallback_tesseract_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    #[cfg(target_os = "windows")]
    {
        for env in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(env) {
                paths.push(
                    PathBuf::from(base)
                        .join("Tesseract-OCR")
                        .join("tesseract.exe"),
                );
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        for dir in [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/opt/local/bin",
            "/usr/bin",
        ] {
            paths.push(PathBuf::from(dir).join("tesseract"));
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for dir in [
            "/usr/bin",
            "/usr/local/bin",
            "/bin",
            "/snap/bin",
            "/run/current-system/sw/bin",
        ] {
            paths.push(PathBuf::from(dir).join("tesseract"));
        }
    }

    paths
}

fn tesseract_command() -> Command {
    let prog = tesseract_program();
    let mut command = Command::new(&prog);
    // On Linux AppImage, the child process inherits LD_LIBRARY_PATH pointing to AppImage's
    // internal libraries, which breaks host system binaries like /usr/bin/tesseract.
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Ok(orig) = std::env::var("LD_LIBRARY_PATH_ORIG") {
            command.env("LD_LIBRARY_PATH", orig);
        } else if let Ok(appdir) = std::env::var("APPDIR")
            && let Ok(current) = std::env::var("LD_LIBRARY_PATH")
        {
            let cleaned: Vec<_> = std::env::split_paths(&current)
                .filter(|p| !p.starts_with(&appdir))
                .collect();
            if cleaned.is_empty() {
                command.env_remove("LD_LIBRARY_PATH");
            } else if let Ok(joined) = std::env::join_paths(cleaned) {
                command.env("LD_LIBRARY_PATH", joined);
            }
        }
    }
    // Tesseract links OpenMP and, left alone, starts one thread per core in
    // every process. Mnemo already runs several OCR processes concurrently,
    // so that oversubscribes the CPU badly (a crawl with 8 workers behaved as
    // if it had one). Pin each child to a single thread; the outer worker pool
    // provides the parallelism instead.
    command.env("OMP_THREAD_LIMIT", "1");
    command.env("OMP_NUM_THREADS", "1");
    // On Windows, tesseract is a console application. Without this flag every
    // invocation flashes a console window on the desktop (issue #N).
    crate::util::process::hide_console(&mut command);
    command
}

/// True when `tesseract --version` runs successfully.
///
/// A lighter probe than [`ocr_status`]: enough to decide at startup whether
/// Tesseract is installed at all, without listing languages.
pub fn tesseract_available() -> bool {
    tesseract_command()
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Inspects the tesseract binary and its language data. Never panics.
pub fn ocr_status() -> OcrStatus {
    // A status check is also the "re-check" button: pick up a Tesseract the
    // user just installed.
    reset_tesseract_probe();
    let dir = tessdata_dir();

    // Some builds print version information to stderr instead of stdout.
    let (engine_present, mut error) = match tesseract_command().arg("--version").output() {
        Ok(output) if output.status.success() => (true, None),
        Ok(output) => (
            false,
            last_non_empty_line(&output.stderr).or_else(|| last_non_empty_line(&output.stdout)),
        ),
        Err(e) => (false, Some(e.to_string())),
    };

    let mut languages = Vec::new();
    let mut list = tesseract_command();
    list.arg("--list-langs");
    // The configured (private) tessdata directory takes precedence when it
    // exists, mirroring what `extract_image_ocr` does at runtime.
    if dir.is_dir() {
        list.env("TESSDATA_PREFIX", &dir);
    }

    match list.output() {
        Ok(output) if output.status.success() => {
            languages = parse_langs(&String::from_utf8_lossy(&output.stdout));
            if languages.is_empty() {
                // Keep working with builds that print the list to stderr.
                languages = parse_langs(&String::from_utf8_lossy(&output.stderr));
            }
        }
        Ok(output) => {
            if error.is_none() {
                error = last_non_empty_line(&output.stderr)
                    .or_else(|| last_non_empty_line(&output.stdout));
            }
        }
        Err(e) => {
            if error.is_none() {
                error = Some(e.to_string());
            }
        }
    }

    build_status(engine_present, languages, dir, error)
}

/// Downloads `eng.traineddata` into [`tessdata_dir`] and returns its path.
///
/// An existing file larger than 1 MB is reused. The download is written to a
/// `.part` file and renamed into place only after it passes a size sanity
/// check, so a failed attempt never leaves a corrupt language file behind.
pub fn download_eng_tessdata() -> Result<PathBuf, String> {
    download_eng_tessdata_to(&tessdata_dir())
}

fn download_eng_tessdata_to(dir: &Path) -> Result<PathBuf, String> {
    const URL: &str = "https://github.com/tesseract-ocr/tessdata_fast/raw/main/eng.traineddata";
    const MIN_BYTES: u64 = 1_000_000;

    let target = dir.join("eng.traineddata");
    if let Ok(metadata) = std::fs::metadata(&target)
        && metadata.is_file()
        && metadata.len() > MIN_BYTES
    {
        return Ok(target);
    }

    std::fs::create_dir_all(dir).map_err(|e| {
        format!(
            "Failed to create tessdata directory {}: {}",
            dir.display(),
            e
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("Failed to secure {}: {}", dir.display(), e))?;
    }

    let part = dir.join("eng.traineddata.part");
    let _ = std::fs::remove_file(&part);

    let download = (|| -> Result<u64, String> {
        let response = match ureq::get(URL).timeout(Duration::from_secs(600)).call() {
            Ok(response) => response,
            Err(ureq::Error::Status(code, _)) => {
                return Err(format!("Download failed: HTTP {}", code));
            }
            Err(e) => return Err(format!("Download failed: {}", e)),
        };
        let status = response.status();
        let mut reader = response.into_reader();
        let mut file = File::create(&part)
            .map_err(|e| format!("Failed to create {}: {}", part.display(), e))?;

        let written = std::io::copy(&mut reader, &mut file)
            .map_err(|e| format!("HTTP {}: download interrupted: {}", status, e))?;
        file.flush()
            .map_err(|e| format!("Failed to flush {}: {}", part.display(), e))?;
        file.sync_all()
            .map_err(|e| format!("Failed to sync {}: {}", part.display(), e))?;
        Ok(written)
    })();

    let written = match download {
        Ok(written) => written,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
    };

    if written <= MIN_BYTES {
        let _ = std::fs::remove_file(&part);
        return Err(format!(
            "Downloaded eng.traineddata is too small ({} bytes; expected more than {})",
            written, MIN_BYTES
        ));
    }

    if let Err(first) = std::fs::rename(&part, &target) {
        // Windows refuses to replace an existing file.
        let _ = std::fs::remove_file(&target);
        if let Err(second) = std::fs::rename(&part, &target) {
            let _ = std::fs::remove_file(&part);
            return Err(format!(
                "Failed to finalize {}: {} (initial rename: {})",
                target.display(),
                second,
                first
            ));
        }
    }

    Ok(target)
}

/// Cached result of [`tesseract_available`]: 0 unknown, 1 yes, 2 no.
///
/// Engine selection runs for every image, and spawning `tesseract --version`
/// each time would double the process count of a crawl. Reset with
/// [`reset_tesseract_probe`] when the user may have installed it.
static TESSERACT_PROBE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// True when Tesseract can run. Probed once, then cached.
pub fn tesseract_usable() -> bool {
    match TESSERACT_PROBE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let usable = tesseract_has_language();
            TESSERACT_PROBE.store(if usable { 1 } else { 2 }, Ordering::Relaxed);
            usable
        }
    }
}

/// True when Tesseract runs *and* has at least one language to read with.
/// A binary without language data starts fine and then fails on every image.
fn tesseract_has_language() -> bool {
    let mut list = tesseract_command();
    list.arg("--list-langs");
    if let Some(dir) = private_tessdata(&tessdata_dir()) {
        list.env("TESSDATA_PREFIX", dir);
    }
    match list.output() {
        Ok(output) if output.status.success() => {
            !parse_langs(&String::from_utf8_lossy(&output.stdout)).is_empty()
                || !parse_langs(&String::from_utf8_lossy(&output.stderr)).is_empty()
        }
        Ok(output) => {
            let err = String::from_utf8_lossy(&output.stderr);
            if !err.trim().is_empty() {
                eprintln!("[Mnemo OCR] tesseract --list-langs failed: {}", err.trim());
            }
            false
        }
        Err(e) => {
            eprintln!("[Mnemo OCR] failed to execute tesseract: {e}");
            false
        }
    }
}

/// Forgets the cached Tesseract probe (Settings "re-check", config changes).
pub fn reset_tesseract_probe() {
    TESSERACT_PROBE.store(0, Ordering::Relaxed);
}

/// Reads text from an image file with whichever engine can run.
///
/// See [`crate::ocr::OcrOutcome`]: `Unavailable` means no engine could run
/// and the file should be retried later; `NoText` is a final answer.
pub fn ocr_image(path: &Path) -> crate::ocr::OcrOutcome {
    use crate::ocr::{ACCURATE, INDRA, OcrOutcome, TESSERACT};

    // Opt-in size cap (`ocr_max_image_bytes`, 0 = off). Skipping is a final
    // answer, not "retry later"; changing the cap re-queues images through
    // the OCR signature.
    let cap = ocr_max_image_bytes();
    if cap > 0
        && let Ok(meta) = std::fs::metadata(path)
        && meta.len() > cap
    {
        return OcrOutcome::NoText;
    }

    match crate::ocr::usable_engine() {
        INDRA => match crate::ocr_indra::extract_image(path) {
            OcrOutcome::Unavailable if crate::ocr::accurate_available() => {
                crate::ocr::extract_image_ocr(path)
            }
            OcrOutcome::Unavailable if tesseract_usable() => tesseract_ocr(path),
            outcome => outcome,
        },
        ACCURATE => match crate::ocr::extract_image_ocr(path) {
            // The accurate engine was usable a moment ago; if it just failed
            // to load, Tesseract is still better than nothing.
            OcrOutcome::Unavailable if tesseract_usable() => tesseract_ocr(path),
            outcome => outcome,
        },
        TESSERACT => tesseract_ocr(path),
        _ => OcrOutcome::Unavailable,
    }
}

/// Reads text from a decoded image (a scanned PDF page).
pub fn ocr_decoded_image(image: &DynamicImage) -> crate::ocr::OcrOutcome {
    use crate::ocr::{ACCURATE, INDRA, OcrOutcome, TESSERACT};

    let via_tesseract = |image: &DynamicImage| {
        // Tesseract needs a file. A unique temp PNG, removed right after.
        let tmp = std::env::temp_dir().join(format!(
            "mnemo-ocr-{}-{}.png",
            std::process::id(),
            THUMB_TMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        if image
            .save_with_format(&tmp, image::ImageFormat::Png)
            .is_err()
        {
            return OcrOutcome::NoText;
        }
        let outcome = tesseract_ocr(&tmp);
        let _ = std::fs::remove_file(&tmp);
        outcome
    };
    match crate::ocr::usable_engine() {
        INDRA => match crate::ocr_indra::extract_decoded(image) {
            OcrOutcome::Unavailable if crate::ocr::accurate_available() => {
                crate::ocr::extract_decoded_image(image)
            }
            OcrOutcome::Unavailable if tesseract_usable() => via_tesseract(image),
            outcome => outcome,
        },
        ACCURATE => match crate::ocr::extract_decoded_image(image) {
            OcrOutcome::Unavailable if tesseract_usable() => via_tesseract(image),
            outcome => outcome,
        },
        TESSERACT => via_tesseract(image),
        _ => OcrOutcome::Unavailable,
    }
}

/// Extracts text from an image via OCR. `None` for no text or no engine;
/// callers that must tell those apart use [`ocr_image`].
pub fn extract_image_ocr(path: &Path) -> Option<String> {
    match ocr_image(path) {
        crate::ocr::OcrOutcome::Text(text) => Some(text),
        _ => None,
    }
}

/// Runs Tesseract on an image file.
fn tesseract_ocr(path: &Path) -> crate::ocr::OcrOutcome {
    use crate::ocr::OcrOutcome;
    // Try tesseract (available on Linux and macOS with Homebrew).
    //
    // On macOS, feed the image bytes through stdin instead of handing
    // tesseract the original path. An external process reading a file inside
    // a protected folder (Documents, Desktop, Downloads, Pictures) triggers
    // its own TCC permission prompt for *every* file, which is what produced
    // the repeated "allow access" dialogs during a crawl. Reading the bytes
    // here (in the main process, which already holds the folder grant) keeps
    // the child process from touching the protected file at all.
    //
    // Linux and Windows have no per-process folder grant, so they keep
    // passing the path: one less full-file copy in memory, and no reliance on
    // how a particular tesseract build handles binary stdin.
    let stdin_bytes = if cfg!(target_os = "macos") {
        match std::fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(_) => return OcrOutcome::NoText,
        }
    } else {
        None
    };

    let mut command = tesseract_command();
    if stdin_bytes.is_some() {
        command.arg("stdin").stdin(Stdio::piped());
    } else {
        command.arg(path).stdin(Stdio::null());
    }
    command
        .args(["stdout", "--psm", "3"]) // Fully automatic page segmentation
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    // Prefer Mnemo's private language data when it has been downloaded, but
    // never override a working system installation.
    if let Some(dir) = private_tessdata(&tessdata_dir()) {
        command.env("TESSDATA_PREFIX", dir);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            // Installed a moment ago and gone now, or not runnable: forget
            // the cached probe so the next status check tells the truth.
            eprintln!("[Mnemo OCR] could not start tesseract: {e}");
            reset_tesseract_probe();
            return OcrOutcome::Unavailable;
        }
    };
    if let (Some(bytes), Some(mut stdin)) = (stdin_bytes, child.stdin.take()) {
        // Writing in a separate thread avoids deadlocking when the input is
        // larger than the OS pipe buffer. The buffer is moved, not cloned:
        // large scans can be tens of megabytes. Dropping `stdin` at the end
        // of the thread closes the pipe, which is tesseract's EOF.
        std::thread::spawn(move || {
            let _ = stdin.write_all(&bytes);
        });
    }
    let Ok(output) = child.wait_with_output() else {
        return OcrOutcome::NoText;
    };

    if !output.status.success() {
        return OcrOutcome::NoText;
    }

    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        OcrOutcome::NoText
    } else {
        OcrOutcome::Text(text)
    }
}

/// Reads the image dimensions without decoding the pixel data.
///
/// Returns `None` for unreadable/corrupt images and for images above the
/// [`MAX_IMAGE_PIXELS`] budget.
pub fn dimensions(path: &Path) -> Option<(u32, u32)> {
    let (width, height) = image::image_dimensions(path).ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return None;
    }
    Some((width, height))
}

/// Backwards-compatible alias for [`dimensions`].
///
/// Older callers shelled out to ImageMagick's `identify`; the `image` crate is
/// faster, has no external dependency and enforces the decode budget.
pub fn image_dimensions(path: &Path) -> Option<(u32, u32)> {
    dimensions(path)
}

/// Decodes an image with the 50 MP budget enforced.
///
/// The dimensions are checked up front and the decoder itself is configured
/// with matching width/height/allocation limits, so an image cannot expand
/// into a much larger in-memory buffer than advertised.
fn decode_guarded(path: &Path) -> Option<DynamicImage> {
    let (width, height) = dimensions(path)?;

    let mut reader = ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(width);
    limits.max_image_height = Some(height);
    limits.max_alloc = Some(MAX_IMAGE_PIXELS * 5);
    reader.limits(limits);

    reader.decode().ok()
}

/// Computes a 64-bit difference hash (dHash) for an image.
///
/// The image is reduced to 9x8 grayscale; each bit records whether the left
/// pixel of a row-major adjacent pair is brighter than the right one. Images
/// that fail to decode, or exceed the pixel budget, return `None` instead of
/// panicking.
pub fn phash64(path: &Path) -> Option<u64> {
    hash_image(&decode_guarded(path)?)
}

fn hash_image(image: &DynamicImage) -> Option<u64> {
    let small = image
        .resize_exact(9, 8, image::imageops::FilterType::Triangle)
        .into_luma8();

    let mut hash = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            let left = small.get_pixel(x, y)[0];
            let right = small.get_pixel(x + 1, y)[0];
            hash <<= 1;
            if left > right {
                hash |= 1;
            }
        }
    }
    Some(hash)
}

/// Hashes a cached thumbnail without touching the source image.
pub fn phash64_from_thumbnail(thumb: &Path) -> Option<u64> {
    let image = image::open(thumb).ok()?;
    hash_image(&image)
}

/// Reuses a *fresh* thumbnail if one already exists, otherwise returns `None`.
///
/// Unlike [`phash64_cached`], this never decodes or writes the source image, so
/// the crawl can opportunistically group already-thumbnailed screenshots
/// without paying the full-image cost that K3 removed from the crawl path.
pub fn phash64_reusing_fresh_thumbnail(path: &Path, thumbs_dir: &Path) -> Option<u64> {
    let thumb = thumbnail_path(path, thumbs_dir);
    if is_fresh(&thumb, path) {
        phash64_from_thumbnail(&thumb)
    } else {
        None
    }
}

/// Returns a perceptual hash, reusing a fresh thumbnail when one exists and
/// otherwise decoding the source once, writing the thumbnail, and hashing it.
pub fn phash64_cached(path: &Path, thumbs_dir: &Path) -> Option<u64> {
    let thumb = thumbnail(path, thumbs_dir)?;
    phash64_from_thumbnail(&thumb)
}

/// Absolute path of the cached thumbnail for `path` inside `out_dir`.
pub fn thumbnail_path(path: &Path, out_dir: &Path) -> PathBuf {
    let hash = fnv1a(path.to_string_lossy().as_bytes());
    out_dir.join(format!("{:016x}.jpg", hash))
}

/// Generates (or reuses) a 240px JPEG thumbnail for `path`.
///
/// Existing thumbnails that are at least as new as the source are returned
/// unchanged, which makes repeated calls idempotent. New thumbnails are
/// written to a temporary file and renamed into place so readers never observe
/// a partially written JPEG.
pub fn thumbnail(path: &Path, out_dir: &Path) -> Option<PathBuf> {
    // Decoding a cloud-only file downloads it. Thumbnails are opportunistic
    // (grid, hash backfill), so skip instead of pulling the file down.
    if crate::indexer::walker::is_cloud_placeholder_path(path) {
        return None;
    }
    let target = thumbnail_path(path, out_dir);
    if is_fresh(&target, path) {
        return Some(target);
    }

    let image = decode_guarded(path)?;
    std::fs::create_dir_all(out_dir).ok()?;

    let temp = out_dir.join(format!(
        ".thumb-{}-{}.tmp",
        std::process::id(),
        THUMB_TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));

    let result = (|| -> Option<()> {
        let rgb = image.thumbnail(THUMB_SIZE, THUMB_SIZE).to_rgb8();
        let mut writer = BufWriter::new(File::create(&temp).ok()?);
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut writer, 80);
        encoder
            .encode(
                &rgb,
                rgb.width(),
                rgb.height(),
                image::ExtendedColorType::Rgb8,
            )
            .ok()?;
        writer.flush().ok()?;
        Some(())
    })();

    if result.is_none() {
        let _ = std::fs::remove_file(&temp);
        return None;
    }

    // `rename` replaces the destination atomically on Unix. Windows refuses to
    // overwrite, so fall back to removing the stale file once.
    if std::fs::rename(&temp, &target).is_err() {
        let _ = std::fs::remove_file(&target);
        if std::fs::rename(&temp, &target).is_err() {
            let _ = std::fs::remove_file(&temp);
            return None;
        }
    }

    Some(target)
}

fn is_fresh(target: &Path, source: &Path) -> bool {
    let Ok(target_meta) = std::fs::metadata(target) else {
        return false;
    };
    let Ok(source_meta) = std::fs::metadata(source) else {
        return false;
    };
    // Only thumbnails produced by the current size policy are reusable.
    let Ok((target_width, target_height)) = image::image_dimensions(target) else {
        return false;
    };
    if target_width.max(target_height) != THUMB_SIZE {
        return false;
    }
    let Ok(_) = image::image_dimensions(source) else {
        return false;
    };
    match (target_meta.modified(), source_meta.modified()) {
        (Ok(target_time), Ok(source_time)) => target_time >= source_time,
        _ => false,
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgb, RgbImage};
    use std::time::Duration;

    static TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    /// A temp directory that removes itself on drop.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let seq = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "mnemo-image-test-{}-{}-{}",
                tag,
                std::process::id(),
                seq
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn join(&self, name: &str) -> PathBuf {
            self.path.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn save(image: &RgbImage, path: &Path) {
        image
            .save_with_format(path, ImageFormat::Png)
            .expect("write test image");
    }

    fn solid(path: &Path, color: [u8; 3]) {
        save(&RgbImage::from_pixel(64, 48, Rgb(color)), path);
    }

    /// Horizontal brightness ramp; `invert` flips the direction, which flips
    /// every dHash comparison.
    fn gradient(path: &Path, invert: bool) {
        let image = RgbImage::from_fn(64, 48, |x, _| {
            let value = ((x * 255) / 63) as u8;
            let value = if invert { 255 - value } else { value };
            Rgb([value, value, value])
        });
        save(&image, path);
    }

    #[test]
    fn dimensions_reads_generated_png() {
        let dir = TempDir::new("dimensions");
        let path = dir.join("small.png");
        save(&RgbImage::from_pixel(10, 7, Rgb([1, 2, 3])), &path);

        assert_eq!(dimensions(&path), Some((10, 7)));
    }

    #[test]
    fn dimensions_rejects_corrupt_and_zero_byte_files() {
        let dir = TempDir::new("corrupt");
        let corrupt = dir.join("corrupt.png");
        std::fs::write(&corrupt, b"definitely not a png").expect("write corrupt file");
        let empty = dir.join("empty.png");
        std::fs::write(&empty, b"").expect("write empty file");

        assert_eq!(dimensions(&corrupt), None);
        assert_eq!(dimensions(&empty), None);
        assert_eq!(phash64(&corrupt), None);
        assert_eq!(thumbnail(&corrupt, &dir.path), None);
    }

    #[test]
    fn identical_images_have_zero_distance() {
        let dir = TempDir::new("phash-identical");
        let first = dir.join("one.png");
        let second = dir.join("two.png");
        solid(&first, [200, 30, 30]);
        solid(&second, [200, 30, 30]);

        let a = phash64(&first).expect("first phash");
        let b = phash64(&second).expect("second phash");
        assert_eq!(a, b);
        assert_eq!((a ^ b).count_ones(), 0);
    }

    #[test]
    fn inverted_pattern_has_large_distance() {
        let dir = TempDir::new("phash-pattern");
        let first = dir.join("ramp.png");
        let second = dir.join("inverted.png");
        gradient(&first, false);
        gradient(&second, true);

        let a = phash64(&first).expect("first phash");
        let b = phash64(&second).expect("second phash");
        assert!(
            (a ^ b).count_ones() >= 32,
            "inverted patterns should be far apart, distance {}",
            (a ^ b).count_ones()
        );
    }

    #[test]
    fn slight_modification_stays_close() {
        let dir = TempDir::new("phash-modified");
        let base = dir.join("base.png");
        let modified = dir.join("modified.png");
        gradient(&base, false);

        // Same ramp with the far-right quarter painted black, which reverses
        // the last gradient comparisons without touching the rest.
        let image = RgbImage::from_fn(64, 48, |x, _| {
            let value = ((x * 255) / 63) as u8;
            if x >= 48 {
                Rgb([0, 0, 0])
            } else {
                Rgb([value, value, value])
            }
        });
        save(&image, &modified);

        let a = phash64(&base).expect("base phash");
        let b = phash64(&modified).expect("modified phash");
        let distance = (a ^ b).count_ones();
        assert!(
            distance > 0 && distance < 32,
            "a small edit should produce a small distance, got {distance}"
        );
    }

    #[test]
    fn solid_colors_are_color_invariant() {
        // dHash compares neighbouring luminance values, so two *uniform*
        // images have no gradient signal regardless of their color. This
        // documents the deliberate behavior; colour-only differences are not
        // near-duplicates under a dHash.
        let dir = TempDir::new("phash-solid");
        let red = dir.join("red.png");
        let blue = dir.join("blue.png");
        solid(&red, [255, 0, 0]);
        solid(&blue, [0, 0, 255]);

        let a = phash64(&red).expect("red phash");
        let b = phash64(&blue).expect("blue phash");
        assert_eq!((a ^ b).count_ones(), 0);
    }

    #[test]
    fn thumbnail_is_written_and_reused() {
        let dir = TempDir::new("thumbnail");
        let source = dir.join("photo.png");
        let out_dir = dir.join("thumbs");
        gradient(&source, false);

        let first = thumbnail(&source, &out_dir).expect("thumbnail generated");
        assert!(first.exists());
        assert_eq!(first.extension().and_then(|e| e.to_str()), Some("jpg"));

        let decoded = image::open(&first).expect("decode thumbnail");
        assert!(decoded.width() <= THUMB_SIZE && decoded.height() <= THUMB_SIZE);

        let first_mtime = std::fs::metadata(&first)
            .and_then(|meta| meta.modified())
            .expect("thumbnail mtime");

        std::thread::sleep(Duration::from_millis(60));
        let second = thumbnail(&source, &out_dir).expect("thumbnail reused");
        assert_eq!(first, second);

        let second_mtime = std::fs::metadata(&second)
            .and_then(|meta| meta.modified())
            .expect("thumbnail mtime");
        assert_eq!(
            first_mtime, second_mtime,
            "fresh thumbnail must not be rewritten"
        );
    }

    #[test]
    fn legacy_thumbnail_is_regenerated() {
        let dir = TempDir::new("legacy-thumb");
        let source = dir.join("photo.png");
        let out_dir = dir.join("thumbs");
        gradient(&source, false);

        let thumb = thumbnail_path(&source, &out_dir);
        std::fs::create_dir_all(&out_dir).expect("thumb dir");
        // A thumbnail from the previous 480 px policy must be replaced.
        let legacy = image::RgbImage::new(480, 360);
        legacy
            .save_with_format(&thumb, image::ImageFormat::Jpeg)
            .expect("legacy thumbnail");
        let cached = phash64_cached(&source, &out_dir).expect("regenerated hash");
        let direct = phash64(&source).expect("direct hash");
        assert_eq!(cached, direct);

        let regenerated = image::open(&thumb).expect("regenerated thumbnail");
        assert_eq!(regenerated.width().max(regenerated.height()), THUMB_SIZE);
    }

    #[test]
    fn cached_hash_uses_thumbnail_and_is_deterministic() {
        let dir = TempDir::new("phash-cache");
        let source = dir.join("photo.png");
        let thumbs = dir.join("thumbs");
        gradient(&source, false);

        let cached = phash64_cached(&source, &thumbs).expect("cached hash");
        let direct = phash64(&source).expect("direct hash");
        assert_eq!(cached, direct);
        let thumb = thumbnail_path(&source, &thumbs);
        assert!(thumb.exists());

        let mtime = std::fs::metadata(&thumb)
            .and_then(|m| m.modified())
            .expect("mtime");
        std::thread::sleep(Duration::from_millis(60));
        let again = phash64_cached(&source, &thumbs).expect("cached hash again");
        assert_eq!(cached, again);
        let mtime2 = std::fs::metadata(&thumb)
            .and_then(|m| m.modified())
            .expect("mtime");
        assert_eq!(mtime, mtime2, "fresh thumbnail must be reused");
    }

    #[test]
    fn reusing_fresh_thumbnail_does_not_decode_source() {
        let dir = TempDir::new("phash-reuse");
        let source = dir.join("photo.png");
        let thumbs = dir.join("thumbs");
        gradient(&source, false);

        let cached = phash64_cached(&source, &thumbs).expect("cached hash");
        let reused =
            phash64_reusing_fresh_thumbnail(&source, &thumbs).expect("fresh thumbnail hash");
        assert_eq!(cached, reused);

        // The thumbnail mtime must be untouched, proving the source was not
        // re-decoded and the thumbnail was not rewritten.
        let mtime = std::fs::metadata(thumbnail_path(&source, &thumbs))
            .and_then(|m| m.modified())
            .expect("mtime");
        std::thread::sleep(Duration::from_millis(60));
        let reused_again =
            phash64_reusing_fresh_thumbnail(&source, &thumbs).expect("thumbnail-only hash");
        assert_eq!(cached, reused_again);
        let mtime2 = std::fs::metadata(thumbnail_path(&source, &thumbs))
            .and_then(|m| m.modified())
            .expect("mtime");
        assert_eq!(mtime, mtime2, "fresh thumbnail must be reused");
    }

    #[test]
    fn parse_langs_skips_header_blanks_crlf_and_duplicates() {
        let raw = "List of available languages in /usr/share/tessdata (3):\r\n\
                   eng\r\n\
                   \r\n\
                   osd\r\n\
                   eng\r\n\
                   \r\n\
                   deu\r\n";

        assert_eq!(parse_langs(raw), vec!["eng", "osd", "deu"]);
        assert!(parse_langs("").is_empty());
        assert!(parse_langs("List of available languages (0):\n").is_empty());
    }

    #[test]
    fn status_can_report_a_missing_engine() {
        let status = build_status(
            false,
            Vec::new(),
            PathBuf::from("/nonexistent/tessdata"),
            Some("No such file or directory (os error 2)".to_string()),
        );

        assert!(!status.engine_present);
        assert!(!status.available);
        assert!(status.languages.is_empty());
        assert_eq!(
            status.error.as_deref(),
            Some("No such file or directory (os error 2)")
        );
        assert_eq!(status.tessdata_dir, "/nonexistent/tessdata");
    }

    #[test]
    fn private_tessdata_requires_a_traineddata_file() {
        let dir = TempDir::new("tessdata-empty");
        assert_eq!(private_tessdata(&dir.path), None);

        // Unrelated files do not count.
        std::fs::write(dir.join("README.txt"), b"not language data").expect("write unrelated file");
        assert_eq!(private_tessdata(&dir.path), None);

        std::fs::write(dir.join("eng.traineddata"), b"x").expect("write language file");
        assert_eq!(private_tessdata(&dir.path), Some(dir.path.clone()));
    }

    #[test]
    fn existing_large_tessdata_is_reused_without_download() {
        let dir = TempDir::new("tessdata-existing");
        let target = dir.join("eng.traineddata");
        let file = std::fs::File::create(&target).expect("create sparse file");
        file.set_len(2_000_000).expect("size sparse file");
        drop(file);

        let resolved = download_eng_tessdata_to(&dir.path).expect("existing file is reused");

        assert_eq!(resolved, target);
        assert!(!dir.join("eng.traineddata.part").exists());
        assert_eq!(
            std::fs::metadata(&target).expect("metadata").len(),
            2_000_000
        );
    }
}
