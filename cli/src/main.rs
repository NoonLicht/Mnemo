//! Thin wrapper around the library CLI implementation.
//!
//! Kept console-only: no `windows_subsystem` attribute, no Tauri startup.

fn main() {
    std::process::exit(mnemo_lib::cli::run(std::env::args()));
}
