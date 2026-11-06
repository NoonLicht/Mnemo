# Licence audit (2026-11-04)

Project code: MIT. The upstream `LICENSE` (copyright notice and permission text) stays in the repo as the MIT licence requires.
Ported prototype modules (`activity/`, `ask/`) are ours.

| Item | Source | Licence as stated by the app / upstream | Status |
|---|---|---|---|
| potion-base-8M static embeddings | huggingface.co (hash-pinned) | MIT (model card, not re-verified here) | check the card before release |
| PP-OCRv6 | PaddleOCR via `paddleocr_rs_onnx` | Apache-2.0 | check before release |
| Indra OCR | huggingface.co (hash-pinned) | modified OpenRAIL-M (stated in the app) | **needs reading: use restrictions apply** |
| bge-small (optional) | huggingface.co | MIT | not verified |
| onnxruntime | Microsoft | MIT | not verified |

Nothing above was fetched or read from the network in this session. Verify each model card
and the Indra OpenRAIL-M terms before publishing binaries; see ADR-0008 in `Projects/Mnemo`.
