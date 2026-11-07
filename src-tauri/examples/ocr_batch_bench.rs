//! Batched and mixed CPU/GPU OCR scenarios on real images. Prints timings
//! only; recognised text is never printed.
//!
//! ```text
//! set ORT_DYLIB_PATH=<onnxruntime.dll>   (CUDA scenarios need the GPU build + CUDA/cuDNN on PATH)
//! set MNEMO_OCR_PACK_DIR=<folder with det.onnx, rec.onnx, dict.txt>
//! cargo run --release --example ocr_batch_bench --features cuda -- <images> <count> <scenario>...
//!
//! scenarios:  cpu-single | cpu-batch:B | gpu-single | gpu-batch:B | gpu-group:K:B
//!             hybrid:K:B:W   (one GPU thread taking K images per turn with batch B,
//!                             plus W CPU worker threads taking one image each)
//! ```
use paddleocr_rs_onnx::{AccelerationDevice, OcrEngine, OrderBy, PreparedCrop, TextRegion};
use rayon::prelude::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[derive(Default, Clone, Copy)]
struct Stages {
    det: Duration,
    prep: Duration,
    rec: Duration,
    blocks: usize,
    crops: usize,
}

impl Stages {
    fn add(&mut self, o: Stages) {
        self.det += o.det;
        self.prep += o.prep;
        self.rec += o.rec;
        self.blocks += o.blocks;
        self.crops += o.crops;
    }
}

/// Detection per image, then ONE padded-batch recognition over the crops of
/// the whole group.
fn process_group(engine: &OcrEngine, imgs: &[&image::DynamicImage], batch: usize) -> Stages {
    let mut st = Stages::default();
    let mut crops: Vec<PreparedCrop> = Vec::new();
    for img in imgs {
        let t = Instant::now();
        let regions: Vec<TextRegion> = engine.detect_text_regions(img).unwrap_or_default();
        st.det += t.elapsed();
        let t = Instant::now();
        let prepared: Vec<PreparedCrop> = regions
            .par_iter()
            .filter_map(|r| engine.prepare_region(img, r).ok())
            .collect();
        st.prep += t.elapsed();
        crops.extend(prepared);
    }
    let t = Instant::now();
    let out = engine.recognize_prepared(&crops, batch);
    st.rec += t.elapsed();
    st.crops = crops.len();
    st.blocks = out.iter().flatten().filter(|d| !d.text.is_empty()).count();
    st
}

fn run_single(engine: &OcrEngine, imgs: &[image::DynamicImage]) -> Stages {
    let mut st = Stages::default();
    for img in imgs {
        st.blocks += engine
            .recognize_all(img, OrderBy::Horizontal)
            .map(|b| b.len())
            .unwrap_or(0);
    }
    st
}

