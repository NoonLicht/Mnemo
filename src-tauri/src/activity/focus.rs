//! Foreground-window tracking on Windows via `SetWinEventHook` (SPEC §5.3). Events are
//! delivered out of context to a dedicated thread that only pumps messages, so there is no
//! polling and no CPU use while the foreground window stays the same.

use std::path::Path;
use std::sync::Mutex;
use std::sync::mpsc::Sender;

use windows_sys::Win32::Foundation::{CloseHandle, HWND};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EVENT_OBJECT_NAMECHANGE, EVENT_SYSTEM_FOREGROUND, GetForegroundWindow, GetMessageW,
    GetWindowTextW, GetWindowThreadProcessId, MSG, OBJID_WINDOW, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    /// Executable file name, e.g. `Code.exe`.
    pub app: String,
    pub title: String,
}

static SINK: Mutex<Option<Sender<Sample>>> = Mutex::new(None);

fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    // SAFETY: `buf` is valid for `buf.len()` UTF-16 units.
    let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn window_app(hwnd: HWND) -> Option<String> {
    let mut pid = 0u32;
    // SAFETY: `pid` is a valid out-pointer.
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid == 0 {
        return None;
    }
    // SAFETY: plain query-only process handle, closed below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return None;
    }
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `buf`/`len` describe a valid buffer; `h` is a valid handle.
    let ok = unsafe { QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) } != 0;
    // SAFETY: `h` was opened above and is not used afterwards.
    unsafe { CloseHandle(h) };
    if !ok {
        return None;
    }
    let full = String::from_utf16_lossy(&buf[..len as usize]);
    Path::new(&full)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

fn sample_of(hwnd: HWND) -> Option<Sample> {
    Some(Sample {
        app: window_app(hwnd)?,
        title: window_title(hwnd),
    })
}

/// The window in front right now, if it can be identified.
pub fn foreground_sample() -> Option<Sample> {
    // SAFETY: no arguments.
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() {
        None
    } else {
        sample_of(hwnd)
    }
}

unsafe extern "system" fn on_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if hwnd.is_null() || id_object != OBJID_WINDOW {
        return;
    }
    // Title changes matter only for the window the user is looking at.
    // SAFETY: no arguments.
    if event == EVENT_OBJECT_NAMECHANGE && hwnd != unsafe { GetForegroundWindow() } {
        return;
    }
    if let Some(sample) = sample_of(hwnd)
        && let Some(tx) = SINK.lock().unwrap().as_ref()
    {
        let _ = tx.send(sample);
    }
}

/// Starts the hook thread. Samples arrive on `tx` for every foreground or title change.
pub fn spawn(tx: Sender<Sample>) -> std::io::Result<std::thread::JoinHandle<()>> {
    *SINK.lock().unwrap() = Some(tx);
    std::thread::Builder::new()
        .name("activity-focus".into())
        .spawn(|| {
            let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
            // SAFETY: `on_event` has the required signature and stays valid for the process lifetime.
            unsafe {
                SetWinEventHook(
                    EVENT_SYSTEM_FOREGROUND,
                    EVENT_SYSTEM_FOREGROUND,
                    std::ptr::null_mut(),
                    Some(on_event),
                    0,
                    0,
                    flags,
                );
                SetWinEventHook(
                    EVENT_OBJECT_NAMECHANGE,
                    EVENT_OBJECT_NAMECHANGE,
                    std::ptr::null_mut(),
                    Some(on_event),
                    0,
                    0,
                    flags,
                );
            }
            let mut msg: MSG = unsafe { std::mem::zeroed() };
            // Blocks until a message arrives; the loop only exists to let Windows call our hook.
            // SAFETY: `msg` is a valid out-pointer.
            while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {}
        })
}
