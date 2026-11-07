//! # PaddleOCR-rs
//!
//! An ONNX-based OCR engine using PaddleOCR models, providing text detection,
//! text recognition, and document orientation classification.
//!
//! ## Features
//!
//! - **Text Detection** -- Locates text regions in images using DBNet
//! - **Text Recognition** -- Recognizes text within detected regions using SVTR/CRNN
//! - **Document Orientation Classification** -- Detects document rotation (0, 90, 180, 270 degrees)
//! - **Hardware Acceleration** -- Supports CUDA, DirectML, OpenVINO, NNAPI, CoreML, and CANN
//! - **Concurrent Recognition** -- Parallel text recognition with a session pool
//!
//! ## Quick Start
//!
//! ```ignore
//! use paddleocr_rs_onnx::OcrEngine;
//!
//! let det_model = std::fs::read("ch_PP-OCRv4_det_infer.onnx")?;
//! let rec_model = std::fs::read("ch_PP-OCRv4_rec_infer.onnx")?;
//! let keys = std::fs::read("ppocr_keys_v1.txt")?;
//!
//! let engine = OcrEngine::new(&det_model, &rec_model, &keys)?;
//! let image = image::open("test.png")?;
//! let results = engine.recognize_all(&image, paddleocr_rs_onnx::OrderBy::Horizontal)?;
//!
//! for block in &results {
//!     println!("{} (confidence: {:.2})", block.text, block.confidence);
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## Feature Flags
//!
//! | Feature      | Description                              |
//! |--------------|------------------------------------------|
//! | `cuda`       | NVIDIA CUDA GPU acceleration             |
//! | `directml`   | Windows DirectML GPU acceleration        |
//! | `openvino`   | Intel OpenVINO acceleration              |
//! | `nnapi`      | Android NNAPI acceleration               |
//! | `coreml`     | Apple CoreML acceleration                |
//! | `cann`       | Huawei CANN / Ascend NPU acceleration    |
//!
//! No hardware acceleration features are enabled by default (CPU-only).
//!
use parking_lot::{Mutex, Condvar};
use std::collections::VecDeque;
use rayon::prelude::*;
use log::{info, warn};

pub mod stage_stats;
mod det;#[cfg(feature = "ffi")] pub mod ffi;
mod decode;
mod rec;
mod cls;
mod error;
pub use det::DetInput;
pub use error::PaddleOcrError;

pub use cls::{DocOrientation, OrientationResult, classify_orientation};
pub use decode::DecodedText;

use image::DynamicImage;
use serde::{Deserialize, Serialize};

/// A detected text region in an image.
///
/// Contains the bounding box coordinates (as four corner points) and a confidence
/// score from the text detection model. The four points are ordered as
/// top-left, top-right, bottom-right, bottom-left.
///
/// # Example
///
/// ```ignore
/// let region = TextRegion {
///     bbox: [[10.0, 20.0], [100.0, 20.0], [100.0, 50.0], [10.0, 50.0]],
///     confidence: 0.95,
/// };
/// assert!(region.confidence > 0.9);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextRegion {
    /// Four corner points of the bounding box: `[[x, y]; 4]`.
    /// Ordered as top-left, top-right, bottom-right, bottom-left.
    pub bbox: [[f32; 2]; 4],
    /// Detection confidence score in the range `[0.0, 1.0]`.
    pub confidence: f32,
}

/// A recognized text block with position and confidence information.
///
/// Produced by the full OCR pipeline ([`OcrEngine::recognize_all`]), each block
/// contains the recognized text, its confidence score, and an axis-aligned
/// bounding rectangle in the original image coordinates.
///
/// # Example
///
/// ```ignore
/// let block = OcrBlock {
///     text: "Hello".to_string(),
///     confidence: 0.98,
///     x: 10.0,
///     y: 20.0,
///     width: 200.0,
///     height: 40.0,
/// };
/// assert_eq!(block.text, "Hello");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrBlock {
    /// The recognized text content.
    pub text: String,
    /// Recognition confidence score in the range `[0.0, 1.0]`.
    pub confidence: f32,
    /// X coordinate of the top-left corner of the bounding rectangle (in pixels).
    pub x: f32,
    /// Y coordinate of the top-left corner of the bounding rectangle (in pixels).
    pub y: f32,
    /// Width of the bounding rectangle (in pixels).
    pub width: f32,
    /// Height of the bounding rectangle (in pixels).
    pub height: f32,
}

/// Ordering strategy for OCR text blocks.
///
/// Controls how detected and recognized text blocks are sorted in the output
/// of [`OcrEngine::recognize_all`].
///
/// # Example
///
/// ```ignore
/// let results = engine.recognize_all(&image, OrderBy::Horizontal)?;
/// // Results are sorted top-to-bottom, then left-to-right
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderBy {
    /// Sort by Y coordinate first, then by X coordinate (top-to-bottom, left-to-right).
    Horizontal,
    /// Sort by X coordinate first, then by Y coordinate (left-to-right, top-to-bottom).
    Vertical,
    /// Sort by recognition confidence, highest first.
    Score,
}

/// Hardware acceleration device for ONNX Runtime inference.
///
/// Controls which execution provider is used for model inference:
/// - `Cpu` — CPU-only inference (default, always available)
/// - `DirectML` — DirectML acceleration (Windows, DirectX 12 GPU)
/// - `Cuda` — CUDA acceleration (NVIDIA GPU)
/// - `OpenVINO` — Intel OpenVINO acceleration (Windows/Linux)
/// - `Nnapi` — Android NNAPI acceleration (Android)
/// - `Coreml` — Apple CoreML acceleration (macOS/iOS)
/// - `Cann` — Huawei CANN / Ascend NPU acceleration (Linux)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccelerationDevice {
    /// CPU-only inference (default, always available)
    Cpu,
    /// DirectML acceleration (Windows, DirectX 12 GPU)
    DirectML,
    /// CUDA acceleration (NVIDIA GPU)
    Cuda,
    /// Intel OpenVINO acceleration (Windows/Linux)
    OpenVINO,
    /// Android NNAPI acceleration (Android)
    Nnapi,
    /// Apple CoreML acceleration (macOS/iOS)
    Coreml,
    /// Huawei CANN / Ascend NPU acceleration (Linux)
    Cann,
}

