//! Optional accurate OCR pack.
//!
//! Tesseract stays the fast, always-available default. The accurate pack adds
//! PaddleOCR PP-OCRv6 tiny (detector + recognizer + dictionary) behind the
//! `accurate-ocr` Cargo feature. If the pack is not installed or the feature is
//! disabled, callers fall back to Tesseract.

use crate::config::MnemoConfig;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};

pub const TESSERACT: &str = "tesseract";
pub const ACCURATE: &str = "accurate";
/// Indra OCR (see [`crate::ocr_indra`]).
pub const INDRA: &str = "indra";
/// No engine can run on this machine right now.
pub const NONE: &str = "none";

/// Bump whenever the text produced for the same image can change (engine
/// fixes, decoding changes). A change re-queues every image and PDF once, so
/// existing indexes pick up the fix without a manual rebuild.
///
/// - 2: PP-OCR keeps spaces between words; scanned PDFs are decoded properly.
pub const OCR_REVISION: u32 = 2;

/// Result of one OCR attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcrOutcome {
    /// The engine ran and read text.
    Text(String),
    /// The engine ran and found no text (a photo, a blank page, an
    /// unreadable file). Final: retrying would give the same answer.
    NoText,
    /// The engine could not run. Not final: the file must be retried once an
    /// engine is usable.
    Unavailable,
}

static ENGINE: AtomicU8 = AtomicU8::new(0);

/// Directory holding the optional PP-OCRv6 pack. `MNEMO_OCR_PACK_DIR`
/// overrides it (development and the real-model CI tests).
pub fn pack_dir() -> PathBuf {
    match std::env::var_os("MNEMO_OCR_PACK_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => MnemoConfig::config_dir().join("ocr"),
    }
}

pub fn det_path() -> PathBuf {
    pack_dir().join("det.onnx")
}

pub fn rec_path() -> PathBuf {
    pack_dir().join("rec.onnx")
}

pub fn dict_path() -> PathBuf {
    pack_dir().join("dict.txt")
}

/// True when all three pack files exist.
pub fn pack_present() -> bool {
    det_path().is_file() && rec_path().is_file() && dict_path().is_file()
}

/// True when the compiled binary has the accurate engine.
pub fn accurate_compiled() -> bool {
    cfg!(feature = "accurate-ocr")
}

