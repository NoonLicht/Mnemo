# Mnemo — remaining work plan

Status: master at `v1.0.5` (2026-10-28). ui-refresh merged.
(`pr3-destroy-on-hide`, `pr5-onboarding`). Everything below is not yet merged.

Legend: **[code]** = I can implement directly. **[you]** = needs your action
(OS test, workflow edit, decision). **[both]** = code + your verification.

---

## 0. Before anything else

### 0.1 Commit the onboarding OCR gate (PR #30) — [both]
The OCR gate change is currently local-only and untested on a clean profile.

- Branch: `pr5-onboarding` (2 screens, defaults on).
- Add back an OCR gate: **Start indexing stays disabled** until Tesseract *or*
  the PP-OCRv6 pack is ready, with a **Download** button per engine.
- Files: `src/components/SetupModal.tsx`, plus `get_ocr_status` /
  `download_ocr_language` / `get_ocr_pack_status` / `download_ocr_pack` commands.
- Acceptance: a new user with neither engine ready cannot start; one click
  downloads and enables Start. Tested on a clean profile per OS.

### 0.2 Merge PR #29 (hide/reopen) to master — [both]
- Branch: `pr3-destroy-on-hide`.
- Already destroys the webview on hide; app stays alive via `ExitRequested`
  prevention in `src-tauri/src/lib.rs`.
- **Do not merge yet** until memory target is met (task 1.1).

---

## Phase 1 — Finish what is open

### 1.1 Memory while hidden — target single to low double-digit MB — [both]
Current PR #29 hides by destroying the window, but WebKit helper processes survive.

- Investigate in `src-tauri/src/main_window.rs` (`hide()` calls `window.destroy()`)
  and in the WebKit process tree on each OS:
  - Linux: `WebKitWebProcess` / `WebKitNetworkProcess` (WebKitGTK).
  - macOS: `WebKit.WebContent` / `com.apple.WebKit.Networking`.
  - Windows: `msedgewebview2.exe` (child processes).
- Candidate fixes:
  1. Ensure the webview is actually dropped: confirm `destroy()` frees the
     `WebviewWindow`; check for leaked `Arc`/`Manager` handles in
     `src-tauri/src/lib.rs` (tray, global shortcut closure capture `app`).
  2. Call `release_memory()` (already in `src-tauri/src/util/memory.rs`) plus
     platform trim after destroy; on Linux also `malloc_trim` is already done.
  3. WebKitGTK: set `WEBKIT_DISABLE_COMPOSITING_MODE`? or use
     `webkit_web_view_terminate_web_process` via a small FFI if needed.
  4. If WebKit helpers cannot be killed without killing the app, fall back to
     keeping one warm process and document the real number.
- Measure: full process tree RSS while hidden (not just main process).
  Add a `mem_probe --hidden` or a documented `ps` recipe in `docs/BENCHMARKS.md`.
- Acceptance: hidden RSS in target on all three OSes, reopen instant, last
  query restored (already wired via `remember_query`/`last_query` in
  `src/App.tsx`).
- **Risk:** target may be unreachable with a WebView app. If so, report the
  real number and decide whether to keep the change.

### 1.2 Onboarding finalization — [both]
- Merge 0.1 + clean-profile test on Mac/Windows/Linux.
- Screens: pick folders → how to use. Defaults: start-at-sign-in on,
  Tesseract engine, default exclusions.
- Acceptance: install → first search with no confusing choices.

---

## Phase 2 — Core usability

### 2.1 Quick access (PR 4) — [code]
- Show the global shortcut in the search hint line (`src/components/SearchBar.tsx`).
- Add a bottom key-hint strip: Enter open, Space preview,
  Cmd/Ctrl+C copy path. New component `src/components/KeyHintStrip.tsx`,
  mounted in `src/App.tsx`.
- Empty-result help: when `results.length === 0 && query`, show suggestions
  (`type:pdf`, wider folder) via `src/components/EmptyState.tsx`.
- Files: `SearchBar.tsx`, `EmptyState.tsx`, `App.tsx`.

### 2.2 Help page (`?`) — [code]
- New `?` key opens a modal listing every filter and key (reuse
  `src/components/SettingsModal.tsx` pattern or new `HelpDialog.tsx`).
- Doubles as the "How to search" guide (covers PR 8).
- Content source: `docs/USAGE.md` filter table; render from a shared
  `src/lib/help.ts` so docs and UI stay in sync.
- Files: new `src/components/HelpDialog.tsx`, `src/lib/help.ts`,
  `src/App.tsx` key handler.

### 2.3 Settings repair (PR 7) — [code]
- Add a "Rebuild index" button in Settings (stats tab). "Reset everything"
  already exists in About — do not duplicate.
- Command: reuse `reset-index` logic or add `rebuild_index` Tauri command
  in `src-tauri/src/commands.rs` that wipes index + re-crawls.
