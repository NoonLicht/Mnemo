# Mnemo v2 — Architecture & Implementation Plan

> **Status:** shipped `v1.0.0`. The daemon topology in §3 is shipped: the GUI,
> CLI, and MCP are clients of `mnemo-daemon` over a Unix socket or Windows
> named pipe, with a read-only in-process fallback. The loopback HTTP transport
> in §9.3 is also shipped (`mnemo-cli serve`). This document describes the
> architecture as built; internal planning documents were removed for release.

> **One sentence:** a warm, local, read-only index of your machine that answers in under 15 ms — for you, and for every AI agent you use.

---

## 1. North star

| Goal | Concrete meaning |
|---|---|
| **Instant** | p50 keystroke → painted results < 15 ms at 1M files. p99 < 60 ms. |
| **Correct** | Zero ghost results. Zero silent staleness. Freshness is a visible number. |
| **Simple** | Zero-config default. One shortcut. One box. One list. No modes, no tabs. |
| **Smart** | Semantic search that costs ~0.1 ms per query and never needs a GPU. |
| **Universal** | The same index serves the GUI, the CLI, and MCP/HTTP for agents. |
| **Small** | Core binary ~15 MB, GUI bundle ~25 MB, semantic model loaded as a ~7.7 MB i8 table (f32 fallback is ~30 MB). |

### UX invariants (non-negotiable, encode these in tests)

1. Typing never blocks. Ever. Degrade results, not input.
2. Never blank → spinner → results. Show the previous results until the new ones are ready.
3. Results never reorder while you're still typing. Stability beats optimality.
4. Freshness is always visible (`12 ms · 1.2M files · index 2 s old`).
5. Filters are visible chips you can click off. An invisible filter is a bug report.
6. Every capability is a `filter:` or a `>` command — never a new screen.
7. First run is one guided flow: folders, preferences, optional packs, first scan, Done.

---

## 2. Non-goals (deliberate)

- No cloud, no accounts, no telemetry, no sync, no cross-device.
- No bundled LLM. "Ask" uses a local Ollama/LM Studio if present.
- No graph database, no agent framework, no plugin marketplace.
- No write operations through the agent API. Read-only, always.
- No cross-encoder reranker in the hot path (30–60 ms; it does not fit).
- No ANN index below 400k semantic units (exact scan is faster and bug-free).

---

## 3. System topology

```
┌────────────────────────────── clients ──────────────────────────────┐
│  GUI (Tauri 2 / React)      CLI (`mnemo search`)     AI agents     │
│         │                          │                   (MCP stdio / │
│         │  IPC (UDS/named pipe)    │                    HTTP)       │
└─────────┼──────────────────────────┼───────────────────┼────────────┘
          └──────────────┬───────────┴───────────────────┘
                         ▼
        ┌────────────────────────────────────────────┐
        │        mnemo-daemon (one process)        │
        │  single instance · single writer · 0600    │
        ├────────────────────────────────────────────┤
        │  Query planner   →   Fusion/RRF   →  Snips │
        ├──────────┬──────────────┬──────────────────┤
        │ Meta     │ Content      │ Semantic         │
        │ tier     │ tier         │ tier (optional)  │
        │ arena +  │ Tantivy BM25 │ model2vec +      │
        │ tree     │ + fastfields │ i8 vectors       │
        ├──────────┴──────────────┴──────────────────┤
        │ Extraction workers (bounded pool, priority) │
        │ Watcher backends: inotify/FSEvents/USN/Poll │
        │ Reconciler · Embed queue · Merger           │
        ├────────────────────────────────────────────┤
        │ SQLite WAL (durable truth) + mmap snapshots │
        └────────────────────────────────────────────┘
```

**Why a daemon, not an in-app backend:** the agent surface and the CLI require the index to exist without the GUI. Building it in-process first makes that a rewrite. The GUI becomes a client — which also means index work never competes with webview paint.

**Two binaries, one core crate:** `mnemo` (daemon + CLI + MCP) and `mnemo-app` (Tauri GUI). The GUI auto-spawns the daemon if it isn't running.

**Current state (v1.0):** the GUI connects to the daemon for search, snippets, status, config, home data, duplicates, scope suggestions, and pin toggles. It constructs only a read-only `IndexerManager` for fallback when the daemon is unavailable. CLI and MCP also prefer the daemon. Unix and Windows transports use `interprocess` local sockets (Unix socket / named pipe); release packaging of the daemon sidecar remains a packaging follow-up.

---

## 4. Data model

### 4.1 SQLite (durable truth, WAL, `mmap_size=512MB`)

```sql
CREATE TABLE roots (
  id INTEGER PRIMARY KEY, path BLOB NOT NULL UNIQUE,
  mode TEXT NOT NULL,            -- index | meta_only | ignore
  added_at INTEGER NOT NULL
);

CREATE TABLE nodes (             -- files AND directories
  id         INTEGER PRIMARY KEY,
  parent     INTEGER REFERENCES nodes(id),
  name       BLOB NOT NULL,      -- raw OsStr bytes
  name_fold  BLOB NOT NULL,      -- NFC + lowercase, for matching only
  kind       INTEGER NOT NULL,   -- 0 dir | 1 file
  ext        INTEGER,            -- interned extension id
  size       INTEGER,
  mtime_ns   INTEGER,
  ctime_ns   INTEGER,
  dev        INTEGER,
  ino        INTEGER,
  file_id    BLOB,               -- Windows FILE_ID / macOS fsobj_id
  flags      INTEGER,            -- symlink, hidden, cloud_placeholder, remote_fs
  content_hash BLOB,             -- BLAKE3, only when extracted
  extract_state INTEGER,         -- 0 pending 1 ok 2 skipped 3 failed
  extract_attempts INTEGER,
  extract_error  TEXT,
  indexed_gen INTEGER            -- generation of content index for this node
);
CREATE INDEX nodes_parent ON nodes(parent);
CREATE INDEX nodes_ident  ON nodes(dev, ino);
CREATE UNIQUE INDEX nodes_path ON nodes(parent, name);

CREATE TABLE units (             -- semantic units (doc-level or chunk-level)
  id INTEGER PRIMARY KEY,
  node_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
  byte_start INTEGER NOT NULL,
  byte_end   INTEGER NOT NULL,
  vec_id     INTEGER NOT NULL,
  model_id   INTEGER NOT NULL
);
CREATE INDEX units_node ON units(node_id);

CREATE TABLE usage (             -- frecency
  node_id INTEGER PRIMARY KEY, opens INTEGER DEFAULT 0,
  last_open INTEGER, last_query_hit INTEGER
);

CREATE TABLE meta (key TEXT PRIMARY KEY, value BLOB NOT NULL);
-- schema_version, index_gen, model_id, last_reconcile, daemon_token
```