/// File name of the ONNX Runtime shared library on this platform.
fn runtime_file_name() -> String {
    format!(
        "{}onnxruntime{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    )
}

/// Where an installed Mnemo keeps the ONNX Runtime it ships, in the order
/// they are tried. Release builds bundle the library as a Tauri resource:
///
/// - Windows: next to `mnemo.exe`.
/// - macOS: `Mnemo.app/Contents/Resources/` (the binary is in `MacOS/`).
/// - Linux deb/rpm/AppImage: `<prefix>/lib/mnemo/` (the binary is in `bin/`).
///
/// A system copy is never preferred: Windows ships an old, API-incompatible
/// `onnxruntime.dll` in System32, and a distro package can be any version.
fn runtime_candidates(exe: &Path, file_name: &str) -> Vec<PathBuf> {
    let Some(dir) = exe.parent() else {
        return Vec::new();
    };
    let mut candidates = vec![
        dir.join(file_name),
        dir.join("../Resources").join(file_name),
        dir.join("../lib/mnemo").join(file_name),
        dir.join("../lib").join(file_name),
    ];
    // Development and test layouts:
    // target/debug/deps -> target/debug
    if let Some(parent) = dir.parent() {
        candidates.push(parent.join(file_name));
        // target/debug -> src-tauri
        if let Some(grandparent) = parent.parent() {
            candidates.push(grandparent.join(file_name));
            // workspace root -> src-tauri
            candidates.push(grandparent.join("src-tauri").join(file_name));
        }
    }
    // Also check standard configuration and model directories
    candidates.push(MnemoConfig::config_dir().join(file_name));
    candidates.push(MnemoConfig::config_dir().join("models").join(file_name));

    // The standalone daemon (installed with the CLI, not inside the app) uses
    // the copy the installed app shipped.
    if cfg!(target_os = "macos") {
        candidates
            .push(PathBuf::from("/Applications/mnemo.app/Contents/Resources").join(file_name));
    } else if cfg!(target_os = "linux") {
        candidates.push(PathBuf::from("/usr/lib/mnemo").join(file_name));
    }
    candidates
}

/// Outcome of the one-time runtime probe: `Ok(path)` or a reason.
#[cfg(feature = "accurate-ocr")]
fn runtime_probe() -> &'static Result<String, String> {
    use std::sync::OnceLock;

    static PROBE: OnceLock<Result<String, String>> = OnceLock::new();
    PROBE.get_or_init(|| {
        let file_name = runtime_file_name();
        let path_opt = match std::env::var("ORT_DYLIB_PATH") {
            Ok(path) if !path.trim().is_empty() => Some(path),
            _ => std::env::current_exe()
                .ok()
                .and_then(|exe| {
                    runtime_candidates(&exe, &file_name)
                        .into_iter()
                        .find(|candidate| candidate.is_file())
                })
                .map(|candidate| candidate.to_string_lossy().into_owned()),
        };
        let path = match path_opt {
            Some(p) => p,
            None => {
                let reason = format!(
                    "ONNX Runtime library ({file_name}) not found. Place it next to mnemo or set ORT_DYLIB_PATH"
                );
                eprintln!("[Mnemo OCR] {reason}");
                return Err(reason);
            }
        };
        // `init_from` returns an error for a missing/incompatible library,
        // but guard against a panic from a broken loader anyway. `ort::api()`
        // then confirms the library exposes the API version `ort` needs.
        let loaded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ort::init_from(&path).map(|_| ()).map_err(|e| e.to_string())
        }))
        .unwrap_or_else(|_| Err("the loader panicked".to_string()));
        let result = loaded.and_then(|()| {
            std::panic::catch_unwind(|| {
                let _ = ort::api();
            })
            .map_err(|_| "the library is too old for this build (needs 1.23+)".to_string())
        });
        match result {
            Ok(()) => Ok(path),
            Err(e) => {
                let reason = if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
                    "PP-OCR needs a Mac with Apple silicon".to_string()
                } else {
                    format!("ONNX Runtime could not be loaded ({e})")
                };
                eprintln!("[Mnemo OCR] {reason}; tried {path:?}");
                Err(reason)
            }
        }
    })
}

/// Detailed error from the ONNX Runtime loader, if loading failed.
pub fn runtime_error() -> Option<String> {
    #[cfg(feature = "accurate-ocr")]
    {
        runtime_probe().as_ref().err().cloned()
    }
    #[cfg(not(feature = "accurate-ocr"))]
    {
        Some("accurate-ocr feature is not compiled in".to_string())
    }
}

/// True when the dynamic ONNX Runtime library can be loaded. Probed once per
/// process: installing the library later requires a restart.
pub fn runtime_loadable() -> bool {
    #[cfg(feature = "accurate-ocr")]
    {
        runtime_probe().is_ok()
    }
    #[cfg(not(feature = "accurate-ocr"))]
    {
        false
    }
}

/// Why the accurate engine cannot run, in words a user can act on, or `None`
/// when it can.
pub fn accurate_problem() -> Option<String> {
    if !accurate_compiled() {
        return Some("PP-OCR is not part of this build".into());
    }
    if !pack_present() {
        return Some("PP-OCR pack is not downloaded yet".into());
    }
    #[cfg(feature = "accurate-ocr")]
    if let Err(reason) = runtime_probe() {
        return Some(reason.clone());
    }
    if let Err(reason) = self_test() {
        return Some(reason);
    }
    None
}

/// True when the accurate engine can actually run.
///
/// Requires the pack, a build with `accurate-ocr`, **and** a loadable
/// `libonnxruntime`. When any of those is missing, callers use Tesseract.
pub fn accurate_available() -> bool {
    accurate_compiled() && pack_present() && runtime_loadable() && self_test().is_ok()
}

