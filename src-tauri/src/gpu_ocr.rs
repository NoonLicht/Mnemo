//! Optional hybrid OCR: CPU and an NVIDIA GPU read images at the same time.
//!
//! The CPU engine and the CUDA engine need different ONNX Runtime builds, and
//! one process can load only one, so the GPU half runs in a helper process:
//! Mnemo starts its own executable with `--ocr-gpu-worker`, pointed at an
//! `onnxruntime` with the CUDA provider. The helper reads image paths from
//! stdin and answers one JSON line per path.
//!
//! Work is pulled, not pushed: an extraction worker offers an image to the
//! helper only when the helper is idle, otherwise it reads the image on the
//! CPU itself. A slow or failed helper therefore costs nothing but its share.
//!
//! Nothing here is on by default. Hybrid needs the user's choice (setting or
//! `--ocr=hybrid`), a CUDA 12 + cuDNN 9 installation and an explicit download
//! of the GPU runtime. If any of it is missing, OCR runs on the CPU exactly as
//! before and the settings page says why.

use crate::config::MnemoConfig;
use crate::ocr::OcrOutcome;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::Duration;

pub const CPU: &str = "cpu";
pub const HYBRID: &str = "hybrid";

/// Command-line switch that turns the process into the GPU helper.
pub const WORKER_FLAG: &str = "--ocr-gpu-worker";

/// ONNX Runtime GPU build used by the helper. The digest is the one GitHub
/// publishes for the release asset.
const RUNTIME_URL: &str = "https://github.com/microsoft/onnxruntime/releases/download/v1.23.2/onnxruntime-win-x64-gpu-1.23.2.zip";
const RUNTIME_SHA256: &str = "e77afdbbc2b8cb6da4e5a50d89841b48c44f3e47dce4fb87b15a2743786d0bb9";
const RUNTIME_BYTES: u64 = 326_087_140;
const RUNTIME_FILES: [&str; 3] = [
    "onnxruntime.dll",
    "onnxruntime_providers_shared.dll",
    "onnxruntime_providers_cuda.dll",
];

/// 0 = CPU, 1 = hybrid, from the saved setting.
static CONFIG_MODE: AtomicU8 = AtomicU8::new(0);
/// 0 = no override, 1 = CPU, 2 = hybrid, from `--ocr=`.
static CLI_MODE: AtomicU8 = AtomicU8::new(0);

static READY: AtomicBool = AtomicBool::new(false);
static STARTING: AtomicBool = AtomicBool::new(false);
static GPU_IMAGES: AtomicU64 = AtomicU64::new(0);
static CPU_FALLBACKS: AtomicU64 = AtomicU64::new(0);
static PROBLEM: Mutex<Option<String>> = Mutex::new(None);
static HELPER: Mutex<Option<Helper>> = Mutex::new(None);
static DOWNLOAD: Mutex<Download> = Mutex::new(Download::new());

struct Helper {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    lines: mpsc::Receiver<String>,
}

struct Download {
    active: bool,
    downloaded: u64,
    total: u64,
    phase: &'static str,
    error: Option<String>,
}

impl Download {
    const fn new() -> Self {
        Download {
            active: false,
            downloaded: 0,
            total: RUNTIME_BYTES,
            phase: "",
            error: None,
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Hybrid OCR exists on Windows builds that include the accurate engine.
pub fn supported() -> bool {
    cfg!(all(windows, feature = "accurate-ocr"))
}

/// The mode in force: `--ocr=` for this run, otherwise the saved setting.
pub fn mode() -> &'static str {
    let wanted = match CLI_MODE.load(Ordering::SeqCst) {
        1 => 0,
        2 => 1,
        _ => CONFIG_MODE.load(Ordering::SeqCst),
    };
    if wanted == 1 && supported() {
        HYBRID
    } else {
        CPU
    }
}

/// Applies the saved setting. Leaving hybrid stops the helper.
pub fn set_mode(value: &str) {
    let before = mode();
    CONFIG_MODE.store(
        u8::from(value.eq_ignore_ascii_case(HYBRID)),
        Ordering::SeqCst,
    );
    if before != mode() {
        reset_helper();
    }
}

/// Reads `--ocr=cpu|hybrid` (or `--ocr cpu|hybrid`). Returns what was applied.
pub fn apply_cli_args(args: &[String]) -> Option<&'static str> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let value = if let Some(v) = arg.strip_prefix("--ocr=") {
            v
        } else if arg == "--ocr" {
            iter.next().map_or("", String::as_str)
        } else {
            continue;
        };
        return match value.to_ascii_lowercase().as_str() {
            CPU => {
                CLI_MODE.store(1, Ordering::SeqCst);
                Some(CPU)
            }
            HYBRID => {
                CLI_MODE.store(2, Ordering::SeqCst);
                Some(HYBRID)
            }
            other => {
                eprintln!("[Mnemo OCR] unknown --ocr value {other:?}; use cpu or hybrid");
                None
            }
        };
    }
    None
}