- Files: `src/components/settings/stats.tsx`, `commands.rs`, `lib.rs`.

### 2.4 Drag results into other apps (Heimdall) — [code + you test per OS]
- Make `ResultItem`/`FileRow` draggable; on drop into another app, copy the
  file (not just the path). Use Tauri drag or HTML5 `draggable` + native
  drop. Investigate `tauri-plugin-drag` or `startDrag` API.
- Files: `src/components/ResultItem.tsx`, `FileRow.tsx`, `src/App.tsx`.
- Test on each OS: drag onto Finder/Explorer/Files.

### 2.5 Folder exclusions on first run (PR 6) — [both, decision needed]
- Decide with you how much of the skip list returns on the first screen.
  Options: (a) none (current PR5), (b) read-only summary, (c) editable chips.
- If (c), restore preset chips from the old `SetupModal` into the new flow.

---

## Phase 3 — Trust and proof

### 3.1 Public benchmark page (Heimdall) — [code]
- New `docs/BENCHMARKS.md` section or `site/benchmarks.md` with method,
  corpus, hardware, raw numbers, caveats.
- Compare with Everything, fd, plocate, Spotlight on the same folder.
- Gate: no "fastest" claim until Mac/Windows numbers exist (task 4.x).

### 3.2 Exact claims in README (Heimdall) — [code]
- Replace vague claims with measured: install size, index bytes per file,
  hidden RAM, idle CPU.
- Each number must cite the test that produced it (harness + command).
- Files: `README.md`, `docs/BENCHMARKS.md`.

### 3.3 Privacy page (Heimdall) — [code]
- New `docs/PRIVACY.md` (and link from README): exactly what stays on the
  machine, what the MCP server can read, no update checks, no telemetry.
- Files: `docs/PRIVACY.md`, `README.md`.

### 3.4 Idle CPU measurement (Heimdall) — [code]
- Extend `crawl_bench`/`mem_probe` to report CPU% while hidden and while
  indexing. Add to benchmark page.

---

## Phase 4 — Power and polish

### 4.1 Power saving (PR 9) — [code]
- Pause indexing on battery or under heavy load. `src-tauri/src/util/power.rs`
  already has `on_battery()`; `src-tauri/src/util/priority.rs` has background
  priority. Wire `should_pace()` into the crawler loop in
  `src-tauri/src/indexer/mod.rs` / `walker.rs`.
- Add heavy-load detection (CPU > threshold) and a Settings toggle.
- Files: `indexer/mod.rs`, `config.rs`, `settings/kit.tsx`.

### 4.2 Config docs (Heimdall) — [code]
- Document every `config.toml` key with comments. New `docs/CONFIG.md`,
  generated or hand-written from `src-tauri/src/config.rs`.
- Files: `docs/CONFIG.md`, `config.rs` doc comments.

### 4.3 Bundle Tesseract (PR 2) — [you workflow edit]
- My token cannot change workflow files. You edit `.github/workflows/release.yml`
  to ship a Tesseract binary per OS; I'll send the exact diff when we reach
  this step.
- Code side: prefer bundled binary in `src-tauri/src/ocr.rs` (already has
  "prefer a Tesseract shipped with the app" commit).

---

## Phase 5 — Testing and verification

### 5.1 Mac and Windows speed checks — [you]
- Run `bench`/`crawl_bench` on Mac and Windows; add numbers to benchmark page.

### 5.2 Spotlight / Everything / Dowse comparison — [you]
- Same corpus, same machine, document methodology.

### 5.3 Real PDF search — [you]
- Previous PDF test used fake files. Re-run with real PDFs; verify content
  extraction and search.

### 5.4 Wayland + tiling WMs — [you]
- Test global shortcut, window show/hide, drag-and-drop on sway/i3/Hyprland
  (Linux setup docs already in `docs/INSTALL.md`).

---

## Later (not in this plan)

- Game mode and per-app hotkey passthrough.
- Calculator and system actions in search.
- Optional Windows indexer for faster first scans.
- Growth (after 1.1): "Reveal in Mnemo" from Finder/Explorer, shareable
  benchmark card, public sample-folder challenge.

---

## What I need from you

1. Clean-install tests on each OS (0.1, 1.2, 5.x).
2. The workflow edit for Tesseract bundling (4.3) — I'll send the exact diff.
3. A decision on the first-run exclusion list (2.5): none / summary / editable.
4. Confirm memory target feasibility decision (1.1) if unreachable.

## Suggested order

0.1 → 1.1 → 0.2 (merge PR29) → 1.2 → 2.1 → 2.2 → 2.3 → 2.4 → 2.5 →
3.x → 4.1 → 4.2 → 4.3 → 5.x.

Want me to start with 0.1 (OCR gate) or 1.1 (memory investigation)?
