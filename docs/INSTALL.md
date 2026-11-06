# Install & build

## Prebuilt bundles

Download from [Releases](<repository-url>).

| Platform | Asset | Notes |
|---|---|---|
| Linux | `mnemo_<ver>_amd64.AppImage` | chmod +x and run; no install needed |
| Linux | `mnemo_<ver>_amd64.deb` | `sudo apt install ./mnemo_*.deb` |
| Linux | `mnemo-<ver>-1.x86_64.rpm` | `sudo rpm -i mnemo-*.rpm` |
| macOS | `Mnemo_<ver>_universal.dmg` | Intel + Apple Silicon |
| Windows | `Mnemo_<ver>_x64_en-US.msi` | per-user install |
| Windows | `mnemo-portable-<ver>-x64.zip` | portable: unzip in a writable folder, run `mnemo.exe`; config/index/data live in `data\` next to the exe, nothing in `%APPDATA%`. Keeps the `portable` marker file in place. |
| Windows on ARM | `Mnemo_<ver>_arm64_en-US.msi`, `mnemo_<ver>_arm64-setup.exe`, `mnemo-portable-<ver>-arm64.zip`, `mnemo-cli-windows-aarch64.exe` | native ARM64 (Snapdragon) builds; same layout as x64 |

### The builds are unsigned

This is the honest trade-off for a project without a paid signing identity yet.

**macOS.** Gatekeeper will block the first launch. Either right-click the app →
**Open** → **Open**, or clear the quarantine flag:

```bash
xattr -dr com.apple.quarantine /Applications/Mnemo.app
```

**Windows.** SmartScreen shows "Windows protected your PC". Choose **More info** →
**Run anyway**.

**Linux.** No signing step; AppImage may need `--no-sandbox` on hardened distros.

If your organization requires signed binaries, build from source or wait for
notarized releases. Verify downloads with the published `SHA256SUMS`.

---

## Build prerequisites

### All platforms

- Rust stable (edition 2024)
- [Bun](https://bun.sh) 1.1+ (Node 18+ also works)
- Git

### Linux (Debian/Ubuntu)

```bash
sudo apt update
sudo apt install -y \
  libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev \
  patchelf libgtk-3-dev build-essential curl wget file
```

Fedora:

```bash
sudo dnf install webkit2gtk4.1-devel libappindicator-gtk3-devel \
  librsvg2-devel patchelf gtk3-devel
```

Arch:

```bash
sudo pacman -S webkit2gtk-4.1 libappindicator-gtk3 librsvg patchelf gtk3 base-devel
```

### macOS

```bash
xcode-select --install
```

### Windows

- [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)
  with **Desktop development with C++**
- WebView2 runtime (preinstalled on Windows 10/11)

---

## Build

```bash
git clone <repository-url>
cd mnemo
bun install

bun run tauri dev        # dev app
bun run tauri build      # bundles into src-tauri/target/release/bundle/
```

Bundle outputs:

- Linux: `bundle/appimage/`, `bundle/deb/`, `bundle/rpm/`
- macOS: `bundle/dmg/`, `bundle/macos/`
- Windows: `bundle/msi/`, `bundle/nsis/`

The release workflow (`.github/workflows/release.yml`) builds all three platforms
on a `v*` tag and uploads a draft release.

---

## Development tasks

A `Makefile` wraps the common commands:

```bash
make            # list tasks
make install    # dependencies (bun, npm fallback)
make dev        # Tauri dev app
make build      # release bundles
make cli        # agent CLI
make test       # Rust tests
make check      # tsc + cargo check
make fmt        # Rust fmt
make bench BENCH_DIR=~/Projects
make data-dir   # where config/index live
make reset-index# delete index, keep settings
make reset      # delete all Mnemo data
make clean      # remove build outputs
```

`reset` and `reset-index` delete immediately. Quit Mnemo first.

---

## The agent CLI

`mnemo-cli` is a separate binary that shares the same index.

```bash
cargo build --release --manifest-path cli/Cargo.toml
```

Install it on your `PATH`:

```bash
# Linux / macOS
install -m 755 cli/target/release/mnemo-cli ~/.local/bin/mnemo-cli