**Why SQLite + in-memory snapshot, not a pure mmap file:** crash safety and migrations for free; the custom mmap format (Everything-style) is a later optimization if load time exceeds 400 ms at 1M.

### 4.2 In-memory snapshot (built at startup, mutated incrementally)

```rust
struct Snapshot {
    // Struct-of-arrays: one allocation each, no per-entry PathBuf/String.
    names:      Vec<u8>,        // concatenated folded names
    name_off:   Vec<u32>,       // len = n+1
    parent:     Vec<u32>,
    first_child:Vec<u32>,       // + next_sibling: Vec<u32>  → child lists
    next_sibling: Vec<u32>,
    kind_ext:   Vec<u32>,       // kind bit + ext id
    size:       Vec<u64>,
    mtime:      Vec<u64>,
    char_bloom: Vec<u64>,       // 64-bit letter-bucket mask per name
    id_map:     HashMap<u32, u32>, // disk id ↔ slot, only for mutations
}
```

- ~64 bytes/file → **64 MB per 1M files** (vs several hundred MB with `PathBuf`).
- `char_bloom` is the cheap prefilter: a query's character set is AND-ed against each entry; one AND rejects 80–95% before the matcher runs.
- Folder scope = DFS over child lists, cached per `(dir_id, generation)`. Targeted scopes (the common case) visit hundreds of nodes, not millions.
- Mutations go to a small delta buffer and merge into the arena at idle; a full rebuild takes ~300 ms per 1M and happens only on schema change.

---

## 5. Index tiers

### 5.1 Metadata tier (always fresh, in-memory) — budget **2–4 ms**

- Built from the filesystem journal (USN/FSEvents/inotify), not from crawling.
- Lookup: `(dev, ino)` and `(parent, name)` — O(1).
- Used for: filename search, scoping, filters on size/mtime/kind, dedupe, path display.

### 5.2 Content tier (Tantivy, batched) — budget **1–3 ms**

Schema:

| field | type | purpose |
|---|---|---|
| `node_id` | u64, fast, indexed | delete-by-term, join to metadata |
| `name` | text (code tokenizer) | filename relevance |
| `content` | text (code+CJK tokenizer), **not stored** | BM25 |
| `ext` | u32, fast | `type:` filter |
| `mtime` | u64, fast | `modified:` range |
| `size` | u64, fast | `size:` range |
| `lang` | u8, fast | `lang:` filter |

- **Custom tokenizer:** The `"mnemo"` tokenizer decomposes CJK scripts (Han, Hiragana, Katakana, Hangul) into interleaved unigrams and bigrams with exact offsets and sequential positions, enabling exact word and substring searches across unbroken CJK runs. SimpleTokenizer behavior is preserved for Latin, code, and paths. Schema marker `content.schema` (v3) automates legacy migration.
- **Never** `QueryParser`. Build `Query` objects from our AST. No OR-by-default.
- **Content policy:** full text up to 1 MB; head 512 KB + tail 512 KB for larger; metadata-only above 20 MB.
- **Commit policy:** debounce 2 s or 2,000 docs; the current writer arena is 16 MB. `wait_merging_threads()` + `garbage_collect_files()` at idle, and a weekly force-merge.
- **Snippets:** never read a file inside the query. Fetch after ranking, in the blocking pool, for the visible rows only, with a small LRU.
- **Format decision:** the 1M-document benchmark rejected replacing this tier with SQLite FTS5 trigram (918 MB, 258 ms broad BM25) and rejected a sidecar; keep Tantivy and the folded field. See [BENCHMARKS.md](BENCHMARKS.md).

### 5.3 Semantic tier — budget **0.5–2 ms**

**Default model: `potion-base-8M` (model2vec, static, 256-dim).** The table is stored as i8 with per-row scales (~7.7 MB; the f32 fallback is ~30 MB). No ONNX, no neural graph at query time: tokenize → table lookup → weighted mean → normalize. **~50 µs per query.** ~92 % of MiniLM's MTEB average. Embedding the corpus is also cheap (~50k docs/s), so full re-embed after a model change is minutes, not hours.

**Optional "Smart" pack** (downloaded, ~40 MB): `fastembed-rs` + `ort` with **`load-dynamic`** (never static — prebuilt static ORT requires AVX2 and SIGILLs on older CPUs; dynamic also fixes macOS codesign and Windows dylib swaps). `bge-small-en-v1.5` or `EmbeddingGemma-Q4` for multilingual quality.

**Units and chunking:**

```rust
struct Unit { node_id, byte_start, byte_end, model_idx }  // 1 vector per unit
```
- Files ≤ 240 tokens → one doc-level unit (offset 0).
- Longer files → 240-token windows, 48 overlap, max 20 units/file.
- Embedding cost is deduped by `content_hash`: identical files share one vector computation (and one vector, referenced twice).
- Query returns **nodes**, with the best unit's byte range attached → highlight and scroll position.

**Retrieval:**
- ≤ 400k units: exact scan over memory-mapped `Vec<i8>` (256 B/unit → 100 MB at 400k), 8-way parallel → < 2 ms.
- \> 400k units: build USearch HNSW (`cos`, i8, M=16, efConstruction=200, ef=64) at idle. Deletions via tombstones; rebuild at idle when tombstones > 15 %.
- **Model identity is per-index:** `units.model_id` + `meta.model_id`. Changing the model never corrupts results — the old index keeps serving while the new one fills.

**Crawl scheduling:** `crawl_mode` selects `full`, `balanced`, or `eco`. Balanced/eco put the crawl thread in the OS idle CPU/I/O class; eco also paces extraction starts on battery. The Settings `pause_indexing` switch gates reads but keeps renames/deletes live. Watcher events settle for 400 ms; files with a fresh mtime are fingerprinted again before their content is trusted.

