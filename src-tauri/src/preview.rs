//! On-demand previews for files the thumbnail decoder cannot read directly.
//!
//! - Images: the shared 240px thumbnail path.
//! - PDF: the first page, rendered by `pdftoppm` (poppler) when it is installed.
//!
//! Previews are generated only when a card is shown, cached as 240px JPEGs next
//! to the image thumbnails, and reused until the source file changes. External
//! tools are optional: when a tool is missing the file keeps its type icon.
//! Renders run with a timeout and a small concurrency limit, so a grid full of
//! PDFs cannot start dozens of processes at once.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

const RENDER_TIMEOUT: Duration = Duration::from_secs(12);
const MAX_CONCURRENT_RENDERS: usize = 2;

/// Counting semaphore for external renders.
struct Slots {
    used: Mutex<usize>,
    free: Condvar,
}

fn slots() -> &'static Slots {
    static SLOTS: std::sync::OnceLock<Slots> = std::sync::OnceLock::new();
    SLOTS.get_or_init(|| Slots {
        used: Mutex::new(0),
        free: Condvar::new(),
    })
}

fn acquire() -> SlotGuard {
    let s = slots();
    let mut used = s.used.lock().unwrap_or_else(|p| p.into_inner());
    while *used >= MAX_CONCURRENT_RENDERS {
        used = s.free.wait(used).unwrap_or_else(|p| p.into_inner());
    }
    *used += 1;
    SlotGuard
}

struct SlotGuard;

impl Drop for SlotGuard {
    fn drop(&mut self) {
        let s = slots();
        let mut used = s.used.lock().unwrap_or_else(|p| p.into_inner());
        *used = used.saturating_sub(1);
        s.free.notify_one();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Image,
    Pdf,
}

/// Classifies a file by extension. `None` means it has no preview.
pub fn kind_for(filename: &str) -> Option<Kind> {
    let ext = filename
        .rsplit('.')
        .next()
        .filter(|_| filename.contains('.'))?;
    match ext.to_ascii_lowercase().as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "tif" => Some(Kind::Image),
        "pdf" => Some(Kind::Pdf),
        _ => None,
    }
}

/// Returns a 480px JPEG preview for `source`, generating it if needed.
pub fn preview_jpeg(source: &Path, out_dir: &Path) -> Result<PathBuf, String> {
    let name = source
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "file has no name".to_string())?;
    // A grid of results must not download every cloud-only file it shows.
    if crate::indexer::walker::is_cloud_placeholder_path(source) {
        return Err(format!("{} is only in the cloud", name));
    }
    match kind_for(name) {
        Some(Kind::Image) => crate::indexer::extract::image::thumbnail(source, out_dir)
            .ok_or_else(|| format!("could not decode image {}", name)),
        Some(Kind::Pdf) => render_external(source, out_dir, Kind::Pdf),
        None => Err(format!("no preview for {}", name)),
    }
}

fn target_for(source: &Path, out_dir: &Path) -> PathBuf {
    let hash = fnv1a(source.to_string_lossy().as_bytes());
    out_dir.join(format!("{:016x}-preview.jpg", hash))
}

fn is_fresh(target: &Path, source: &Path) -> bool {
    match (std::fs::metadata(target), std::fs::metadata(source)) {
        (Ok(t), Ok(s)) => match (t.modified(), s.modified()) {
            (Ok(tt), Ok(st)) => tt >= st && t.len() > 0,
            _ => false,
        },
        _ => false,
    }
}

fn render_external(source: &Path, out_dir: &Path, kind: Kind) -> Result<PathBuf, String> {
    let target = target_for(source, out_dir);
    if is_fresh(&target, source) {
        return Ok(target);
    }
    std::fs::create_dir_all(out_dir).map_err(|e| format!("create preview dir: {}", e))?;

    let tool = match kind {
        Kind::Pdf => find_tool("pdftoppm").ok_or("pdftoppm (poppler) is not installed")?,
        Kind::Image => return Err("images use the thumbnail path".into()),
    };

    // Render to a temp file, then move it into place so readers never see a
    // half-written JPEG.
    let work = out_dir.join(format!(
        ".render-{}-{}",
        std::process::id(),
        fnv1a(source.to_string_lossy().as_bytes())
    ));
    std::fs::create_dir_all(&work).map_err(|e| format!("render dir: {}", e))?;
    let raw_base = work.join("frame");

    let _slot = acquire();
    let ok = match kind {
        Kind::Pdf => {
            let mut cmd = Command::new(tool);
            cmd.args(["-jpeg", "-r", "110", "-f", "1", "-l", "1", "-singlefile"]);
            // On macOS, pdftoppm reads the PDF from stdin (`-`). Passing the
            // original path makes the child process (a separate binary) touch
            // the protected folder itself, which re-triggers a TCC permission
            // prompt per file. Other platforms have no per-process grant and
            // keep the path, so their behaviour is unchanged.
            let stdin_data = if cfg!(target_os = "macos") {
                match std::fs::read(source) {
                    Ok(bytes) => {
                        cmd.arg("-");
                        Some(bytes)
                    }
                    Err(_) => {
                        return Err(format!("could not read source PDF: {}", source.display()));
                    }
                }
            } else {
                cmd.arg(source);
                None
            };
            cmd.arg(&raw_base);
            run_timed(&mut cmd, stdin_data)
        }
        Kind::Image => false,
    };
    let raw = raw_base.with_extension("jpg");
    let result = if ok && raw.is_file() {
        image::open(&raw)
            .map_err(|e| format!("could not read rendered frame: {}", e))
            .and_then(|img| {
                img.thumbnail(
                    crate::indexer::extract::image::THUMB_SIZE,
                    crate::indexer::extract::image::THUMB_SIZE,
                )
                .to_rgb8()
                .save_with_format(&target, image::ImageFormat::Jpeg)
                .map_err(|e| format!("could not write preview: {}", e))
            })
            .map(|_| target.clone())
    } else {
        Err("render failed or timed out".into())
    };
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// Runs a command with a hard timeout. Returns true on a zero exit status.
///
/// When `stdin_data` is `Some`, the bytes are written to the child's stdin in a
/// background thread so the (possibly large) input cannot deadlock the pipe.
fn run_timed(cmd: &mut Command, stdin_data: Option<Vec<u8>>) -> bool {
    // Hide console windows on Windows (pdftoppm is a console app).
    crate::util::process::hide_console(cmd);
    let cmd = cmd.stdin(if stdin_data.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let Ok(mut child) = cmd.stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
        return false;
    };
    if let (Some(data), Some(mut stdin)) = (stdin_data, child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&data);
        });
    }
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if start.elapsed() > RENDER_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(_) => return false,
        }
    }
}

/// Finds an executable on PATH, plus the usual Homebrew and system locations
/// so a Mac launched from the Finder still sees a Homebrew install.
fn find_tool(name: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for extra in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
        dirs.push(PathBuf::from(extra));
    }
    dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file())
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_previewable_extensions() {
        assert_eq!(kind_for("photo.JPG"), Some(Kind::Image));
        assert_eq!(kind_for("report.pdf"), Some(Kind::Pdf));
        assert_eq!(kind_for("clip.MOV"), None);
        assert_eq!(kind_for("notes.txt"), None);
        assert_eq!(kind_for("README"), None);
    }

    #[test]
    fn missing_tool_is_an_error_not_a_panic() {
        let dir = std::env::temp_dir().join(format!("mnemo-preview-{}", std::process::id()));
        let source = dir.join("nope.txt");
        assert!(preview_jpeg(&source, &dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
