//! Best-effort return of freed heap pages to the OS.
//!
//! Long extractions (images, PDFs, Office documents) can leave large freed
//! allocations in the process allocator. Calling this after each unit of work
//! keeps the daemon's resident set close to its baseline. It is always a no-op
//! on unsupported targets and never fails.

#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn release_memory() {
    unsafe {
        libc::malloc_trim(0);
    }
}

#[cfg(target_os = "macos")]
pub fn release_memory() {
    // `malloc_zone_pressure_relief` is declared in <malloc/malloc.h>, not in
    // the libc crate's macOS bindings. Declare the two symbols directly so the
    // universal macOS build links without a C shim.
    unsafe extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut core::ffi::c_void, goal: usize) -> usize;
        fn malloc_default_zone() -> *mut core::ffi::c_void;
    }
    unsafe {
        let _ = malloc_zone_pressure_relief(malloc_default_zone(), 0);
    }
}

#[cfg(windows)]
pub fn release_memory() {
    // `EmptyWorkingSet` trims the process working set; `GetCurrentProcess`
    // returns a pseudo-handle that does not need closing.
    #[link(name = "psapi")]
    unsafe extern "system" {
        fn EmptyWorkingSet(h_process: *mut core::ffi::c_void) -> i32;
        fn GetCurrentProcess() -> *mut core::ffi::c_void;
    }
    unsafe {
        let _ = EmptyWorkingSet(GetCurrentProcess());
    }
}

#[cfg(not(any(
    all(target_os = "linux", target_env = "gnu"),
    target_os = "macos",
    windows
)))]
pub fn release_memory() {}

/// Terminates the WebKit/WebView helper processes that survive the destruction
/// of the search window, so the hidden app keeps only the Rust side in memory.
///
/// These helpers are children of the main process. Killing them is safe once
/// the window (and its webview) has been destroyed; showing the window again
/// spawns fresh ones.
pub fn kill_webview_processes() {
    let pid = std::process::id();

    #[cfg(target_os = "linux")]
    {
        if let Ok(entries) = std::fs::read_dir("/proc") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let pid_str = name.to_string_lossy();
                if !pid_str.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                let Ok(child_pid) = pid_str.parse::<i32>() else {
                    continue;
                };
                if child_pid <= 0 || child_pid == pid as i32 {
                    continue;
                }
                let stat = std::fs::read_to_string(format!("/proc/{}/stat", child_pid))
                    .unwrap_or_default();
                let Some(idx) = stat.rfind(')') else { continue };
                let rest = &stat[idx + 1..];
                let fields: Vec<&str> = rest.split_whitespace().collect();
                // fields[0] = state, fields[1] = ppid
                if fields.len() < 2 || fields[1] != pid.to_string() {
                    continue;
                }
                let comm = std::fs::read_to_string(format!("/proc/{}/comm", child_pid))
                    .unwrap_or_default();
                let comm = comm.trim();
                if comm.contains("WebKit") || comm.contains("webkit") {
                    unsafe {
                        libc::kill(child_pid, libc::SIGKILL);
                    }
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("pkill")
            .args(["-P", &pid.to_string(), "WebKit"])
            .status();
    }

    #[cfg(windows)]
    {
        // taskkill cannot filter by parent PID (its /FI grammar has no PPID),
        // so enumerate processes ourselves and terminate the WebView2 helpers
        // directly. Best-effort: any API failure just skips that step.
        for child in windows_webview_children(pid) {
            unsafe {
                use windows::Win32::Foundation::CloseHandle;
                use windows::Win32::System::Threading::{
                    OpenProcess, PROCESS_TERMINATE, TerminateProcess,
                };
                if let Ok(handle) = OpenProcess(PROCESS_TERMINATE, false, child) {
                    let _ = TerminateProcess(handle, 0);
                    let _ = CloseHandle(handle);
                }
            }
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = pid;
    }
}

/// Process IDs of live processes whose parent is `parent` and whose image
/// name is `msedgewebview2.exe`, via a Toolhelp32 snapshot.
///
/// An empty result covers every failure mode (snapshot could not be created,
/// iteration failed) — callers treat "unknown" the same as "none".
#[cfg(windows)]
fn windows_webview_children(parent: u32) -> Vec<u32> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    let mut children = Vec::new();
    unsafe {
        let snapshot = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(handle) => handle,
            Err(_) => return children,
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                if entry.th32ParentProcessID == parent {
                    let name_len = entry
                        .szExeFile
                        .iter()
                        .position(|&unit| unit == 0)
                        .unwrap_or(entry.szExeFile.len());
                    let name = String::from_utf16_lossy(&entry.szExeFile[..name_len]);
                    if name.eq_ignore_ascii_case("msedgewebview2.exe") {
                        children.push(entry.th32ProcessID);
                    }
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    children
}

#[cfg(test)]
mod tests {
    #[test]
    fn release_memory_is_callable() {
        super::release_memory();
        super::release_memory();
    }

    #[cfg(windows)]
    #[test]
    fn webview_child_scan_never_panics() {
        // A dead parent must answer "none" without touching the OS APIs that
        // could fail loudly, and the snapshot walk must be repeatable.
        assert!(super::windows_webview_children(u32::MAX - 1).is_empty());
        let _ = super::windows_webview_children(std::process::id());
    }

    #[cfg(windows)]
    #[test]
    fn kill_webview_processes_is_callable() {
        // Safe on a live process: at most it terminates real msedgewebview2
        // children, which is exactly the production behavior.
        super::kill_webview_processes();
    }
}