**Embedding queue:** bounded, resumable cursor in `nodes.indexed_gen`, priority = recently-modified first, then small files first. Hard rules: stop on battery, stop when a query is in flight, stop during fullscreen apps, max 2 threads.

**Optional ONNX smart embedding pack:**
- **Model:** `bge-small-en-v1.5` (quantized int8 ONNX).
- **License:** MIT License.
- **Parameters & Dimensions:** 33.4M parameters, 384 dimensions (quantized to i8, unit-normalized).
- **Disk size:** ~34.7 MB total (`model.onnx` 34,014,426 B, `tokenizer.json` 711,396 B, `config.json` 683 B).
- **Inference & Concurrency:** Loaded through `ort` under the optional `smart-embeddings` feature. Single intra-op thread per session; maximum 2 parallel inference workers in the embed queue; batch size capped at 16 chunks.
- **Memory overhead:** ~45–55 MB peak RSS when active; exactly 0 MB when the feature is off or provider is `static`/`none`.
- **Model isolation:** Managed with `model_id = 3` (distinct from default hashing `1` and static table `2`), ensuring units are strictly filtered and incompatible vectors never mix. Missing or disabled smart pack falls back to the static table with an explicit fallback reason surfaced in Doctor/stats.

### 5.4 Folded / substring tier

Exact BM25 is the first tier. When it cannot fill the result limit, a folded
tier handles partial words and OCR look-alikes:

- Normalize look-alike characters (`0/o`, `1/l/i/|`, `5/s/$`, `8/b`, `m/rn`,
  `w/vv`, `d/cl`) into `content_folded`, indexed with Tantivy's 3-gram
  tokenizer. `content_raw` is stored, not indexed, for verification.
- Verify each folded candidate against the raw text before returning it: one
  misread letter in a 4–7 letter word, two in 8+, none under 4. This prevents
  `email` matching inside `internally` and rejects broad n-gram false positives.
- Folded hits always rank after every exact hit and carry
  `match_type = ContentFolded`.
- Budget: the tier only runs when the exact/fuzzy/semantic tiers leave a
  shortfall, so a normal query never pays for it. On the synthetic 500-doc
  corpus, `p95 = 262 µs`; index growth was 1.61x with stored raw text.
- **Filter pushdown (schema v3):** the content index stores the
  lowercase `ext` (indexed `STRING`) and `size` (fast `u64`) for every
  document. Positive `ext:`/`size:` filters are added as `Must` clauses before
  top-k collection, so BM25 and folded tiers never score documents the
  metadata filter would reject. Type/lang/name/path and negated filters remain
  post-filters; the metadata check is still authoritative.
- Upgrading an older index preserves it as `index.legacy-*`, creates a fresh
  index, and queues every file for one content re-extraction.

### 5.5 Image metadata tier (thumbnails and hashes)

- Width/height come from the image header; the perceptual hash is deferred out
  of the crawl.
- Generate the 480 px thumbnail once, compute the dHash from it, and reuse the
  thumbnail for the grid and `/dupes`. Never decode the full image just for a
  hash.
- The backfill runs one decode at a time, pauses during crawls/queries, and
  releases memory after each image (`malloc_trim` / `malloc_zone_pressure_relief`
  / `EmptyWorkingSet`).
- Budget: no full-resolution decode for hashing; peak indexer RSS target
  ≤ 100 MB for an image-heavy backfill.
- Screenshot bursts: at write time (watcher) or from an already-fresh thumbnail
  during a crawl, compare an image with assigned images in the same directory
  within ten minutes. The pair must share decoded dimensions and either strong
  line overlap with a close dHash or very close dHash with too little text to
  compare (scrolling is allowed up to a wider hash distance when line overlap is
  strong). Matching bursts merge into the lowest node id, stored as `burst`;
  `look` stores the dHash. A compact bottom-k line signature is stored with the
  image so comparisons never re-run OCR. Old rows are backfilled 50 at a time
  while idle. Search groups image rows by `burst` in one in-memory pass, and
  `/dupes` exposes the same groups.

---

## 6. Query pipeline

```
input ──► Lexer ──► AST ──► Router ──► Planner ──► Executors ──► RRF ──► Rank ──► Snip ──► IPC
         (0.02ms)          (0ms)        (0.05ms)    ┌ fuzzy  2–4ms │  0.1ms   0.2ms   5–20ms
                                                    │ BM25   1–3ms │         (visible rows only,
                                                    └ vector 0.5–2ms          post-paint)
```

### 6.1 AST

```rust
struct Query {
    terms:   Vec<Term>,        // { text, field: Any|Name|Content|Path, phrase: bool }
    filters: Vec<Filter>,      // Ext(Type) | Modified(..) | Size(..) | Kind(..) | Lang(..) | Not(..)
    scopes:  Vec<Scope>,       // dir id + include/exclude
    mode:    Mode,             // Ranked | Exhaustive
    raw:     String,
}
```

### 6.2 Grammar (all optional — plain typing always works)

```
type:pdf ext:rs in:~/Projects/mnemo path:src-tauri
modified:<7d size:>2mb lang:rust is:image -node_modules "exact phrase"
re:"fn\s+\w+"        name:config        content:timeout
```

Rules: quoted strings support spaces; `-` negates; unknown `key:` is treated as literal text; parse errors produce an inline hint, never zero results.

### 6.3 Router (0 ms, no model)

| Signal | Route |
|---|---|
| ≤ 2 tokens, no spaces, has `camelCase`/`_`/`.` | name-heavy + code tokenizer |
| quoted or `re:` | content + phrase/regex |
| `type:`/`in:` present | filtered, scope-first |
| ≥ 4 tokens or contains NL stopwords | semantic weight ↑, BM25 ↓ |
| ends with `/` | pure scope navigation (list the folder) |

### 6.4 Scope resolution

`in:` and leading directory-looking tokens resolve against a **directory name trie** (folded). Exact > prefix > fuzzy. Ambiguous match → keep as a search term *and* show a "did you mean 📁 mnemo?" chip. A resolved scope uses cached DFS ranges, so scoped queries are O(subtree), not O(corpus).

### 6.5 Fusion and ranking

