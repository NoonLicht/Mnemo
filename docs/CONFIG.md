# Configuration reference (`config.toml` / `config.json`)

Mnemo stores its settings in the OS config directory:
- Linux: `~/.config/mnemo/config.json`
- macOS: `~/Library/Application Support/mnemo/config.json`
- Windows: `%APPDATA%\\mnemo\\config.json`

Two overrides apply to this location (config file, metadata database, content
index, OCR state, everything else under the data directory):

1. **`MNEMO_DATA_DIR`** environment variable. Highest precedence; useful for
   tests and for Nix/Flatpak or other sandboxed packaging that must pin the
   data directory.
2. **Portable marker file.** If a file named `portable` (or `mnemo.portable`)
   sits next to the executable, Mnemo keeps everything in `<exe_dir>/data`.
   This makes the Windows portable build self-contained: copy the folder, keep
   your index.

The file is JSON (despite the `config.toml` shorthand used in docs). Keys are
optional where noted; missing keys fall back to the listed default.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `watched_dirs` | string[] | `[]` | Folders to index. |
| `excluded_dirs` | string[] | common build/cache names | Directory names to skip. |
| `excluded_extensions` | string[] | media/archive/binary exts | File extensions to skip. |
| `shortcut` | string | `Alt+Space` | Global toggle shortcut. |
| `max_results` | number | `200` | Maximum results per query. |
| `is_setup_complete` | bool | `false` | First-run setup finished. |
| `first_scan_done` | bool | `false` | First full crawl finished. |
| `skip_hidden` | bool | `true` | Skip dotfiles/dotdirs. |
| `semantic_enabled` | bool | `true` | Enable semantic (embedding) tier. |
| `semantic_model` | string | `static` | `static`, `smart`, `none`. |
| `semantic_f32_table` | bool | `false` | Keep static table in f32. |
| `crawl_mode` | string | `full` | `full`, `balanced`, `eco`. |
| `pause_indexing` | bool | `false` | Pause background crawling. |
| `mcp_allow_reindex` | bool | `false` | Allow MCP `reindex` write tool. |
| `ocr_enabled` | bool | `true` | Enable OCR for images. |
| `ocr_engine` | string | `accurate` | `accurate` (PP-OCRv6, default), `indra` (Indra OCR; ~27 MB model download, weights under a modified OpenRAIL-M license) or `tesseract`. |
| `extractors` | object[] | `[]` | Opt-in external extractors. |
| `visual_search_beta_enabled` | bool | `false` | Beta visual search tab. |
| `agent_scoped_roots` | string[] | `[]` | MCP/CLI read-only scope roots. |
| `include_ignored` | bool | `false` | Honor `.gitignore` (false = ignore parity). |
| `results_view` | string | `grid` | `grid` or `list`. |
| `show_thumbnails` | bool | `true` | Show thumbnails. |
| `start_at_login` | bool | `false` | Autostart on sign-in. |
| `auto_update` | bool | `true` | Automatically check for updates in background. |
