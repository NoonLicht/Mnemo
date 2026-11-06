fn collect(
    dir: &std::path::Path,
    base: &std::path::Path,
    out: &mut Vec<(String, std::path::PathBuf)>,
) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        if p.is_dir() {
            collect(&p, base, out);
        } else if let Ok(rel) = p.strip_prefix(base) {
            out.push((rel.to_string_lossy().replace('\\', "/"), p.clone()));
        }
    }
}

/// Writes `embedded_assets.rs` listing every file under `embedded/` (the ONNX Runtime library
/// and the OCR model packs). The directory is not committed; a build without it simply embeds
/// nothing and the app falls back to downloads.
fn generate_embedded_assets() {
    let manifest =
        std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap_or_default());
    let root = manifest.join("embedded");
    println!("cargo:rerun-if-changed=embedded");
    let mut files = Vec::new();
    collect(&root, &root, &mut files);
    let mut code = String::from("pub static ASSETS: &[(&str, &[u8])] = &[\n");
    for (rel, path) in &files {
        code.push_str(&format!(
            "    ({:?}, include_bytes!({:?})),\n",
            rel,
            path.to_string_lossy()
        ));
    }
    code.push_str("];\n");
    let _ = std::fs::write(out.join("embedded_assets.rs"), code);
}

fn main() {
    tauri_build::build();
    generate_embedded_assets();

    // Copy onnxruntime library from src-tauri to target directories
    // so tests and debug builds can find and load it without manual setup.
    let manifest_dir =
        std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap_or_default());

    #[cfg(windows)]
    let dll_name = "onnxruntime.dll";
    #[cfg(target_os = "linux")]
    let dll_name = "libonnxruntime.so";
    #[cfg(target_os = "macos")]
    let dll_name = "libonnxruntime.dylib";

    let src_dll = manifest_dir.join(dll_name);
    if src_dll.is_file()
        && let Some(target_dir) = out_dir.ancestors().nth(3)
    {
        let _ = std::fs::copy(&src_dll, target_dir.join(dll_name));
        let _ = std::fs::copy(&src_dll, target_dir.join("deps").join(dll_name));
    }
}