impl Default for AccelerationDevice {
    fn default() -> Self {
        Self::Cpu
    }
}

impl std::fmt::Display for AccelerationDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cpu => write!(f, "CPU"),
            Self::DirectML => write!(f, "DirectML"),
            Self::Cuda => write!(f, "CUDA"),
            Self::OpenVINO => write!(f, "OpenVINO"),
            Self::Nnapi => write!(f, "NNAPI"),
            Self::Coreml => write!(f, "CoreML"),
            Self::Cann => write!(f, "CANN"),
        }
    }
}

// ort permits a single environment per process; retain it across all engines,
// classifiers, and availability probes, including concurrent initialization.
//
// Mnemo patch: the environment is public so other `ort` users in the same
// process (Mnemo's Indra OCR) share it instead of failing with "only one
// environment is allowed per process".
pub fn shared_environment() -> ort::Result<ort::environment::Environment> {
    static ENVIRONMENT: Mutex<Option<ort::environment::Environment>> = Mutex::new(None);
    let mut environment = ENVIRONMENT.lock();
    if environment.is_none() {
        *environment = Some(ort::init().build()?);
    }
    Ok(environment.as_ref().unwrap().clone())
}

fn session_builder() -> ort::Result<ort::session::builder::SessionBuilder> {
    ort::session::Session::builder(&shared_environment()?)
}

#[cfg(test)]
mod runtime_tests {
    use super::*;

    #[test]
    fn concurrent_session_builders_share_environment() {
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| scope.spawn(|| {
                    session_builder().unwrap();
                    session_builder().unwrap();
                }))
                .collect();
            for handle in handles {
                handle.join().unwrap();
            }
        });
    }

    #[test]
    fn disabled_acceleration_features_fall_back_to_cpu() {
        for (device, enabled) in [
            (AccelerationDevice::DirectML, cfg!(feature = "directml")),
            (AccelerationDevice::Cuda, cfg!(feature = "cuda")),
            (AccelerationDevice::OpenVINO, cfg!(feature = "openvino")),
            (AccelerationDevice::Nnapi, cfg!(feature = "nnapi")),
            (AccelerationDevice::Coreml, cfg!(feature = "coreml")),
            (AccelerationDevice::Cann, cfg!(feature = "cann")),
        ] {
            if !enabled {
                assert!(!device.is_available());
                configure_session_builder(session_builder().unwrap(), device).unwrap();
            }
        }
    }
}

macro_rules! ep_available {
    ($feature:literal, $ep:ty) => {{
        #[cfg(feature = $feature)]
        {
            session_builder()
                .ok()
                .and_then(|b| {
                    b.with_execution_providers([<$ep>::default().build()])
                        .ok()
                })
                .is_some()
        }
        #[cfg(not(feature = $feature))]
        {
            false
        }
    }};
}

impl AccelerationDevice {
    /// Parse a device string (case-insensitive).
    ///
    /// Supported values: `"cpu"`, `"cuda"`/`"nvidia"`, `"directml"`/`"dml"`,
    /// `"openvino"`/`"open-vino"`/`"ov"`, `"nnapi"`, `"coreml"`/`"apple"`,
    /// `"cann"`/`"ascend"`/`"huawei"`.
    ///
    /// # Returns
    ///
    /// `Some(device)` if the string is recognized, `None` otherwise.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use paddleocr_rs_onnx::AccelerationDevice;
    ///
    /// assert_eq!(AccelerationDevice::from_str_loose("CUDA"), Some(AccelerationDevice::Cuda));
    /// assert_eq!(AccelerationDevice::from_str_loose("nvidia"), Some(AccelerationDevice::Cuda));
    /// assert_eq!(AccelerationDevice::from_str_loose("unknown"), None);
    /// ```
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "cpu" => Some(Self::Cpu),
            "cuda" | "nvidia" => Some(Self::Cuda),
            "directml" | "dml" => Some(Self::DirectML),
            "openvino" | "open-vino" | "ov" => Some(Self::OpenVINO),
            "nnapi" => Some(Self::Nnapi),
            "coreml" | "apple" => Some(Self::Coreml),
            "cann" | "ascend" | "huawei" => Some(Self::Cann),
            _ => None,
        }
    }

    /// Check if this acceleration device is available at runtime.
    ///
    /// Checks two conditions:
    /// 1. The corresponding Cargo feature flag is enabled at compile time.
    /// 2. The ONNX Runtime execution provider can be initialized (runtime check).
    ///
    /// `Cpu` is always available.
    ///
    /// # Returns
    ///
    /// `true` if the device is available for use.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use paddleocr_rs_onnx::AccelerationDevice;
    ///
    /// if AccelerationDevice::Cuda.is_available() {
    ///     let engine = OcrEngine::new_with_device(&det, &rec, &keys, AccelerationDevice::Cuda)?;
    /// }
    /// ```
    pub fn is_available(&self) -> bool {
        match self {
            Self::Cpu => true,
            Self::DirectML => ep_available!("directml", ort::ep::DirectML),
            Self::Cuda => ep_available!("cuda", ort::ep::CUDA),
            Self::OpenVINO => ep_available!("openvino", ort::ep::OpenVINO),
            Self::Nnapi => ep_available!("nnapi", ort::ep::NNAPI),
            Self::Coreml => ep_available!("coreml", ort::ep::CoreML),
            Self::Cann => ep_available!("cann", ort::ep::CANN),
        }
    }

    /// List all acceleration devices available in the current environment.
    ///
    /// Returns a vector of [`AccelerationDevice`] variants that pass both the
    /// compile-time feature check and the runtime EP availability check.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use paddleocr_rs_onnx::AccelerationDevice;
    ///
    /// let available = AccelerationDevice::available_devices();
    /// for device in &available {
    ///     println!("{} is available", device);
    /// }
    /// ```
    pub fn available_devices() -> Vec<Self> {
        let all = [
            Self::Cpu,
            Self::DirectML,
            Self::Cuda,
            Self::OpenVINO,
            Self::Nnapi,
            Self::Coreml,
            Self::Cann,
        ];
        all.into_iter().filter(|d| d.is_available()).collect()
    }
}

