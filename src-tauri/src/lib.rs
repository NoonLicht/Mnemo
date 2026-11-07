pub mod activity;
pub mod agent;
pub mod ask;
pub mod cli;
pub mod commands;
pub mod config;
pub mod crashlog;
pub mod crypt;
pub mod daemon;
pub mod doctor;
pub mod embed;
pub mod embedded;
pub mod gpu_ocr;
pub mod indexer;
pub mod main_window;
pub mod mnemo_commands;
pub mod mnemo_err;
pub mod mnemo_settings;
pub mod models;
pub mod ocr;
pub mod ocr_indra;
pub mod onnx_patch;
pub mod perf;
pub mod preview;
pub mod query;
pub mod root_stats;
pub mod search;
pub mod updater;
pub mod util;

use config::MnemoConfig;
use indexer::IndexerManager;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, OnceLock};
use tauri::{
    Manager,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

pub struct AppState {
    pub indexer: Arc<IndexerManager>,
    pub daemon: Option<crate::daemon::DaemonClient>,
}

/// Owns the single-instance lock for the whole process lifetime: the OS releases
/// the advisory lock when the process exits, and dropping the `File` earlier
/// would let a second instance start.
static INSTANCE_LOCK: OnceLock<std::fs::File> = OnceLock::new();

fn instance_lock_path() -> PathBuf {
    MnemoConfig::config_dir().join("mnemo.lock")
}

/// Tries to take the process-wide exclusive lock on `<config_dir>/mnemo.lock`.
///
/// * `Ok(Some(file))` — acquired; the caller must keep `file` alive.
/// * `Ok(None)` — another instance already holds the lock.
/// * `Err(_)` — the lock file could not be created or opened. Startup continues
///   without the guard rather than bricking the app on a read-only config dir.
fn acquire_instance_lock() -> Result<Option<std::fs::File>, String> {
    let dir = MnemoConfig::config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {}", dir.display(), e))?;

    let path = instance_lock_path();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| format!("{}: {}", path.display(), e))?;

    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(format!("{}: {}", path.display(), e)),
    }
}

/// Request file a second launch writes to ask the running app to toggle its
/// window. A file works on every OS and needs no sockets; the running app
/// polls for it.
fn toggle_request_path() -> PathBuf {
    MnemoConfig::config_dir().join("toggle.request")
}

/// `mnemo --toggle`: ask the running app to show or hide its window, then
/// exit. Used by desktop shortcuts on Linux, where global keys may be blocked.
fn request_toggle_and_exit() -> ! {
    let path = toggle_request_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, b"toggle");
    std::process::exit(0);
}

/// Acquires the lock, exiting quietly when another instance is running.
fn acquire_instance_lock_or_exit() -> Option<std::fs::File> {
    match acquire_instance_lock() {
        Ok(Some(file)) => Some(file),
        Ok(None) => {
            if std::env::args().any(|a| a == "--toggle") {
                request_toggle_and_exit();
            }
            eprintln!("[Mnemo] Another Mnemo instance is already running; exiting.");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!(
                "[Mnemo] Could not acquire single-instance lock ({}); continuing without it",
                e
            );
            None
        }
    }
}

/// Marker consumed on the next launch to wipe all Mnemo state.
fn reset_marker_path_in(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join(".reset-requested")
}

fn reset_marker_path() -> std::path::PathBuf {
    reset_marker_path_in(&MnemoConfig::config_dir())
}

/// True when a reset was requested but not applied yet.
pub fn reset_pending() -> bool {
    reset_marker_path().exists()
}

/// Applies a reset marker in `dir`. Returns true when a marker was found and
/// the directory was cleared/recreated. Split from the startup path so the
/// marker/restart contract has an automated test.
fn apply_pending_reset_in(dir: &std::path::Path) -> bool {
    let marker = reset_marker_path_in(dir);
    if !marker.exists() {
        return false;
    }

    match std::fs::remove_dir_all(dir) {
        Ok(()) => {
            if let Err(e) = std::fs::create_dir_all(dir) {
                eprintln!(
                    "[Mnemo] Reset requested but could not recreate {}: {}",
                    dir.display(),
                    e
                );
                return true;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            }
            eprintln!(
                "[Mnemo] Reset requested: cleared all data in {}",
                dir.display()
            );
        }
        Err(e) => eprintln!(
            "[Mnemo] Reset requested but could not clear {}: {}",
            dir.display(),
            e
        ),
    }
    true
}