```
score(doc) = Σ_tier  w_tier · 1/(60 + rank_tier)      // RRF
           + 0.20 · exact_substring_in_name
           + 0.15 · prefix_in_name
           + 0.10 · exp(-age_days / 7)                 // recency, smooth
           + 0.08 · log1p(opens) · exp(-last_open_days/14)  // frecency
           - 0.02 · depth
```

Weights live in config; RRF is chosen because BM25 scores are corpus-dependent and unbounded. Ties break on path (deterministic — no `HashMap` iteration order).

### 6.6 Execution rules

- All work happens on a bounded blocking pool; the async layer only routes and serializes.
- One `Arc<Snapshot>` + one Tantivy `Searcher` per query → consistent snapshot, no write locks.
- Per-stage time budget; if the semantic stage blows its budget, return lexical results and mark the response `partial: true`.
- Requests carry a monotonic `seq`. A new request cancels the previous scan (`AtomicBool` checked every 1k iterations) and the client drops stale responses.
- `mode: exhaustive` streams results in pages with a total count — for agents and regex-style enumeration.

---

## 7. Freshness & correctness model

This is the section that makes the speed claims true.

1. **Identity, not paths.** Every node is keyed by `(dev, ino)` / `file_id`. A rename updates one row. Content is never re-extracted on a move.
2. **Three generations.** `meta_gen` (metadata changes), `content_gen` (BM25), `vector_gen`. Every response carries all three plus `as_of` timestamps.
3. **Search first, reconcile after.** Startup loads the durable snapshot and is searchable in < 400 ms. A background **stat-only** reconcile then diffs the tree (`jwalk` without reads: 1M files ≈ 20–40 s warm) and patches gaps. Until it finishes, the UI shows `index Ns old`.
4. **Watcher is a hint, not the truth.** Events are coalesced per path with a 150 ms debounce. Any of `IN_Q_OVERFLOW`, FSEvents `MustScanSubDirs`, or Windows buffer overflow schedules a scoped rescan. A watch failure downgrades to polling with a visible banner.
5. **Reconcile-on-resume.** After sleep, replay from the last event cursor; if the cursor is invalid (USN journal rolled over), full rescan.
6. **Repair pass** every 6 h or on boot: verify a random 1 % sample by `(size, mtime)`; mismatch → re-extract. Full integrity check is a manual command.
7. **Content diff rule:** re-extract only if `(size, mtime_ns, ino)` changed, or `content_hash` mismatches after a settle re-read. `chmod`/`atime`-only events never trigger content work.
8. **Junk filter:** `.swp`, `4913`, `~$*.docx`, `.git/index.lock`, `*.tmp`, `.DS_Store`, `.goutputstream-*` — filtered before any work.
9. **Deletes** are tombstoned, then purged in batches at idle (Tantivy delete-by-term, vector tombstone, SQLite delete), so the interactive path never pays for them.
10. **Crash safety:** SQLite WAL + Tantivy's own crash recovery. A `kill -9` mid-crawl leaves a resumable state; unfinished content is marked `pending`, never presented as indexed.

---

## 8. Extraction

```
magic-byte sniff ─► dispatcher ─► built-in or opt-in external extractor
                                      (isolated worker, timeout) ─► state machine
```

| Format | Path | Notes |
|---|---|---|
| text/code/config | encoding detect → UTF-8 | UTF-16 BOM, Latin-1 fallback; strict-UTF-8 rejection is a bug |
| CSV / TSV | delimiter sniff → RFC 4180 rows | one line per row as `column = "value"` pairs with the header repeated; header-only files index column names; line breaks inside cells become spaces |
| DOCX/PPTX/XLSX/EPUB | ZIP + quick-xml | shared strings, speaker notes, hidden sheets; **DTDs disabled** (billion-laughs) |
| PDF | `pdf-extract` first; `pdfium` feature flag if quality fails | page-level offsets → "jump to page"; encrypted/scanned → explicit state |
| Images | PP-OCRv6 tiny by default; optional Indra pack (higher quality) → text units | OCR before CLIP; screenshots prioritized |
| Archives | metadata only (v2) | no recursive indexing |
| External | config-defined executable, extension/MIME allowlist | no shell, cleared env, 100–7000 ms timeout, bounded stdout, process-group kill; zero plugins by default |

State machine: `pending → ok | skipped(reason) | failed(reason)`, max 3 attempts with backoff. Counts are surfaced ("38 files could not be read"), never silently dropped. Each extractor runs with a timeout and a panic boundary in its own worker — a malformed PDF must never take down the daemon. External extractors are opt-in, never shell-interpreted, and bounded by timeout/output/process-group limits; failures are logged and surfaced in Doctor, and a disabled entry never runs. See [EXTRACTORS.md](EXTRACTORS.md).

---

## 9. The three surfaces

### 9.1 GUI — "super simple" is a spec, not a vibe

- **First run (shipped):** guided flow — welcome, folders, exclusions, preferences, optional packs, first scan, Done. The first scan finishes with counts and a Done summary before the main window opens; setup markers are gated by `first_scan_done`. Settings → About offers **Run setup again**.
- **Default screen:** input + list + right-side preview. No tabs, no sidebar, no modes.
- **Chips** under the input appear only when a filter/scope is active; click to remove.
- **`/`** in the search box: command menu (filters, `/dupes`, `/reindex`, `/doctor`, `/settings`). **`@`** opens a directory scope picker and inserts `in:"<path>"`. No global palette shortcut.
- **Sort** control: Best match / Newest / Oldest / Largest / Name, applied server-side.
- **Plain language** is compiled by deterministic rules (`query/intent.rs`) into the existing grammar + sort; every inference is returned as a removable chip. Explicit grammar bypasses the compiler. Personal aliases expand to a case-insensitive OR.
- **Facets** (`SearchResponse.facets`) are computed server-side over the full matched set before the display limit.
- **`>`** in the input switches to commands: `/reindex`, `/add folder`, `/model`, `/doctor`.
- **Bottom bar:** `12 ms · 1,204,553 files · 2 s old · semantic ✓`.
- **Preview:** monospace text with highlighted matches for code/text; page render for PDF; thumbnail for images. Virtualized list, fixed row height, no layout shift.
- **Semantic badge:** a subtle `≈` on results that only the vector tier found. No mode switch — semantics are part of the one box.
- **Settings:** progressive disclosure. Watched folders, exclusions preset, shortcut, and AI toggle stay visible. Downloads live in setup; Settings shows installed status and **Run setup again** (§18.8). Everything else is a config file, documented, not a UI.