pub fn runtime_dir() -> PathBuf {
    MnemoConfig::config_dir().join("gpu-runtime")
}

fn runtime_dll() -> PathBuf {
    runtime_dir().join("onnxruntime.dll")
}

pub fn runtime_present() -> bool {
    RUNTIME_FILES
        .iter()
        .all(|f| runtime_dir().join(f).is_file())
}

/// Directories of an installed CUDA 12 runtime and cuDNN 9, both needed on
/// the helper's `PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CudaDirs {
    pub cuda: PathBuf,
    pub cudnn: PathBuf,
}

fn has_all(dir: &Path, files: &[&str]) -> bool {
    files.iter().all(|f| dir.join(f).is_file())
}

fn child_dirs(parent: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(parent)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    // Highest version last: "v12.9" sorts after "v12.4" for the CUDA 12 line.
    dirs.sort();
    dirs
}

/// Looks for CUDA 12 (`cudart`, `cublas`, `cublasLt`) and cuDNN 9 on `PATH`
/// and in the NVIDIA installers' default folders.
pub fn find_cuda() -> Option<CudaDirs> {
    const CUDA_FILES: [&str; 3] = ["cudart64_12.dll", "cublas64_12.dll", "cublasLt64_12.dll"];
    const CUDNN_FILES: [&str; 1] = ["cudnn64_9.dll"];

    let mut cuda_candidates: Vec<PathBuf> = Vec::new();
    let mut cudnn_candidates: Vec<PathBuf> = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            cuda_candidates.push(dir.clone());
            cudnn_candidates.push(dir);
        }
    }
    if let Some(root) = std::env::var_os("CUDA_PATH") {
        cuda_candidates.push(PathBuf::from(&root).join("bin"));
        cuda_candidates.push(PathBuf::from(&root).join("bin").join("x64"));
    }
    let program_files = std::env::var_os("ProgramFiles")
        .map_or_else(|| PathBuf::from(r"C:\Program Files"), PathBuf::from);
    let toolkit = program_files
        .join("NVIDIA GPU Computing Toolkit")
        .join("CUDA");
    for version in child_dirs(&toolkit).into_iter().rev() {
        if version
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("v12"))
        {
            cuda_candidates.push(version.join("bin"));
            cuda_candidates.push(version.join("bin").join("x64"));
        }
    }
    let cudnn_root = program_files.join("NVIDIA").join("CUDNN");
    for version in child_dirs(&cudnn_root).into_iter().rev() {
        let bin = version.join("bin");
        for cuda_line in child_dirs(&bin).into_iter().rev() {
            if cuda_line
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("12"))
            {
                cudnn_candidates.push(cuda_line.join("x64"));
            }
        }
        cudnn_candidates.push(bin);
    }

    let cuda = cuda_candidates
        .into_iter()
        .find(|d| has_all(d, &CUDA_FILES))?;
    let cudnn = cudnn_candidates
        .into_iter()
        .find(|d| has_all(d, &CUDNN_FILES))?;
    Some(CudaDirs { cuda, cudnn })
}

