//! Local semantic model artifact manager.
//!
//! Mnemo ships without the Potion embedding weights. This module tracks the
//! three files needed by the semantic layer, downloads the missing ones from
//! HuggingFace in a single background thread, and exposes progress to the UI.

use crate::config::MnemoConfig;
use serde::Serialize;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Size of the read buffer, and therefore the progress update interval.
const CHUNK_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct Artifact {
    pub name: &'static str,
    pub urls: &'static [&'static str],
    pub bytes_expected: u64,
    pub sha256: Option<&'static str>,
}

/// Static manifest of the files required by the semantic model.
const ARTIFACTS: [Artifact; 3] = [
    Artifact {
        name: "model.safetensors",
        urls: &[
            "https://huggingface.co/minishlab/potion-base-8M/resolve/main/model.safetensors",
            "https://hf-mirror.com/minishlab/potion-base-8M/resolve/main/model.safetensors",
        ],
        bytes_expected: 30_236_760,
        sha256: Some("f65d0f325faadc1e121c319e2faa41170d3fa07d8c89abd48ca5358d9a223de2"),
    },
    Artifact {
        name: "tokenizer.json",
        urls: &[
            "https://huggingface.co/minishlab/potion-base-8M/resolve/main/tokenizer.json",
            "https://hf-mirror.com/minishlab/potion-base-8M/resolve/main/tokenizer.json",
        ],
        bytes_expected: 683_666,
        sha256: Some("e67e803f624fb4d67dea1c730d06e1067e1b14d830e2c2202569e3ef0f70bb50"),
    },
    Artifact {
        name: "config.json",
        urls: &[
            "https://huggingface.co/minishlab/potion-base-8M/resolve/main/config.json",
            "https://hf-mirror.com/minishlab/potion-base-8M/resolve/main/config.json",
        ],
        bytes_expected: 202,
        sha256: Some("2a6ac0e9aaa356a68a5688070db78fc3a464fefe85d2f06a1905ce3718687553"),
    },
];

/// Optional accurate OCR pack (PP-OCRv6 tiny detector + recognizer + dict).
const OCR_ARTIFACTS: [Artifact; 3] = [
    Artifact {
        name: "det.onnx",
        urls: &[
            "https://huggingface.co/PaddlePaddle/PP-OCRv6_tiny_det_onnx/resolve/main/inference.onnx",
            "https://hf-mirror.com/PaddlePaddle/PP-OCRv6_tiny_det_onnx/resolve/main/inference.onnx",
        ],
        bytes_expected: 1_780_590,
        sha256: Some("193bab7a04fca699a6c82e6abb5b81bdb28177f0abd4062552b04908dafb19f8"),
    },
    Artifact {
        name: "rec.onnx",
        urls: &[
            "https://huggingface.co/PaddlePaddle/PP-OCRv6_tiny_rec_onnx/resolve/main/inference.onnx",
            "https://hf-mirror.com/PaddlePaddle/PP-OCRv6_tiny_rec_onnx/resolve/main/inference.onnx",
        ],
        bytes_expected: 4_462_639,
        sha256: Some("9ef676d6ed3c88256a2d92c640c44f25b0c40947e111b14b8be8f594091563e6"),
    },
    Artifact {
        name: "dict.txt",
        urls: &["https://github.com/GreatV/oar-ocr/releases/download/v0.7.0/ppocrv6_tiny_dict.txt"],
        bytes_expected: 27_156,
        sha256: Some("c5cbe34ef40c29c4df07ed012bf96569cb69a2d2a01a07027e9f13cb832bd9cd"),
    },
];

