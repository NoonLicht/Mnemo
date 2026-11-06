//! Checks that the bundled ONNX Runtime loads and both OCR packs are usable.
fn main() {
    mnemo_lib::embedded::install();
    println!("ORT_DYLIB_PATH={:?}", std::env::var("ORT_DYLIB_PATH").ok());
    println!("runtime_loadable={}", mnemo_lib::ocr::runtime_loadable());
    println!("runtime_error={:?}", mnemo_lib::ocr::runtime_error());
    println!("ppocr pack_present={}", mnemo_lib::ocr::pack_present());
    println!(
        "indra pack_present={}",
        mnemo_lib::ocr_indra::pack_present()
    );
    println!("indra problem={:?}", mnemo_lib::ocr_indra::problem());
}
