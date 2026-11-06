//! Optional Indra OCR engine (datalab-to/indra-ocr), ported natively to Rust.
//!
//! The model pack (about 27 MB of ONNX files plus meta.json) is downloaded
//! on demand when the user selects the `indra` engine; its weights are under
//! a modified OpenRAIL-M license, so they are never bundled with Mnemo.
//! It runs on the same `ort` runtime as PP-OCR and is compiled with the
//! `accurate-ocr` feature. When it cannot run, callers fall back to PP-OCR,
//! then Tesseract.

#[cfg(feature = "accurate-ocr")]
mod engine;
pub mod imageops;
pub mod order;
pub mod skew;
pub mod text;

use crate::config::MnemoConfig;
use crate::ocr::OcrOutcome;
use std::path::{Path, PathBuf};

/// Files of the pack that the port reads. The n-gram language models, the
/// math and the reading-order models of the Python package are not needed.
pub const PACK_FILES: [&str; 8] = [
    "det.onnx",
    "rec.onnx",
    "rec_cjk.onnx",
    "rec_rtl.onnx",
    "rec_world.onnx",
    "script.onnx",
    "orient.onnx",
    "meta.json",
];

/// Directory holding the Indra pack. `MNEMO_INDRA_PACK_DIR` overrides it
/// (development and the real-model tests).
pub fn pack_dir() -> PathBuf {
    match std::env::var_os("MNEMO_INDRA_PACK_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => MnemoConfig::config_dir().join("ocr-indra"),
    }
}

pub fn pack_present() -> bool {
    let dir = pack_dir();
    PACK_FILES.iter().all(|f| dir.join(f).is_file())
}

/// Why Indra cannot run, in words a user can act on, or `None` when it can.
pub fn problem() -> Option<String> {
    if !crate::ocr::accurate_compiled() {
        return Some("Indra OCR is not part of this build".into());
    }
    if !pack_present() {
        return Some("Indra OCR models are not downloaded yet".into());
    }
    if !crate::ocr::runtime_loadable() {
        return crate::ocr::runtime_error()
            .or_else(|| Some("ONNX Runtime could not be loaded".into()));
    }
    self_test().err()
}

/// True when Indra can actually read: build, pack, runtime and self-test.
pub fn available() -> bool {
    crate::ocr::accurate_compiled()
        && pack_present()
        && crate::ocr::runtime_loadable()
        && self_test().is_ok()
}

/// Reads an image file with Indra. `Unavailable` when it cannot run.
pub fn extract_image(path: &Path) -> OcrOutcome {
    if !(pack_present() && crate::ocr::runtime_loadable()) {
        return OcrOutcome::Unavailable;
    }
    match image::open(path) {
        Ok(image) => extract_decoded(&image),
        Err(_) => OcrOutcome::NoText,
    }
}

/// Reads an already-decoded image (scanned PDF pages) with Indra.
pub fn extract_decoded(image: &image::DynamicImage) -> OcrOutcome {
    #[cfg(feature = "accurate-ocr")]
    {
        if !(pack_present() && crate::ocr::runtime_loadable()) {
            return OcrOutcome::Unavailable;
        }
        let page = imageops::Gray::from_image(image);
        match pool::read(&page) {
            None => OcrOutcome::Unavailable,
            Some(Ok(text)) if !text.trim().is_empty() => OcrOutcome::Text(text),
            Some(Ok(_)) => OcrOutcome::NoText,
            Some(Err(e)) => {
                eprintln!("[Mnemo OCR] Indra failed on an image: {e}");
                OcrOutcome::Unavailable
            }
        }
    }
    #[cfg(not(feature = "accurate-ocr"))]
    {
        let _ = image;
        OcrOutcome::Unavailable
    }
}

/// Runs Indra once on a known image and checks the words come back. Cached
/// once it has run against a present pack: "ready" in the UI means a correct
/// read, not files on disk.
pub fn self_test() -> Result<(), String> {
    use std::sync::OnceLock;

    static RESULT: OnceLock<Result<(), String>> = OnceLock::new();
    if let Some(result) = RESULT.get() {
        return result.clone();
    }
    if !crate::ocr::accurate_compiled() {
        return Err("Indra OCR is not part of this build".into());
    }
    if !pack_present() {
        // Not cached: the pack may be downloaded later in this process.
        return Err("Indra OCR models are not downloaded yet".into());
    }
    if !crate::ocr::runtime_loadable() {
        return Err("ONNX Runtime could not be loaded".into());
    }
    let result = image::load_from_memory(crate::ocr::SELF_TEST_IMAGE)
        .map_err(|e| format!("self-test image is unreadable: {e}"))
        .and_then(|image| {
            let page = imageops::Gray::from_image(&image);
            match pool::read(&page) {
                None => Err("Indra models failed to load".into()),
                Some(Err(e)) => Err(format!("Indra engine failed during self-test: {e}")),
                Some(Ok(text)) => {
                    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    if normalized
                        .to_lowercase()
                        .contains(crate::ocr::SELF_TEST_EXPECTED)
                    {
                        Ok(())
                    } else if normalized.is_empty() {
                        Err("Indra read nothing in its self-test".into())
                    } else {
                        Err(format!("Indra misread its self-test: {normalized:?}"))
                    }
                }
            }
        });
    if let Err(reason) = &result {
        eprintln!("[Mnemo OCR] {reason}");
    }
    RESULT.get_or_init(|| result).clone()
}

