# Privacy

Mnemo is local-first. Everything below describes the shipped behavior.

## What stays on your machine

- The index (metadata, full-text, embeddings) lives in your OS config directory.
- No account, no sign-in, no cloud search.
- No telemetry, no account, no analytics: search queries, file names, and usage
  data are never sent anywhere.
- Mnemo checks GitHub's public releases API for a newer version about once
  every 12 hours. The request contains no search, file, or usage data; blocking
  `api.github.com` in your firewall disables the check.
- Model downloads fetch files from their configured source (Hugging Face or a
  mirror). The default text model downloads automatically after setup, and OCR
  data for the enabled engine may download automatically while OCR is on. The
  optional Indra, smart-embedding, and visual packs download only when you
  choose them.

## What leaves the machine

- Nothing about your files or searches during normal search.
- An update check to GitHub (see above).
- Model/OCR downloads fetch files from their configured source. Those requests
  go to the model host, not to Mnemo servers (there are none).
- The optional GPU runtime for hybrid OCR (Settings → OCR, only when you press
  Download) is fetched from `github.com/microsoft/onnxruntime` (about 311 MB,
  SHA-256 checked). No file names, text or images are sent.

## MCP server

- The MCP server is read-only by default. It can read the local index and file
  metadata/contents that Mnemo has already indexed.
- It cannot delete or modify files. The only write-capable tool (`reindex`) is
  disabled unless `mcp_allow_reindex = true` in config.
- `agent_scoped_roots` can restrict MCP/CLI results to specific roots.
- The MCP server does not expose arbitrary filesystem access beyond the index.