/// Optional ONNX smart embedding pack (bge-small-en-v1.5 int8).
/// Optional Indra OCR pack (datalab-to/indra-ocr, pinned to the v1.1.0 model
/// tag): detector, orientation and script classifiers, four recognizers and
/// their vocabularies. Weights under a modified OpenRAIL-M license, so they
/// are downloaded only when the user selects Indra.
const INDRA_ARTIFACTS: [Artifact; 8] = [
    Artifact {
        name: "det.onnx",
        urls: &[
            "https://huggingface.co/datalab-to/indra-ocr/resolve/v1.1.0/det.onnx",
            "https://hf-mirror.com/datalab-to/indra-ocr/resolve/v1.1.0/det.onnx",
        ],
        bytes_expected: 389_948,
        sha256: Some("f2bd4cba23e9c328cf7ec824634e423e191a2c6aacbbf35c62bd79dd70f62f59"),
    },
    Artifact {
        name: "rec.onnx",
        urls: &[
            "https://huggingface.co/datalab-to/indra-ocr/resolve/v1.1.0/rec.onnx",
            "https://hf-mirror.com/datalab-to/indra-ocr/resolve/v1.1.0/rec.onnx",
        ],
        bytes_expected: 5_042_161,
        sha256: Some("475972a2710be4a3cbfeb2bc7f3624ffd563de07de42476fc7a290d4e1ece484"),
    },
    Artifact {
        name: "rec_cjk.onnx",
        urls: &[
            "https://huggingface.co/datalab-to/indra-ocr/resolve/v1.1.0/rec_cjk.onnx",
            "https://hf-mirror.com/datalab-to/indra-ocr/resolve/v1.1.0/rec_cjk.onnx",
        ],
        bytes_expected: 10_176_018,
        sha256: Some("4a035516a17e62cdf33a4fe2d88077654cb8841cff024685cdef4e351694ef50"),
    },
    Artifact {
        name: "rec_rtl.onnx",
        urls: &[
            "https://huggingface.co/datalab-to/indra-ocr/resolve/v1.1.0/rec_rtl.onnx",
            "https://hf-mirror.com/datalab-to/indra-ocr/resolve/v1.1.0/rec_rtl.onnx",
        ],
        bytes_expected: 4_812_710,
        sha256: Some("468796bd684486a8388c30c8b4ed2a26e3eeb1f8ff02831c24c2e12c79a2a834"),
    },
    Artifact {
        name: "rec_world.onnx",
        urls: &[
            "https://huggingface.co/datalab-to/indra-ocr/resolve/v1.1.0/rec_world.onnx",
            "https://hf-mirror.com/datalab-to/indra-ocr/resolve/v1.1.0/rec_world.onnx",
        ],
        bytes_expected: 5_627_990,
        sha256: Some("c8d1e59f21b04966f179d2d2d8e22b230d22110b3f89855bbbf2624a58c40043"),
    },
    Artifact {
        name: "script.onnx",
        urls: &[
            "https://huggingface.co/datalab-to/indra-ocr/resolve/v1.1.0/script.onnx",
            "https://hf-mirror.com/datalab-to/indra-ocr/resolve/v1.1.0/script.onnx",
        ],
        bytes_expected: 191_630,
        sha256: Some("b6ce4e926e19c243faaf37bd46880398bf2cb4ae56a29e1177a3b017c1280e36"),
    },
    Artifact {
        name: "orient.onnx",
        urls: &[
            "https://huggingface.co/datalab-to/indra-ocr/resolve/v1.1.0/orient.onnx",
            "https://hf-mirror.com/datalab-to/indra-ocr/resolve/v1.1.0/orient.onnx",
        ],
        bytes_expected: 371_802,
        sha256: Some("20eaf670f55a7cf9facfe1bdd6c7b8e40be75bbf4d48a3df43f66aa3bbd874fd"),
    },
    Artifact {
        name: "meta.json",
        urls: &[
            "https://huggingface.co/datalab-to/indra-ocr/resolve/v1.1.0/meta.json",
            "https://hf-mirror.com/datalab-to/indra-ocr/resolve/v1.1.0/meta.json",
        ],
        bytes_expected: 211_869,
        sha256: Some("4ce4c94b46f5d644033c02e718322014ff4081270f385038c571912b56696339"),
    },
];