/// Clears Mnemo state when a reset was requested by `reset_all`.
///
/// Must run before `MnemoConfig::load()` and before `IndexerManager::new()`:
/// removing the directory while the database or index is open would leave
/// handles pointing at deleted inodes.
fn rebuild_marker_path_in(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join(".rebuild-requested")
}

fn rebuild_marker_path() -> std::path::PathBuf {
    rebuild_marker_path_in(&MnemoConfig::config_dir())
}

pub fn rebuild_pending() -> bool {
    rebuild_marker_path().exists()
}

fn apply_pending_rebuild_in(dir: &std::path::Path) -> bool {
    let marker = rebuild_marker_path_in(dir);
    if !marker.exists() {
        return false;
    }
    // Keep settings (config.json); wipe only the index and metadata.
    let _ = std::fs::remove_dir_all(dir.join("index"));
    let _ = std::fs::remove_dir_all(dir.join("thumbs"));
    for name in ["metadata.db", "metadata.db-wal", "metadata.db-shm"] {
        let _ = std::fs::remove_file(dir.join(name));
    }
    let _ = std::fs::remove_file(&marker);
    eprintln!(
        "[Mnemo] Rebuild requested: index cleared in {}",
        dir.display()
    );
    true
}

fn maybe_apply_pending_rebuild() {
    apply_pending_rebuild_in(&MnemoConfig::config_dir());
}

