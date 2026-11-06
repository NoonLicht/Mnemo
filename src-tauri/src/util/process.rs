use std::process::Command;

/// Prevents a console window from flashing on screen when spawning a
/// console application (tesseract, pdftoppm, external extractors) on
/// Windows. On other platforms this is a no-op.
pub fn hide_console(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}