/// Why hybrid cannot start right now, or `None` when everything is in place.
fn missing() -> Option<String> {
    if !supported() {
        return Some("Hybrid OCR needs the Windows build with the accurate OCR engine".into());
    }
    if !crate::ocr::pack_present() {
        return Some("The PP-OCR pack is not downloaded yet".into());
    }
    if !runtime_present() {
        return Some("The GPU runtime is not downloaded yet".into());
    }
    if find_cuda().is_none() {
        return Some("CUDA 12 and cuDNN 9 were not found on this PC".into());
    }
    None
}

fn set_problem(message: Option<String>) {
    *lock(&PROBLEM) = message;
}

/// Stops the helper and forgets a previous failure.
fn reset_helper() {
    READY.store(false, Ordering::SeqCst);
    if let Some(mut helper) = lock(&HELPER).take() {
        let _ = helper.child.kill();
        let _ = helper.child.wait();
    }
    set_problem(None);
}

/// Starts the helper in the background when hybrid is wanted and possible.
fn ensure_started() {
    if mode() != HYBRID
        || READY.load(Ordering::SeqCst)
        || STARTING.load(Ordering::SeqCst)
        || lock(&PROBLEM).is_some()
        || missing().is_some()
    {
        return;
    }
    if STARTING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        match start_helper() {
            Ok(helper) => {
                *lock(&HELPER) = Some(helper);
                READY.store(true, Ordering::SeqCst);
            }
            Err(message) => {
                eprintln!("[Mnemo OCR] GPU helper unavailable: {message}");
                set_problem(Some(message));
            }
        }
        STARTING.store(false, Ordering::SeqCst);
    });
}

fn start_helper() -> Result<Helper, String> {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    let dirs = find_cuda().ok_or("CUDA 12 and cuDNN 9 were not found")?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut path = std::ffi::OsString::new();
    for dir in [&dirs.cuda, &dirs.cudnn, &runtime_dir()] {
        path.push(dir);
        path.push(";");
    }
    path.push(std::env::var_os("PATH").unwrap_or_default());

    let log = std::fs::File::create(MnemoConfig::config_dir().join("gpu-ocr.log"))
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null());
    let mut command = Command::new(exe);
    command
        .arg(WORKER_FLAG)
        .env("ORT_DYLIB_PATH", runtime_dll())
        .env("MNEMO_OCR_PACK_DIR", crate::ocr::pack_dir())
        .env("MNEMO_CUDA_STRICT", "1")
        .env("MNEMO_CUDA_ALGO", "heuristic")
        .env("MNEMO_REC_POOL", "1")
        .env("PATH", path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(log);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("cannot start helper: {e}"))?;
    let stdin = child.stdin.take().ok_or("helper has no stdin")?;
    let stdout = child.stdout.take().ok_or("helper has no stdout")?;
    let (tx, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    // The first line is READY or ERR <reason>. Loading the engine, compiling
    // CUDA kernels and the self-test take several seconds.
    match lines.recv_timeout(Duration::from_secs(300)) {
        Ok(line) if line == "READY" => Ok(Helper {
            child,
            stdin,
            lines,
        }),
        Ok(line) => {
            let _ = child.kill();
            Err(line.strip_prefix("ERR ").unwrap_or(&line).to_string())
        }
        Err(_) => {
            let _ = child.kill();
            Err("the GPU helper did not become ready in time (see gpu-ocr.log)".into())
        }
    }
}

fn fail(message: String) {
    eprintln!("[Mnemo OCR] GPU helper stopped: {message}");
    READY.store(false, Ordering::SeqCst);
    if let Some(mut helper) = lock(&HELPER).take() {
        let _ = helper.child.kill();
        let _ = helper.child.wait();
    }
    set_problem(Some(message));
}