fn maybe_apply_pending_reset() {
    apply_pending_reset_in(&MnemoConfig::config_dir());
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let args: Vec<String> = std::env::args().collect();
    // The GPU OCR helper is this executable started with a flag: it has no
    // window, no index and no instance lock, only an OCR engine on stdin/stdout.
    if args.iter().any(|a| a == gpu_ocr::WORKER_FLAG) {
        std::process::exit(gpu_ocr::worker_main());
    }
    if let Some(mode) = gpu_ocr::apply_cli_args(&args) {
        eprintln!("[Mnemo OCR] --ocr={mode} for this run");
    }
    // Install the crash log hook before anything else so startup failures are
    // captured too. It only touches the config directory path; it never loads
    // or mutates the config.
    crashlog::install_panic_hook();
    crashlog::info(&format!(
        "starting {} {} ({} {})",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    ));

    commands::init_process_start();
    embedded::install();
    crypt::init(mnemo_settings::MnemoSettings::load().encrypt_at_rest);

    maybe_apply_pending_rebuild();
    maybe_apply_pending_reset();

    #[cfg(unix)]
    let daemon_available = crate::daemon::DaemonClient::ensure();
    #[cfg(not(unix))]
    let daemon_available = false;

    // The daemon owns the single-instance lock and the writer. The GUI only
    // takes it in the explicit fallback mode.
    let mut instance_lock = if daemon_available {
        None
    } else {
        acquire_instance_lock_or_exit()
    };

    if instance_lock.is_some() && !instance_lock_path().exists() {
        instance_lock = acquire_instance_lock_or_exit();
    }

    if let Some(file) = instance_lock {
        let _ = INSTANCE_LOCK.set(file);
    }

    let config = MnemoConfig::load();
    crashlog::info(&format!(
        "config loaded: setup_complete={} app_mode={} ocr_enabled={} engine={}",
        config.is_setup_complete, config.app_mode, config.ocr_enabled, config.ocr_engine
    ));
    indexer::extract::set_ocr_enabled(config.ocr_enabled);
    indexer::extract::image::set_ocr_max_image_bytes(config.ocr_max_image_bytes);
    indexer::extract::set_content_caps(
        config.content_max_file_size_mb,
        config.content_max_text_bytes,
    );
    crate::embed::set_max_semantic_units(config.content_max_semantic_units);
    ocr::set_engine(&config.ocr_engine);
    crate::gpu_ocr::set_mode(&config.ocr_compute);

    // Self-heal on launch, without surprise downloads. HTTP downloads only
    // happen when the user actually enabled OCR:
    // - Tesseract language data only when the Tesseract binary is installed
    //   but has no language. Without the binary the data is useless.
    // - The PP-OCR (or Indra) pack only when OCR is on. (A crawl also fetches
    //   it when needed; see `IndexerManager::sync_ocr_state`.)
    if config.ocr_enabled {
        std::thread::spawn(|| {
            use crate::indexer::extract::image;
            if image::tesseract_available() && !image::tesseract_usable() {
                let _ = image::download_eng_tessdata();
            }
        });
        if crate::ocr::accurate_compiled() && crate::ocr::runtime_loadable() {
            // One download at a time: the selected engine's pack first.
            if crate::ocr::engine_name() == crate::ocr::INDRA && !crate::ocr_indra::pack_present() {
                crate::models::download_indra_pack();
            } else if !crate::ocr::pack_present() {
                crate::models::download_ocr_pack();
            }
        }
    }

    // OCR engine selection (switching to PP-OCR when Tesseract is missing,
    // fetching its pack) happens at the start of every crawl, in
    // `IndexerManager::sync_ocr_state`, so it also runs in the daemon and
    // re-queues already-indexed images.

    let is_setup = config.is_setup_complete;
    // Desktop-app mode launches with its window visible; launcher mode starts
    // in the tray. Pending first-run setup always needs UI.
    let open_window_on_start = !is_setup || config.app_mode == "app";

    let (indexer_manager, daemon_client) = if daemon_available {
        (
            Arc::new(IndexerManager::new_readonly(config.clone())),
            Some(crate::daemon::DaemonClient),
        )
    } else {
        let manager = Arc::new(IndexerManager::new(config.clone()));
        if is_setup {
            manager.start_indexing();
        }
        (manager, None)
    };

    // When the PP-OCR pack finishes downloading, OCR every image that was
    // indexed without it. In daemon mode the daemon owns the index: re-sending
    // the config makes it notice the new OCR signature and crawl.
    {
        let manager = Arc::clone(&indexer_manager);
        let daemon = daemon_client.is_some();
        models::on_ocr_pack_ready(move || {
            if daemon {
                let config = manager.config.read().clone();
                if let Err(e) = crate::daemon::DaemonClient.save_config(config) {
                    eprintln!("[Mnemo OCR] could not notify the daemon: {e}");
                }
            } else {
                manager.start_indexing_when_idle();
            }
        });
    }

    let app_state = AppState {
        indexer: indexer_manager,
        daemon: daemon_client,
    };

    let shortcut_str = config.shortcut.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        main_window::toggle(app);
                    }
                })
                .build(),
        )
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::search,
            mnemo_commands::ask,
            mnemo_commands::get_mnemo_settings,
            mnemo_commands::encryption_active,
            mnemo_commands::save_mnemo_settings,
            mnemo_commands::get_activity,
            mnemo_commands::forget_activity,
            commands::legacy_search,
            commands::get_indexing_status,
            commands::get_stats,
            commands::get_root_stats,
            commands::get_perf_report,
            commands::get_gpu_ocr_status,
            commands::download_gpu_runtime,
            commands::clear_perf_report,
            commands::set_theme,
            commands::open_file,
            commands::open_folder,
            commands::get_file_preview,
            commands::get_snippets,
            commands::get_thumbnail,
            commands::get_thumbnail_data,
            commands::get_file_details,
            commands::get_ocr_status,
            commands::download_ocr_language,
            commands::get_ocr_pack_status,
            commands::download_ocr_pack,
            commands::get_indra_pack_status,
            commands::download_indra_pack,
            commands::get_smart_pack_status,
            commands::download_smart_pack,
            commands::get_visual_pack_status,
            commands::download_visual_pack,
            commands::find_duplicates,
            commands::find_similar_images,
            commands::execute_command,
            commands::read_file_content,
            commands::get_config,
            commands::save_config,
            commands::is_setup_complete,
            commands::apply_window_mode,
            commands::is_setup_configured,
            commands::run_setup_again,
            commands::complete_setup,
            commands::get_suggested_directories,
            commands::validate_directory,
            commands::get_scope_suggestions,
            commands::get_home_data,
            commands::toggle_pin,
            commands::hide_window,
            commands::remember_query,
            commands::last_query,
            commands::run_doctor,
            commands::reset_all,
            commands::rebuild_index,
            commands::get_reset_pending,
            commands::get_autostart,
            commands::set_autostart,
            commands::agent_connect_status,
            commands::agent_connect_install,
            commands::agent_connect_snippet,
            commands::check_for_updates,
            commands::download_update,
            commands::get_updater_status,
            commands::install_update_and_restart,
            models::get_models_status,
            models::download_models,
            models::delete_models,
        ])
        .setup(move |app| {
            mnemo_commands::restart_activity();
            // Watch for `mnemo --toggle` from a second launch: it drops a request
            // file, and we toggle the window here. Cheap: one stat every 150 ms.
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    loop {
                        std::thread::sleep(std::time::Duration::from_millis(150));
                        let path = toggle_request_path();
                        if path.exists() {
                            let _ = std::fs::remove_file(&path);
                            main_window::toggle(&handle);
                        }
                    }
                });
            }

            if app.state::<AppState>().daemon.is_none() {
                app.state::<AppState>()
                    .indexer
                    .set_app_handle(app.handle().clone());
            }

            // 1. Register global shortcut (e.g. Alt+Space). The summon shortcut
            // only makes sense in launcher mode; full-app mode stays open.
            if config.app_mode == "launcher" {
                if let Ok(shortcut) = Shortcut::from_str(&shortcut_str) {
                    if let Err(e) = app.global_shortcut().register(shortcut) {
                        eprintln!(
                            "[Mnemo] Could not register global shortcut '{}': {}",
                            shortcut_str, e
                        );
                    }
                } else {
                    eprintln!("[Mnemo] Could not parse global shortcut: {}", shortcut_str);
                }
            }

            // 2. Setup System Tray
            let show_item = MenuItem::with_id(app, "toggle", "Открыть поиск", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Выйти из Mnemo", true, None::<&str>)?;
            let tray_menu = Menu::with_items(app, &[&show_item, &quit_item])?;

            // Windows users expect a click on the tray icon to offer the
            // menu (Open / Quit), and a double-click to open. Elsewhere a
            // left click toggles the window and the menu is a right click.
            let mut tray_builder = TrayIconBuilder::new()
                .menu(&tray_menu)
                .show_menu_on_left_click(cfg!(windows))
                .on_menu_event(move |app, event| match event.id().as_ref() {
                    "toggle" => main_window::toggle(app),
                    "quit" => {
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| match event {
                    // Click fires on press and on release. Acting on both
                    // toggled twice, so the window opened and closed again.
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } if !cfg!(windows) => main_window::toggle(tray.app_handle()),
                    TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    } => main_window::show(tray.app_handle()),
                    _ => {}
                });

            if let Some(icon) = app.default_window_icon() {
                tray_builder = tray_builder.icon(icon.clone());
            }

            let _ = tray_builder.build(app);

            // 3. Launcher mode starts in the background (tray, watcher and
            // indexer only); the window is created by the first toggle
            // (Alt+Space, tray click). Desktop-app mode is an ordinary app:
            // show its window on launch. A pending first-run setup always
            // needs UI right away.
            if open_window_on_start {
                main_window::show(app.handle());
            }

            // A dev session may have registered the debug binary for
            // sign-in; that would open a console at every login.
            commands::self_heal_autostart();

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while running Mnemo application")
        .run(|_app, event| {
            // Destroying the search window (hide) must not end the app. The
            // indexer, watcher and tray keep running. Only the tray's Quit
            // (app.exit) ends it, and that call is not prevented here.
            if let tauri::RunEvent::Exit = event {
                mnemo_commands::shutdown_activity();
            }
            if let tauri::RunEvent::ExitRequested {
                api, code: None, ..
            } = event
            {
                api.prevent_exit();
            }
        });
}

#[cfg(test)]
mod reset_tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn pending_reset_clears_and_recreates_the_directory() {
        let dir = std::env::temp_dir().join(format!(
            "mnemo-reset-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("metadata.db"), b"stale").unwrap();
        std::fs::write(reset_marker_path_in(&dir), b"1").unwrap();
        assert!(reset_pending_at(&dir));

        assert!(apply_pending_reset_in(&dir));
        assert!(dir.is_dir());
        assert!(!reset_marker_path_in(&dir).exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);

        // A second call without a marker is a no-op.
        assert!(!apply_pending_reset_in(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn reset_pending_at(dir: &std::path::Path) -> bool {
        reset_marker_path_in(dir).exists()
    }
}
