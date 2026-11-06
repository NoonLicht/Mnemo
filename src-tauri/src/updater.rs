use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use tauri::AppHandle;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubRelease {
    pub tag_name: String,
    pub name: Option<String>,
    pub body: Option<String>,
    pub html_url: String,
    pub published_at: Option<String>,
    #[serde(default)]
    pub assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateInfo {
    pub current_version: String,
    pub latest_version: String,
    pub has_update: bool,
    pub release_name: String,
    pub release_notes: String,
    pub release_url: String,
    pub published_at: String,
    pub asset_name: Option<String>,
    pub asset_url: Option<String>,
    pub asset_size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateProgress {
    pub phase: String,
    pub downloaded: u64,
    pub total: u64,
    pub percent: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateStatus {
    pub busy: bool,
    pub progress: Option<UpdateProgress>,
    pub error: Option<String>,
    pub downloaded_path: Option<String>,
    pub ready_to_install: bool,
}

/// Compares two version strings (`candidate` vs `current`), stripping any leading
/// 'v' and discarding pre-release / build metadata tags (e.g. "1.0.14-beta").
/// Returns true only if `candidate` is strictly newer.
pub fn is_newer_version(candidate: &str, current: &str) -> bool {
    fn parse(v: &str) -> Vec<u64> {
        let clean = v
            .trim()
            .trim_start_matches('v')
            .split(['-', '+'])
            .next()
            .unwrap_or("");
        clean
            .split('.')
            .filter_map(|s| s.parse::<u64>().ok())
            .collect()
    }
    let a = parse(candidate);
    let b = parse(current);
    let len = a.len().max(b.len());
    for i in 0..len {
        let val_a = a.get(i).copied().unwrap_or(0);
        let val_b = b.get(i).copied().unwrap_or(0);
        if val_a != val_b {
            return val_a > val_b;
        }
    }
    false
}

/// Matches the running OS and CPU architecture against the released assets.
pub fn select_platform_asset(assets: &[ReleaseAsset]) -> Option<ReleaseAsset> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    match os {
        "windows" => {
            let is_arm = arch == "aarch64" || arch == "arm64";
            if is_arm {
                if let Some(a) = assets.iter().find(|a| {
                    a.name.to_lowercase().contains("arm64") && a.name.ends_with("-setup.exe")
                }) {
                    return Some(a.clone());
                }
                if let Some(a) = assets
                    .iter()
                    .find(|a| a.name.to_lowercase().contains("arm64") && a.name.ends_with(".msi"))
                {
                    return Some(a.clone());
                }
            } else {
                if let Some(a) = assets.iter().find(|a| {
                    (a.name.to_lowercase().contains("x64")
                        || a.name.to_lowercase().contains("x86_64"))
                        && a.name.ends_with("-setup.exe")
                }) {
                    return Some(a.clone());
                }
                if let Some(a) = assets.iter().find(|a| {
                    (a.name.to_lowercase().contains("x64")
                        || a.name.to_lowercase().contains("x86_64"))
                        && a.name.ends_with(".msi")
                }) {
                    return Some(a.clone());
                }
            }
            if let Some(a) = assets.iter().find(|a| a.name.ends_with("-setup.exe")) {
                return Some(a.clone());
            }
            if let Some(a) = assets.iter().find(|a| a.name.ends_with(".msi")) {
                return Some(a.clone());
            }
            if let Some(a) = assets.iter().find(|a| a.name.ends_with(".exe")) {
                return Some(a.clone());
            }
        }
        "macos" => {
            if let Some(a) = assets
                .iter()
                .find(|a| a.name.to_lowercase().contains("universal") && a.name.ends_with(".dmg"))
            {
                return Some(a.clone());
            }
            if let Some(a) = assets
                .iter()
                .find(|a| a.name.to_lowercase().contains(arch) && a.name.ends_with(".dmg"))
            {
                return Some(a.clone());
            }
            if let Some(a) = assets.iter().find(|a| a.name.ends_with(".dmg")) {
                return Some(a.clone());
            }
        }
        "linux" => {
            if let Some(a) = assets.iter().find(|a| a.name.ends_with(".AppImage")) {
                return Some(a.clone());
            }
            if let Some(a) = assets.iter().find(|a| a.name.ends_with(".deb")) {
                return Some(a.clone());
            }
            if let Some(a) = assets.iter().find(|a| a.name.ends_with(".rpm")) {
                return Some(a.clone());
            }
        }
        _ => {}
    }
    None
}

static STATUS: OnceLock<Mutex<UpdateStatus>> = OnceLock::new();
static DOWNLOADING: AtomicBool = AtomicBool::new(false);

fn status_slot() -> &'static Mutex<UpdateStatus> {
    STATUS.get_or_init(|| Mutex::new(UpdateStatus::default()))
}

pub fn get_status() -> UpdateStatus {
    status_slot().lock().unwrap().clone()
}

/// Checks GitHub for the latest release metadata.
pub fn check_for_update() -> Result<UpdateInfo, String> {
    // Mnemo never contacts a release server on its own: this fork has no update
    // channel yet, and the upstream channel would install someone else's builds.
    Err("Automatic updates are disabled in this build.".to_string())
}

#[allow(dead_code)]
fn check_for_update_unused() -> Result<UpdateInfo, String> {
    let current_version = env!("CARGO_PKG_VERSION");
    let url = "https://invalid.example/releases/latest";

    let resp: GitHubRelease = ureq::get(url)
        .set("User-Agent", &format!("mnemo-updater/{}", current_version))
        .call()
        .map_err(|e| format!("Failed to reach GitHub release API: {e}"))?
        .into_json()
        .map_err(|e| format!("Failed to parse release information: {e}"))?;

    let latest_tag = resp.tag_name.trim_start_matches('v');
    let has_update = is_newer_version(latest_tag, current_version);
    let chosen_asset = select_platform_asset(&resp.assets);

    Ok(UpdateInfo {
        current_version: current_version.to_string(),
        latest_version: latest_tag.to_string(),
        has_update,
        release_name: resp.name.unwrap_or_else(|| resp.tag_name.clone()),
        release_notes: resp.body.unwrap_or_default(),
        release_url: resp.html_url,
        published_at: resp.published_at.unwrap_or_default(),
        asset_name: chosen_asset.as_ref().map(|a| a.name.clone()),
        asset_url: chosen_asset
            .as_ref()
            .map(|a| a.browser_download_url.clone()),
        asset_size: chosen_asset.as_ref().map(|a| a.size),
    })
}

/// Initiates download of the chosen asset in a background worker thread.
pub fn download_update(asset_url: &str, asset_name: &str) -> Result<UpdateStatus, String> {
    if DOWNLOADING.swap(true, Ordering::SeqCst) {
        return Ok(get_status());
    }

    let update_dir = std::env::temp_dir().join("mnemo-updates");
    if let Err(e) = std::fs::create_dir_all(&update_dir) {
        DOWNLOADING.store(false, Ordering::SeqCst);
        let mut s = status_slot().lock().unwrap();
        s.busy = false;
        s.error = Some(format!("Could not create update directory: {e}"));
        return Ok(s.clone());
    }

    let final_path = update_dir.join(asset_name);
    let part_path = update_dir.join(format!("{}.part", asset_name));

    // Reset status
    {
        let mut s = status_slot().lock().unwrap();
        s.busy = true;
        s.error = None;
        s.ready_to_install = false;
        s.downloaded_path = None;
        s.progress = Some(UpdateProgress {
            phase: "downloading".to_string(),
            downloaded: 0,
            total: 0,
            percent: 0,
        });
    }

    let url = asset_url.to_string();

    std::thread::Builder::new()
        .name("mnemo-updater".to_string())
        .spawn(move || {
            let res = (|| -> Result<(), String> {
                let resp = ureq::get(&url)
                    .set(
                        "User-Agent",
                        &format!("mnemo-updater/{}", env!("CARGO_PKG_VERSION")),
                    )
                    .call()
                    .map_err(|e| format!("Download request failed: {e}"))?;

                let total_bytes: u64 = resp
                    .header("Content-Length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);

                let mut reader = resp.into_reader();
                let mut file = std::fs::File::create(&part_path)
                    .map_err(|e| format!("Failed to create temporary file: {e}"))?;

                let mut downloaded: u64 = 0;
                let mut buf = [0u8; 64 * 1024];

                loop {
                    use std::io::Read;
                    use std::io::Write;
                    let n = reader
                        .read(&mut buf)
                        .map_err(|e| format!("Read error: {e}"))?;
                    if n == 0 {
                        break;
                    }
                    file.write_all(&buf[..n])
                        .map_err(|e| format!("Write error: {e}"))?;
                    downloaded += n as u64;

                    let percent = if total_bytes > 0 {
                        ((downloaded as f64 / total_bytes as f64) * 100.0).min(100.0) as u8
                    } else {
                        0
                    };

                    let mut s = status_slot().lock().unwrap();
                    s.progress = Some(UpdateProgress {
                        phase: "downloading".to_string(),
                        downloaded,
                        total: total_bytes,
                        percent,
                    });
                }

                let _ = std::fs::remove_file(&final_path);
                std::fs::rename(&part_path, &final_path)
                    .map_err(|e| format!("Failed to finalize update file: {e}"))?;

                Ok(())
            })();

            let mut s = status_slot().lock().unwrap();
            s.busy = false;
            DOWNLOADING.store(false, Ordering::SeqCst);
            match res {
                Ok(()) => {
                    s.ready_to_install = true;
                    s.downloaded_path = Some(final_path.to_string_lossy().to_string());
                    if let Some(ref mut p) = s.progress {
                        p.phase = "ready".to_string();
                        p.percent = 100;
                    }
                }
                Err(err) => {
                    s.error = Some(err);
                    if let Some(ref mut p) = s.progress {
                        p.phase = "error".to_string();
                    }
                }
            }
        })
        .map_err(|e| format!("Failed to spawn updater thread: {e}"))?;

    Ok(get_status())
}

/// Spawns the installer detached and exits Mnemo so files are replaced cleanly.
pub fn install_and_restart(app: &AppHandle) -> Result<(), String> {
    let path_str = {
        let s = status_slot().lock().unwrap();
        s.downloaded_path
            .clone()
            .ok_or_else(|| "No update has been downloaded yet".to_string())?
    };

    let path = PathBuf::from(&path_str);
    if !path.is_file() {
        return Err(format!(
            "Downloaded update file does not exist at {:?}",
            path
        ));
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const DETACHED_PROCESS: u32 = 0x00000008;

        if path_str.ends_with(".msi") {
            let mut cmd = std::process::Command::new("msiexec");
            cmd.args(["/i", &path_str]);
            cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
            cmd.spawn()
                .map_err(|e| format!("Failed to launch MSI installer: {e}"))?;
        } else {
            let mut cmd = std::process::Command::new(&path);
            cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
            cmd.spawn()
                .map_err(|e| format!("Failed to launch installer: {e}"))?;
        }
        app.exit(0);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        let _ = app;
        std::process::Command::new("open")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open DMG installer: {e}"))?;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    {
        if path_str.ends_with(".AppImage") {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path)
                .map_err(|e| e.to_string())?
                .permissions();
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(&path, perms);
            std::process::Command::new(&path)
                .spawn()
                .map_err(|e| format!("Failed to launch AppImage: {e}"))?;
            app.exit(0);
            Ok(())
        } else {
            let _ = app;
            std::process::Command::new("xdg-open")
                .arg(&path)
                .spawn()
                .map_err(|e| format!("Failed to launch package installer: {e}"))?;
            Ok(())
        }
    }

    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        let _ = app;
        Err("Unsupported operating system for in-app update launch".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_newer_version() {
        assert!(is_newer_version("1.0.14", "1.0.13"));
        assert!(is_newer_version("v1.0.14", "1.0.13"));
        assert!(is_newer_version("1.0.14-beta.1", "1.0.13"));
        assert!(is_newer_version("1.1.0", "1.0.13"));
        assert!(is_newer_version("2.0.0", "1.0.13"));

        assert!(!is_newer_version("1.0.13", "1.0.13"));
        assert!(!is_newer_version("v1.0.13", "1.0.13"));
        assert!(!is_newer_version("1.0.12", "1.0.13"));
        assert!(!is_newer_version("0.9.9", "1.0.13"));
    }

    #[test]
    fn test_select_platform_asset() {
        let assets = vec![
            ReleaseAsset {
                name: "mnemo_1.0.14_x64-setup.exe".to_string(),
                browser_download_url: "http://example.com/setup.exe".to_string(),
                size: 1000,
            },
            ReleaseAsset {
                name: "mnemo_1.0.14_arm64-setup.exe".to_string(),
                browser_download_url: "http://example.com/arm-setup.exe".to_string(),
                size: 1000,
            },
            ReleaseAsset {
                name: "mnemo_1.0.14_universal.dmg".to_string(),
                browser_download_url: "http://example.com/mnemo.dmg".to_string(),
                size: 1000,
            },
            ReleaseAsset {
                name: "mnemo_1.0.14_amd64.AppImage".to_string(),
                browser_download_url: "http://example.com/mnemo.AppImage".to_string(),
                size: 1000,
            },
        ];

        let selected = select_platform_asset(&assets);
        assert!(selected.is_some());
        let name = selected.unwrap().name;
        if cfg!(windows) {
            if cfg!(target_arch = "aarch64") {
                assert!(name.contains("arm64"));
            } else {
                assert!(name.contains("x64"));
            }
        } else if cfg!(target_os = "macos") {
            assert!(name.ends_with(".dmg"));
        } else if cfg!(target_os = "linux") {
            assert!(name.ends_with(".AppImage"));
        }
    }
}