> This is the **shipped `v1.0.0`** surface: plain-language intent, `/`
> commands, `@` scopes, interpretation chips, facet counts, and grouped
> versions. The legacy `>` grammar stays available underneath.

**Perceived-speed rules:** render previous results during computation; fire immediately on Enter/paste; prefetch top-5 snippets; never animate reordering; empty state = recent files, rendered before any query.

### 9.2 CLI

```bash
mnemo search "docker error" --type png --since 7d --json
mnemo find --name '*.rs' --in src-tauri --exhaustive
mnemo state --json
mnemo index add ~/Projects
mnemo doctor
mnemo mcp            # stdio MCP server
```

Same AST and index as the GUI. CLI, MCP, and the GUI prefer the warm daemon
(§18.4), so queries do not re-open SQLite or Tantivy; without a daemon they fall
back to a read-only in-process open. The GUI keeps a read-only manager only for
that fallback.

### 9.3 MCP / agent API

Transport: **stdio** (default, spawned per client) plus **loopback HTTP/SSE**
(`mnemo-cli serve`, bearer token, loopback-only, read-only). The server pings
the daemon and forwards searches/state to it; with no daemon it opens the index
read-only. `/ask` runs in the CLI/GUI, not on the HTTP transport.

| Tool | Contract |
|---|---|
| `health_check` | daemon/db/index/model state, pid, watcher, phase → `ok` or a reason (never a tool error) |
| `search` | `query`, `mode: ranked\|exhaustive`, `path?`, `ext?`, `modified_after?`, `page_size≤50`, `cursor` → path, score, bounded `snippet`, `snippet_truncated`, `next_cursor`, `total`, `as_of` |
| `read_snippet` | `path`, `query?`, `max_tokens≤1024` → window + `truncated`, **never whole files** |
| `index_state` | counts, content docs, semantic units, model ids, phase/progress, roots, queue freshness |
| `regex_search` | bounded `pattern≤512`, filters, `page_size`, `cursor` |
| `find_related` | planned |
| `find_symbol` | tree-sitter definitions (Rust, Python, JavaScript, TypeScript); `query`, `language?`, `path?`, `page_size≤50` |
| `reindex` | opt-in write; only listed when `mcp_allow_reindex=true` and the daemon is running |

Design rules:
- **Ranked vs exhaustive is the key distinction.** Agents need completeness (exhaustive, cursor-paginated); humans need relevance. Conflating them produces bad tools.
- Responses are token-budgeted (search: ~1,500 tokens, 100-char snippets; `read_snippet`: 32–1,024 tokens) and delimited so file content reads as data, never instructions.
- `.gitignore` / `.cursorignore` parity ships by default (`include_ignored` opts out) — agents assume parity and produce garbage context otherwise.
- Read-only by default. No shell and no deletion; `reindex` is the only write tool and requires an explicit config opt-in plus the daemon.
- `agent_scoped_roots` restricts results and single-path reads; comparisons resolve the path (including symlinks and `..`) before accepting it.
- Ship ready-to-paste config for Cursor, Claude Code, and Claude Desktop in the docs and in Settings.

---

## 10. Performance budgets (1M files, warm, p50)

| Stage | Budget |
|---|---|
| parse + route + plan | 0.1 ms |
| scope resolve (cached) | 0.02 ms |
| filename prefilter + match (parallel) | 2–4 ms |
| BM25 | 1–3 ms |
| static-embedding query + exact scan (≤400k units) | 0.5–2 ms |
| fusion + rank | 0.2 ms |
| IPC serialize (50 results) | 0.5 ms |
| React render (virtualized) | 3–8 ms |
| facets (100k / 1M matched, release) | 3.1 / 17.3 ms |
| **first paint** | **≈ 8–18 ms** |
| snippet fetch (post-paint, top 20) | 5–20 ms |

Gates: p50 < 15 ms, p95 < 40 ms, p99 < 60 ms. The 1M facet pass is now
within its measured budget, but the full 1M query path still materializes the
metadata arena per query (~207 ms p50 in `mem_probe`, down from 675 ms); that gap is tracked as
the arena-backed query path remains the next optimization. **Daemon idle RSS at 1M files:** ≤ 150 MB interim (compact arena); ≤ 60 MB target once the daemon + mmap'd index land. **Indexer peak RSS:** ≤ 100 MB for an image-heavy backfill; the current measured peak is ~73 MB for a mixed 700-file crawl. Indexing throughput > 2,000 files/s metadata, > 150 files/s text content (OCR-bound image content is limited by tesseract; see [BENCHMARKS.md](BENCHMARKS.md#indexing-and-extraction-reference)). Semantic embedding > 2,000 docs/s.

---

## 11. Degradation matrix

| Failure | Behavior |
|---|---|
| Daemon unavailable | GUI offers to start it; CLI/MCP fall back to read-only direct access with a banner; no writes from clients |
| Tantivy open error | lexical + semantic only; persistent banner; "Rebuild index" action |
| Watcher unavailable / limit hit | poll mode (30 s), banner, no silent staleness |
| Vector file corrupt | drop semantic tier, re-embed at idle, lexical unaffected |
| Model missing | static tier absent; everything else works |
| Optional OCR pack missing/checksum fail | fall back to Tesseract; setup shows the failed download and retry |
| ONNX runtime library missing/unloadable | accurate OCR reports unavailable and extraction falls back to Tesseract; install the runtime and restart |
| Folded tier absent (old index) | exact + semantic only; rebuild prompt, never an error |
| Disk full | pause writers, banner, keep serving reads |
| Extract worker panic/hang | per-file timeout, mark failed, continue |
| Second instance | focuses the first, exits |
| Sleep/resume | replay cursor; invalid → scoped rescan |

---

## 12. Repository layout

