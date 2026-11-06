# Engineering notes

## Windows first-scan speed
`walk_directories` on Windows uses a level-parallel `std::fs::read_dir` walk; size, mtime and
attributes come from the directory listing (no open per file). Documents (23k entries) 4.4 s -> 0.04 s,
whole home directory (238k entries) 0.41 s, warm cache, one machine. Measure with
`cargo run --release --example walkbench <dir>`.

## Encryption at rest
- Full-text index (stores extracted document text): encrypted tantivy `Directory` (`src-tauri/src/crypt.rs`),
  ChaCha20, random nonce per file, seekable. Confidentiality only (no authentication). Fails closed when the
  key cannot be unwrapped.
- Activity database: titles and URLs sealed with ChaCha20-Poly1305; legacy rows sealed on first open.
- Key: 32 random bytes wrapped with Windows DPAPI (`key.bin`). Other platforms: file with mode 0600 (untested).
- Switching the setting rebuilds the index and deletes the unencrypted copy.
- Not covered: file names/paths and vectors in `metadata.db`, thumbnails, `mnemo.json`. Use BitLocker.
- Tests: `crypt::tests`, `activity::store::encryption_tests`, `tests/encrypted_content.rs`.

## Embedded runtime and OCR packs
`src-tauri/embedded/` (not committed, ~53 MB) is compiled into the executable by `build.rs`:
`onnxruntime.dll`, `ocr/` (PP-OCR: det.onnx, rec.onnx, dict.txt) and `ocr-indra/` (7 files). On startup
`embedded::install()` writes them to the data directory and sets `ORT_DYLIB_PATH`. A build without the
directory embeds nothing and falls back to downloads. ONNX is pinned to API 23 (runtime >= 1.23).
Check with `cargo run --release --example ortcheck`.

## Open items
- Replace the embedded `onnxruntime.dll` (copied from a Microsoft-signed install, ONNX Runtime 1.23) with the
  official MIT release build before publishing, and ship the ONNX Runtime third-party notices.
- Read the Indra OpenRAIL-M terms before redistributing its weights inside the executable.
- SQLCipher for `metadata.db` needs a native Windows Perl to build OpenSSL.
- Recall (screen), audio, meetings, Inbox: not built; they record or contact services and need owner consent.