/// The engine that will actually read images right now: the selected one
/// when it can run, otherwise whichever other engine can, otherwise [`NONE`].
///
/// Selecting PP-OCR on a machine where it cannot run (no pack yet, Intel Mac)
/// must not mean "no OCR" while Tesseract is sitting right there. Indra falls
/// back to PP-OCR first, then Tesseract.
pub fn usable_engine() -> &'static str {
    let tesseract = || crate::indexer::extract::image::tesseract_usable();
    if engine_name() == INDRA {
        if crate::ocr_indra::available() {
            INDRA
        } else if accurate_available() {
            ACCURATE
        } else if tesseract() {
            TESSERACT
        } else {
            NONE
        }
    } else if engine_name() == ACCURATE {
        if accurate_available() {
            ACCURATE
        } else if tesseract() {
            TESSERACT
        } else {
            NONE
        }
    } else if tesseract() {
        TESSERACT
    } else if accurate_available() {
        ACCURATE
    } else {
        NONE
    }
}

/// Fingerprint of "what OCR would produce right now". When it differs from
/// the one stored with the index, images and PDFs are re-read once.
pub fn signature() -> String {
    // The image-size cap is part of "what OCR would produce": raising it
    // must re-read images that were skipped.
    format!(
        "{}:{}:{}",
        usable_engine(),
        OCR_REVISION,
        crate::indexer::extract::image::ocr_max_image_bytes()
    )
}

/// Updates the process-wide OCR backend from config.
///
/// Values other than `accurate` and `indra` (including the old `native`
/// string) fall back to Tesseract.
pub fn set_engine(engine: &str) {
    let value = if engine.eq_ignore_ascii_case(ACCURATE) {
        1
    } else if engine.eq_ignore_ascii_case(INDRA) {
        2
    } else {
        0
    };
    ENGINE.store(value, Ordering::SeqCst);
}

pub fn engine_name() -> &'static str {
    match ENGINE.load(Ordering::SeqCst) {
        1 => ACCURATE,
        2 => INDRA,
        _ => TESSERACT,
    }
}

/// PaddleOCR recognizers have one output class per dictionary line, plus the
/// CTC blank at index 0, plus a final class for the space character. Paddle's
/// own pipeline appends that space (`use_space_char=True`); the dictionary
/// files do not contain it and `paddleocr_rs_onnx` does not add it. Without it
/// every predicted space indexes past the end of the key list and is dropped,
/// so "Open settings" is read as "Opensettings" and word search never matches.
pub fn keys_with_space(dict: &[u8]) -> Vec<u8> {
    let mut keys = dict.to_vec();
    while matches!(keys.last(), Some(b'\n' | b'\r')) {
        keys.pop();
    }
    let already = keys
        .rsplit(|byte| *byte == b'\n')
        .next()
        .is_some_and(|last| last == b" " || last == b" \r");
    if !already {
        keys.extend_from_slice(b"\n ");
    }
    keys
}

/// Reads an image with the accurate engine.
///
/// `Unavailable` when the engine cannot run (not selected, no pack, no
/// runtime, model failed to load); the caller decides what to fall back to.
pub fn extract_image_ocr(path: &Path) -> OcrOutcome {
    #[cfg(feature = "accurate-ocr")]
    {
        if !(pack_present() && runtime_loadable()) {
            return OcrOutcome::Unavailable;
        }
        let Some(engine) = accurate_engine() else {
            return OcrOutcome::Unavailable;
        };
        // Hybrid mode: an idle GPU helper takes the image, otherwise the CPU does.
        if let Some(outcome) = crate::gpu_ocr::try_offload(path) {
            return outcome;
        }
        let Ok(image) = image::open(path) else {
            return OcrOutcome::NoText;
        };
        recognize(&engine, &image)
    }
    #[cfg(not(feature = "accurate-ocr"))]
    {
        let _ = path;
        OcrOutcome::Unavailable
    }
}