pub const SMART_ARTIFACTS: [Artifact; 3] = [
    Artifact {
        name: "model.onnx",
        urls: &[
            "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/onnx/model_quantized.onnx",
            "https://hf-mirror.com/Xenova/bge-small-en-v1.5/resolve/main/onnx/model_quantized.onnx",
        ],
        bytes_expected: 34_014_426,
        sha256: Some("6c9c6101a956d62dfb5e7190c538226c0c5bb9cb27b651234b6df063ee7dbfe4"),
    },
    Artifact {
        name: "tokenizer.json",
        urls: &[
            "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/tokenizer.json",
            "https://hf-mirror.com/Xenova/bge-small-en-v1.5/resolve/main/tokenizer.json",
        ],
        bytes_expected: 711_396,
        sha256: Some("d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66"),
    },
    Artifact {
        name: "config.json",
        urls: &[
            "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/config.json",
            "https://hf-mirror.com/Xenova/bge-small-en-v1.5/resolve/main/config.json",
        ],
        bytes_expected: 683,
        sha256: Some("fa73f90bf92c8cace1fbcb709626306f2bdbc9ea3e5b5f94b440df9b6aa56350"),
    },
];

/// Optional ONNX visual embedding pack (CLIP ViT-B/32 int8).
///
/// The vision and text encoders must share one embedding space. `tokenizer.json`
/// is the matching CLIP byte-level BPE tokenizer.
pub const VISUAL_ARTIFACTS: [Artifact; 4] = [
    Artifact {
        name: "vision_model.onnx",
        urls: &[
            "https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/main/onnx/vision_model_quantized.onnx",
            "https://hf-mirror.com/Xenova/clip-vit-base-patch32/resolve/main/onnx/vision_model_quantized.onnx",
        ],
        bytes_expected: 89_117_001,
        sha256: Some("98ae9acf1f7e3233fac66f3b965e5e0b1ebe744527dcf5e3b5ff0e4ef80617be"),
    },
    Artifact {
        name: "text_model.onnx",
        urls: &[
            "https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/main/onnx/text_model_quantized.onnx",
            "https://hf-mirror.com/Xenova/clip-vit-base-patch32/resolve/main/onnx/text_model_quantized.onnx",
        ],
        bytes_expected: 64_504_507,
        sha256: Some("c5caef8bde14268ad23f01116dd72135abbf6182d14b407e375336838372bd58"),
    },
    Artifact {
        name: "tokenizer.json",
        urls: &[
            "https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/main/tokenizer.json",
            "https://hf-mirror.com/Xenova/clip-vit-base-patch32/resolve/main/tokenizer.json",
        ],
        bytes_expected: 2_224_119,
        sha256: Some("f7f3b7af117d467b58374797691a6438d3e6b9e9cef800dfd5dced7f697a90cd"),
    },
    Artifact {
        name: "config.json",
        urls: &[
            "https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/main/config.json",
            "https://hf-mirror.com/Xenova/clip-vit-base-patch32/resolve/main/config.json",
        ],
        bytes_expected: 4_524,
        sha256: Some("493ef57ff783e42d1530c91b53469b7fdf8db8a9c1408e86998fcb7899a4f495"),
    },
];

#[derive(Debug, Clone, Serialize)]
pub struct ModelArtifact {
    pub name: String,
    pub url: String,
    pub bytes_expected: u64,
    pub bytes_on_disk: u64,
    pub present: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelProgress {
    pub file: String,
    pub downloaded: u64,
    pub total: u64,
    /// "verifying" while an existing artifact's checksum is being checked,
    /// "downloading" while bytes are being fetched.
    pub phase: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelsStatus {
    pub dir: String,
    pub ready: bool,
    pub downloading: bool,
    pub progress: Option<ModelProgress>,
    pub error: Option<String>,
    pub artifacts: Vec<ModelArtifact>,
    pub total_bytes: u64,
    pub total_present_bytes: u64,
}

/// Process-global guard so only one download thread can run at a time.
static DOWNLOADING: AtomicBool = AtomicBool::new(false);
static PROGRESS: OnceLock<Mutex<Option<ModelProgress>>> = OnceLock::new();
static LAST_ERROR: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn progress_slot() -> &'static Mutex<Option<ModelProgress>> {
    PROGRESS.get_or_init(|| Mutex::new(None))
}

fn error_slot() -> &'static Mutex<Option<String>> {
    LAST_ERROR.get_or_init(|| Mutex::new(None))
}

