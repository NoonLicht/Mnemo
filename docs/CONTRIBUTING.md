# Contributing

Thanks for helping. Bug reports, docs fixes, and focused PRs are all welcome.

## Before you start

- Open an issue for anything larger than a small fix, so we can agree on scope.
- Read [`ARCHITECTURE.md`](ARCHITECTURE.md) for the index/search shape and
  [`AGENTS.md`](AGENTS.md) for the read-only agent contract.
- Keep the local-first, read-only-by-default thesis intact.
- Security issues: do **not** open a public issue. See [`../SECURITY.md`](../SECURITY.md).

## Setup

```bash
git clone <repository-url>
cd mnemo
bun install
bun run tauri dev
```

System prerequisites: [`INSTALL.md`](INSTALL.md#build-prerequisites).

## The checks CI runs

```bash
# Rust
cargo test --all-targets --manifest-path src-tauri/Cargo.toml
cargo clippy --all-targets --manifest-path src-tauri/Cargo.toml -- -D warnings

# Web
npx tsc --noEmit
bun run build
```

All four must pass. Do not add `#[allow]` to silence a lint without a comment
explaining why.

## Tests

The suite is hermetic: temp directories, no network, no user data. When you touch:

| Area | Add |
|---|---|
| query parser / AST | a test asserting on the AST, not strings |
| config | a migration/save test using the disk-free helpers |
| extraction | a malformed-input case that must not panic or hang |
| ranking | a fixture where the expected file must land in the top 3 |
| MCP / CLI | a request-handler test with a temp index |
| SQLite | a prefix/duplicate/grouping test with `open_at(temp)` |

## Style

- Rust 2024, `rustfmt` for new files. The repo is not globally reformatted; do not
  reformat unrelated files in a PR.
- TypeScript is strict; no `any` without a comment. Components use the design
  tokens in `src/App.css` — no raw colors.
- Commits: imperative subject, a body that explains *why*. Keep each commit
  focused; a PR may contain several.
- Do not commit build output (`target/`, `dist/`, `.playwright-mcp/`).

## Design changes

The UI is intentionally quiet: hairlines, no shadows, two themes, no emoji icons.
If you change layout or interaction, include a screenshot in both themes and check
760px and 1200px widths. Respect `prefers-reduced-motion`.

## Performance changes

If you touch the query path or the indexer, include before/after numbers from
`cargo run --example bench --release -- <dir>` (methodology in
[`BENCHMARKS.md`](BENCHMARKS.md)). Regressions above ~20% need justification.

## What we are not accepting (for now)

- An in-app LLM or chat panel — Mnemo is the search backend for agents.
- Cloud sync, accounts, telemetry.
- Writing through the agent API.
- Bundling large model runtimes by default.

Keep changes scoped to the shipped surface; larger product directions should
start as an issue.