/// Lets two processes start their timed runs at the same wall-clock moment.
fn barrier() {
    if let Some(at) = std::env::var("MNEMO_BARRIER_MS")
        .ok()
        .and_then(|v| v.parse::<u128>().ok())
    {
        loop {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            if now >= at {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn main() {
    let pack = PathBuf::from(std::env::var("MNEMO_OCR_PACK_DIR").expect("MNEMO_OCR_PACK_DIR"));
    let mut args = std::env::args().skip(1);
    let folder = PathBuf::from(args.next().expect("images folder"));
    let count: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(100);
    let scenarios: Vec<String> = args.collect();

    let det = std::fs::read(pack.join("det.onnx")).unwrap();
    let rec_file = std::env::var("MNEMO_REC_FILE").unwrap_or_else(|_| "rec.onnx".into());
    println!("rec model: {rec_file}");
    let rec = std::fs::read(pack.join(&rec_file)).unwrap();
    let dict = mnemo_lib::ocr::keys_with_space(&std::fs::read(pack.join("dict.txt")).unwrap());

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
    // MNEMO_RANGE="a-b" keeps only that slice, so two processes can split one set.
    if let Some((a, b)) = std::env::var("MNEMO_RANGE").ok().and_then(|v| {
        let (a, b) = v.split_once('-')?;
        Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?))
    }) {
        files = files[a.min(files.len())..b.min(files.len())].to_vec();
    }
    let images: Vec<image::DynamicImage> =
        files.iter().filter_map(|p| image::open(p).ok()).collect();
    println!(
        "{} images, cpu cores {}",
        images.len(),
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(0)
    );

    let mut cpu: Option<OcrEngine> = None;
    let mut gpu: Option<OcrEngine> = None;
    let load = |device| OcrEngine::new_with_device(&det, &rec, &dict, device).expect("engine");

    println!("scenario | wall s | img/s | blocks | det s | prep s | rec s | crops | notes");
    for sc in &scenarios {
        let p: Vec<&str> = sc.split(':').collect();
        let num = |i: usize, d: usize| p.get(i).and_then(|s| s.parse().ok()).unwrap_or(d);
        let need_gpu = p[0].starts_with("gpu") || p[0] == "hybrid";
        let need_cpu = p[0].starts_with("cpu") || p[0] == "hybrid";
        if need_cpu && cpu.is_none() {
            cpu = Some(load(AccelerationDevice::Cpu));
        }
        if need_gpu && gpu.is_none() {
            gpu = Some(load(AccelerationDevice::Cuda));
        }
        let warm = |e: &OcrEngine| {
            // Warm-up on a few images so kernel compilation and shape setup are
            // not counted; the same images are then timed again.
            let t = Instant::now();
            let refs: Vec<&image::DynamicImage> = images.iter().take(3).collect();
            let _ = process_group(e, &refs, 64);
            // The per-image path cannot read the reduced (argmax) model output.
            if std::env::var("MNEMO_REC_FILE").is_err() {
                let _ = e.recognize_all(&images[0], OrderBy::Horizontal);
            }
            t.elapsed()
        };

        let started;
        let mut st = Stages::default();
        let mut note = String::new();
        match p[0] {
            "cpu-single" | "gpu-single" => {
                let e = if p[0] == "cpu-single" {
                    cpu.as_ref()
                } else {
                    gpu.as_ref()
                }
                .unwrap();
                warm(e);
                barrier();
                started = Instant::now();
                st = run_single(e, &images);
            }
            "cpu-batch" | "gpu-batch" | "gpu-group" => {
                let e = if p[0] == "cpu-batch" {
                    cpu.as_ref()
                } else {
                    gpu.as_ref()
                }
                .unwrap();
                let (k, b) = if p[0] == "gpu-group" {
                    (num(1, 8), num(2, 64))
                } else {
                    (1, num(1, 64))
                };
                warm(e);
                barrier();
                started = Instant::now();
                for group in images.chunks(k) {
                    let refs: Vec<&image::DynamicImage> = group.iter().collect();
                    st.add(process_group(e, &refs, b));
                }
                note = format!("images/turn {k}, batch {b}");
            }
            "hybrid" => {
                let (k, b, w) = (num(1, 8), num(2, 64), num(3, 2));
                let (c, g) = (cpu.as_ref().unwrap(), gpu.as_ref().unwrap());
                warm(c);
                warm(g);
                let next = AtomicUsize::new(0);
                let blocks = AtomicUsize::new(0);
                let gpu_imgs = AtomicUsize::new(0);
                barrier();
                started = Instant::now();
                std::thread::scope(|s| {
                    s.spawn(|| {
                        loop {
                            let i = next.fetch_add(k, Ordering::SeqCst);
                            if i >= images.len() {
                                break;
                            }
                            let end = (i + k).min(images.len());
                            let refs: Vec<&image::DynamicImage> = images[i..end].iter().collect();
                            let r = process_group(g, &refs, b);
                            blocks.fetch_add(r.blocks, Ordering::SeqCst);
                            gpu_imgs.fetch_add(end - i, Ordering::SeqCst);
                        }
                    });
                    for _ in 0..w {
                        s.spawn(|| {
                            loop {
                                let i = next.fetch_add(1, Ordering::SeqCst);
                                if i >= images.len() {
                                    break;
                                }
                                let r = process_group(c, &[&images[i]], 64);
                                blocks.fetch_add(r.blocks, Ordering::SeqCst);
                            }
                        });
                    }
                });
                st.blocks = blocks.load(Ordering::SeqCst);
                note = format!(
                    "GPU did {} imgs, CPU did {}; K={k} B={b} cpu workers={w}",
                    gpu_imgs.load(Ordering::SeqCst),
                    images.len() - gpu_imgs.load(Ordering::SeqCst).min(images.len())
                );
            }
            other => {
                println!("{other}: unknown scenario");
                continue;
            }
        }
        let wall = started.elapsed().as_secs_f64();
        println!(
            "{sc} | {wall:.1} | {:.1} | {} | {:.1} | {:.1} | {:.1} | {} | {note}",
            images.len() as f64 / wall,
            st.blocks,
            st.det.as_secs_f64(),
            st.prep.as_secs_f64(),
            st.rec.as_secs_f64(),
            st.crops,
        );
    }
}