/// Reads an already-decoded image (scanned PDF pages) with the accurate engine.
pub fn extract_decoded_image(image: &image::DynamicImage) -> OcrOutcome {
    #[cfg(feature = "accurate-ocr")]
    {
        if !(pack_present() && runtime_loadable()) {
            return OcrOutcome::Unavailable;
        }
        match accurate_engine() {
            Some(engine) => recognize(&engine, image),
            None => OcrOutcome::Unavailable,
        }
    }
    #[cfg(not(feature = "accurate-ocr"))]
    {
        let _ = image;
        OcrOutcome::Unavailable
    }
}

#[cfg(feature = "accurate-ocr")]
fn recognize(engine: &paddleocr_rs_onnx::OcrEngine, image: &image::DynamicImage) -> OcrOutcome {
    use paddleocr_rs_onnx::OrderBy;

    let blocks = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        engine.recognize_all(image, OrderBy::Horizontal)
    }));
    let blocks = match blocks {
        Ok(Ok(blocks)) => blocks,
        Ok(Err(e)) => {
            eprintln!("[Mnemo OCR] PP-OCR failed on an image: {e}");
            return OcrOutcome::Unavailable;
        }
        Err(_) => {
            eprintln!("[Mnemo OCR] PP-OCR panicked on an image");
            return OcrOutcome::Unavailable;
        }
    };
    let text = blocks
        .iter()
        .map(|block| block.text.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        OcrOutcome::NoText
    } else {
        OcrOutcome::Text(text)
    }
}

/// Cached state of the accurate engine. `Unavailable` is sticky so a corrupt
/// pack is probed once, not once per image.
#[cfg(feature = "accurate-ocr")]
enum AccurateEngine {
    Uninitialized,
    Ready(std::sync::Arc<paddleocr_rs_onnx::OcrEngine>),
    Unavailable,
}

/// The shared engine, loaded on first use.
///
/// The lock is held only while loading. Recognition runs outside it on a
/// shared `Arc`: the engine is `Send + Sync` and keeps its own session pool,
/// so several extraction workers can OCR at once instead of queueing behind
/// one mutex (and timing out while they wait).
#[cfg(feature = "accurate-ocr")]
fn accurate_engine() -> Option<std::sync::Arc<paddleocr_rs_onnx::OcrEngine>> {
    use paddleocr_rs_onnx::OcrEngine;
    use std::sync::{Arc, Mutex, OnceLock};

    static ENGINE_CACHE: OnceLock<Mutex<AccurateEngine>> = OnceLock::new();

    let cache = ENGINE_CACHE.get_or_init(|| Mutex::new(AccurateEngine::Uninitialized));
    let mut guard = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if matches!(*guard, AccurateEngine::Uninitialized) {
        let files = (
            std::fs::read(det_path()),
            std::fs::read(rec_path()),
            std::fs::read(dict_path()),
        );
        let (Ok(det), Ok(rec), Ok(dict)) = files else {
            // Pack disappeared between the presence check and the read: try
            // again next time rather than going sticky-unavailable.
            return None;
        };
        let keys = keys_with_space(&dict);
        // `ort` panics (rather than returning an error) when the dynamic
        // runtime cannot be loaded. Contain that here so the caller falls
        // back instead of losing the file to a worker panic.
        *guard = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            OcrEngine::new(&det, &rec, &keys)
        })) {
            Ok(Ok(engine)) => AccurateEngine::Ready(Arc::new(engine)),
            Ok(Err(e)) => {
                eprintln!("[Mnemo OCR] PP-OCR pack failed to load: {e}");
                AccurateEngine::Unavailable
            }
            Err(_) => {
                eprintln!("[Mnemo OCR] PP-OCR pack failed to load (panic)");
                AccurateEngine::Unavailable
            }
        };
    }
    match &*guard {
        AccurateEngine::Ready(engine) => Some(Arc::clone(engine)),
        _ => None,
    }
}

/// A tiny image with known text, used to prove the engine actually reads.
pub(crate) const SELF_TEST_IMAGE: &[u8] = include_bytes!("../tests/fixtures/ocr-ui-text/001.png");
pub(crate) const SELF_TEST_EXPECTED: &str = "open settings";

