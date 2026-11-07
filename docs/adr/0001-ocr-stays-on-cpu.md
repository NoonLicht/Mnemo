# ADR 0001: OCR stays on the CPU

Status: accepted, 2026-11-07

## Context
The owner asked whether OCR could run on the GPU when one is present (RTX 5060 Ti on the main PC). Both OCR stacks run on ONNX Runtime: PP-OCR through the vendored `paddleocr_rs_onnx` (which already has opt-in `directml`, `cuda` and other provider features), and Indra through its own engine that pins the CPU provider.

## Options
1. Keep CPU only.
2. Enable DirectML (any DirectX 12 GPU, small dependency).
3. Enable CUDA (NVIDIA only, large runtime, Blackwell needs recent CUDA and ONNX Runtime builds).

## Measurement
`examples/ocr_gpu_bench.rs`, PP-OCR pack, 40 of the owner's screenshots (up to 3437x1437), the same ONNX Runtime 1.23.2 DirectML-capable build, RTX 5060 Ti, debug profile for Mnemo's own code:

| Device | Total for 40 images | Median per image | Text blocks |
|---|---|---|---|
| CPU | 1338 ms | 23 ms | 442 |
| DirectML | 3567 ms | 66 ms | 442 |

DirectML also paid 8.0 s for first-use shader compilation on the first run (0.4 s later). Results are identical, only slower. Indra was not measured; its provider is fixed to CPU.

## Decision
Keep OCR on the CPU. The `directml` Cargo feature stays in the tree, off by default, so the benchmark can be repeated with a different model or GPU.

## Consequences
- The bundled ONNX Runtime does not need the DirectML provider or DirectML.dll.
- Battery use on the laptop is not affected by an unused GPU path.
- Revisit if a larger OCR model is adopted or the batch shapes become static.

## Addendum: CUDA, batching and CPU+GPU (2026-11-07)
Setup: 200 of the owner's screenshots, RTX 5060 Ti, 12 CPU cores, CUDA 12.9, cuDNN 9.26, ONNX Runtime 1.23.2 GPU build, cuDNN algorithm search `heuristic` (the default `exhaustive` needed 59 s for the first image on Blackwell). Benchmark: `examples/ocr_batch_bench.rs`. One run per row, no repetitions.

| Scenario | Wall time | Images/s |
|---|---|---|
| CPU, per image (production path, bundled ONNX Runtime) | 10.7 s | 18.7 |
| CUDA, per image | 30.8-31.4 s | 6.4 |
| CUDA, recognition in padded batches of 64 (full 6906-class output) | 24.2 s | 8.3 |
| CUDA, batches of 64, argmax inside the model | 18.5 s | 10.8 |
| CUDA, 32 images per turn, batches of 256, argmax inside the model | 12.9-13.8 s | 14.5-15.6 |
| CPU in batches, argmax inside the model | 18.2 s | 11.0 |
| CPU and CUDA as two processes, 140/60 split | 7.9 s | 25.3 |
| CPU and CUDA as two processes, 155/45 split | 7.7 s | 26.0 |

Findings:
- The recognition model emits 6906 probabilities per time step. Copying and decoding that tensor, not GPU compute, was the bottleneck; adding `ArgMax` and `ReduceMax` nodes to the model (output `[n, t]` instead of `[n, t, 6906]`) cut batched recognition time by about 40%.
- A few very wide crops produced a 2.5 GB activation and failures; batches need a cap on total padded columns (16384 here).
- Alone, CUDA stays 20-30% slower than the CPU. Detection (not batched, about 37 ms per image) is now the largest part of the GPU time.
- Running both at the same time gives a 28% shorter wall time (7.7 s against 10.7 s) when the split is balanced. The GPU side slows down by about 40% while the CPU is busy. Text block counts differ by under 1%.
- The CPU path in the GPU-capable ONNX Runtime build was 2.5-3x slower than in the bundled build, so an in-process CPU+GPU engine would need one ONNX Runtime that is fast on both.

Decision unchanged: OCR stays on the CPU by default. A hybrid mode would cost about 1 GB of CUDA libraries, NVIDIA-only code, a modified recognition model and a work queue across two runtimes, for roughly 28% on large OCR batches. The `cuda` feature and both benchmarks stay in the tree, off by default.

## Addendum 2: hybrid mode built into the app (2026-11-07)
Hybrid OCR is an opt-in setting (`ocr_compute`, `--ocr=cpu|hybrid`): a helper process (`Mnemo.exe --ocr-gpu-worker`) loads an ONNX Runtime GPU build, patches the recogniser in memory so ArgMax/ReduceMax run on the device (`onnx_patch.rs`) and reads images in groups when idle. The GPU runtime (311 MB) is downloaded on request and checked against GitHub's published SHA-256.