/// Offers an image file to the GPU helper. `None` means "read it on the CPU":
/// hybrid is off, the helper is not ready or is busy, or it could not answer.
pub fn try_offload(path: &Path) -> Option<OcrOutcome> {
    use std::io::Write;

    if mode() != HYBRID {
        return None;
    }
    ensure_started();
    if !READY.load(Ordering::SeqCst) {
        return None;
    }
    let text = path.to_str().filter(|p| !p.contains(['\n', '\r']))?;
    let mut guard = HELPER.try_lock().ok()?;
    let helper = guard.as_mut()?;
    if writeln!(helper.stdin, "{text}")
        .and_then(|()| helper.stdin.flush())
        .is_err()
    {
        drop(guard);
        fail("the GPU helper closed its input".into());
        return None;
    }
    let reply = match helper.lines.recv_timeout(Duration::from_secs(180)) {
        Ok(line) => line,
        Err(_) => {
            drop(guard);
            fail("the GPU helper stopped answering".into());
            return None;
        }
    };
    drop(guard);
    let value: serde_json::Value = serde_json::from_str(&reply).ok()?;
    match value.get("r").and_then(|r| r.as_str()) {
        Some("text") => {
            GPU_IMAGES.fetch_add(1, Ordering::Relaxed);
            Some(OcrOutcome::Text(value.get("text")?.as_str()?.to_string()))
        }
        Some("none") => {
            GPU_IMAGES.fetch_add(1, Ordering::Relaxed);
            Some(OcrOutcome::NoText)
        }
        _ => {
            CPU_FALLBACKS.fetch_add(1, Ordering::Relaxed);
            None
        }
    }
}

// ---- status and runtime download ---------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct GpuOcrStatus {
    pub supported: bool,
    /// The mode in force (`cpu` or `hybrid`), after `--ocr=`.
    pub mode: String,
    /// True when `--ocr=` overrides the saved setting for this run.
    pub overridden: bool,
    pub runtime_present: bool,
    pub runtime_bytes: u64,
    pub cuda_found: bool,
    pub cuda_dir: String,
    pub cudnn_dir: String,
    pub pack_present: bool,
    pub downloading: bool,
    pub downloaded: u64,
    pub total: u64,
    pub phase: String,
    pub download_error: Option<String>,
    pub helper_ready: bool,
    pub helper_starting: bool,
    /// Why hybrid is not running although it is wanted.
    pub problem: Option<String>,
    pub gpu_images: u64,
    pub cpu_fallbacks: u64,
}

pub fn status() -> GpuOcrStatus {
    let cuda = find_cuda();
    let download = lock(&DOWNLOAD);
    let wanted = mode() == HYBRID;
    GpuOcrStatus {
        supported: supported(),
        mode: mode().to_string(),
        overridden: CLI_MODE.load(Ordering::SeqCst) != 0,
        runtime_present: runtime_present(),
        runtime_bytes: RUNTIME_BYTES,
        cuda_found: cuda.is_some(),
        cuda_dir: cuda
            .as_ref()
            .map(|d| d.cuda.display().to_string())
            .unwrap_or_default(),
        cudnn_dir: cuda
            .as_ref()
            .map(|d| d.cudnn.display().to_string())
            .unwrap_or_default(),
        pack_present: crate::ocr::pack_present(),
        downloading: download.active,
        downloaded: download.downloaded,
        total: download.total,
        phase: download.phase.to_string(),
        download_error: download.error.clone(),
        helper_ready: READY.load(Ordering::SeqCst),
        helper_starting: STARTING.load(Ordering::SeqCst),
        problem: if wanted {
            lock(&PROBLEM).clone().or_else(missing)
        } else {
            None
        },
        gpu_images: GPU_IMAGES.load(Ordering::Relaxed),
        cpu_fallbacks: CPU_FALLBACKS.load(Ordering::Relaxed),
    }
}

/// Downloads the GPU runtime in the background (one at a time).
pub fn download_runtime() {
    {
        let mut state = lock(&DOWNLOAD);
        if state.active || runtime_present() {
            return;
        }
        *state = Download {
            active: true,
            phase: "downloading",
            ..Download::new()
        };
    }
    std::thread::spawn(|| {
        let result = download_runtime_blocking();
        let mut state = lock(&DOWNLOAD);
        state.active = false;
        state.phase = "";
        match result {
            Ok(()) => state.error = None,
            Err(message) => {
                eprintln!("[Mnemo OCR] GPU runtime download failed: {message}");
                state.error = Some(message);
            }
        }
        drop(state);
        set_problem(None);
    });
}