fn set_progress(progress: Option<ModelProgress>) {
    let mut guard = match progress_slot().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *guard = progress;
}

fn set_error(error: Option<String>) {
    let mut guard = match error_slot().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *guard = error;
}

fn current_progress() -> Option<ModelProgress> {
    match progress_slot().lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

fn current_error() -> Option<String> {
    match error_slot().lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

pub fn models_dir() -> PathBuf {
    MnemoConfig::config_dir().join("models")
}

/// Creates the models directory (0700 on unix) if needed.
fn ensure_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("Failed to create models directory {}: {}", dir.display(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("Failed to set permissions on {}: {}", dir.display(), e))?;
    }
    Ok(())
}

/// Current artifact inventory, derived entirely from disk.
pub fn status() -> ModelsStatus {
    status_for(&models_dir(), &ARTIFACTS)
}

/// Current optional OCR pack inventory.
pub fn ocr_status() -> ModelsStatus {
    status_for(&crate::ocr::pack_dir(), &OCR_ARTIFACTS)
}

pub fn indra_status() -> ModelsStatus {
    status_for(&crate::ocr_indra::pack_dir(), &INDRA_ARTIFACTS)
}

pub fn smart_dir() -> PathBuf {
    models_dir().join("smart")
}

pub fn smart_pack_present() -> bool {
    smart_dir().join("model.onnx").is_file()
        && smart_dir().join("tokenizer.json").is_file()
        && smart_dir().join("config.json").is_file()
}

pub fn smart_status() -> ModelsStatus {
    status_for(&smart_dir(), &SMART_ARTIFACTS)
}

pub fn visual_dir() -> PathBuf {
    models_dir().join("visual")
}

pub fn visual_pack_present() -> bool {
    let dir = visual_dir();
    dir.join("vision_model.onnx").is_file()
        && dir.join("text_model.onnx").is_file()
        && dir.join("tokenizer.json").is_file()
        && dir.join("config.json").is_file()
}

pub fn visual_status() -> ModelsStatus {
    status_for(&visual_dir(), &VISUAL_ARTIFACTS)
}

fn status_for(dir: &Path, artifacts_list: &[Artifact]) -> ModelsStatus {
    let dir = dir.to_path_buf();
    let mut artifacts = Vec::with_capacity(artifacts_list.len());
    let mut total_bytes = 0u64;
    let mut total_present_bytes = 0u64;

    for artifact in artifacts_list.iter() {
        let path = dir.join(artifact.name);
        let metadata = std::fs::metadata(&path).ok();
        let is_file = metadata.as_ref().map(|m| m.is_file()).unwrap_or(false);
        let bytes_on_disk = if is_file {
            metadata.map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };

        total_bytes = total_bytes.saturating_add(artifact.bytes_expected);
        total_present_bytes = total_present_bytes.saturating_add(bytes_on_disk);

        artifacts.push(ModelArtifact {
            name: artifact.name.to_string(),
            url: artifact.urls.first().copied().unwrap_or("").to_string(),
            bytes_expected: artifact.bytes_expected,
            bytes_on_disk,
            present: is_file,
        });
    }

    let ready = artifacts.iter().all(|artifact| artifact.present);

    ModelsStatus {
        dir: dir.to_string_lossy().to_string(),
        ready,
        downloading: DOWNLOADING.load(Ordering::Relaxed),
        progress: current_progress(),
        error: current_error(),
        artifacts,
        total_bytes,
        total_present_bytes,
    }
}

/// Starts a background download of every missing artifact.
///
/// Returns immediately. Calling this while a download is already running is a
/// no-op.
pub fn download_all() {
    start_download(models_dir(), &ARTIFACTS, "mnemo-model-download", "model");
}

/// Called once an OCR pack (PP-OCR or Indra) has finished downloading and
/// verifying.
static OCR_PACK_READY: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// Registers what to do when the OCR pack becomes available (start a crawl
/// so already-indexed images are OCR'd). First registration wins.
pub fn on_ocr_pack_ready(callback: impl Fn() + Send + Sync + 'static) {
    let _ = OCR_PACK_READY.set(Box::new(callback));
}

/// Starts a background download of the optional accurate OCR pack.
pub fn download_ocr_pack() {
    start_download(
        crate::ocr::pack_dir(),
        &OCR_ARTIFACTS,
        "mnemo-ocr-download",
        "OCR pack",
    );
}

/// Starts a background download of the optional Indra OCR pack.
pub fn download_indra_pack() {
    start_download(
        crate::ocr_indra::pack_dir(),
        &INDRA_ARTIFACTS,
        "mnemo-indra-download",
        "Indra OCR pack",
    );
}

/// Starts a background download of the optional ONNX smart embedding pack.
pub fn download_smart_pack() {
    start_download(
        smart_dir(),
        &SMART_ARTIFACTS,
        "mnemo-smart-download",
        "smart embedding pack",
    );
}

/// Starts a background download of the optional ONNX visual embedding pack.
pub fn download_visual_pack() {
    start_download(
        visual_dir(),
        &VISUAL_ARTIFACTS,
        "mnemo-visual-download",
        "visual embedding pack",
    );
}

fn start_download(
    dir: PathBuf,
    artifacts: &'static [Artifact],
    thread_name: &str,
    label: &'static str,
) {
    if DOWNLOADING.swap(true, Ordering::SeqCst) {
        return;
    }

    if let Err(e) = ensure_dir(&dir) {
        set_error(Some(e));
        DOWNLOADING.store(false, Ordering::SeqCst);
        return;
    }

    set_error(None);
    set_progress(None);

    let spawned = std::thread::Builder::new()
        .name(thread_name.to_string())
        .spawn(move || run_download(dir, artifacts, label));

    if let Err(e) = spawned {
        set_error(Some(format!(
            "Failed to start {} download thread: {}",
            label, e
        )));
        DOWNLOADING.store(false, Ordering::SeqCst);
    }
}

fn run_download(dir: PathBuf, artifacts: &'static [Artifact], label: &'static str) {
    match download_missing_for(&dir, artifacts, label) {
        Ok(()) => {
            set_progress(None);
            set_error(None);
            if (std::ptr::eq(artifacts, &OCR_ARTIFACTS[..])
                || std::ptr::eq(artifacts, &INDRA_ARTIFACTS[..]))
                && let Some(callback) = OCR_PACK_READY.get()
            {
                // Clear the flag first so a callback that checks status sees
                // the download as finished.
                DOWNLOADING.store(false, Ordering::SeqCst);
                callback();
            }
        }
        Err(e) => {
            set_progress(None);
            set_error(Some(e));
        }
    }
    DOWNLOADING.store(false, Ordering::SeqCst);
}

fn download_missing_for(dir: &Path, artifacts: &[Artifact], label: &str) -> Result<(), String> {
    ensure_dir(dir)?;

    let missing_bytes: u64 = artifacts
        .iter()
        .filter(|artifact| !dir.join(artifact.name).is_file())
        .map(|artifact| artifact.bytes_expected)
        .sum();
    if missing_bytes > 0
        && let Some(free) = free_bytes(dir)
        && free < missing_bytes.saturating_add(64 * 1024 * 1024)
    {
        return Err(format!(
            "Not enough disk space for {} files: need {} MB, have {} MB",
            label,
            missing_bytes / (1024 * 1024),
            free / (1024 * 1024)
        ));
    }

    let total_bytes: u64 = artifacts.iter().map(|a| a.bytes_expected).sum();
    let mut completed_bytes: u64 = 0;
    // Always verify an existing artifact. A truncated or corrupt final file is
    // removed and downloaded again instead of being trusted by name alone.
    for artifact in artifacts {
        download_one(dir, artifact, total_bytes, completed_bytes)?;
        completed_bytes = completed_bytes.saturating_add(artifact.bytes_expected);
    }

    Ok(())
}

#[cfg(unix)]
fn free_bytes(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } == 0 {
        Some(stat.f_bavail as u64 * stat.f_frsize as u64)
    } else {
        None
    }
}

