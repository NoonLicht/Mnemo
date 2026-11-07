//! End-to-end check of hybrid OCR through the real extraction entry point
//! (`ocr::extract_image_ocr`), with the real GPU helper process.
//!
//! The example binary doubles as the helper: `mnemo_lib::run()` handles the
//! `--ocr-gpu-worker` flag.
//!
//! ```text
//! set ORT_DYLIB_PATH=<bundled onnxruntime.dll>        (CPU side)
//! set MNEMO_DATA_DIR=<dir that contains gpu-runtime\ with the three DLLs>
//! set MNEMO_OCR_PACK_DIR=<folder with det.onnx, rec.onnx, dict.txt>
//! cargo run --release --example gpu_ocr_probe -- <images folder> <count> <threads>
//! ```
use mnemo_lib::{gpu_ocr, ocr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn pass(files: &[PathBuf], threads: usize) -> (Duration, Vec<String>) {
    let next = AtomicUsize::new(0);
    let results = std::sync::Mutex::new(vec![String::new(); files.len()]);
    let started = Instant::now();
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    if i >= files.len() {
                        break;
                    }
                    let text = match ocr::extract_image_ocr(&files[i]) {
                        ocr::OcrOutcome::Text(t) => t,
                        _ => String::new(),
                    };
                    results.lock().unwrap()[i] = text;
                }
            });
        }
    });
    (started.elapsed(), results.into_inner().unwrap())
}

fn norm(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == gpu_ocr::WORKER_FLAG) {
        mnemo_lib::run();
        return;
    }
    let folder = PathBuf::from(&args[1]);
    let count: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(100);
    let threads: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(4);

    let mut files: Vec<PathBuf> = std::fs::read_dir(&folder)
        .unwrap()
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
    ocr::set_engine("accurate");
    println!("{} images, {threads} extraction threads", files.len());

    // Warm the CPU engine, then time the CPU-only pass.
    let _ = ocr::extract_image_ocr(&files[0]);
    let (cpu_time, cpu_text) = pass(&files, threads);
    println!(
        "cpu     : {:.1} s ({:.1} img/s)",
        cpu_time.as_secs_f64(),
        files.len() as f64 / cpu_time.as_secs_f64()
    );

    gpu_ocr::set_mode("hybrid");
    println!("mode now {}", gpu_ocr::mode());
    let boot = Instant::now();
    loop {
        let _ = ocr::extract_image_ocr(&files[0]);
        let status = gpu_ocr::status();
        if status.helper_ready {
            println!("helper ready after {:.1} s", boot.elapsed().as_secs_f64());
            break;
        }
        if let Some(problem) = status.problem {
            println!("helper problem: {problem}");
            return;
        }
        if boot.elapsed() > Duration::from_secs(300) {
            println!("helper never became ready");
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let before = gpu_ocr::status();
    let (hy_time, hy_text) = pass(&files, threads);
    let after = gpu_ocr::status();
    println!(
        "hybrid  : {:.1} s ({:.1} img/s)",
        hy_time.as_secs_f64(),
        files.len() as f64 / hy_time.as_secs_f64()
    );
    println!(
        "GPU read {} images, {} fell back to the CPU",
        after.gpu_images - before.gpu_images,
        after.cpu_fallbacks - before.cpu_fallbacks
    );
    let same = cpu_text
        .iter()
        .zip(&hy_text)
        .filter(|(a, b)| norm(a) == norm(b))
        .count();
    let words = |t: &str| t.split_whitespace().count();
    let cpu_words: usize = cpu_text.iter().map(|t| words(t)).sum();
    let hy_words: usize = hy_text.iter().map(|t| words(t)).sum();
    println!(
        "identical text for {same}/{} images; words cpu {cpu_words}, hybrid {hy_words}",
        files.len()
    );
}