```
crates/
  mnemo-core/      model, snapshot, sqlite, watcher, reconcile, config
  mnemo-index/     metadata arena, fuzzy, tantivy, semantic
  mnemo-query/     lexer, ast, router, planner, fusion, rank, snippet
  mnemo-extract/   text, encoding, docx/pptx/xlsx/epub, pdf, ocr, external
  mnemo-embed/     model2vec, optional ort/fastembed, model registry
  mnemo-proto/     request/response types (shared by CLI, GUI, MCP)
  mnemo-daemon/    server, scheduler, lifecycle, MCP, HTTP
  mnemo-cli/       mnemo binary
  mnemo-bench/     corpus generator, golden queries, criterion benches
src-tauri/ + src/    GUI (client only — no indexing logic)
```

Rule: **`mnemo-core` must not depend on Tauri.** The GUI must not depend on index internals. That boundary is what makes the CLI and MCP possible.

---

## 13. Phase plan (each phase ships something usable)

**Status (v1.0.0):** the foundation, query layer, semantic tier, document
extraction, OCR/thumbnail work, opt-in external extractors, daemon split,
PP-OCRv6 pack, model/download hygiene, HTTP/SSE agent transport, one-click
connect, and the search-quality pass are implemented. The GUI, CLI, and MCP
are daemon clients with a read-only fallback. Signing/notarization and the
auto-updater remain packaging follow-ups.

### Phase 0 — Trustworthy foundation · ~2 weeks
Workspace split · SQLite schema + snapshot warm-load · `(dev, ino)` identity · mtime/size diffing · rename & subtree delete · watcher rewrite (overflow → rescan, single owned watcher, events during crawl) · search-first/reconcile-after startup · `spawn_blocking` + snapshot queries + cancellation · single instance · atomic config · schema versioning · `mnemo doctor`.
**Gate:** all §10 gates; 1,000 random renames/deletes → zero ghosts; `kill -9` × 20 → no corruption.

### Phase 1 — The query layer · ~1.5 weeks
Lexer/AST/parser · filters (`type/ext/in/path/modified/size/is/lang/-`) · directory trie + DFS scope cache · router · RRF fusion · deterministic ranking · frecency · char-bloom + top-k heap · snippets post-rank · chips, preview pane, actions, `>` commands · events instead of polling · request sequence guard.
**Gate:** recall@5 ≥ 0.90 on the golden set; `type:pdf invoice` on 800k files < 5 ms; filters never produce silent zero-results.

### Phase 2 — Semantic, default-on · ~1 week
`model2vec` static embeddings bundled · unit chunking + dedupe by hash · i8 exact scan · query path wiring · `≈` badge · embed queue with battery/idle policy · model registry + per-index `model_id`.
**Gate:** semantic query p50 < 2 ms; NDCG@10 ≥ 0.85 of a MiniLM baseline on the golden set; model swap re-embeds without breaking search.

### Phase 3 — Documents · ~1.5 weeks
Magic sniffing · encoding detection · DOCX/PPTX/XLSX/EPUB · PDF text layer + page offsets · extract state machine + surfacing failures · 1 MB/20 MB content policy.
**Gate:** 50-page PDF < 50 ms; malformed/encrypted/scanned handled without daemon crash; zero panics on a 500-file fuzz corpus.

### Phase 4 — Agent surface · ~1 week
Daemon socket + protocol (shipped) · CLI commands (shipped) · MCP stdio (shipped) + loopback HTTP/SSE with token auth (shipped) · `search` / `read_snippet` / `index_state` / `find_symbol` (shipped) · keyset pagination past the 10k cap (shipped) · `.gitignore` / `.cursorignore` parity (shipped) · `agent_scoped_roots` read-only scoping with resolved-path containment (shipped) · token budgets (shipped) · one-click connect for Claude Desktop/Code and Cursor (shipped) · `find_related` (planned).
**Gate:** `search` p95 < 50 ms; MCP client round-trip works from Cursor and Claude Code; exhaustive mode paginates 100k matches without OOM.

### Phase 5 — Vision & OCR · ~1.5 weeks
Screenshot OCR (PP-OCRv6 default, optional Indra pack) → text units · image embeddings (SigLIP or CLIP ViT-B/32 int8, **not** MobileCLIP on x86) · EXIF orientation · megapixel/size caps · perceptual-hash dedupe · thumbnails.
**Gate:** "screenshot of docker error" returns the right image in top 5; 50k-image scan < 10 ms.

### Phase 6 — Ask & act · ~1 week
Ollama/LM Studio detection → "Ask about this folder" with path+line citations · summarize/explain actions · duplicate detection · near-duplicate file cleanup view.
**Gate:** answers cite real paths; no network when no local model is configured.

### Phase 7 — Launch · ~1 week
GitHub Actions (AppImage/deb/AUR, dmg universal, msi/exe) · benchmark harness + published numbers · 15-second demo · docs · crash reporting (local, opt-in).

**Sequencing rationale:** foundation before features (or every feature multiplies divergence bugs), query layer before semantics (filters + RRF beat embeddings for perceived quality), agent surface before vision (it's the differentiator and cheap once the AST exists), "ask" last (it's a garnish on a working index, and it needs Ollama anyway).

---

## 14. Test & benchmark harness (build this in Phase 0, not at the end)

- **Corpus generator:** seeded, 10k/100k/1M synthetic files; configurable mix of code, prose, CJK, minified, binary, deep nesting, duplicate content.
- **Golden set:** 50 real queries with expected files (human-labelled once, versioned in git). Metrics: recall@5, MRR, NDCG@10.
- **Latency gates in CI** (`criterion` + thresholds): fail the build on > 20 % regression.
- **Chaos suite:** `kill -9` mid-crawl, disk full, permission flips, clock jumps, watcher overflow, index dir deleted at runtime.
- **Platform matrix:** ext4, APFS (case-insensitive), NTFS; non-UTF-8 names; CJK; emoji; cloud placeholders; 2-core no-AVX2 machine.
- **24 h soak:** RSS drift, fd leaks, watcher thread count, segment count.

---

## 15. Risk register

| Risk | Mitigation |
|---|---|
| Daemon lifecycle scares users | GUI auto-spawns; autostart optional but default-on; `mnemo status` explains everything |
| Windows USN journal effort | Ship `ReadDirectoryChangesW` + overflow rescan first; USN as a 2× indexing speedup in Phase 5 |
| Portable model quality below expectations | Ship the static tier as default, the ORT "smart pack" as a one-click upgrade; measure on the golden set |
| Tantivy memory/segment growth | Batched commits, idle merges, weekly force-merge, RSS gate in CI |
| Agent API scope creep | Read-only, five tools, versioned schema; refuse writes by design |
| 1M-file snapshot load too slow | Threshold at 400 ms; fall back to a memory-mapped snapshot format |
| Semantic storage at 1M files | Units capped 20/file, i8, hash dedupe, HNSW above 400k, chunk only mid-size files |