/// Mnemo patch: host-thread tuning for the CPU sessions, from the environment so it
/// can be measured: `MNEMO_ORT_INTRA` (threads per session) and `MNEMO_ORT_SPIN=0`.
fn configure_cpu(
    builder: ort::session::builder::SessionBuilder,
) -> Result<ort::session::builder::SessionBuilder, PaddleOcrError> {
    let mut builder = builder;
    if let Some(n) = std::env::var("MNEMO_ORT_INTRA").ok().and_then(|v| v.parse::<usize>().ok()).filter(|n| *n > 0) {
        builder = builder.with_intra_threads(n).map_err(|e| PaddleOcrError::General(e.to_string()))?;
    }
    if std::env::var("MNEMO_ORT_SPIN").as_deref() == Ok("0") {
        builder = builder.with_intra_op_spinning(false).map_err(|e| PaddleOcrError::General(e.to_string()))?;
    }
    Ok(builder)
}

/// Mnemo patch: CUDA provider with the cuDNN algorithm search selectable through
/// `MNEMO_CUDA_ALGO` (`exhaustive` = ORT default, `heuristic`, `default`).
fn configure_cuda(
    builder: ort::session::builder::SessionBuilder,
) -> Result<ort::session::builder::SessionBuilder, PaddleOcrError> {
    #[cfg(feature = "cuda")]
    {
        use ort::ep::cuda::ConvAlgorithmSearch as S;
        // The GPU helper shares the machine with the CPU engine; a few host
        // threads are enough for it (`MNEMO_GPU_ORT_THREADS`).
        let builder = match std::env::var("MNEMO_GPU_ORT_THREADS").ok().and_then(|v| v.parse::<usize>().ok()) {
            Some(n) if n > 0 => builder
                .with_intra_threads(n)
                .and_then(|b| b.with_intra_op_spinning(false))
                .map_err(|e| PaddleOcrError::General(e.to_string()))?,
            _ => builder,
        };
        let search = match std::env::var("MNEMO_CUDA_ALGO").unwrap_or_default().as_str() {
            "heuristic" => S::Heuristic,
            "default" => S::Default,
            _ => S::Exhaustive,
        };
        let ep = ort::ep::CUDA::default().with_conv_algorithm_search(search).build();
        // Mnemo's GPU helper must never silently run on the CPU: with
        // `MNEMO_CUDA_STRICT` a provider that fails to register is an error.
        let strict = std::env::var_os("MNEMO_CUDA_STRICT").is_some();
        let ep = if strict { ep.error_on_failure() } else { ep };
        match builder.clone().with_execution_providers([ep]) {
            Ok(b) => Ok(b),
            Err(e) if strict => Err(PaddleOcrError::General(format!("CUDA provider failed: {e}"))),
            Err(e) => {
                warn!("[ep] CUDA unavailable, falling back to CPU: {}", e);
                Ok(builder)
            }
        }
    }
    #[cfg(not(feature = "cuda"))]
    {
        if std::env::var_os("MNEMO_CUDA_STRICT").is_some() {
            return Err(PaddleOcrError::General("this build has no CUDA support".into()));
        }
        warn!("[ep] CUDA feature disabled, falling back to CPU");
        Ok(builder)
    }
}

/// Configure execution providers on a session builder.
///
/// If the requested EP is not available (not compiled into ORT, missing runtime
/// libraries, or unsupported on this platform), a warning is logged and the
/// session falls back to CPU.
fn configure_session_builder(
    builder: ort::session::builder::SessionBuilder,
    device: AccelerationDevice,
) -> Result<ort::session::builder::SessionBuilder, PaddleOcrError> {
    macro_rules! configure_ep {
        ($feature:literal, $ep:ty) => {{
            #[cfg(feature = $feature)]
            {
                info!("[ep] configuring {} execution provider", device);
                let ep = <$ep>::default().build();
                match builder.clone().with_execution_providers([ep]) {
                    Ok(b) => Ok(b),
                    Err(e) => {
                        warn!("[ep] {} unavailable, falling back to CPU: {}", device, e);
                        Ok(builder)
                    }
                }
            }
            #[cfg(not(feature = $feature))]
            {
                warn!("[ep] {} feature disabled, falling back to CPU", device);
                Ok(builder)
            }
        }};
    }

    match device {
        AccelerationDevice::Cpu => configure_cpu(builder),
        AccelerationDevice::DirectML => configure_ep!("directml", ort::ep::DirectML),
        AccelerationDevice::Cuda => configure_cuda(builder),
        AccelerationDevice::OpenVINO => configure_ep!("openvino", ort::ep::OpenVINO),
        AccelerationDevice::Nnapi => configure_ep!("nnapi", ort::ep::NNAPI),
        AccelerationDevice::Coreml => configure_ep!("coreml", ort::ep::CoreML),
        AccelerationDevice::Cann => configure_ep!("cann", ort::ep::CANN),
    }
}

/// The main OCR engine combining text detection and recognition.
///
/// `OcrEngine` manages ONNX sessions for both the detection model (DBNet) and
/// the recognition model (SVTR/CRNN). It maintains a pool of recognition sessions
/// for concurrent inference across multiple detected text regions.
///
/// # Thread Safety
///
/// `OcrEngine` is `Send + Sync`. The internal sessions are protected by mutexes,
/// and the recognition session pool uses a `Condvar` for efficient blocking when
/// all sessions are in use.
///
/// # Example
///
/// ```ignore
/// use paddleocr_rs_onnx::{OcrEngine, OrderBy};
///
/// let det_model = std::fs::read("ch_PP-OCRv4_det_infer.onnx")?;
/// let rec_model = std::fs::read("ch_PP-OCRv4_rec_infer.onnx")?;
/// let keys = std::fs::read("ppocr_keys_v1.txt")?;
///
/// let engine = OcrEngine::new(&det_model, &rec_model, &keys)?;
///
/// let image = image::open("screenshot.png")?;
/// let blocks = engine.recognize_all(&image, OrderBy::Horizontal)?;
///
/// for block in &blocks {
///     println!("[{:.0},{:.0}] {} ({:.0}%)",
///         block.x, block.y, block.text, block.confidence * 100.0);
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct OcrEngine {
    /// Detector sessions. A pool, like the recogniser's: a single locked session
    /// serialised detection across every extraction thread (Mnemo patch).
    det_sessions: Mutex<VecDeque<ort::session::Session>>,
    det_sessions_cvar: Condvar,
    rec_sessions: Mutex<VecDeque<ort::session::Session>>,
    rec_sessions_cvar: Condvar,
    det_input: String,
    det_output: String,
    rec_input: String,
    rec_output: String,
    /// Mnemo patch: set when the recognition model has two reduced outputs (argmax, max).
    rec_argmax: Option<(String, String)>,
    rec_height: u32,
    rec_width: Option<u32>,
    keys: Vec<String>,
    device: AccelerationDevice,
}

