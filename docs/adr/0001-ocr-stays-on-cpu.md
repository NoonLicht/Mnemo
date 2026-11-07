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