# Windows (PowerShell)
Copy-Item cli\target\release\mnemo-cli.exe "$env:USERPROFILE\bin\"
```

Then point your MCP client at it:

```json
{
  "mcpServers": {
    "mnemo": { "command": "mnemo-cli", "args": ["mcp"] }
  }
}
```

If `mnemo-cli` is not on `PATH`, use the absolute path in `command`.

CLI binaries are published as **workflow artifacts** on the release run (the
desktop bundles are the release assets). Build from source, or download the
artifact from the Actions run.

---

## OCR (optional)

OCR needs the `tesseract` binary **and** language data. Mnemo can download
`eng.traineddata` (~4 MB) into its own directory from the guided setup flow
(**About → Run setup again → Optional packs**), so you don't need root. If you
prefer a system install:

```bash
# Linux (Debian/Ubuntu)
sudo apt install tesseract-ocr tesseract-ocr-eng

# macOS
brew install tesseract

# Windows
winget install UB-Mannheim.TesseractOCR
```

Settings → Search shows one of four states: ready (with languages), engine found
but no language data (points to setup), engine missing (platform install command),
or unknown. The OCR switch is disabled until OCR can actually work. Doctor repeats
the same check.

### OCR packs

Normal app builds compile the PP-OCRv6 tiny engine via the default
`accurate-ocr` feature. In **Setup → Optional packs** and Settings → Search you
pick **one** OCR engine: PP-OCRv6 (default, recommended), Indra (higher
quality: deskew, orientation and multi-script recognition) or Tesseract. Only
the selected engine's download is offered. PP-OCRv6
is ~6.1 MB (detector 1.7 MB, recognizer 4.3 MB, dictionary 27 KB); the Indra
pack is ~27 MB — both are verified
against pinned SHA-256 checksums. Release installers bundle ONNX Runtime 1.31 (Windows, Apple silicon, Linux x64). If the pack is
missing, or the build uses `--no-default-features`, Mnemo falls back to
Tesseract. Native macOS/Windows OCR is not implemented yet and the Settings
option is disabled.

Models: PaddleOCR PP-OCRv6 tiny (Apache-2.0), published by PaddlePaddle; Indra
weights by Datalab (indra-ocr), under a modified OpenRAIL-M license. The Rust
ONNX wrapper is `paddleocr_rs_onnx` (MIT).

---

## First run

1. Pick folders (Home, Projects, Documents, Downloads).
2. Choose an exclusion preset (Developer / Creative / All-round).
3. Set preferences (theme, global shortcut). Click the shortcut field and press
   the key combo; Esc cancels.
4. Optionally download OCR language data and the semantic model. Each button
   shows live state (downloading, progress, ready, or the error).
5. Start. The first scan shows live counts, a progress bar, and an ETA, then ends
   with a Done summary; later launches are search-first and instant. You can
   search while indexing runs.

The index lives at:

| OS | Path |
|---|---|
| Linux | `~/.config/mnemo/` |
| macOS | `~/Library/Application Support/mnemo/` (config via dirs) |
| Windows | `%APPDATA%\mnemo\` |

Contents: `config.json`, `metadata.db` (SQLite WAL), `index/` (Tantivy),
`thumbs/`, optional `models/`, `crash.log`.

---

## Uninstall / reset

- **Reset inside the app:** Settings → About → **Reset everything** (type `RESET`).
  Deletes the index, database, models, and config, then restarts.
- **Manual:** quit Mnemo, delete the config directory above.
- **Uninstall:** remove the app bundle/package; the data directory is separate.


## Linux

Mnemo ships as an AppImage, a `.deb`, and an `.rpm`. The build is tested on
Ubuntu 22.04 (X11). Other desktops are not yet verified.

### Set a quick shortcut

Mnemo's own Alt+Space shortcut may not work on Wayland, where desktops block
global keys for apps. Instead, bind any key to this command:

```
mnemo --toggle
```

`--toggle` shows the window if it's hidden and hides it if it's shown. If the
app isn't running yet, it starts it.

Use the full path to the app if `mnemo` isn't on your PATH (for an AppImage,
the path where you saved the file).

- **GNOME:** Settings → Keyboard → View and Customize Shortcuts → Custom Shortcuts → `+`.
  Name it "Mnemo", set the command to `mnemo --toggle`, then press your key.
- **KDE Plasma:** System Settings → Shortcuts → Add New → Global Shortcut → Command/URL.
  Set the trigger and the command `mnemo --toggle`.
- **sway:** add `bindsym $mod+space exec mnemo --toggle` to `~/.config/sway/config`, then reload.
- **i3:** add `bindsym $mod+space exec mnemo --toggle` to `~/.config/i3/config`, then reload.
- **Hyprland:** add `bind = SUPER, space, exec, mnemo --toggle` to `~/.config/hypr/hyprland.conf`.

Pick a key that no other app uses. Mnemo doesn't change or reserve any keys.