impl OcrEngine {
    /// Create a new OCR engine with default (CPU) acceleration.
    ///
    /// # Arguments
    ///
    /// * `det_model` -- Raw bytes of the detection ONNX model (e.g., `ch_PP-OCRv4_det_infer.onnx`).
    /// * `rec_model` -- Raw bytes of the recognition ONNX model (e.g., `ch_PP-OCRv4_rec_infer.onnx`).
    /// * `keys_data` -- UTF-8 bytes of the character dictionary file (e.g., `ppocr_keys_v1.txt`),
    ///   one character per line.
    ///
    /// # Returns
    ///
    /// A ready-to-use [`OcrEngine`] instance.
    ///
    /// # Errors
    ///
    /// Returns [`PaddleOcrError::Model`] if any model fails to load, or
    /// [`PaddleOcrError::General`] if the keys file is not valid UTF-8.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let det_model = std::fs::read("ch_PP-OCRv4_det_infer.onnx")?;
    /// let rec_model = std::fs::read("ch_PP-OCRv4_rec_infer.onnx")?;
    /// let keys = std::fs::read("ppocr_keys_v1.txt")?;
    /// let engine = OcrEngine::new(&det_model, &rec_model, &keys)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn new(det_model: &[u8], rec_model: &[u8], keys_data: &[u8]) -> Result<Self, PaddleOcrError> {
        Self::new_with_device(det_model, rec_model, keys_data, AccelerationDevice::default())
    }

    /// Create a new OCR engine with a specific hardware acceleration device.
    ///
    /// If the requested acceleration device is unavailable, the engine falls back
    /// to CPU inference with a warning logged.
    ///
    /// # Arguments
    ///
    /// * `det_model` -- Raw bytes of the detection ONNX model.
    /// * `rec_model` -- Raw bytes of the recognition ONNX model.
    /// * `keys_data` -- UTF-8 bytes of the character dictionary file.
    /// * `device` -- The desired [`AccelerationDevice`] for inference.
    ///
    /// # Returns
    ///
    /// A ready-to-use [`OcrEngine`] instance.
    ///
    /// # Errors
    ///
    /// Returns an error if any model fails to load or the keys data is invalid.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use paddleocr_rs_onnx::{OcrEngine, AccelerationDevice};
    ///
    /// let det_model = std::fs::read("ch_PP-OCRv4_det_infer.onnx")?;
    /// let rec_model = std::fs::read("ch_PP-OCRv4_rec_infer.onnx")?;
    /// let keys = std::fs::read("ppocr_keys_v1.txt")?;
    /// let engine = OcrEngine::new_with_device(
    ///     &det_model, &rec_model, &keys,
    ///     AccelerationDevice::Cuda,
    /// )?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn new_with_device(
        det_model: &[u8],
        rec_model: &[u8],
        keys_data: &[u8],
        device: AccelerationDevice,
    ) -> Result<Self, PaddleOcrError> {
        info!("[ocr] creating engine with device: {}", device);

        let det_session = configure_session_builder(session_builder()?, device)?
            .commit_from_memory(det_model)?;
        let rec_session = configure_session_builder(session_builder()?, device)?
            .commit_from_memory(rec_model)?;

        let det_input = det_session.inputs()[0].name().to_string();
        let det_output = det_session.outputs()[0].name().to_string();
        let rec_input = rec_session.inputs()[0].name().to_string();
        let rec_output = rec_session.outputs()[0].name().to_string();
        let rec_argmax = (rec_session.outputs().len() == 2).then(|| {
            (rec_session.outputs()[0].name().to_string(), rec_session.outputs()[1].name().to_string())
        });
        let rec_shape = rec_session.inputs()[0]
            .dtype()
            .tensor_shape()
            .cloned()
            .unwrap_or_else(|| vec![1_i64, 3, 48, 320].into());
        let rec_height = rec_shape.get(2).copied().unwrap_or(48).max(1) as u32;
        let rec_width = rec_shape
            .get(3)
            .copied()
            .filter(|dim| *dim > 0)
            .map(|dim| dim as u32);

        let keys_str = std::str::from_utf8(keys_data)?;
        let keys: Vec<String> = keys_str.lines().map(|s| s.to_string()).collect();

        // debug: validate keys vs model output
        let model_classes = rec_session.outputs()[0]
            .dtype().tensor_shape()
            .and_then(|s| s.last())
            .copied()
            .unwrap_or(0) as usize;
        info!(
            "[ocr] keys: {} lines, model output classes: {} (blank + {} chars)",
            keys.len(),
            model_classes,
            model_classes.saturating_sub(1),
        );
        if keys.len() + 1 != model_classes {
            warn!(
                "[ocr] WARNING: keys({}) + 1(blank) = {} != model_classes({})",
                keys.len(),
                keys.len() + 1,
                model_classes,
            );
        }

        // build session pool for concurrent recognition
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2);
                // Mobile optimization: use smaller pool size on Android/iOS
        let pool_size = {
            #[cfg(target_os = "android")]
            { 1 }
            #[cfg(target_os = "ios")]
            { 1 }
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            { std::env::var("MNEMO_REC_POOL").ok().and_then(|v| v.parse::<usize>().ok()).filter(|n| *n > 0).unwrap_or((cores / 2).max(1)) }
        };
        info!("[ocr] creating rec session pool of size {} (cores={})", pool_size, cores);

        let mut sessions = VecDeque::with_capacity(pool_size);
        sessions.push_back(rec_session);
        for _ in 1..pool_size {
            sessions.push_back(
                configure_session_builder(session_builder()?, device)?
                    .commit_from_memory(rec_model)?,
            );
        }

        let mut det_sessions = VecDeque::with_capacity(pool_size);
        det_sessions.push_back(det_session);
        for _ in 1..pool_size {
            det_sessions.push_back(
                configure_session_builder(session_builder()?, device)?
                    .commit_from_memory(det_model)?,
            );
        }

        Ok(Self {
            det_sessions: Mutex::new(det_sessions),
            det_sessions_cvar: Condvar::new(),
            rec_sessions: Mutex::new(sessions),
            rec_sessions_cvar: Condvar::new(),
            det_input,
            det_output,
            rec_input,
            rec_output,
            rec_argmax,
            rec_height,
            rec_width,
            keys,
            device,
        })
    }

    /// Returns the acceleration device used by this engine.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use paddleocr_rs_onnx::AccelerationDevice;
    ///
    /// # let det = std::fs::read("det.onnx").unwrap();
    /// # let rec = std::fs::read("rec.onnx").unwrap();
    /// # let keys = std::fs::read("keys.txt").unwrap();
    /// let engine = OcrEngine::new_with_device(&det, &rec, &keys, AccelerationDevice::Cuda)?;
    /// assert_eq!(engine.device(), AccelerationDevice::Cuda);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn device(&self) -> AccelerationDevice {
        self.device
    }

    /// Detect text regions in an image.
    ///
    /// Runs the DBNet detection model to find bounding boxes around text areas.
    /// This is a low-level method; prefer [`recognize_all`] for the full pipeline.
    ///
    /// # Arguments
    ///
    /// * `image` -- The input image to analyze.
    ///
    /// # Returns
    ///
    /// A vector of [`TextRegion`] instances, one per detected text area, sorted
    /// by detection confidence.
    ///
    /// # Errors
    ///
    /// Returns [`PaddleOcrError::Inference`] if the model execution fails, or
    /// [`PaddleOcrError::Preprocessing`] if image preprocessing fails.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let image = image::open("document.png")?;
    /// let regions = engine.detect_text_regions(&image)?;
    /// println!("Found {} text regions", regions.len());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// [`recognize_all`]: OcrEngine::recognize_all
    pub fn detect_text_regions(&self, image: &DynamicImage) -> Result<Vec<TextRegion>, PaddleOcrError> {
        let (w, h) = (image.width(), image.height());
        self.detect_prepared(det::prepare(image)?, w, h)
    }

    /// Mnemo patch: the detector input for an image. Cheap to compute on another
    /// thread while a session is busy; pair with [`detect_prepared`](Self::detect_prepared).
    pub fn prepare_detection(image: &DynamicImage) -> Result<DetInput, PaddleOcrError> {
        det::prepare(image)
    }

    /// Mnemo patch: detection from a prepared input. Only inference holds a
    /// session; the contour work runs outside any lock so extraction threads
    /// detect in parallel.
    pub fn detect_prepared(&self, input: DetInput, orig_w: u32, orig_h: u32) -> Result<Vec<TextRegion>, PaddleOcrError> {
        let scale = input.scale();
        let mut session = {
            let mut pool = self.det_sessions.lock();
            loop {
                if let Some(s) = pool.pop_front() {
                    break s;
                }
                self.det_sessions_cvar.wait(&mut pool);
            }
        };
        let result = det::infer(&mut session, input, &self.det_input, &self.det_output);
        {
            let mut pool = self.det_sessions.lock();
            pool.push_back(session);
            self.det_sessions_cvar.notify_one();
        }
        let (prob, out_h, out_w) = result?;
        Ok(det::finish(&prob, out_h, out_w, orig_w, orig_h, scale))
    }

    /// Recognize text within a single detected region.
    ///
    /// Performs perspective rectification on the region, runs the recognition model,
    /// and decodes the output using CTC decoding. This is a low-level method;
    /// prefer [`recognize_all`] for the full pipeline.
    ///
    /// # Arguments
    ///
    /// * `image` -- The original image.
    /// * `region` -- A [`TextRegion`] obtained from [`detect_text_regions`].
    ///
    /// # Returns
    ///
    /// A [`DecodedText`] containing the recognized text and its confidence score.
    ///
    /// # Errors
    ///
    /// Returns [`PaddleOcrError::DegenerateRegion`] if the bounding box is too small
    /// or degenerate, [`PaddleOcrError::Projection`] if perspective rectification fails,
    /// or [`PaddleOcrError::Inference`] if the model execution fails.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let regions = engine.detect_text_regions(&image)?;
    /// if let Some(region) = regions.first() {
    ///     let decoded = engine.recognize_text(&image, region)?;
    ///     println!("Text: {} (score: {:.2})", decoded.text, decoded.score);
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// [`detect_text_regions`]: OcrEngine::detect_text_regions
    /// [`recognize_all`]: OcrEngine::recognize_all
    pub fn recognize_text(&self, image: &DynamicImage, region: &TextRegion) -> Result<decode::DecodedText, PaddleOcrError> {
        self.recognize_text_rgb(&image.to_rgb8(), region)
    }

    /// Mnemo patch: [`recognize_text`](Self::recognize_text) for an image already in RGB8.
    pub fn recognize_text_rgb(&self, rgb: &image::RgbImage, region: &TextRegion) -> Result<decode::DecodedText, PaddleOcrError> {
        let t = std::time::Instant::now();
        let (data, width) = rec::preprocess_region_rgb(rgb, region, self.rec_height, self.rec_width)?;
        stage_stats::add(stage_stats::REC_PRE, t);

        // Pop a session from the pool, blocking until one is available
        let mut session = {
            let mut pool = self.rec_sessions.lock();
            loop {
                if let Some(s) = pool.pop_front() {
                    break s;
                }
                self.rec_sessions_cvar.wait(&mut pool);
            }
        };

        // A recogniser with on-device argmax outputs skips copying the full
        // [steps, classes] tensor.
        if let Some((idx_name, max_name)) = &self.rec_argmax {
            let t = std::time::Instant::now();
            let result = rec::run_recognition_batch_argmax(
                &mut session, data, 1, width, self.rec_height, &self.rec_input, idx_name, max_name,
            );
            stage_stats::add(stage_stats::REC_RUN, t);
            {
                let mut pool = self.rec_sessions.lock();
                pool.push_back(session);
                self.rec_sessions_cvar.notify_one();
            }
            let (idx, mx, _) = result?;
            let t = std::time::Instant::now();
            let decoded = decode::ctc_decode_argmax(&idx, &mx, &self.keys);
            stage_stats::add(stage_stats::REC_DEC, t);
            return Ok(decoded);
        }

        let t = std::time::Instant::now();
        let result = rec::run_recognition(&mut session, &data, width, self.rec_height, &self.rec_input, &self.rec_output);
        stage_stats::add(stage_stats::REC_RUN, t);
        // Always return the session to the pool, even when the run failed.
        {
            let mut pool = self.rec_sessions.lock();
            pool.push_back(session);
            self.rec_sessions_cvar.notify_one();
        }
        let result = result?;

        let t = std::time::Instant::now();
        let decoded = decode::ctc_decode(&result, &self.keys);
        stage_stats::add(stage_stats::REC_DEC, t);
        Ok(decoded)
    }

    /// Mnemo patch: like [`prepare_region`](Self::prepare_region) for an RGB8 image.
    pub fn prepare_region_rgb(&self, rgb: &image::RgbImage, region: &TextRegion) -> Result<PreparedCrop, PaddleOcrError> {
        let t = std::time::Instant::now();
        let (data, width) = rec::preprocess_region_rgb(rgb, region, self.rec_height, self.rec_width)?;
        stage_stats::add(stage_stats::REC_PRE, t);
        Ok(PreparedCrop { data, width })
    }

    /// Mnemo patch: crops a region into the recognition input without running the model.
    pub fn prepare_region(&self, image: &DynamicImage, region: &TextRegion) -> Result<PreparedCrop, PaddleOcrError> {
        let (data, width) = rec::preprocess_region(image, region, self.rec_height, self.rec_width)?;
        Ok(PreparedCrop { data, width })
    }

    /// Mnemo patch: recognises many prepared crops in padded batches of at most
    /// `max_batch`. Crops are sorted by width so padding stays small. Results come
    /// back in input order; a failed batch yields `None` for its crops.
    pub fn recognize_prepared(&self, crops: &[PreparedCrop], max_batch: usize) -> Vec<Option<decode::DecodedText>> {
        let h = self.rec_height as usize;
        let mut order: Vec<usize> = (0..crops.len()).collect();
        order.sort_by_key(|&i| crops[i].width);
        let mut out: Vec<Option<decode::DecodedText>> = vec![None; crops.len()];
        // Greedy chunks over the width-sorted crops: stop at `max_batch` crops or when
        // the padded batch would exceed a column budget (very wide crops otherwise
        // create multi-gigabyte `[n, t, classes]` activations).
        let col_budget: i64 = std::env::var("MNEMO_BATCH_COLS").ok().and_then(|v| v.parse().ok()).unwrap_or(16384);
        let mut chunks: Vec<&[usize]> = Vec::new();
        let mut start = 0usize;
        while start < order.len() {
            let mut end = start + 1;
            while end < order.len()
                && end - start < max_batch.max(1)
                && ((end - start + 1) as i64) * crops[order[end]].width <= col_budget
            {
                end += 1;
            }
            chunks.push(&order[start..end]);
            start = end;
        }
        for chunk in chunks {
            let w = chunk.iter().map(|&i| crops[i].width).max().unwrap_or(4) as usize;
            // Few distinct shapes keep GPU kernels and plans reusable.
            let w = w.div_ceil(rec_width_quant()) * rec_width_quant();
            let n_padded = chunk.len().div_ceil(rec_batch_quant()) * rec_batch_quant();
            let mut data = vec![0.0f32; n_padded * 3 * h * w];
            for (slot, &i) in chunk.iter().enumerate() {
                let cw = crops[i].width as usize;
                for c in 0..3 {
                    for y in 0..h {
                        let src = (c * h + y) * cw;
                        let dst = ((slot * 3 + c) * h + y) * w;
                        data[dst..dst + cw].copy_from_slice(&crops[i].data[src..src + cw]);
                    }
                }
            }
            let mut session = {
                let mut pool = self.rec_sessions.lock();
                loop {
                    if let Some(s) = pool.pop_front() {
                        break s;
                    }
                    self.rec_sessions_cvar.wait(&mut pool);
                }
            };
            if let Some((idx_name, max_name)) = &self.rec_argmax {
                let t = std::time::Instant::now();
                let result = rec::run_recognition_batch_argmax(
                    &mut session, data, n_padded, w as i64, self.rec_height, &self.rec_input, idx_name, max_name,
                );
                stage_stats::add(stage_stats::REC_RUN, t);
                {
                    let mut pool = self.rec_sessions.lock();
                    pool.push_back(session);
                    self.rec_sessions_cvar.notify_one();
                }
                let (idx, mx, steps) = match result { Ok(v) => v, Err(e) => { eprintln!("rec batch failed: {e}"); continue } };
                for (slot, &i) in chunk.iter().enumerate() {
                    let keep = (((steps as f64) * (crops[i].width as f64) / (w as f64)).ceil() as usize).clamp(1, steps);
                    let base = slot * steps;
                    out[i] = Some(decode::ctc_decode_argmax(&idx[base..base + keep], &mx[base..base + keep], &self.keys));
                }
                continue;
            }
            let result = rec::run_recognition_batch(
                &mut session, data, n_padded, w as i64, self.rec_height, &self.rec_input, &self.rec_output,
            );
            {
                let mut pool = self.rec_sessions.lock();
                pool.push_back(session);
                self.rec_sessions_cvar.notify_one();
            }
            let Ok((flat, steps, classes)) = result else { continue };
            for (slot, &i) in chunk.iter().enumerate() {
                // Keep only the timesteps that belong to this crop, not to its padding.
                let keep = ((steps as f64) * (crops[i].width as f64) / (w as f64)).ceil() as usize;
                let keep = keep.clamp(1, steps);
                let base = slot * steps * classes;
                let probs: Vec<Vec<f32>> = (0..keep)
                    .map(|t| flat[base + t * classes..base + (t + 1) * classes].to_vec())
                    .collect();
                out[i] = Some(decode::ctc_decode(&probs, &self.keys));
            }
        }
        out
    }

    /// Complete OCR pipeline: detection + recognition.
    ///
    /// Detects all text regions in the image, recognizes text in each region
    /// concurrently using a session pool, and returns the results sorted
    /// according to the specified ordering.
    ///
    /// When no text regions are detected, falls back to treating the entire
    /// image as a single text region.
    ///
    /// # Arguments
    ///
    /// * `image` -- The input image to process.
    /// * `order` -- The [`OrderBy`] strategy for sorting the output blocks.
    ///
    /// # Returns
    ///
    /// A vector of [`OcrBlock`] instances, each containing the recognized text,
    /// confidence, and bounding rectangle.
    ///
    /// # Errors
    ///
    /// Returns an error if detection or recognition fails for a non-degenerate region.
    /// Degenerate regions are silently skipped.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use paddleocr_rs_onnx::{OcrEngine, OrderBy};
    ///
    /// # let det = std::fs::read("det.onnx").unwrap();
    /// # let rec = std::fs::read("rec.onnx").unwrap();
    /// # let keys = std::fs::read("keys.txt").unwrap();
    /// let engine = OcrEngine::new(&det, &rec, &keys)?;
    /// let image = image::open("screenshot.png")?;
    ///
    /// // Read top-to-bottom, left-to-right
    /// let blocks = engine.recognize_all(&image, OrderBy::Horizontal)?;
    /// for block in &blocks {
    ///     println!("{}", block.text);
    /// }
    ///
    /// // Or sort by confidence
    /// let blocks = engine.recognize_all(&image, OrderBy::Score)?;
    /// if let Some(best) = blocks.first() {
    ///     println!("Most confident: {} ({:.0}%)", best.text, best.confidence * 100.0);
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn recognize_all(&self, image: &DynamicImage, order: OrderBy) -> Result<Vec<OcrBlock>, PaddleOcrError> {
        let regions = self.detect_text_regions(image)?;

        if regions.is_empty() {
            // Try full-image recognition as fallback
            return self.recognize_full_image(image)
                .map(|b| b.into_iter().collect())
                .map_err(|e| PaddleOcrError::General(e.to_string()));
        }

        let rgb = image.to_rgb8();
        let mut blocks: Vec<OcrBlock> = regions
            .par_iter()
            .filter_map(|region| {
                match self.recognize_text_rgb(&rgb, region) {
                    Ok(decoded) => {
                        if decoded.text.is_empty() {
                            None
                        } else {
                            let (x, y, width, height) = bbox_to_rect(&region.bbox);
                            Some(OcrBlock {
                                text: decoded.text,
                                confidence: decoded.score,
                                x,
                                y,
                                width,
                                height,
                            })
                        }
                    }
                    Err(PaddleOcrError::DegenerateRegion { .. }) => {
                        // Skip degenerate regions silently
                        None
                    }
                    Err(_) => None,
                }
            })
            .collect();

        match order {
            OrderBy::Horizontal => {
                blocks.sort_by(|a, b| {
                    a.y.partial_cmp(&b.y)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
                });
            }
            OrderBy::Vertical => {
                blocks.sort_by(|a, b| {
                    a.x.partial_cmp(&b.x)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal))
                });
            }
            OrderBy::Score => {
                blocks.sort_by(|a, b| {
                    b.confidence
                        .partial_cmp(&a.confidence)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            }
        }

        Ok(blocks)
    }

    fn recognize_full_image(&self, image: &DynamicImage) -> Result<Option<OcrBlock>, PaddleOcrError> {
        let width = image.width() as f32;
        let height = image.height() as f32;
        if width < 1.0 || height < 1.0 {
            return Ok(None);
        }

        let full_region = TextRegion {
            bbox: [
                [0.0, 0.0],
                [width - 1.0, 0.0],
                [width - 1.0, height - 1.0],
                [0.0, height - 1.0],
            ],
            confidence: 0.0,
        };
        let decoded = self.recognize_text(image, &full_region)?;
        if decoded.text.is_empty() {
            return Ok(None);
        }

        Ok(Some(OcrBlock {
            text: decoded.text,
            confidence: decoded.score,
            x: 0.0,
            y: 0.0,
            width,
            height,
        }))
    }
}