#[cfg(not(unix))]
fn free_bytes(_path: &Path) -> Option<u64> {
    None
}

fn download_one(
    dir: &Path,
    artifact: &Artifact,
    total_bytes: u64,
    completed_before: u64,
) -> Result<(), String> {
    let final_path = dir.join(artifact.name);
    if final_path.is_file() {
        // Show "verifying" instead of a misleading 0% while the existing
        // file's checksum is being read.
        set_progress(Some(ModelProgress {
            file: artifact.name.to_string(),
            downloaded: completed_before,
            total: total_bytes,
            phase: "verifying".to_string(),
        }));
        if sha256_matches(&final_path, artifact.sha256)? {
            set_progress(Some(ModelProgress {
                file: artifact.name.to_string(),
                downloaded: completed_before.saturating_add(artifact.bytes_expected),
                total: total_bytes,
                phase: "downloading".to_string(),
            }));
            return Ok(());
        }
        let _ = std::fs::remove_file(&final_path);
    }

    let part_path = dir.join(format!("{}.part", artifact.name));
    let mut downloaded = part_path.metadata().map(|meta| meta.len()).unwrap_or(0);
    if downloaded > artifact.bytes_expected && artifact.bytes_expected > 0 {
        let _ = std::fs::remove_file(&part_path);
        downloaded = 0;
    }

    set_progress(Some(ModelProgress {
        file: artifact.name.to_string(),
        downloaded: completed_before.saturating_add(downloaded),
        total: total_bytes,
        phase: "downloading".to_string(),
    }));

    let mut last_error = None;
    for url in artifact.urls {
        match download_url(
            url,
            artifact,
            &part_path,
            &mut downloaded,
            completed_before,
            total_bytes,
        ) {
            Ok(()) => {
                if sha256_matches(&part_path, artifact.sha256)? {
                    std::fs::rename(&part_path, &final_path).map_err(|e| {
                        format!("Failed to finalize {}: {}", final_path.display(), e)
                    })?;
                    set_progress(Some(ModelProgress {
                        file: artifact.name.to_string(),
                        downloaded: completed_before.saturating_add(artifact.bytes_expected),
                        total: total_bytes,
                        phase: "downloading".to_string(),
                    }));
                    return Ok(());
                }
                let _ = std::fs::remove_file(&part_path);
                downloaded = 0;
                last_error = Some(format!("Checksum mismatch for {}", artifact.name));
            }
            Err(error) => {
                last_error = Some(error);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| format!("Download failed for {}", artifact.name)))
}

fn download_url(
    url: &str,
    artifact: &Artifact,
    part_path: &Path,
    downloaded: &mut u64,
    completed_before: u64,
    total_bytes: u64,
) -> Result<(), String> {
    let mut request = ureq::get(url).timeout(Duration::from_secs(600));
    if *downloaded > 0 {
        request = request.set("Range", &format!("bytes={}-", *downloaded));
    }
    let response = request
        .call()
        .map_err(|e| format!("Download failed for {}: {}", artifact.name, e))?;

    let status = response.status();
    let (append, total) = match status {
        206 => {
            let total = response
                .header("Content-Range")
                .and_then(parse_content_range_total)
                .or_else(|| {
                    response
                        .header("Content-Length")
                        .and_then(|value| value.parse::<u64>().ok())
                        .map(|len| len.saturating_add(*downloaded))
                })
                .unwrap_or(artifact.bytes_expected);
            (true, total)
        }
        200 => {
            let _ = std::fs::remove_file(part_path);
            *downloaded = 0;
            let total = response
                .header("Content-Length")
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
                .unwrap_or(artifact.bytes_expected);
            (false, total)
        }
        other => {
            return Err(format!(
                "Unexpected HTTP status {} for {}",
                other, artifact.name
            ));
        }
    };

    let mut reader = response.into_reader();
    let mut file = if append {
        std::fs::OpenOptions::new()
            .append(true)
            .open(part_path)
            .map_err(|e| format!("Failed to reopen {}: {}", part_path.display(), e))?
    } else {
        std::fs::File::create(part_path)
            .map_err(|e| format!("Failed to create {}: {}", part_path.display(), e))?
    };

    let mut buffer = vec![0u8; CHUNK_BYTES];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|e| format!("Download interrupted for {}: {}", artifact.name, e))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .map_err(|e| format!("Failed to write {}: {}", part_path.display(), e))?;
        *downloaded = downloaded.saturating_add(read as u64);
        set_progress(Some(ModelProgress {
            file: artifact.name.to_string(),
            downloaded: completed_before.saturating_add(*downloaded),
            total: total_bytes,
            phase: "downloading".to_string(),
        }));
    }
    if total > 0 && *downloaded < total {
        return Err(format!(
            "Download interrupted for {}: expected {} bytes, got {}",
            artifact.name, total, downloaded
        ));
    }
    file.flush()
        .map_err(|e| format!("Failed to flush {}: {}", part_path.display(), e))?;
    Ok(())
}

