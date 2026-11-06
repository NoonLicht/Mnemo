# Mnemo — public benchmark page

Method, corpus, hardware, raw numbers, and caveats. All numbers are from Linux
release builds unless noted. Mac and Windows numbers are not yet available, so
no "fastest" claim is made.

## Hardware

- Linux, 8 vCPU, SSD.

## Corpus

- ~101,101 files in one tree (mixed names, documents, images, code).
- Default exclusions applied (`node_modules`, `.git`, `target`, `dist`, …).

## Results

| What | Result |
|---|---|
| Name search, in memory (avg of 8 queries) | 1.3 ms (best 0.5 ms, worst 2.4 ms) |
| Index 101,101 files, first run | 184 s, peak 183 MB |
| Re-index with nothing changed | 0.56 s |
| Memory for file names and metadata, 100k files | ~12 MB |

## Comparison (same folder, same machine, filename search, cold process each run)

| Tool | "invoice" | "deploy_error" |
|---|---|---|
| Mnemo (in memory, no start-up) | 1.3 ms avg | 1.3 ms avg |
| plocate | 34 ms | 6 ms |
| fd | 109 ms | 98 ms |

## Caveats

- Mnemo's search is fuzzy and runs inside a running app; plocate/fd start a
  new process per search. Not an apples-to-apples comparison.
- Spotlight, Everything, and Dowse are not measured yet.
- Numbers are Linux only; macOS and Windows are unmeasured.
- Install size: Linux release binary is ~34 MB. Index size and per-file bytes
  depend on the corpus and enabled features (OCR, embeddings).
- Idle CPU/RAM while hidden: measure with `pidstat` or by sampling
  `/proc/<pid>/stat`; WebKit helper processes are terminated on hide, leaving
  only the Rust process.

## How to reproduce

See `docs/BENCHMARKS.md` for harness commands.