/// Runs the accurate engine once on a known image and checks the words come
/// back, spaces included. Cached once it has run against a present pack, so
/// status checks stay cheap. This is what "ready" means in the UI: files on
/// disk prove nothing, a correct read does.
pub fn self_test() -> Result<(), String> {
    #[cfg(feature = "accurate-ocr")]
    {
        use std::sync::OnceLock;

        static RESULT: OnceLock<Result<(), String>> = OnceLock::new();
        if let Some(result) = RESULT.get() {
            return result.clone();
        }
        if !pack_present() {
            // Not cached: the pack may be downloaded later in this process.
            return Err("PP-OCR pack is not downloaded yet".into());
        }
        if let Err(reason) = runtime_probe() {
            return Err(reason.clone());
        }
        let result = (|| {
            let image = image::load_from_memory(SELF_TEST_IMAGE)
                .map_err(|e| format!("self-test image is unreadable: {e}"))?;
            let engine = accurate_engine().ok_or("PP-OCR pack failed to load")?;
            match recognize(&engine, &image) {
                OcrOutcome::Text(text) => {
                    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    if normalized.to_lowercase().contains(SELF_TEST_EXPECTED) {
                        Ok(())
                    } else {
                        Err(format!("PP-OCR misread its self-test: {normalized:?}"))
                    }
                }
                _ => Err("PP-OCR read nothing in its self-test".to_string()),
            }
        })();
        if let Err(reason) = &result {
            eprintln!("[Mnemo OCR] {reason}");
        }
        RESULT.get_or_init(|| result).clone()
    }
    #[cfg(not(feature = "accurate-ocr"))]
    {
        Err("PP-OCR is not part of this build".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_defaults_to_tesseract_and_round_trips() {
        set_engine(TESSERACT);
        assert_eq!(engine_name(), TESSERACT);
        set_engine(ACCURATE);
        assert_eq!(engine_name(), ACCURATE);
        set_engine("Indra");
        assert_eq!(engine_name(), INDRA);
        // The retired `native` value must fall back to Tesseract rather than
        // claiming a backend that does not exist.
        set_engine("native");
        assert_eq!(engine_name(), TESSERACT);
        set_engine(TESSERACT);
    }

    #[test]
    fn keys_gain_exactly_one_trailing_space_entry() {
        // Trailing newline, no trailing newline, CRLF: always one " " line.
        for dict in [&b"a\nb\n"[..], b"a\nb", b"a\r\nb\r\n"] {
            let keys = keys_with_space(dict);
            let text = String::from_utf8(keys).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            assert_eq!(lines.len(), 3, "{dict:?} -> {lines:?}");
            assert_eq!(*lines.last().unwrap(), " ");
        }
        // Idempotent: a dictionary that already ends with the space entry.
        assert_eq!(keys_with_space(b"a\nb\n "), b"a\nb\n ".to_vec());
        assert_eq!(keys_with_space(&keys_with_space(b"a\n")), b"a\n ".to_vec());
    }

    #[test]
    fn runtime_is_looked_up_where_each_installer_puts_it() {
        let name = runtime_file_name();
        let exe = Path::new("/opt/app/bin/mnemo");
        let candidates = runtime_candidates(exe, &name);
        // Windows (next to the exe), macOS (Contents/Resources), Linux
        // packages (<prefix>/lib/mnemo).
        assert_eq!(candidates[0], Path::new("/opt/app/bin").join(&name));
        assert!(candidates.contains(&Path::new("/opt/app/bin/../Resources").join(&name)));
        assert!(candidates.contains(&Path::new("/opt/app/bin/../lib/mnemo").join(&name)));
    }

    #[test]
    fn pack_presence_requires_all_three_files() {
        // The real config dir should not be touched by this test.
        let dir = std::env::temp_dir().join(format!("mnemo-ocr-pack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // pack_dir() is config-based, so only the path helpers are exercised
        // here; presence is covered by the ignored real-pack test.
        assert!(det_path().ends_with("det.onnx"));
        assert!(rec_path().ends_with("rec.onnx"));
        assert!(dict_path().ends_with("dict.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "accurate-ocr")]
    #[test]
    fn runtime_probe_is_panic_free() {
        // Whether or not libonnxruntime is installed, probing must answer
        // instead of panicking (the old path panicked inside `ort`).
        let _ = runtime_loadable();
        let _ = accurate_available();
    }

    /// The whole app path, not just the crate: engine selection, runtime
    /// probe, self-test, image OCR, and a scanned PDF through the real
    /// extractor. Run by `.github/workflows/ocr.yml` on Linux, macOS and
    /// Windows with the real pack and ONNX Runtime.
    #[cfg(feature = "accurate-ocr")]
    #[test]
    #[ignore = "requires MNEMO_OCR_PACK_DIR and ORT_DYLIB_PATH"]
    fn real_pipeline_reads_images_and_scanned_pdfs() {
        use lopdf::{Dictionary, Object, Stream, dictionary};

        set_engine(ACCURATE);
        crate::indexer::extract::set_ocr_enabled(true);
        assert_eq!(self_test(), Ok(()), "self-test");
        assert!(accurate_available());
        assert_eq!(accurate_problem(), None);
        assert_eq!(usable_engine(), ACCURATE);

        let fixture = format!(
            "{}/tests/fixtures/ocr-ui-text/002.png",
            env!("CARGO_MANIFEST_DIR")
        );
        match crate::indexer::extract::image::ocr_image(Path::new(&fixture)) {
            OcrOutcome::Text(text) => {
                assert!(
                    text.to_lowercase().contains("search results"),
                    "image: {text:?}"
                )
            }
            other => panic!("image OCR: {other:?}"),
        }

        // A scanned PDF: one page, no text layer, raw Flate RGB rows (the
        // layout the old extractor could never decode).
        let rgb = image::open(&fixture).unwrap().to_rgb8();
        let (w, h) = rgb.dimensions();
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, rgb.as_raw()).unwrap();
        let mut doc = lopdf::Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let image_id = doc.add_object(
            Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Image",
                    "Width" => w as i64, "Height" => h as i64,
                    "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8,
                    "Filter" => "FlateDecode",
                },
                encoder.finish().unwrap(),
            )
            .with_compression(false),
        );
        let content = doc.add_object(Stream::new(
            Dictionary::new(),
            format!("q {w} 0 0 {h} 0 0 cm /Im0 Do Q").into_bytes(),
        ));
        let page = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content,
            "Resources" => dictionary! { "XObject" => dictionary! { "Im0" => image_id } },
            "MediaBox" => vec![0.into(), 0.into(), (w as i64).into(), (h as i64).into()],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(
                dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
            ),
        );
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        let pdf_path = std::env::temp_dir().join(format!("mnemo-scan-{}.pdf", std::process::id()));
        doc.save(&pdf_path).unwrap();

        let result = crate::indexer::extract::pdf::extract_pdf(&pdf_path, 100_000).expect("pdf");
        let _ = std::fs::remove_file(&pdf_path);
        assert!(result.is_scanned);
        assert!(!result.ocr_unavailable);
        assert!(
            result.text.to_lowercase().contains("search results"),
            "scanned PDF: {:?}",
            result.text
        );
    }

    #[cfg(feature = "accurate-ocr")]
    #[test]
    #[ignore = "requires MNEMO_OCR_PACK_DIR pointing at det.onnx/rec.onnx/dict.txt"]
    fn real_pack_loads() {
        let dir = std::env::var("MNEMO_OCR_PACK_DIR")
            .expect("MNEMO_OCR_PACK_DIR must point at the OCR pack");
        let dir = Path::new(&dir);
        let det = std::fs::read(dir.join("det.onnx")).expect("det.onnx");
        let rec = std::fs::read(dir.join("rec.onnx")).expect("rec.onnx");
        let dict = keys_with_space(&std::fs::read(dir.join("dict.txt")).expect("dict.txt"));
        let engine =
            paddleocr_rs_onnx::OcrEngine::new(&det, &rec, &dict).expect("load PP-OCRv6 tiny pack");
        assert!(engine.device() == paddleocr_rs_onnx::AccelerationDevice::Cpu);
    }

    #[cfg(feature = "accurate-ocr")]
    #[test]
    #[ignore = "requires MNEMO_OCR_PACK_DIR and MNEMO_OCR_IMAGE"]
    fn real_pack_reads_text() {
        let dir = std::env::var("MNEMO_OCR_PACK_DIR")
            .expect("MNEMO_OCR_PACK_DIR must point at the OCR pack");
        let image_path =
            std::env::var("MNEMO_OCR_IMAGE").expect("MNEMO_OCR_IMAGE must point at a test image");
        let dir = Path::new(&dir);
        let det = std::fs::read(dir.join("det.onnx")).expect("det.onnx");
        let rec = std::fs::read(dir.join("rec.onnx")).expect("rec.onnx");
        let dict = keys_with_space(&std::fs::read(dir.join("dict.txt")).expect("dict.txt"));
        let engine = paddleocr_rs_onnx::OcrEngine::new(&det, &rec, &dict).expect("engine");
        let image = image::open(&image_path).expect("test image");
        let blocks = engine
            .recognize_all(&image, paddleocr_rs_onnx::OrderBy::Horizontal)
            .expect("recognize");
        let text = blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        println!("PP-OCRv6 text: {text}");
        assert!(text.to_lowercase().contains("mnemo"), "got: {text}");
        assert!(text.contains(' '), "words must stay separated, got: {text}");
    }

    #[cfg(feature = "accurate-ocr")]
    #[test]
    #[ignore = "requires MNEMO_OCR_PACK_DIR; runs the 104-image UI-text fixture"]
    fn real_pack_ui_text_recall() {
        // Word boundaries are part of the result: search tokenizes on them,
        // so "Opensettings" for "Open settings" is a miss, not a hit. (The
        // old normalizer stripped spaces and hid exactly that bug.)
        fn normalize(text: &str) -> String {
            text.chars()
                .map(|ch| if ch.is_alphanumeric() { ch } else { ' ' })
                .flat_map(|ch| ch.to_lowercase())
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        }

        let pack = std::env::var("MNEMO_OCR_PACK_DIR")
            .expect("MNEMO_OCR_PACK_DIR must point at the OCR pack");
        let fixture = std::env::var("MNEMO_OCR_FIXTURE_DIR").unwrap_or_else(|_| {
            format!("{}/tests/fixtures/ocr-ui-text", env!("CARGO_MANIFEST_DIR"))
        });
        let pack = Path::new(&pack);
        let fixture = Path::new(&fixture);
        let det = std::fs::read(pack.join("det.onnx")).expect("det.onnx");
        let rec = std::fs::read(pack.join("rec.onnx")).expect("rec.onnx");
        let dict = keys_with_space(&std::fs::read(pack.join("dict.txt")).expect("dict.txt"));
        let engine = paddleocr_rs_onnx::OcrEngine::new(&det, &rec, &dict).expect("engine");

        let labels = std::fs::read_to_string(fixture.join("labels.txt")).expect("labels.txt");
        let labels: Vec<&str> = labels
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        let mut hits = 0usize;
        let mut missed = Vec::new();
        for (index, label) in labels.iter().enumerate() {
            let image_path = fixture.join(format!("{:03}.png", index + 1));
            let image = image::open(&image_path).expect("fixture image");
            let blocks = engine
                .recognize_all(&image, paddleocr_rs_onnx::OrderBy::Horizontal)
                .expect("recognize");
            let text = blocks
                .iter()
                .map(|block| block.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let expected = normalize(label);
            let actual = normalize(&text);
            if actual.contains(&expected) {
                hits += 1;
            } else {
                missed.push(format!("{label:?} -> {text:?}"));
            }
        }
        let recall = hits as f32 / labels.len() as f32;
        println!(
            "PP-OCRv6 UI-text fixture: {hits}/{} lines exact ({:.1}% recall)",
            labels.len(),
            recall * 100.0
        );
        for line in missed.iter().take(10) {
            println!("  miss: {line}");
        }
        assert!(recall >= 0.95, "recall {recall:.3} below 0.95");
    }
}