/// Convert a quadrilateral bounding box to an axis-aligned rectangle.
///
/// Returns `(x, y, width, height)` where `(x, y)` is the top-left corner.
fn bbox_to_rect(bbox: &[[f32; 2]; 4]) -> (f32, f32, f32, f32) {
    let min_x = bbox.iter().map(|p| p[0]).reduce(f32::min).unwrap_or(0.0);
    let min_y = bbox.iter().map(|p| p[1]).reduce(f32::min).unwrap_or(0.0);
    let max_x = bbox.iter().map(|p| p[0]).reduce(f32::max).unwrap_or(0.0);
    let max_y = bbox.iter().map(|p| p[1]).reduce(f32::max).unwrap_or(0.0);
    (min_x, min_y, max_x - min_x, max_y - min_y)
}

/// Document orientation classifier using PP-LCNet_x1_0_doc_ori model.
///
/// This classifier detects the orientation of document images (0°, 90°, 180°, 270°).
/// Useful for preprocessing documents before OCR when the scan/capture orientation
/// is unknown.
///
/// # Example
///
/// ```ignore
/// let model_data = std::fs::read("PP-LCNet_x1_0_doc_ori.onnx")?;
/// let classifier = DocOrientationClassifier::new(&model_data)?;
/// let result = classifier.classify(&image)?;
/// println!("Orientation: {}°, confidence: {}", result.orientation.angle(), result.confidence);
/// ```
pub struct DocOrientationClassifier {
    session: Mutex<ort::session::Session>,
    input_name: String,
    output_name: String,
    device: AccelerationDevice,
}

