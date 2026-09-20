//! Stub launcher entry point.
//!
//! The compiled binary is reused as `bin/launcher-stub.exe`; the snug CLI
//! embeds it via `include_bytes!()` and appends the encoded payload.
//!
//! At runtime we:
//! 1. Find our own executable path.
//! 2. Scan it for the SNUGEMBD payload.
//! 3. If found, hand off to the platform-specific launcher.
//! 4. If not found (bare stub, no payload appended), emit a friendly
//!    message and exit non-zero.

#![cfg_attr(
    all(windows, not(debug_assertions)),
    windows_subsystem = "windows"
)]

use std::process::ExitCode;

use snug_launcher::platform;
use snug_launcher::LauncherError;

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code as u8),
        Err(err) => {
            eprintln!("snug-launcher: {err}");
            #[cfg(windows)]
            {
                show_error_box(&format!("{err}"));
            }
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<u32, LauncherError> {
    let self_path = std::env::current_exe().map_err(LauncherError::SelfPath)?;

    let payload = match snug_launcher::find_in_file(&self_path)? {
        Some(p) => p,
        None => {
            // Bare stub — no payload appended yet. Be helpful about it.
            eprintln!(
                "snug-launcher: no SNUGEMBD payload found in {}",
                self_path.display()
            );
            eprintln!("This is the bare stub binary. Build it with `snug` to embed a payload:");
            eprintln!("    snug your-app.jar -o app.exe");
            return Ok(2);
        }
    };

    platform::run(&self_path, &payload)
}

#[cfg(windows)]
fn show_error_box(msg: &str) {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_OK,
    };

    let title: Vec<u16> = OsStr::new("snug launcher")
        .encode_wide()
        .chain(Some(0))
        .collect();
    let body: Vec<u16> = OsStr::new(msg).encode_wide().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(std::ptr::null_mut(), body.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR);
    }
}
