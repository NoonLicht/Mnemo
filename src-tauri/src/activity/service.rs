//! Activity tracking inside the daemon: foreground-window focus with idle detection, and an
//! optional periodic import of browser history. Both are off unless enabled in the config.

use std::sync::mpsc::{RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::filters::{
    default_excluded_apps, default_excluded_url_parts, is_app_excluded, is_url_excluded,
    sanitize_title,
};
use super::history;
use super::{ActivityStore, Event};
use crate::mnemo_err::Result;
use crate::mnemo_settings::MnemoSettings;

macro_rules! warn { ($($t:tt)*) => { crate::crashlog::info(&format!($($t)*)) }; }

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(windows)]
fn idle_secs() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::GetTickCount;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: `info` is valid and `cbSize` is set as the API requires.
    if unsafe { GetLastInputInfo(&mut info) } == 0 {
        return None;
    }
    // SAFETY: no arguments.
    let now = unsafe { GetTickCount() };
    Some(u64::from(now.wrapping_sub(info.dwTime)) / 1000)
}

/// No keyboard/mouse input for this long ends the current activity (SPEC §5.3: idle is not work).
const IDLE_AFTER_SECS: u64 = 300;
const TICK: Duration = Duration::from_secs(30);
const HISTORY_EVERY: Duration = Duration::from_secs(30 * 60);
const HISTORY_FIRST_IMPORT_DAYS: i64 = 7;

pub struct ActivityService {
    store: ActivityStore,
    exclude_apps: Vec<String>,
    exclude_urls: Vec<String>,
    /// Id of the open focus event, if any.
    open: Mutex<Option<i64>>,
}

#[derive(Default)]
struct Tracker {
    current: Option<(i64, String, String)>,
    idle: bool,
}

impl ActivityService {
    /// Returns `None` when neither activity source is enabled.
    pub fn start(config: &MnemoSettings) -> Result<Option<Arc<Self>>> {
        if !config.activity_enabled && !config.browser_history_enabled {
            return Ok(None);
        }
        let list = |custom: &[String], default: Vec<String>| {
            if custom.is_empty() {
                default
            } else {
                custom.to_vec()
            }
        };
        let svc = Arc::new(Self {
            store: ActivityStore::open(
                &crate::config::MnemoConfig::config_dir().join("activity.sqlite"),
            )?,
            exclude_apps: list(&config.activity_exclude_apps, default_excluded_apps()),
            exclude_urls: list(&config.activity_exclude_urls, default_excluded_url_parts()),
            open: Mutex::new(None),
        });
        svc.store.close_dangling()?;
        if config.activity_enabled {
            svc.start_focus();
        }
        if config.browser_history_enabled {
            let me = Arc::clone(&svc);
            std::thread::Builder::new()
                .name("activity-history".into())
                .spawn(move || {
                    loop {
                        me.import_history();
                        std::thread::sleep(HISTORY_EVERY);
                    }
                })
                .expect("spawn history thread");
        }
        Ok(Some(svc))
    }

