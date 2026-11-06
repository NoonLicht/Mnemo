# Mnemo for AI agents

Mnemo exposes its index to external agents. It is **read-only by default,
local-only, and shares the same index as the desktop app** — no API keys, no
network. An opt-in `reindex` tool is the only write action.

Two surfaces, one binary:

- **MCP** over stdio — for Claude Code, Cursor, and any MCP client.
- **CLI** — for shell-capable agents and one-off queries.

Both use the existing `~/.config/mnemo` index. The MCP server pings the
daemon and forwards searches/state to it when available; with no daemon it
opens the index read-only and loads metadata/vectors once per session.

---

## MCP setup

Use the absolute path if `mnemo-cli` is not on your `PATH`.

```json
{
  "mcpServers": {
    "mnemo": { "command": "mnemo-cli", "args": ["mcp"] }
  }
}
```

Protocol: JSON-RPC 2.0, newline-delimited, over stdin/stdout. Only protocol
messages are written to stdout; logs go to stderr.

### Tools

#### `search`

```json
{
  "query": "docker compose error",
  "limit": 10,
  "mode": "ranked",
  "path": "~/Projects",
  "ext": "rs",
  "modified_after": "7d"
}
```

| field | type | notes |
|---|---|---|
| `query` | string, required | full query language: `type:`, `ext:`, `in:`, `modified:`, `size:`, `re:"..."`, `-negation` |
| `page_size` | 1–50, default 10 | results per page (`limit` remains a deprecated alias) |
| `cursor` | string | opaque `next_cursor` from the previous page |
| `mode` | `ranked` \| `exhaustive` | ranked for relevance; exhaustive for completeness (path-sorted, cap 10k) |
| `path` | string | becomes `in:"<path>"` |
| `ext` | string | becomes `ext:<ext>` |
| `modified_after` | string | lookback window, becomes `modified:<window>` (e.g. `7d`, `30d`, `1h`) |

Returns JSON in `content[0].text`:

```json
{
  "results": [
    { "path": "...", "score": 0.91, "snippet": "...", "match_type": "Both",
      "size": 1024, "modified": 1761400000 }
  ],
  "total": 12,
  "mode": "ranked",
  "returned": 10,
  "page_size": 10,
  "next_cursor": "k1-a",
  "has_more": true,
  "token_budget": 5000,
  "as_of": 1761400000
}
```

Snippets are capped at 100 characters and flagged with `snippet_truncated`.
`match_type` is `FileName`, `Content`, `ContentFolded`, `Semantic`, or `Both`.
Use `next_cursor` until `has_more` is false. Offsets are capped at 10,000; a page
at the cap reports `cursor_limit_reached: true` instead of materialising more.

#### `read_snippet`

```json
{ "path": "/abs/file.rs", "query": "timeout", "max_tokens": 256 }
```

Returns `{ path, line_start, line_end, text, truncated }`. Text is capped at
`max_tokens × 4` characters and centered on the first match. Never returns a
whole large file.

#### `health_check`

No arguments. Returns `{ ok, daemon, db, index, model, model_real,
protocol_version, watcher_ok, files, content_docs, semantic_units, phase,
is_indexing, paused, error, as_of }`. An unhealthy index returns `ok: false`
with a reason, not a tool error.

#### `index_state`

No arguments. Returns:

```json
{ "files": 1204553, "dirs": 84210, "content_docs": 186420, "semantic_units": 402118,
  "index_bytes": 759000000, "model_id": 2, "active_model_id": 2,
  "model_name": "potion-base-8M", "roots": ["/home/you"], "as_of": 1761400000 }
```

`model_id` is the model that produced the stored vectors (`null` if none);
`active_model_id` is what new queries use. If they differ, a re-embed is pending.
The payload also carries `phase`, `is_indexing`, `paused`, extraction progress,
`model_real` and `last_reconcile`.

#### `regex_search`

`{ "pattern": "fn \\w+", "page_size": 20 }`. Patterns are capped at 512 bytes,
compiled before execution, and return the same pagination/snippet shape as
`search`.