---

## 16. Decision log

1. **Daemon-first, GUI-as-client.** Enables CLI + agents without a rewrite; isolates indexing from paint.
2. **SQLite as truth, arena as hot path.** Durability without per-file `PathBuf` memory.
3. **Identity by inode/file_id, never by path.** Makes renames cheap and ghosts impossible.
4. **Static embeddings by default, ONNX optional.** Semantic search at ~50 µs/query, a ~7.7 MB i8 table (~30 MB in the f32 fallback), zero runtime deps; honest about the quality/size trade.
5. **No ANN until 400k units.** Exact scan is faster, exact, and dependency-free at desktop scale.
6. **RRF over weighted score fusion.** No calibration data, robust to corpus-dependent BM25.
7. **Search-first, reconcile-after startup.** The only way "instant on boot" coexists with correctness.
8. **Exact scan + fast fields before fancy relevance.** Filters are the real speed feature.
9. **Read-only agent surface.** Security, simplicity, and a clean mental model.
10. **No bundled LLM.** Ollama integration gives "ask" without shipping a 4 GB dependency.
11. **Daemon owns the index before more workers land.** The webview must not be in the memory path for OCR, image decoding, or model packs.
12. **Compact arena inside the daemon.** The daemon owns the name/attribute arenas; clients never receive the full metadata payload.
13. **Thumbnail hashing, not full-image hashing.** A 480 px thumbnail is the source for dHash; full decodes never happen just to compute a duplicate key.
14. **PP-OCRv6 by default, Tesseract as fallback** (since 1.0.10). PP-OCRv6 tiny runs in-process on a bundled ONNX Runtime at ~0.15 s/page and reads UI text better; Tesseract is used where it cannot run (Intel Macs). Native platform OCR is not implemented. 1.0.11 adds Indra as an optional higher-quality engine sharing the same bundled ONNX Runtime process.
15. **Exact → folded → semantic.** Never let tolerant matching run when exact results already fill the limit; verify folded candidates against the raw text.

---

## 17. First three PRs

1. **Workspace + durable metadata + snapshot.** Split crates, add SQLite schema, build the arena, warm-load at startup, keep current search working on top of it. *No new features — just a foundation you can measure.*
2. **Identity + incremental reconcile + watcher rewrite.** `(dev, ino)` keys, mtime/size diffing, rename/subtree-delete, single owned watcher, overflow → rescan, search-first startup.
3. **Query layer skeleton.** AST + lexer + `type:`/`in:`/`modified:`/`-` filters + scope trie + RRF fusion + deterministic tiebreak. Ship behind the existing UI; add chips.

After those three, the remaining phases are additive rather than corrective.

---

## 18. Future product layers

This section is the technical shape of future layers.

### 18.1 Intent layer

Today the parser turns grammar into an AST. The next layer turns **language**
into the same AST, then shows the result as chips.

```rust
struct Interpretation {
    terms:    Vec<Term>,        // text, with aliases expanded
    filters:  Vec<Filter>,      // existing Filter enum
    scopes:   Vec<Scope>,       // existing directory-trie resolution
    sort:     Sort,             // new: BestMatch | Newest | Oldest | Largest | Name
    aliases:  Vec<AliasHit>,    // what was expanded, for the chips
    confidence: f32,
    source:   InterpretationSource, // Rules | Llm | Grammar
}
```

Pipeline:

```
raw input
  ├─ grammar parse (existing) ──────────────► AST
  ├─ alias expansion ───────────────────────► terms
  ├─ deterministic intent rules ─────────────► filters + sort + scope
  └─ optional local LLM compiler ───────────► canonical JSON, schema-validated
        └─ on failure/timeout: keep the deterministic result
```

Rules:

- Rules first, LLM never required. The app must work offline and instantly.
- The LLM returns **only** an interpretation (a strict, versioned JSON schema),
  never file results. Retrieval stays with the index.
- Validate every field against the existing AST enums; unknown keys are dropped,
  not executed.
- Show `interpretation` in the response and render it as chips.
- Cache `(raw input, config version) -> Interpretation` with a small LRU.
- If confidence is low, ask one chip-based clarification ("@Work or @Personal?"),
  never a chat paragraph.

Sort is executed after candidate collection, before the display limit. `Newest`
sorts matches by `mtime` desc with the existing deterministic path tiebreak;
`BestMatch` is today's RRF order. Recency stays a ranking bonus only in
`BestMatch`.

### 18.2 Facets and filter pushdown

Request additions:

```json
{ "query": "...", "facets": ["type", "lang", "scope", "modified", "size"] }
```

Response additions:

```json
{
  "facets": {
    "type":  [{ "value": "image", "count": 1204 }, ...],
    "lang":  [{ "value": "rust",  "count": 88 }],
    "modified": [{ "value": "7d", "count": 41 }]
  },
  "total": 1402
}
```

- Counts are for the **whole** result set, computed with the facet's own
  dimension excluded (standard faceted search).
- Push `ext`, `size`, `mtime`, and `lang` into Tantivy as fast fields and a
  `BooleanQuery`; stop scoring documents that cannot match.
- Compute metadata-only facets from the snapshot where possible; content facets
  from fast fields. Cache by `(query, generation)` and invalidate on write.
- Budget: facet computation ≤ 3 ms at 100k, ≤ 10 ms at 1M.
- Multi-select is OR within a dimension, AND across dimensions; negation is a
  `MustNot`. An empty facet selection is never sent as a filter.

### 18.3 Visual tier

Add a fourth retrieval tier alongside fuzzy name, BM25, and static vectors.

- **Model:** SigLIP or CLIP ViT-B/32 int8, `load-dynamic`, optional Vision Pack.
  Normalize embeddings; one vector per image; optional region/caption vectors.
- **Storage:** `visual_units(node_id, model_id, vec_id)`; vectors in the existing
  i8 store with `model_id` so a model swap never corrupts results. Reuse the
  thumbnail and dHash already stored per image.