impl DocOrientationClassifier {
    /// Create a new orientation classifier from ONNX model data.
    ///
    /// # Arguments
    /// * `model_data` - Raw ONNX model bytes (PP-LCNet_x1_0_doc_ori.onnx)
    ///
    /// # Returns
    /// A new classifier instance ready to classify images.
    /// Create a new orientation classifier from ONNX model data.
    ///
    /// Uses the default CPU acceleration device. To use a specific device,
    /// see [`new_with_device`](Self::new_with_device).
    ///
    /// # Arguments
    ///
    /// * `model_data` -- Raw ONNX model bytes (e.g., `PP-LCNet_x1_0_doc_ori.onnx`).
    ///
    /// # Returns
    ///
    /// A new classifier instance ready to classify images.
    ///
    /// # Errors
    ///
    /// Returns [`PaddleOcrError::Model`] if the model fails to load.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let model_data = std::fs::read("PP-LCNet_x1_0_doc_ori.onnx")?;
    /// let classifier = DocOrientationClassifier::new(&model_data)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn new(model_data: &[u8]) -> Result<Self, PaddleOcrError> {
        Self::new_with_device(model_data, AccelerationDevice::default())
    }

    /// Create a new orientation classifier with a specific acceleration device.
    ///
    /// # Arguments
    ///
    /// * `model_data` -- Raw ONNX model bytes.
    /// * `device` -- The desired [`AccelerationDevice`] for inference.
    ///
    /// # Returns
    ///
    /// A new classifier instance ready to classify images.
    ///
    /// # Errors
    ///
    /// Returns [`PaddleOcrError::Model`] if the model fails to load.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use paddleocr_rs_onnx::{DocOrientationClassifier, AccelerationDevice};
    ///
    /// let model_data = std::fs::read("PP-LCNet_x1_0_doc_ori.onnx")?;
    /// let classifier = DocOrientationClassifier::new_with_device(
    ///     &model_data,
    ///     AccelerationDevice::Cuda,
    /// )?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn new_with_device(model_data: &[u8], device: AccelerationDevice) -> Result<Self, PaddleOcrError> {
        info!("[doc_ori] creating classifier with device: {}", device);

        let session = configure_session_builder(session_builder()?, device)?
            .commit_from_memory(model_data)?;

        let input_name = session.inputs()[0].name().to_string();
        let output_name = session.outputs()[0].name().to_string();

        // Log model info
        let input_shape = session.inputs()[0]
            .dtype()
            .tensor_shape()
            .cloned()
            .unwrap_or_default();
        let output_shape = session.outputs()[0]
            .dtype()
            .tensor_shape()
            .cloned()
            .unwrap_or_default();
        info!(
            "[doc_ori] model loaded: input {:?}, output {:?}",
            input_shape, output_shape
        );

        Ok(Self {
            session: Mutex::new(session),
            input_name,
            output_name,
            device,
        })
    }

    /// Returns the acceleration device used by this classifier.
    pub fn device(&self) -> AccelerationDevice {
        self.device
    }

    /// Classify the orientation of a document image.
    ///
    /// # Arguments
    /// * `image` - The document image to classify
    ///
    /// # Returns
    /// An `OrientationResult` containing the detected orientation and confidence score.
    pub fn classify(&self, image: &DynamicImage) -> Result<OrientationResult, PaddleOcrError> {
        let mut session = self.session.lock();
        cls::classify_orientation(&mut session, image, &self.input_name, &self.output_name)
    }

    /// Classify and correct the orientation of a document image.
    ///
    /// This is a convenience method that classifies the orientation and returns
    /// a correctly oriented image in a single call.
    ///
    /// # Arguments
    ///
    /// * `image` -- The document image to correct.
    ///
    /// # Returns
    ///
    /// A tuple of `(corrected_image, orientation_result)`.
    ///
    /// # Errors
    ///
    /// Returns an error if classification or image rotation fails.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let model_data = std::fs::read("PP-LCNet_x1_0_doc_ori.onnx")?;
    /// let classifier = DocOrientationClassifier::new(&model_data)?;
    /// let image = image::open("scan.png")?;
    ///
    /// let (corrected, result) = classifier.correct_orientation(&image)?;
    /// corrected.save("corrected.png")?;
    /// println!("Detected {} degrees rotation, corrected.", result.orientation.angle());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn correct_orientation(&self, image: &DynamicImage) -> Result<(DynamicImage, OrientationResult), PaddleOcrError> {
        let result = self.classify(image)?;

        let corrected = match result.orientation {
            DocOrientation::Upright => image.clone(),
            DocOrientation::Rotate90 => {
                // Rotate 90° counter-clockwise to correct
                image.rotate270()
            }
            DocOrientation::Rotate180 => {
                // Rotate 180° to correct
                image.rotate180()
            }
            DocOrientation::Rotate270 => {
                // Rotate 270° counter-clockwise (90° clockwise) to correct
                image.rotate90()
            }
        };

        Ok((corrected, result))
    }
}



