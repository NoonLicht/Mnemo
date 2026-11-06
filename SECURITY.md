# Security

## Reporting

Please **do not** open a public issue for a vulnerability. Use GitHub's private
vulnerability reporting (Security → Advisories → Report a vulnerability) or email
the maintainer listed on the repository profile. You will get an acknowledgement
within a few days.

## Threat model

Mnemo is a local desktop app and a read-only search backend. It is designed to
be safe when pointed at untrusted files.

**In scope**

- Parsing untrusted documents and images (PDF, DOCX/PPTX/XLSX, EPUB, RTF, images)
  without code execution, unbounded memory, or hangs.
- Path traversal or symlink tricks escaping watched roots.
- Command injection through file paths or file contents.
- XSS in the UI from indexed text, filenames, or metadata.
- The agent surface leaking data or permitting writes.
- Secrets ending up in the index or in logs.

**Out of scope**

- An attacker who can already read/write your user account or the index directory.
- Malicious OS-level components (kernel, filesystem, WebView).
- Denial of service by indexing an enormous corpus on purpose.

## Controls

- **Extraction isolation.** Parsers run on a bounded worker pool with an 8-second
  timeout and panic isolation. XML external entities/DTDs are not resolved.
- **Decode caps.** Images are limited to 50 MP; content is capped at 1 MB indexed
  and 20 MB extraction ceiling.
- **No shell.** Files are opened through OS APIs; the agent surface has no write,
  shell, or network tools.
- **Read-only agent access.** `mnemo-cli` and the MCP server open the index
  without the Tantivy writer lock and cannot mutate it.
- **UI escaping.** Indexed content (filenames, snippets, previews) is rendered as
  text, never as HTML or `dangerouslySetInnerHTML`. The webview currently ships
  without a Content-Security-Policy; tightening it is tracked as hardening work,
  and the app loads no remote content.
- **Default exclusions** skip hidden files and common junk directories.
- **Local only.** No telemetry and no data about your files or searches leaves
  the machine. Outbound requests are limited to a periodic GitHub release check
  (no payload beyond the request itself) and model/OCR pack downloads. The
  default text model downloads after setup; OCR data for the enabled engine may
  download automatically while OCR is on; optional Indra, smart-embedding, and
  visual packs are user-initiated.

## Sensitive data

The index contains filenames and extracted text from your watched folders. Treat
`~/.config/mnemo/` as sensitive: it is created with user-only permissions where
the platform supports it, and should not be synced to a shared location.

Default-deny covers hidden entries, `.ssh`, `.gnupg`, `.aws`, `.kube`, `.docker`,
`.password-store`, common password-manager folders, `.env*`, and credential
extensions (`.pem`, `.key`, `.pfx`, `.p12`, `.crt`, `.cer`). Additional paths
(browser profiles outside hidden app-data directories, custom secret stores)
should still be added to exclusions.

## Agent surface

- Bind to stdio only; no TCP listener.
- Logs go to stderr, never stdout, so protocol streams stay clean.
- Tool responses are token-budgeted; file content is data, never instructions.
- No `.gitignore` bypass by default — agents see what the index contains.

## Release integrity

Current release builds are **unsigned** (see `docs/INSTALL.md`). Verify downloads
against the checksums attached to the GitHub release. Signing and notarization are
tracked in the project issue tracker.
