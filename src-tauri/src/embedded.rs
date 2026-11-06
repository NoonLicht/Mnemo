//! Assets compiled into the executable: the ONNX Runtime library and the OCR model packs.
//! They are written to the config directory on first run (or when a file is missing or has a
//! different size), so OCR works without any download or manual setup.

use std::path::{Path, PathBuf};

include!(concat!(env!("OUT_DIR"), "/embedded_assets.rs"));

fn runtime_dir() -> PathBuf {
    crate::config::MnemoConfig::config_dir().join("runtime")
}

/// Where a bundled asset belongs on disk.
fn target_for(rel: &str) -> PathBuf {
    if rel.starts_with("ocr/") || rel.starts_with("ocr-indra/") {
        // Packs honour the same directory overrides as the downloader.
        let (pack, file) = rel.split_once('/').unwrap_or((rel, ""));
        let dir = if pack == "ocr" {
            crate::ocr::pack_dir()
        } else {
            crate::ocr_indra::pack_dir()
        };
        dir.join(file)
    } else {
        runtime_dir().join(rel)
    }
}

fn write_if_needed(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if std::fs::metadata(path)
        .map(|m| m.len() == data.len() as u64)
        .unwrap_or(false)
    {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}

/// Extracts the bundled files and points ONNX Runtime at the bundled library. Call at startup,
/// before any OCR or embedding code probes the runtime. An explicit `ORT_DYLIB_PATH` wins.
pub fn install() {
    for (rel, data) in ASSETS {
        if let Err(e) = write_if_needed(&target_for(rel), data) {
            crate::crashlog::info(&format!("could not install bundled {rel}: {e}"));
        }
    }
    let lib = runtime_dir().join(format!(
        "{}onnxruntime{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ));
    if lib.is_file()
        && std::env::var_os("ORT_DYLIB_PATH")
            .map(|v| v.is_empty())
            .unwrap_or(true)
    {
        // SAFETY: called once at startup before other threads read the environment.
        unsafe { std::env::set_var("ORT_DYLIB_PATH", &lib) };
    }
}
