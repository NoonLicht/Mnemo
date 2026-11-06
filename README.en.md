<p align="center">
  <img src="assets/icon.png" width="112" height="112" alt="Mnemo" />
</p>

<h1 align="center">Mnemo</h1>

<p align="center">
  <b>Local-first search for your whole PC: file names, file contents, images (OCR) and local AI answers.</b><br/>
  Nothing leaves your machine.<br/>
  English · <a href="README.md">Русский</a>
</p>

---

## What it is

Mnemo is a Windows desktop app (Tauri 2 + React + Rust) that builds one local index of your drives and lets you search it instantly.

- **File names**: a fast in-memory name index with a small query language.
- **File contents**: text, PDF, Office documents and code are extracted and indexed with full-text search (tantivy).
- **Images**: embedded OCR (PP-OCR and Indra packs) makes text inside pictures searchable.
- **Hybrid search**: full-text plus vector search, merged with reciprocal rank fusion.
- **Ask**: questions answered by a local LLM that you run yourself (any OpenAI-compatible endpoint on `127.0.0.1`), with citations checked against the indexed sources.
- **Activity**: optional window-activity tracking and browser-history import, with privacy filters.
- **Encryption at rest**: the index is encrypted with a key protected by Windows DPAPI.
- **Agents**: a read-only MCP server and CLI let local AI agents search the same index.

## Install

Download from the [Releases](../../releases) page:

| File | What it is |
|---|---|
| `Mnemo_1.0.0_x64-setup.exe` | Installer (NSIS). Uninstalling also removes the index and settings. |
| `Mnemo-portable.exe` | Portable build, no installation. |
| `Mnemo_1.0.0_amd64.AppImage`, `.deb`, `Mnemo-1.0.0-1.x86_64.rpm` | Linux x64. |
| `Mnemo_1.0.0_universal.dmg` | macOS (universal build, not signed or notarized). |

The builds are **not code-signed**, so Windows SmartScreen will warn you on first start ("More info" → "Run anyway").

On first start choose the folders to index, then press **Index**. Changing the folder list later in **Settings → Save → Reindex** reopens the progress screen.

## Privacy

- No telemetry, no analytics, no accounts, no external fonts or CDNs.
- The index lives in `%APPDATA%\mnemo`; deleting the data from the app removes it end to end.
- The network is used only when you start it yourself (downloading OCR or model packs, or pointing Ask at a model server). Details: [docs/PRIVACY.md](docs/PRIVACY.md).
- The in-app auto-update check is disabled in this build.

## Build from source

Requirements: Rust (MSVC toolchain), Node.js 20+.

```bash
npm install
npx vite build
npx tauri build --bundles nsis --config '{"build":{"beforeBuildCommand":""}}'
```

Quality gates used for this release: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo deny check`.

## Origin and licence

Mnemo is built on the MIT-licensed Kensaku desktop-search project (© 2026 Yash Jangid) with substantial changes. MIT, see [LICENSE](LICENSE) and [docs/LICENCES.md](docs/LICENCES.md).