fn parse_content_range_total(value: &str) -> Option<u64> {
    value.rsplit('/').next()?.parse::<u64>().ok()
}

fn sha256_matches(path: &Path, expected: Option<&str>) -> Result<bool, String> {
    let Some(expected) = expected else {
        return Ok(true);
    };
    let actual = sha256_file(path)?;
    Ok(actual.eq_ignore_ascii_case(expected))
}

fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let bytes =
        std::fs::read(path).map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for &byte in digest.as_slice() {
        use std::fmt::Write as _;
        let _ = write!(hex, "{:02x}", byte);
    }
    Ok(hex)
}

/// Removes every model artifact and resets progress/error state.
pub fn delete_all() -> Result<(), String> {
    if DOWNLOADING.load(Ordering::SeqCst) {
        return Err("A model download is in progress. Try again once it finishes.".to_string());
    }

    let dir = models_dir();
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .map_err(|e| format!("Failed to delete {}: {}", dir.display(), e))?;
    }
    ensure_dir(&dir)?;

    set_progress(None);
    set_error(None);
    Ok(())
}

#[tauri::command]
pub async fn get_models_status() -> Result<ModelsStatus, String> {
    Ok(status())
}

#[tauri::command]
pub async fn download_models() -> Result<(), String> {
    download_all();
    Ok(())
}