/// A few engines, loaded on first use, so several extraction workers can OCR
/// at once instead of queueing behind one lock (and timing out).
#[cfg(feature = "accurate-ocr")]
mod pool {
    use super::engine::Engine;
    use super::imageops::Gray;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, OnceLock};

    /// Threads per engine; engines = half the cores / this, at most 3.
    const THREADS: usize = 2;

    /// `None` once loading failed: a corrupt pack is probed once, not once
    /// per image.
    static ENGINES: OnceLock<Option<Vec<Mutex<Engine>>>> = OnceLock::new();
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn load() -> Option<Vec<Mutex<Engine>>> {
        let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
        let count = (cores / 2 / THREADS).clamp(1, 3);
        // `ort` can panic instead of erroring when the runtime is broken.
        let loaded = std::panic::catch_unwind(|| {
            // One ONNX Runtime environment per process: share PP-OCR's.
            let env = paddleocr_rs_onnx::shared_environment().map_err(|e| e.to_string())?;
            let dir = super::pack_dir();
            (0..count)
                .map(|_| Engine::load(&env, &dir, THREADS).map(Mutex::new))
                .collect::<Result<Vec<_>, String>>()
        });
        match loaded {
            Ok(Ok(engines)) => Some(engines),
            Ok(Err(e)) => {
                eprintln!("[Mnemo OCR] Indra models failed to load: {e}");
                None
            }
            Err(_) => {
                eprintln!("[Mnemo OCR] Indra models failed to load (panic)");
                None
            }
        }
    }

    /// `None` when no engine could load.
    pub fn read(page: &Gray) -> Option<Result<String, String>> {
        let engines = ENGINES.get_or_init(load).as_ref()?;
        let start = NEXT.fetch_add(1, Ordering::Relaxed);
        let free =
            (0..engines.len()).find_map(|i| engines[(start + i) % engines.len()].try_lock().ok());
        let mut engine = match free {
            Some(guard) => guard,
            None => engines[start % engines.len()]
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| engine.read(page)));
        Some(result.unwrap_or_else(|_| Err("the engine panicked".into())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_lives_in_its_own_folder() {
        assert!(
            pack_dir().ends_with("ocr-indra") || std::env::var_os("MNEMO_INDRA_PACK_DIR").is_some()
        );
        assert!(PACK_FILES.contains(&"meta.json"));
    }

    /// PP-OCR and Indra in one process, PP-OCR first, as in the app. `ort`
    /// allows one environment per process; Indra used to build its own and
    /// fail to load ("only one environment is allowed per process").
    #[cfg(feature = "accurate-ocr")]
    #[test]
    #[ignore = "requires MNEMO_OCR_PACK_DIR, MNEMO_INDRA_PACK_DIR and ORT_DYLIB_PATH"]
    fn indra_loads_after_pp_ocr_in_the_same_process() {
        assert_eq!(crate::ocr::self_test(), Ok(()), "PP-OCR self-test");
        assert_eq!(self_test(), Ok(()), "Indra self-test after PP-OCR");
    }

    /// Mnemo's 104-image UI-text fixture through the real port. Run by
    /// hand or CI with `MNEMO_INDRA_PACK_DIR` and `ORT_DYLIB_PATH` set.
    #[cfg(feature = "accurate-ocr")]
    #[test]
    #[ignore = "requires MNEMO_INDRA_PACK_DIR and ORT_DYLIB_PATH"]
    fn real_pack_ui_text_recall() {
        fn normalize(text: &str) -> String {
            text.chars()
                .map(|ch| if ch.is_alphanumeric() { ch } else { ' ' })
                .flat_map(|ch| ch.to_lowercase())
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        }
        assert_eq!(self_test(), Ok(()), "self-test");
        crate::ocr::set_engine(crate::ocr::INDRA);
        crate::indexer::extract::set_ocr_enabled(true);
        assert_eq!(problem(), None);
        assert_eq!(crate::ocr::usable_engine(), crate::ocr::INDRA);
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ocr-ui-text");
        let labels = std::fs::read_to_string(fixture.join("labels.txt")).expect("labels.txt");
        let labels: Vec<&str> = labels.lines().filter(|l| !l.trim().is_empty()).collect();
        let mut missed = Vec::new();
        for (index, label) in labels.iter().enumerate() {
            let path = fixture.join(format!("{:03}.png", index + 1));
            // Through the app's dispatch, as indexing calls it.
            let text = match crate::indexer::extract::image::ocr_image(&path) {
                OcrOutcome::Text(text) => text,
                other => format!("{other:?}"),
            };
            if !normalize(&text).contains(&normalize(label)) {
                missed.push(format!("{label:?} -> {text:?}"));
            }
        }
        let recall = 1.0 - missed.len() as f32 / labels.len() as f32;
        println!("Indra UI-text fixture: {:.1}% recall", recall * 100.0);
        for line in missed.iter().take(10) {
            println!("  miss: {line}");
        }
        assert!(recall >= 0.95, "recall {recall:.3} below 0.95");
    }
}