    #[cfg(windows)]
    fn start_focus(self: &Arc<Self>) {
        let (tx, rx) = channel();
        if let Err(e) = super::focus::spawn(tx) {
            warn!("cannot start window tracking: {e}");
            return;
        }
        let me = Arc::clone(self);
        std::thread::Builder::new()
            .name("activity-tracker".into())
            .spawn(move || {
                let mut t = Tracker::default();
                if let Some(s) = super::focus::foreground_sample() {
                    me.on_sample(&mut t, s.app, s.title, now_secs());
                }
                loop {
                    match rx.recv_timeout(TICK) {
                        Ok(s) => me.on_sample(&mut t, s.app, s.title, now_secs()),
                        Err(RecvTimeoutError::Timeout) => me.on_tick(&mut t),
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
            })
            .expect("spawn tracker thread");
    }

    #[cfg(not(windows))]
    fn start_focus(self: &Arc<Self>) {
        let _ = channel::<()>; // window tracking is Windows-only for now (X11/Wayland: later)
        warn!("window activity tracking is not implemented on this platform yet");
    }

    fn end_current(&self, t: &mut Tracker, ts: i64) {
        if let Some((id, _, _)) = t.current.take() {
            if let Err(e) = self.store.end_event(id, ts) {
                warn!("activity: {e}");
            }
            *self.open.lock().unwrap() = None;
        }
    }

    fn on_sample(&self, t: &mut Tracker, app: String, title: String, now: i64) {
        t.idle = false;
        let title = sanitize_title(&title);
        if t.current
            .as_ref()
            .is_some_and(|(_, a, ti)| *a == app && *ti == title)
        {
            return;
        }
        self.end_current(t, now);
        if is_app_excluded(&app, &self.exclude_apps) {
            return;
        }
        match self.store.begin_focus(now, &app, &title) {
            Ok(id) => {
                *self.open.lock().unwrap() = Some(id);
                t.current = Some((id, app, title));
            }
            Err(e) => warn!("activity: {e}"),
        }
    }

    #[cfg(windows)]
    fn on_tick(&self, t: &mut Tracker) {
        let Some(idle) = idle_secs() else {
            return;
        };
        let now = now_secs();
        if !t.idle && idle >= IDLE_AFTER_SECS {
            // Input stopped `idle` seconds ago; that is when the activity really ended.
            self.end_current(t, now - idle as i64);
            t.idle = true;
        } else if t.idle && idle < IDLE_AFTER_SECS {
            if let Some(s) = super::focus::foreground_sample() {
                self.on_sample(t, s.app, s.title, now - idle as i64);
            }
            t.idle = false;
        }
    }

    /// Closes the open event so a clean shutdown leaves no dangling activity.
    pub fn shutdown(&self) {
        if let Some(id) = self.open.lock().unwrap().take() {
            let _ = self.store.end_event(id, now_secs());
        }
    }

    pub fn between(&self, from: i64, to: i64) -> Result<Vec<Event>> {
        self.store.between(from, to, now_secs())
    }

    pub fn forget(&self, from: i64, to: i64) -> Result<usize> {
        self.store.delete_between(from, to)
    }

    fn import_history(&self) {
        let now = now_secs();
        for browser in history::installed_browsers() {
            let known = self.store.last_web_ts(&browser.name).unwrap_or(0);
            let since = if known == 0 {
                now - HISTORY_FIRST_IMPORT_DAYS * 86_400
            } else {
                known
            };
            match history::read(&browser, since, 5000) {
                Ok(visits) => {
                    let mut added = 0;
                    for v in visits {
                        let http = v.url.starts_with("http://") || v.url.starts_with("https://");
                        if !http || is_url_excluded(&v.url, &self.exclude_urls) {
                            continue;
                        }
                        if self
                            .store
                            .add_web(v.ts, &browser.name, &v.url, &v.title)
                            .unwrap_or(false)
                        {
                            added += 1;
                        }
                    }
                    if added > 0 {
                        let _ = added;
                    }
                }
                Err(e) => {
                    let _ = e;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> ActivityService {
        ActivityService {
            store: ActivityStore::open_in_memory().unwrap(),
            exclude_apps: default_excluded_apps(),
            exclude_urls: default_excluded_url_parts(),
            open: Mutex::new(None),
        }
    }

    fn focus(events: &[Event]) -> Vec<(String, String, i64, i64)> {
        events
            .iter()
            .filter(|e| e.kind == "focus")
            .map(|e| (e.app.clone(), e.title.clone(), e.ts_start, e.ts_end))
            .collect()
    }

    #[test]
    fn switching_windows_closes_the_previous_event() {
        let s = service();
        let mut t = Tracker::default();
        s.on_sample(&mut t, "code.exe".into(), "main.rs".into(), 100);
        s.on_sample(&mut t, "code.exe".into(), "main.rs".into(), 110); // duplicate: ignored
        s.on_sample(&mut t, "chrome.exe".into(), "Docs".into(), 160);
        s.shutdown();
        let got = focus(&s.between(0, 1000).unwrap());
        assert_eq!(got[0], ("code.exe".into(), "main.rs".into(), 100, 160));
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn excluded_apps_and_private_windows_are_not_leaked() {
        let s = service();
        let mut t = Tracker::default();
        s.on_sample(&mut t, "code.exe".into(), "a".into(), 10);
        s.on_sample(&mut t, "KeePassXC.exe".into(), "Vault".into(), 20);
        s.on_sample(
            &mut t,
            "chrome.exe".into(),
            "Bank statement (Incognito)".into(),
            30,
        );
        s.shutdown();
        let got = focus(&s.between(0, 1000).unwrap());
        assert!(
            got.iter()
                .all(|g| g.0 != "KeePassXC.exe" && !g.1.contains("Vault"))
        );
        assert!(got.iter().all(|g| !g.1.contains("Bank")));
        assert!(got.iter().any(|g| g.1 == "(private window)"));
        assert_eq!(got[0].3, 20, "recording stops at the excluded app");
    }

    #[test]
    fn crash_leaves_no_dangling_events() {
        let s = service();
        let mut t = Tracker::default();
        s.on_sample(&mut t, "code.exe".into(), "a".into(), 10);
        // simulate restart without shutdown()
        assert_eq!(s.store.close_dangling().unwrap(), 1);
    }
}