#### `reindex` (opt-in write)

No arguments. Hidden from `tools/list` and disabled by default. To enable it,
set `"mcp_allow_reindex": true` in config (Settings → Agents) and restart the MCP
client. The daemon re-checks the setting on every call; without a daemon the
tool is unavailable.

---

## CLI

```bash
mnemo-cli search "docker error" --json --limit 10
mnemo-cli search "type:pdf invoice" --path ~/Documents
mnemo-cli search 're:"fn \w+"' --ext rs --since 30d
mnemo-cli search "config" --exhaustive --json
mnemo-cli state --json
mnemo-cli mcp
```

Default output is `path<TAB>score<TAB>snippet`; `--json` prints the full
response. Exit codes: `0` ok, `2` usage, `3` no index (launch Mnemo once).

---

## Behavior and limits

- **Read-only by default.** No shell and no network. `reindex` is the only
  write tool and requires the explicit config opt-in plus the daemon.
- Honors the watched folders and exclusions configured in the app.
- Ranked mode fuses fuzzy filename, BM25 content, and static-vector tiers with
  RRF; semantic-only hits below a model-dependent cosine floor are dropped.
- `exhaustive` skips the ranked tiers and enumerates by path, capped at 10,000.
- No index → tools return a clear error; they never create one.
- Daemon-first sessions avoid opening SQLite, Tantivy or the embedding table in
  the MCP process. Without a daemon, startup cost is once per session; a
  one-shot CLI call pays metadata load, so prefer the MCP session.

---

## Also shipped (agent surface v2)

- **`find_symbol`** — tree-sitter definitions (Rust, Python, JavaScript,
  TypeScript): `{ query, language?, path?, page_size≤50 }` → path, kind, line
  range, one-line signature. Candidates come from the content index; up to
  200 files are parsed per call.
- **Keyset pagination past the 10k cap** — exhaustive cursors `k2-<hex path>`
  return matches strictly after the last path; `total` is then a windowed
  lower bound (`total_is_lower_bound: true`). Offset cursors (`k1-`) still
  work for ranked pages.
- **`.gitignore` / `.cursorignore` parity** — common pattern syntax, honored
  by default so agents see the same tree developers do; `include_ignored`
  opts out. An excluded parent directory cannot be re-included (git
  semantics).
- **Read-only scoping per root** — `agent_scoped_roots` in config restricts
  search results and `read_snippet`/`find_symbol` paths to those roots;
  out-of-scope requests are rejected, not silently trimmed. Paths are resolved
  before comparison, so `..` and symlink escapes do not pass the check.
- **One-click connect** — Settings → Agents detects Claude Desktop/Code and
  Cursor, merges one config entry atomically (with a `.bak` backup); manual
  JSON stays. Claude Desktop paths follow each platform (`~/Library/
  Application Support` on macOS, `~/.config` on Linux, `%APPDATA%` on
  Windows). CLI: `mnemo-cli connect <client> [--write]`.
- **Loopback HTTP/SSE** — `mnemo-cli serve [--port 8765]`: MCP Streamable
  HTTP on `POST /mcp` (JSON or SSE), read-only `GET /health`, `GET /search?q=`,
  and an SSE keepalive at `GET /events`. Bearer token in
  `<config>/http.token` (0600); loopback bind + per-connection peer check;
  `reindex` is never enabled on this transport. `/ask` is a CLI/GUI feature,
  not an HTTP route.
- **Optional local LLM** — query compiler (validated chips) and `/ask` with
  path+line citations. Off by default; the model never retrieves on its own
  and is never required. CLI: `mnemo-cli ask "<question>"`. The compiler
  validates both filter keys and values against the query grammar, so chips
  that would do nothing are rejected rather than shown.

## Planned

- **`find_related`** — related-file lookups (same stem, imports, neighbors).
- **Visual agent tools** — text-to-image and similar-image lookups once the
  vision tier is wired into search; `similar_images` exists and self-hides
  until then. Read-only and local like everything else.

Technical shape: [ARCHITECTURE.md](ARCHITECTURE.md) §18.