- **Query:** text → visual embedding, or selected image → embedding. Fuse the
  visual rank with the other tiers through RRF; apply the same semantic floor
  and the same metadata filters first. A visual-only hit is marked as such.
- **Captions:** optional idle-time VLM captions enter the BM25 tier as text
  units, so "system design diagram" works even when the visual model is weak.
- **Face identity:** separate, opt-in table of 512-d face embeddings, clustered
  at idle; the user labels a cluster as "me"; search is cosine-to-centroid
  intersected with visual attributes. Explain the match
  (`face 0.82 · red cap 0.91`). Never upload, never export, delete-all must be
  verifiable.
- **Cost:** ~20 ms/image on 2 low-priority threads; 50k photos ≈ 17 min at idle.
  Honor battery/fullscreen/query-in-flight pauses; resume from a cursor.

### 18.4 Daemon migration

Shipped: one warm process, three clients.

```
GUI ─┐
CLI ─┼─► mnemo-daemon (Unix socket / Windows named pipe, 0600 token)
MCP ─┘        │
              ├─ indexer + watcher + embed queue
              └─ query planner + snippets
```

- **Dual mode remains:** if the daemon is absent, the GUI falls back to a
  read-only `IndexerManager`; CLI/MCP fall back to a read-only direct open.
- IPC uses `interprocess` local sockets: a `0600` Unix socket or a namespaced
  Windows named pipe. Protocol version and token are checked on every request.
- The daemon owns the single-instance lock and the writer; clients never open a
  second writer. `daemon.pid`, socket cleanup, SIGTERM/SIGINT, and graceful
  shutdown are implemented.
- Loopback HTTP/SSE is shipped for web-capable MCP clients (`mnemo-cli
  serve`); it reuses the same token and read-only rules. `/ask` is CLI/GUI
  only and is not exposed on this transport.
- Release packaging of the daemon sidecar (installer/bundle) remains a
  packaging follow-up.

### 18.5 Memory and personalization

All local, all inspectable, all deletable.

- **Aliases:** config-backed `alias -> [terms]`, seeded by onboarding preset
  (developer, designer, student, office) and user-editable.
- **Usage:** extend the existing frecency with query→result opens, refinements,
  and pins. Never upload; expose a "clear history" action.
- **Saved searches:** a named `Interpretation` + chips. User-created first;
  auto-suggestions require explicit acceptance.
- **Version groups:** conservative name + size + time heuristics, user-mergeable.
  A wrong group is worse than no group.
- **Home:** recent files, recent queries, pins, and suggestions derived from the
  local usage table.

### 18.6 Response and schema evolution

Additive fields only; existing clients keep working.

- `interpretation`: terms, filters, scopes, sort, aliases, confidence, source.
- `sort`: the effective sort order.
- `facets`: counts keyed by dimension.
- `partial`: which tier missed its deadline.
- `as_of`: already present; keep per-root freshness.

Bump the JSON schema version for MCP/CLI when a field changes meaning, not when
a field is added. The CLI default output stays `path<TAB>score<TAB>snippet`.

### 18.7 Testing these layers

- **Intent golden set:** 40–50 labelled queries, including `latest resume`,
  `screenshots from last week`, `docker error in Projects`, `my biggest videos`,
  `the invoice from March`. Gate on top-1/top-3 and one-shot success.
- **Facet tests:** count accuracy with multi-select and negation; latency at
  100k and 1M.
- **Visual tests:** a frozen 200-image set with labelled queries (`red cap`,
  `diagram`, `whiteboard`), measured at top-5.
- **Privacy tests:** default configuration makes zero network calls; sensitive
  paths never enter an index; face data delete-all is verified.
- **Migration tests:** index/model version changes never mix vector spaces.

### 18.8 Setup and first-run flow

`SetupModal` now runs the guided flow, `complete_setup` saves config and calls
`start_indexing`, and the modal stays on screen through the first scan until a
Done summary is acknowledged. Downloads (OCR language, semantic models) are
acquired as optional packs in setup; Settings shows status and links to
**Run setup again**. Future Vision Packs will follow the same pattern.

**State machine**

```text
Welcome → Folders → Preferences → Optional packs → Scanning → Done
                                        │             │
                                        └─ skipped ───┘
Scanning ── interrupted → resume on next launch
Scanning ── failed → actionable error + retry, never Done
```

**Rules**

- **Setup offers downloads.** `download_ocr_language` and `download_models` are
  offered as optional packs during setup and remain available in Settings.
  Settings → About offers a "Run setup again" entry point.
- **Setup owns initial configuration.** Watched folders, exclusion preset,
  theme, shortcut, and optional packs are asked in context. `complete_setup`
  persists them atomically; changing them later means re-running setup.
- **Gate on the first scan.** A persisted `first_scan_done` marker is written
  when the crawl completes. `is_setup_complete()` returns true only when both
  setup is configured and the first crawl finished. `complete_setup` starts the
  crawl but does not mark the first scan complete. `run_setup_again` resets the
  markers without deleting the index.
- **Resume, don't lie.** If the app exits mid-scan, the next launch resumes the
  crawl and re-enters the Scanning state. A partial index is never reported as
  Done. Persist the setup config before starting the crawl; on launch, config
  present + `first_scan_done == false` routes straight to Scanning, not Welcome.
- **Show real progress.** The Scanning screen reads `get_indexing_status` and
  the metadata/content counts: files, folders, documents, images, OCR'd images,
  and embedded units, plus a progress bar and ETA. The Done screen shows
  the final summary and "Start searching".
- **Optional means optional.** Declining packs leaves keyword + lexical search
  fully usable. The flow never blocks on a download the user did not choose.
- **Only the first scan waits.** After `first_scan_done`, startup is the
  existing search-first + background reconcile path. Do not regress
  warm-start latency for returning users.
- **Re-running setup** resets the setup markers, not the index, unless the user
  explicitly chooses a rebuild.

**Acceptance**

- A fresh install reaches Done without opening Settings and without any download
  initiated elsewhere.
- Killing the app during Scanning resumes and eventually reaches Done.
- Done is unreachable while the index is unusable.
- Second launch paints results while reconcile runs in the background.
