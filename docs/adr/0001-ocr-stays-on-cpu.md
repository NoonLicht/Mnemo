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
Hybrid OCR now exists as an opt-in setting (`ocr_compute`, `--ocr=cpu|hybrid`): a helper process (`Mnemo.exe --ocr-gpu-worker`) loads an ONNX Runtime GPU build, patches the recogniser in memory so ArgMax/ReduceMax run on the device (`onnx_patch.rs`) and reads one image at a time when idle. The GPU runtime (311 MB) is downloaded on request and checked against GitHub's published SHA-256.

Measured through the real entry point (`examples/gpu_ocr_probe.rs`, 200 screenshots, 12 extraction threads, one run per row, CPU-only pass 8.2-8.4 s every time):

| Configuration | Hybrid wall time | Images read by the GPU |
|---|---|---|
| Normal priority (shipped) | 9.1 s | 24 |
| Helper at above-normal priority | 11.2 s | 55-73 |
| Helper limited to 1-2 ONNX threads | 10.9-11.6 s | 62-70 |
| Same, spinning off, 2 rayon threads | 10.9 s | 61 |

The hybrid pass was slower than the CPU alone in every configuration. The helper used about 58-67 CPU seconds for 61-74 images (roughly 0.9 CPU-s per image, three times the CPU path's cost) while the GPU stayed at 1-5% load, so the cost is in host-side work around the GPU, not in the model. The cause was not found. The stand-alone benchmark above (7.7 s) used batches of 32 images per turn and a fixed split; the app offers one image per turn.

Decision: the feature ships as experimental, off by default, and says so in Settings and the README. A next step would be profiling the helper (image decoding, detection pre- and post-processing, CUDA synchronisation) and sending it groups of images.