**Correction.** v1.1.0 was built without the `cuda` Cargo feature, so the helper's CUDA provider was never registered and the helper silently ran on the CPU of the GPU-build ONNX Runtime. The in-app numbers first recorded here (hybrid 9 to 11 s against 8.3 s CPU-only, "the GPU is not faster") measured CPU against a slower CPU and say nothing about the GPU. The examples used for those runs were built the same way. The bench examples built with `--features cuda` were right. `cuda` is now a default feature and the helper refuses to start without it (`MNEMO_CUDA_STRICT`).

## Addendum 3: speed work, measured (2026-11-07, same PC: Ryzen 5 5600X 6 cores / 12 threads, RTX 5060 Ti)
200 of the owner's screenshots, one run per row unless stated, so differences under about 0.5 s are noise.

### CPU
Single-thread time per stage over the 200 images (27.7 s): recognition inference 17.9 s (64%), detector inference 6.5 s (23%), recognition crop and normalise 1.4 s, detector preprocessing 1.0 s, recognition decode 0.55 s, image decoding 0.41 s, detector postprocessing 0.27 s. Image decoding is about 1.5%, so decoding once or at reduced size is not worth it; the RGB conversion is now done once per image instead of once per text region.

| Change | Result |
|---|---|
| Before (detector behind one locked session) | 8.2 to 10.2 s (20 to 24 img/s) |
| Detector session pool, preprocessing and postprocessing outside the lock | 7.2 to 8.4 s; the one-thread-per-session setting went from 12.1 s to 7.4 s |
| + argmax inside the recogniser on the CPU path | 6.9 to 7.4 s (27 to 29 img/s) |
| ORT threads per session, spinning, session pool size (12 combinations) | all within noise (6.9 to 7.4 s); defaults kept |

The remaining gap to a perfect 12-way speed-up is the hardware: 6 physical cores.

Rejected after measuring:
- INT8, dynamic, recogniser: slower (7.6 s against 6.8 s), 95.2% exact lines on the 104-line ground-truth fixture (100% for FP32), words broken apart ("imag es"). The signed variant produced no text.
- INT8, static, recogniser (calibrated on fixture crops): per-channel does not load in ONNX Runtime 1.23 with this opset-11 model; per-tensor gave 0% (all operators) and 28.8% (MatMul only) exact lines.
- INT8, detector: dynamic was slower (8.5 s) and changed 73% of words; static QDQ was faster (5.7 s against 6.8 s, +17%) but changed about 35% of the words, and there is no ground truth for screenshot-size detection to prove it is not worse. This CPU has no VNNI, which limits INT8 gains.
- Detector input size: 640 px took 4.4 s but found 20% fewer words; 736 px took 5.0 s and found 12% fewer; 1280 px took 8.4 s and found 4% more. The default 960 stays.

### Hybrid (GPU helper)
- The helper alone (CUDA actually in use): 24.4 img/s on the first 60 images, 17 img/s on images 100 to 160, using about 0.05 to 0.085 CPU-seconds per image (the CPU path uses about 0.43).
- Fixed detector input (960 x 960 canvas): detector inference 39 ms to 9-17 ms per image. Padding recognition batches to fixed shapes made recognition slower (21 to 28-34 ms per image) and was dropped.
- FP16 models: recognition 2x slower (9.3 s against 4.3 s per 100 images), detector unchanged; rejected.
- Pipelining: a second thread decodes and preprocesses the next image while the GPU detects the current one. Preprocessing is 15 to 50 ms per group of 6 against 60 to 70 ms of detection, and the GPU thread waited 2 to 7 ms for it, so moving normalisation onto the GPU could save only a few percent and was not built.
- Groups: one image per turn 10.8 img/s, groups of 8 up to 24.9 img/s in the stand-alone bench.
- In the app flow (probe, 12 extraction threads), CPU-only 8.0 s:

| Threads allowed to wait for the GPU | Helper priority | Hybrid time | Images read by the GPU |
|---|---|---|---|
| 6 | normal or above | 10.1 to 11.0 s | 176 to 191 |
| 1 to 4 | normal | 7.4 to 8.0 s | 7 to 25 |
| 2 to 5 | above-normal | 6.4 to 7.0 s | 54 to 69 |

With 6 waiting threads the extraction threads queue up behind the GPU and the CPU idles; at normal priority the helper is starved by the saturated CPU engine. The shipped defaults are about 5 waiting threads per 12 logical cores and above-normal priority, giving 6.4 to 7.0 s (29 to 31 img/s), 17 to 20% faster than CPU-only in the same session. The CPU-only figure used default thread settings; against the best CPU-only runs (6.9 s) the margin is smaller. Text read on the GPU differs from the CPU's in 25 to 30% of images in small ways (words: 10966 to 11004 against 10978).

Final check with the shipped defaults (three runs): hybrid 6.5, 6.4 and 6.3 s against CPU-only 8.1, 8.0 and 8.0 s, the GPU reading 65 to 70 images.

Decision: hybrid stays experimental and off by default. Production checks: the patched recogniser reads the 104-line fixture at 100% exact.
