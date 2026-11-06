# External extractors

Mnemo ships **zero plugins** by default. Every external extractor is opt-in,
lives in `config.json`, and only runs when a path matches its extension or MIME
selector. A new format does not require recompiling Mnemo.

> External extractors run with your user permissions. Mnemo never uses a
> shell, clears the child environment, bounds stdout/time, and kills the whole
> process group on timeout, but this is an allowlist and resource boundary —
> **not an OS-level sandbox**. Only configure scripts you trust.

## Configuration

Add an `extractors` array to:

- Linux: `~/.config/mnemo/config.json`
- macOS: `~/Library/Application Support/mnemo/config.json`
- Windows: `%APPDATA%\mnemo\config.json`

```json
{
  "extractors": [
    {
      "name": "STL (ASCII)",
      "extensions": ["stl"],
      "mime": ["model/stl"],
      "command": "/home/me/.local/bin/mnemo-stl-text",
      "args": ["--max-bytes", "1000000", "{path}"],
      "timeout_ms": 3000,
      "max_output_bytes": 1000000,
      "enabled": true
    }
  ]
}
```

| Field | Required | Meaning |
|---|---|---|
| `name` | yes | Label stored as the document format and shown in failures. |
| `extensions` | one selector | Lowercase extensions, with or without a dot. `*` matches every file. |
| `mime` | one selector | Exact MIME (`text/plain`), type wildcard (`model/*`), or `*/*`. |
| `command` | yes | **Absolute** path to an executable file. `PATH` lookup is not used. |
| `args` | no | Individual arguments, never split or shell-expanded. `{path}` becomes the file path; if no argument contains it, the path is appended as the final argument. |
| `timeout_ms` | no | 100–7000 ms, default 5000. The extraction worker's outer watchdog is 8 s. |
| `max_output_bytes` | no | 1 KiB–4 MiB, default 1 MB. Also capped by the content policy. |
| `enabled` | no | Defaults to `false`. No plugin runs without an explicit `true`. |

Extensions and MIME selectors are ORed. The first enabled matching entry in
array order wins. MIME selectors use the built-in extension map (`pdf`,
`text/*`, Office/OpenDocument, common images, `model/stl`, `model/obj`,
`model/gltf+json`, …); unknown extensions only match `extensions` or `*/*`.
Excluded extensions and directories still win: remove a format from
`excluded_extensions` if you want a plugin to see it.

## Plugin contract

The child process receives:

- the configured arguments, with `{path}` substituted; otherwise the file path
  is appended as the last argument;
- an empty environment except a minimal `PATH` (and platform basics such as
  `SYSTEMROOT` on Windows);
- the file's parent directory as its working directory;
- no stdin, and a closed/ignored stderr.

The plugin must:

1. exit `0`;
2. write UTF-8 text to stdout;
3. stay under `max_output_bytes` and `timeout_ms`.

Anything else marks the file failed for this pass; Mnemo logs the reason and
surfaces it in **Settings → Doctor** (and the daemon stderr), and the file keeps
its last good indexed text. Output is truncated to the content cap before
indexing. Fix the plugin and save the extractor config (or run a full reindex)
to retry.

## Doctor and failures

Doctor lists every configured extractor with its absolute command and whether it
is enabled. A broken path, missing selector or non-executable command is a
failure; configured-but-disabled entries are a warning so the command is
reviewable before enabling. Recent runtime failures include the path and reason
(e.g. timeout, non-zero exit, output cap, non-UTF-8 output).

## Applying changes

Saving config through the app applies the registry immediately, clears cached
extraction results, marks files with the affected extensions for a content
re-read, and starts a crawl. If you edit `config.json` directly while Mnemo is
running, restart the app. If a daemon is running, restart it (or the MCP client)
so its in-memory registry picks up the change.

## Example: ASCII STL

Save this as an executable (`chmod +x`) and point `command` at it. It is an
example, not a shipped pack.

```sh
#!/bin/sh
# Print printable ASCII strings from an STL file, capped by the caller.
tr -c '[:print:]\n' '\n' < "$1" | sed '/^.\{0,3\}$/d'
```

Use `"args": []` so Mnemo appends the path. The result is indexed as ordinary
text and searchable alongside filenames and other documents.

## Safety boundaries

- Only commands present in `config.json` are ever executed. Files cannot
  declare or trigger plugins. The config entry is the consent surface: the
  absolute command is visible there, and `enabled` must be explicitly `true`.
- The command must be an absolute executable file; relative names and
  directories are rejected before spawn. On Windows, `.bat`/`.cmd` wrappers are
  rejected so Rust cannot implicitly route them through `cmd.exe`.
- Arguments are passed as OS strings. Shell metacharacters in a filename are
  data, not syntax.
- On Unix the child runs in its own process group. A timeout sends `SIGKILL`
  to the group, so helper processes are killed too; Windows kills the direct
  child.
- Environment secrets (API keys, daemon tokens) are not inherited.
- Resource use is bounded by the slot pool, timeout, and output cap.

OS-level sandboxing (filesystem/network namespaces, seccomp, App Sandbox) is
future work; use trusted executables and files.
