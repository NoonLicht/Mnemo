//! Battery detection for eco crawling.
//!
//! All implementations are best-effort and fail soft: an unknown state is
//! treated as AC power, so indexing is never paused or slowed by a probe error.

use crate::config::CrawlMode;

/// True only in `eco` mode while the machine is running on battery.
pub fn should_pace(crawl_mode: CrawlMode) -> bool {
    crawl_mode == CrawlMode::Eco && (on_battery() || heavy_load())
}

#[cfg(target_os = "linux")]
pub fn on_battery() -> bool {
    use std::fs;

    let Ok(entries) = fs::read_dir("/sys/class/power_supply") else {
        return false;
    };
    let mut battery_seen = false;
    let mut charging = false;
    for entry in entries.flatten() {
        let dir = entry.path();
        let kind = fs::read_to_string(dir.join("type")).unwrap_or_default();
        if kind.trim() != "Battery" {
            continue;
        }
        battery_seen = true;
        let status = fs::read_to_string(dir.join("status")).unwrap_or_default();
        match status.trim() {
            "Discharging" => {}
            "Charging" | "Full" | "Not charging" => charging = true,
            _ => {}
        }
    }
    battery_seen && !charging
}

#[cfg(windows)]
pub fn on_battery() -> bool {
    #[repr(C)]
    struct SystemPowerStatus {
        ac_line_status: u8,
        battery_flag: u8,
        battery_life_percent: u8,
        system_status_flag: u8,
        battery_life_time: u32,
        battery_full_life_time: u32,
    }

    unsafe extern "system" {
        fn GetSystemPowerStatus(status: *mut SystemPowerStatus) -> i32;
    }

    let mut status = SystemPowerStatus {
        ac_line_status: 255,
        battery_flag: 0,
        battery_life_percent: 0,
        system_status_flag: 0,
        battery_life_time: 0,
        battery_full_life_time: 0,
    };
    let ok = unsafe { GetSystemPowerStatus(&mut status) };
    ok != 0 && status.ac_line_status == 0
}

#[cfg(target_os = "macos")]
pub fn on_battery() -> bool {
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        /// Minutes remaining on battery, or a negative sentinel for
        /// unlimited/unknown.
        fn IOPSGetTimeRemainingEstimate() -> f64;
    }

    // -2.0 = unlimited (AC), -1.0 = unknown; battery estimates are >= 0.
    unsafe { IOPSGetTimeRemainingEstimate() >= 0.0 }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn on_battery() -> bool {
    false
}

#[cfg(target_os = "linux")]
pub fn heavy_load() -> bool {
    use std::fs;
    let Ok(s) = fs::read_to_string("/proc/loadavg") else {
        return false;
    };
    let Some(first) = s.split_whitespace().next() else {
        return false;
    };
    let Ok(load1) = first.parse::<f64>() else {
        return false;
    };
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    load1 > cpus as f64 * 0.8
}

#[cfg(target_os = "macos")]
pub fn heavy_load() -> bool {
    // `getloadavg` is a 1-minute load average read, available on every macOS
    // release, so eco mode paces the crawl on a busy Mac the same way it does
    // on Linux. Failures degrade to "not heavy" (never throttles).
    let mut averages = [0.0f64; 3];
    let reported =
        unsafe { libc::getloadavg(averages.as_mut_ptr(), averages.len() as libc::c_int) };
    if reported < 1 {
        return false;
    }
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    averages[0] > cpus as f64 * 0.8
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn heavy_load() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_and_balanced_never_pace() {
        assert!(!should_pace(CrawlMode::Full));
        assert!(!should_pace(CrawlMode::Balanced));
    }

    /// Eco mode calls on_battery() and heavy_load() which do OS I/O. Whatever
    /// they return, should_pace must not panic.
    #[test]
    fn eco_does_not_panic() {
        let _ = should_pace(CrawlMode::Eco);
    }

    /// on_battery and heavy_load must always return a bool without panicking
    /// regardless of the host environment.
    #[test]
    fn os_probes_never_panic() {
        let _ = on_battery();
        let _ = heavy_load();
    }
}
