# Releasing

The workflow builds, the maintainer publishes. This keeps release creation out of
GitHub Actions (no release-token permissions to fight) and makes the published
assets explicit.

## 1. Prepare

```bash
git checkout master && git pull
# bump the version in package.json, src-tauri/Cargo.toml, src-tauri/tauri.conf.json
# update CHANGELOG.md
bun install
cargo test --all-targets --manifest-path src-tauri/Cargo.toml
cargo clippy --all-targets --manifest-path src-tauri/Cargo.toml -- -D warnings
npx tsc --noEmit && bun run build
git add -A && git commit -m "chore(release): vX.Y.Z"
git push origin master
```

## 2. Build on CI

```bash
git tag -a vX.Y.Z -m "Mnemo vX.Y.Z"
git push origin vX.Y.Z
gh run watch   # or: gh run list --workflow=release.yml
```

`.github/workflows/release.yml` produces artifacts:

| Artifact | Contents |
|---|---|
| `mnemo-linux` | `.deb`, `.rpm`, `.AppImage` |
| `mnemo-macos` | universal `.dmg` / `.app` |
| `mnemo-windows` | `.msi`, NSIS `.exe` |
| `mnemo-cli-{linux,macos,windows}` | `mnemo-cli` binary |

Builds are **unsigned**. See `INSTALL.md` for the user-facing consequences.

## 3. Review and publish the draft

The tag push builds every platform and assembles a **draft** GitHub release with
all bundles, the CLI binaries, and `SHA256SUMS`.

```bash
# wait for the release build
gh run list --workflow=release.yml --limit 1

# review the draft
gh release view vX.Y.Z
gh release download vX.Y.Z -D /tmp/mnemo-release
(cd /tmp/mnemo-release && sha256sum -c SHA256SUMS)
```

Check that the expected assets are present:

| Asset | Platform |
|---|---|
| `mnemo_*_amd64.deb`, `mnemo-*.rpm`, `mnemo_*.AppImage` | Linux |
| `mnemo_*_universal.dmg` | macOS |
| `mnemo_*_x64-setup.exe`, `mnemo_*_x64_en-US.msi` | Windows |
| `mnemo_*_arm64-setup.exe`, `mnemo_*_arm64_en-US.msi`, `mnemo-portable-*-{x64,arm64}.zip` | Windows (x64 and ARM64) |
| `mnemo-cli-linux-*`, `mnemo-cli-macos-*`, `mnemo-cli-windows-*` | Agent CLI |
| `SHA256SUMS` | All assets |

Then publish the draft (or edit the notes first):

```bash
gh release edit vX.Y.Z --draft=false --latest
```

If the workflow could not create the draft, the manual fallback is:

```bash
RUN=$(gh run list --workflow=release.yml --limit 1 --json databaseId --jq '.[0].databaseId')
rm -rf /tmp/mnemo-release && mkdir -p /tmp/mnemo-release
gh run download "$RUN" -D /tmp/mnemo-release
find /tmp/mnemo-release -type f -exec sha256sum {} + > /tmp/mnemo-release/SHA256SUMS
gh release create vX.Y.Z --title "Mnemo vX.Y.Z" --notes-file /tmp/release-notes.md \
  $(find /tmp/mnemo-release -type f)
```

## 4. Verify

- The release page lists all bundles, the CLI binaries, and `SHA256SUMS`.
- `gh release view vX.Y.Z`
- Download one bundle, verify its checksum, install it, and launch.
- Check `mnemo-cli state --json` on a machine with an index.

## Signing / notarization (not yet)

Add the secrets and env vars described in the comment block in
`release.yml`, then re-run the bundle job. macOS notarization needs
`APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`,
`APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`. Windows needs
`WINDOWS_CERTIFICATE` + password (or a `signtool` step).