fn download_runtime_blocking() -> Result<(), String> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};

    let dir = runtime_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let zip_path = dir.join("onnxruntime-gpu.zip.part");

    let response = ureq::get(RUNTIME_URL)
        .timeout(Duration::from_secs(1800))
        .call()
        .map_err(|e| format!("download failed: {e}"))?;
    let total = response
        .header("Content-Length")
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(RUNTIME_BYTES);
    lock(&DOWNLOAD).total = total;

    let mut reader = response.into_reader();
    let mut file = std::fs::File::create(&zip_path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    let mut done = 0u64;
    loop {
        let n = reader
            .read(&mut buffer)
            .map_err(|e| format!("download interrupted: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        file.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        lock(&DOWNLOAD).downloaded = done;
    }
    drop(file);

    lock(&DOWNLOAD).phase = "verifying";
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest.iter() {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    if hex != RUNTIME_SHA256 {
        let _ = std::fs::remove_file(&zip_path);
        return Err(format!("checksum mismatch (got {hex})"));
    }

    lock(&DOWNLOAD).phase = "extracting";
    let archive = std::fs::File::open(&zip_path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(archive).map_err(|e| format!("bad archive: {e}"))?;
    for name in RUNTIME_FILES {
        let index = (0..zip.len())
            .find(|i| {
                zip.name_for_index(*i)
                    .is_some_and(|n| n.is_ok_and(|n| n.ends_with(&format!("/lib/{name}"))))
            })
            .ok_or_else(|| format!("{name} is missing from the archive"))?;
        let mut entry = zip.by_index(index).map_err(|e| e.to_string())?;
        let target = dir.join(name);
        let temp = dir.join(format!("{name}.part"));
        let mut out = std::fs::File::create(&temp).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
        drop(out);
        std::fs::rename(&temp, &target).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_file(&zip_path);
    Ok(())
}

// ---- the helper process --------------------------------------------------

/// Entry point of `--ocr-gpu-worker`. Returns the process exit code.
pub fn worker_main() -> i32 {
    #[cfg(feature = "accurate-ocr")]
    {
        worker::run()
    }
    #[cfg(not(feature = "accurate-ocr"))]
    {
        println!("ERR this build has no accurate OCR engine");
        1
    }
}

#[cfg(feature = "accurate-ocr")]
mod worker {
    use paddleocr_rs_onnx::{AccelerationDevice, OcrEngine, TextRegion, block_from};
    use std::io::{BufRead, Write};

    /// Crops recognised per model call.
    const BATCH: usize = 64;

    pub fn run() -> i32 {
        let engine = match load() {
            Ok(engine) => engine,
            Err(message) => {
                println!("ERR {message}");
                let _ = std::io::stdout().flush();
                return 1;
            }
        };
        println!("READY");
        let _ = std::io::stdout().flush();

        let stdin = std::io::stdin();
        for line in stdin.lock().lines().map_while(Result::ok) {
            let reply = match read_file(&engine, std::path::Path::new(&line)) {
                Ok(Some(text)) => serde_json::json!({"r": "text", "text": text}),
                Ok(None) => serde_json::json!({"r": "none"}),
                Err(message) => serde_json::json!({"r": "fallback", "msg": message}),
            };
            println!("{reply}");
            let _ = std::io::stdout().flush();
        }
        0
    }

    fn load() -> Result<OcrEngine, String> {
        let pack = crate::ocr::pack_dir();
        let det = std::fs::read(pack.join("det.onnx")).map_err(|e| format!("det.onnx: {e}"))?;
        let rec = std::fs::read(pack.join("rec.onnx")).map_err(|e| format!("rec.onnx: {e}"))?;
        let dict = std::fs::read(pack.join("dict.txt")).map_err(|e| format!("dict.txt: {e}"))?;
        let rec = crate::onnx_patch::add_argmax_outputs(&rec)
            .map_err(|e| format!("cannot prepare the recogniser for the GPU: {e}"))?;
        let keys = crate::ocr::keys_with_space(&dict);
        let engine = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            OcrEngine::new_with_device(&det, &rec, &keys, AccelerationDevice::Cuda)
        }))
        .map_err(|_| "the GPU runtime could not be loaded".to_string())?
        .map_err(|e| e.to_string())?;

        // Prove it reads correctly before announcing READY.
        let image = image::load_from_memory(crate::ocr::SELF_TEST_IMAGE)
            .map_err(|e| format!("self-test image: {e}"))?;
        let text = recognise(&engine, &image)?.unwrap_or_default();
        let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if !normalized
            .to_lowercase()
            .contains(crate::ocr::SELF_TEST_EXPECTED)
        {
            return Err(format!("the GPU misread its self-test: {normalized:?}"));
        }
        Ok(engine)
    }

    fn read_file(engine: &OcrEngine, path: &std::path::Path) -> Result<Option<String>, String> {
        let Ok(image) = image::open(path) else {
            return Ok(None);
        };
        recognise(engine, &image)
    }

    /// `Ok(None)`: no text. `Err`: let the CPU decide (no regions found, or
    /// the batch failed).
    fn recognise(
        engine: &OcrEngine,
        image: &image::DynamicImage,
    ) -> Result<Option<String>, String> {
        let regions: Vec<TextRegion> = engine
            .detect_text_regions(image)
            .map_err(|e| format!("detection: {e}"))?;
        if regions.is_empty() {
            return Err("no text regions".into());
        }
        let mut kept: Vec<&TextRegion> = Vec::new();
        let mut crops = Vec::new();
        for region in &regions {
            if let Ok(crop) = engine.prepare_region(image, region) {
                kept.push(region);
                crops.push(crop);
            }
        }
        let decoded = engine.recognize_prepared(&crops, BATCH);
        if !decoded.is_empty() && decoded.iter().all(Option::is_none) {
            return Err("recognition failed".into());
        }
        let mut blocks: Vec<_> = kept
            .into_iter()
            .zip(decoded)
            .filter_map(|(region, text)| text.map(|t| block_from(region, t)))
            .filter(|b| !b.text.trim().is_empty())
            .collect();
        blocks.sort_by(|a, b| {
            a.y.partial_cmp(&b.y)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
        });
        let text = blocks
            .iter()
            .map(|b| b.text.trim())
            .collect::<Vec<_>>()
            .join("\n");
        Ok(if text.is_empty() { None } else { Some(text) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_flag_overrides_the_saved_mode() {
        // Process-wide state: restore it so other tests are unaffected.
        let saved = CLI_MODE.load(Ordering::SeqCst);
        assert_eq!(apply_cli_args(&["--ocr=cpu".into()]), Some(CPU));
        assert_eq!(
            apply_cli_args(&["x".into(), "--ocr".into(), "HYBRID".into()]),
            Some(HYBRID)
        );
        assert_eq!(apply_cli_args(&["--ocr=gpu".into()]), None);
        assert_eq!(apply_cli_args(&["--toggle".into()]), None);
        CLI_MODE.store(saved, Ordering::SeqCst);
    }

    #[test]
    fn cpu_is_the_default_and_unsupported_builds_never_report_hybrid() {
        assert_eq!(CONFIG_MODE.load(Ordering::SeqCst), 0);
        if !supported() {
            CONFIG_MODE.store(1, Ordering::SeqCst);
            assert_eq!(mode(), CPU);
            CONFIG_MODE.store(0, Ordering::SeqCst);
        }
    }

    #[test]
    fn newline_paths_are_not_offered_to_the_helper() {
        assert!(try_offload(Path::new("a\nb.png")).is_none());
    }

    #[test]
    fn cuda_probe_never_panics() {
        let found = find_cuda();
        eprintln!("cuda: {found:?}");
    }

    #[test]
    fn runtime_digest_is_a_sha256() {
        assert_eq!(RUNTIME_SHA256.len(), 64);
        assert!(RUNTIME_SHA256.bytes().all(|b| b.is_ascii_hexdigit()));
    }
}
