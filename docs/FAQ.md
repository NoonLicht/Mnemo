# FAQ

**Is anything sent to the cloud?**
No. There is no telemetry and no account, and search queries or file contents
never leave your machine. Mnemo makes two kinds of outbound requests: a GitHub
release check about once every 12 hours, and model/OCR pack downloads. The
default text model downloads automatically after setup, and OCR data for the
enabled engine may download automatically while OCR is on; the optional Indra,
smart-embedding, and visual packs are user-initiated.

**Why is the first launch slow?**
It builds the index. The guided first run completes the initial scan before
opening the main window, showing live counts and ending with a Done summary;
later launches stay search-first and instant.

**What happens during setup?**
The guided flow covers folders, exclusions, preferences (theme + shortcut),
optional OCR/model downloads, and the first scan, ending with a Done summary
before the main window opens. Downloads are acquired there; Settings shows
installed status and **About → Run setup again**.

**Why does search return nothing for a word I know is in a file?**
Check: is the folder watched? Is the extension in the excluded list? Is the file
over the content cap (1 MB indexed, 20 MB extraction ceiling)? Is it binary? Run
Settings → Doctor. The Search syntax card shows filters that may be active.

**Does it index hidden files?**
No by default. `skip_hidden` is on; you can opt in via config.

**Does it index PDFs and Office documents?**
Yes — PDF, DOCX/PPTX/XLSX, EPUB/ODT, RTF. Scanned PDFs need OCR; encrypted PDFs are
flagged, not indexed.

**Does it search images?**
By filename, and by OCR text: PP-OCRv6 by default (runs inside Mnemo, ~0.15 s
a page), or the optional higher-quality Indra engine (~27 MB pack, deskew and
multi-script recognition), or Tesseract where neither can run (Intel Macs) or
if you pick it.
Scanned PDFs are OCR'd too. Also by perceptual near-duplicate. Natural-language visual search ("red cap",
"system design diagram"), similar-image lookup, and an
optional local "this is me" identity are not part of 1.0.
Semantic image search (CLIP) is not implemented yet.

**Can I just type "latest resume" or use `/` and `@`?**
Yes. A deterministic, local intent compiler turns recency, size, type, and place
phrases into the normal filters and sort, and shows each inference as a removable
chip. `/` opens the command menu and `@` inserts a directory scope. The explicit
`key:value` grammar (for example `type:pdf` and `modified:<7d`) always works too.

**OCR is on but no image text is indexed.**
Open Settings → Search → OCR: it shows which engine is actually reading
images and, if PP-OCRv6 cannot run, why (pack still downloading, Intel Mac).
"Verified" means the selected engine read a test image correctly. With
Tesseract, make sure language data is installed (`tesseract-ocr-eng`) or
download it in setup.
Existing images are re-scanned. Doctor repeats the check. The setup pack button
shows download state, success, or the backend error.

**Can it search across multiple drives?**
Yes. Add each root in Settings → Folders. Unplugged drives are preserved rather
than pruned.

**How much memory does it use?**
The metadata layer keeps roughly 128 bytes per file entry (≈12 MB per 100k
files), plus a path index and the ~30 MB static embedding table when the semantic
pack is installed. The crawl is bounded: extracted text is written through a
small queue instead of accumulating in memory, perceptual image hashing runs in a
separate background pass that decodes one image at a time, and the Tantivy writer
arena is 16 MB. On the development machine the indexer process peaked at roughly
72–73 MB for both text-only and mixed image/text corpora (before adding the
~30 MB embedding table). Total app memory is dominated by the OS webview that
hosts the desktop window.

**Does it conflict with Spotlight / Everything / Recoll?**
No. It builds its own index and does not read or write theirs.

**Can two instances run?**
No — a file lock makes a second launch exit cleanly.

**Is the app signed?**
Not yet. macOS needs right-click → Open (or `xattr -dr com.apple.quarantine`),
Windows shows SmartScreen. See [`INSTALL.md`](INSTALL.md#the-builds-are-unsigned).

**Where is my data?**
`~/.config/mnemo/` on Linux, `~/Library/Application Support/mnemo/` on macOS,
`%APPDATA%\mnemo\` on Windows. Settings → About → Reset everything wipes it.

**Can I use it from an AI agent?**
Yes — that's the point of `mnemo-cli` and the MCP server. See
[`AGENTS.md`](AGENTS.md). The app itself contains no LLM.

**Why not bundle an LLM / "ask my files"?**
A local LLM is a multi-gigabyte dependency and a different product. Mnemo stays
the fast, trustworthy index by default. An **optional** local Ollama/LM Studio
connection may compile plain-language intent and power `/ask` with path
citations; agents bring their own model through MCP. No model is bundled, and
file contents never leave the machine unless you explicitly configure a cloud
provider — Mnemo is local-first by design.

**How do I contribute?**
See [`CONTRIBUTING.md`](CONTRIBUTING.md). Tests and clippy must pass.

**Why is the binary not 18 MB like the early plan?**
That plan assumed static ONNX inside the app. The shipped base uses a bundled
static embedding table instead, keeping the app small; the neural tier is optional.