#[tauri::command]
pub async fn delete_models() -> Result<(), String> {
    delete_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_artifacts_manifest_sanity() {
        assert_eq!(SMART_ARTIFACTS.len(), 3);
        assert!(SMART_ARTIFACTS.iter().all(|a| a.sha256.is_some()));
        assert!(SMART_ARTIFACTS.iter().all(|a| a.bytes_expected > 0));
        assert!(SMART_ARTIFACTS.iter().all(|a| !a.urls.is_empty()));
    }

    #[test]
    fn visual_artifacts_manifest_sanity() {
        assert_eq!(VISUAL_ARTIFACTS.len(), 4);
        assert!(VISUAL_ARTIFACTS.iter().all(|a| a.sha256.is_some()));
        assert!(VISUAL_ARTIFACTS.iter().all(|a| a.bytes_expected > 0));
        assert!(VISUAL_ARTIFACTS.iter().all(|a| !a.urls.is_empty()));
    }

    #[test]
    fn sha256_matches_detects_corruption() {
        let dir = std::env::temp_dir().join("mnemo-model-test-corrupt");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("artifact.bin");
        std::fs::write(&path, b"hello").unwrap();
        let digest = sha256_file(&path).unwrap();
        assert!(sha256_matches(&path, Some(&digest)).unwrap());
        assert!(
            !sha256_matches(
                &path,
                Some("0000000000000000000000000000000000000000000000000000000000000000")
            )
            .unwrap()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sha256_matches_skips_missing_checksum() {
        let dir = std::env::temp_dir().join("mnemo-model-test-skip");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("artifact.bin");
        std::fs::write(&path, b"hello").unwrap();
        assert!(sha256_matches(&path, None).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod download_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn spawn_server<F>(handler: F) -> String
    where
        F: Fn(std::net::TcpStream) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                handler(stream);
            }
        });
        format!("http://{}", addr)
    }

    #[test]
    fn download_resumes_from_partial_file() {
        let full = b"hello world";
        let url = spawn_server(move |mut stream| {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let request = String::from_utf8_lossy(&buf);
            let start = request
                .lines()
                .find_map(|line| line.strip_prefix("Range: bytes="))
                .and_then(|value| value.trim_end_matches('-').parse::<usize>().ok())
                .unwrap_or(0);
            let body = &full[start..];
            let response = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                start,
                full.len() - 1,
                full.len(),
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(body);
        });

        let dir = std::env::temp_dir().join("mnemo-download-resume");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let part = dir.join("artifact.bin.part");
        std::fs::write(&part, b"hello ").unwrap();

        let url_static: &'static str = Box::leak(url.into_boxed_str());
        let artifact = Artifact {
            name: "artifact.bin",
            urls: Box::leak(vec![url_static].into_boxed_slice()),
            bytes_expected: full.len() as u64,
            sha256: None,
        };
        download_one(&dir, &artifact, artifact.bytes_expected, 0).unwrap();
        assert_eq!(std::fs::read(dir.join("artifact.bin")).unwrap(), full);
        assert!(!part.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn download_falls_back_to_mirror() {
        let full = b"payload";
        let bad = spawn_server(|mut stream| {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        });
        let good = spawn_server(move |mut stream| {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                full.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(full);
        });

        let dir = std::env::temp_dir().join("mnemo-download-mirror");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bad_static: &'static str = Box::leak(bad.into_boxed_str());
        let good_static: &'static str = Box::leak(good.into_boxed_str());
        let artifact = Artifact {
            name: "artifact.bin",
            urls: Box::leak(vec![bad_static, good_static].into_boxed_slice()),
            bytes_expected: full.len() as u64,
            sha256: None,
        };
        download_one(&dir, &artifact, artifact.bytes_expected, 0).unwrap();
        assert_eq!(std::fs::read(dir.join("artifact.bin")).unwrap(), full);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn interrupted_download_keeps_partial_file() {
        let full = b"hello world";
        let url = spawn_server(move |mut stream| {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                full.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(&full[..5]);
        });

        let dir = std::env::temp_dir().join("mnemo-download-interrupted");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let url_static: &'static str = Box::leak(url.into_boxed_str());
        let artifact = Artifact {
            name: "artifact.bin",
            urls: Box::leak(vec![url_static].into_boxed_slice()),
            bytes_expected: full.len() as u64,
            sha256: None,
        };
        let result = download_one(&dir, &artifact, artifact.bytes_expected, 0);
        assert!(result.is_err());
        assert!(dir.join("artifact.bin.part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