/// Width granularity of padded recognition batches (`MNEMO_REC_WIDTH_QUANT`, default 32).
fn rec_width_quant() -> usize {
    static V: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("MNEMO_REC_WIDTH_QUANT").ok().and_then(|v| v.parse().ok()).filter(|v| *v > 0).unwrap_or(32))
}

/// Batches are padded with blank crops up to a multiple of this
/// (`MNEMO_REC_BATCH_QUANT`, default 1 = off) so a GPU sees few distinct shapes.
fn rec_batch_quant() -> usize {
    static V: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("MNEMO_REC_BATCH_QUANT").ok().and_then(|v| v.parse().ok()).filter(|v| *v > 0).unwrap_or(1))
}

/// Mnemo patch: a cropped, normalised recognition input (CHW, `width` columns).
pub struct PreparedCrop {
    pub data: Vec<f32>,
    pub width: i64,
}

impl PreparedCrop {
    /// Plain accessors for callers outside the crate.
    pub fn width(&self) -> i64 {
        self.width
    }
}

/// Mnemo patch: converts a recognised text region into an [`OcrBlock`] rectangle.
pub fn block_from(region: &TextRegion, decoded: decode::DecodedText) -> OcrBlock {
    let (x, y, width, height) = bbox_to_rect(&region.bbox);
    OcrBlock { text: decoded.text, confidence: decoded.score, x, y, width, height }
}
