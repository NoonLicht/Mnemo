//! Measures PP-OCR on CPU against DirectML (any DirectX 12 GPU) on real
//! images, so the GPU decision rests on numbers. Prints timings only; the
//! recognised text is never printed.
//!
//! ```text
//! set ORT_DYLIB_PATH=<a DirectML-enabled onnxruntime.dll>
//! set MNEMO_OCR_PACK_DIR=<folder with det.onnx, rec.onnx, dict.txt>
//! cargo run --example ocr_gpu_bench --features directml,cuda -- <images folder> [count]
//! ```
use paddleocr_rs_onnx::{AccelerationDevice, OcrEngine, OrderBy};
use std::path::PathBuf;
use std::time::Instant;

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

fn main() {
    let pack = PathBuf::from(
        std::env::var("MNEMO_OCR_PACK_DIR").expect("MNEMO_OCR_PACK_DIR must point at the pack"),
    );
    let mut args = std::env::args().skip(1);
    let folder = PathBuf::from(args.next().expect("usage: ocr_gpu_bench <folder> [count]"));
    let count: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(12);

    let det = std::fs::read(pack.join("det.onnx")).expect("det.onnx");
    let rec = std::fs::read(pack.join("rec.onnx")).expect("rec.onnx");
    let dict =
        mnemo_lib::ocr::keys_with_space(&std::fs::read(pack.join("dict.txt")).expect("dict.txt"));

    let mut files: Vec<PathBuf> = std::fs::read_dir(&folder)
        .expect("images folder")
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("png") || e.eq_ignore_ascii_case("jpg"))
        })
        .collect();
    files.sort();
    files.truncate(count);
    let images: Vec<image::DynamicImage> =
        files.iter().filter_map(|p| image::open(p).ok()).collect();
    println!("{} images loaded", images.len());
    println!(
        "available devices: {:?}",
        AccelerationDevice::available_devices()
    );

    for device in [
        AccelerationDevice::Cpu,
        AccelerationDevice::DirectML,
        AccelerationDevice::Cuda,
    ] {
        if !device.is_available() {
            println!("{device}: not available (feature off or runtime has no such provider)");
            continue;
        }
        let load = Instant::now();
        let engine = match OcrEngine::new_with_device(&det, &rec, &dict, device) {
            Ok(e) => e,
            Err(e) => {
                println!("{device}: engine failed: {e}");
                continue;
            }
        };
        println!(
            "{device}: engine loaded in {:.0} ms",
            load.elapsed().as_secs_f64() * 1000.0
        );
        // First call pays kernel compilation and shape setup; report it apart.
        let warm = Instant::now();
        let _ = engine.recognize_all(&images[0], OrderBy::Horizontal);
        println!(
            "{device}: first image (warm-up) {:.0} ms",
            warm.elapsed().as_secs_f64() * 1000.0
        );
        let mut times = Vec::new();
        let mut blocks = 0usize;
        for img in &images {
            let t = Instant::now();
            match engine.recognize_all(img, OrderBy::Horizontal) {
                Ok(b) => blocks += b.len(),
                Err(e) => println!("{device}: recognize failed: {e}"),
            }
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let total: f64 = times.iter().sum();
        let med = median(&mut times.clone());
        println!(
            "{device}: {} images, total {:.0} ms, median {:.0} ms/image, {} text blocks",
            times.len(),
            total,
            med,
            blocks
        );
    }
}
