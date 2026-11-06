//! Best-effort background priority for the crawler.
//!
//! Indexing must never fight typing. `balanced` and `eco` put the crawl thread
//! into the OS idle/background class; `full` restores normal priority. Every
//! syscall is advisory and failures are ignored, so unsupported sandboxes and
//! permission-restricted environments still index correctly.
//!
//! Set `MNEMO_NO_BG_PRIORITY=1` to disable the policy (useful for tests,
//! profiling, and diagnosing slow crawls).

use crate::config::CrawlMode;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(test)]
static BACKGROUND_REQUESTS: AtomicUsize = AtomicUsize::new(0);

pub fn apply(crawl_mode: CrawlMode) {
    if std::env::var_os("MNEMO_NO_BG_PRIORITY").is_some() {
        return;
    }
    let background = crawl_mode.is_background();
    #[cfg(test)]
    if background {
        BACKGROUND_REQUESTS.fetch_add(1, Ordering::SeqCst);
    }
    apply_platform(background);
}

#[cfg(test)]
pub fn background_requests() -> usize {
    BACKGROUND_REQUESTS.load(Ordering::SeqCst)
}

#[cfg(target_os = "linux")]
fn apply_platform(background: bool) {
    // SCHED_IDLE is not exported by libc on every Linux target.
    const SCHED_IDLE: libc::c_int = 5;
    const IOPRIO_CLASS_IDLE: libc::c_int = 3;
    const IOPRIO_CLASS_BE: libc::c_int = 2;
    const IOPRIO_BE_NORM: libc::c_int = 4;

    unsafe {
        let param: libc::sched_param = std::mem::zeroed();
        let policy = if background {
            SCHED_IDLE
        } else {
            libc::SCHED_OTHER
        };
        let _ = libc::sched_setscheduler(0, policy, &param);

        let (class, data) = if background {
            (IOPRIO_CLASS_IDLE, 0)
        } else {
            (IOPRIO_CLASS_BE, IOPRIO_BE_NORM)
        };
        let ioprio = (class << 13) | data;
        let _ = libc::syscall(libc::SYS_ioprio_set, 1i32, 0i32, ioprio);
    }
}

#[cfg(target_os = "macos")]
fn apply_platform(background: bool) {
    let priority = if background { libc::PRIO_DARWIN_BG } else { 0 };
    unsafe {
        let _ = libc::setpriority(libc::PRIO_DARWIN_PROCESS, 0, priority);
    }
}

#[cfg(windows)]
fn apply_platform(background: bool) {
    const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
    const NORMAL_PRIORITY_CLASS: u32 = 0x0000_0020;

    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut core::ffi::c_void;
        fn SetPriorityClass(process: *mut core::ffi::c_void, class: u32) -> i32;
    }

    let class = if background {
        BELOW_NORMAL_PRIORITY_CLASS
    } else {
        NORMAL_PRIORITY_CLASS
    };
    unsafe {
        let _ = SetPriorityClass(GetCurrentProcess(), class);
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn apply_platform(_background: bool) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CrawlMode;

    #[test]
    fn only_full_stays_at_normal_priority() {
        assert!(!CrawlMode::Full.is_background());
        assert!(CrawlMode::Balanced.is_background());
        assert!(CrawlMode::Eco.is_background());
    }

    #[test]
    fn full_does_not_request_background_priority() {
        let before = background_requests();
        apply(CrawlMode::Full);
        assert_eq!(background_requests(), before);
    }
}
