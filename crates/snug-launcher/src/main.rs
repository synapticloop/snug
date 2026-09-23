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
                show_launcher_error(&err);
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

/// Surface a [`LauncherError`] to the user via the custom-painted
/// `error_window::show` dialog — the same window the JDK-install
/// failure path uses — rather than a bare `MessageBoxW`. The
/// `error_content` slot gets the formatted error; the launcher pulls
/// the optional `update_check_url` from the embedded payload (if it
/// was decodable before the error) and renders it as a clickable
/// "Check for a newer version" link below the info box.
///
/// Two narrow cases fall back to `MessageBoxW`:
/// 1. The error happened *before* the payload was decoded (e.g.
///    `SelfPath`, `Format`) — there's no `AppMetadata` to read a URL
///    from, so we can't paint the link row.
/// 2. The error happened *during* `find_in_file` itself (a corrupt
///    RCDATA resource). Same reason.
///
/// In those cases we keep the old fallback. Everything else
/// (`JvmNotFound`, `JniCreate`, `JniInvoke`, `MainClassNotFound`,
/// `JavaException`, etc.) lands in the rich window.
#[cfg(windows)]
fn show_launcher_error(err: &LauncherError) {
    use snug_format::SnugEmbedded;

    let self_path = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => {
            // Can't even find ourselves — fall back to MessageBoxW.
            show_error_box(&format!("{err}"));
            return;
        }
    };

    // Best-effort payload decode: if we can still read the embedded
    // payload we want the `update_check_url` for the link row. If
    // this fails (e.g. the error happened during decode) we just
    // skip the link and pass `None`.
    let update_url: Option<String> = match snug_launcher::find_in_file(&self_path) {
        Ok(Some(SnugEmbedded { payload, .. })) => payload
            .config
            .app
            .update_check_url
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        _ => None,
    };

    let dialogs = snug_launcher::dialogs::dialogs();
    let content = snug_launcher::dialogs::fill(
        dialogs.launcher.error.content.as_str(),
        &[("error", &format!("{err}"))],
    );

    // SAFETY: `error_window::show` takes a `&str` for the optional
    // `update_check_url`; we pass `None` when the payload didn't yield
    // a usable URL. The window is modal and returns once dismissed.
    let update_url_ref = update_url.as_deref();
    let label_ref: Option<&str> = None;
    unsafe {
        snug_launcher::error_window::show(
            std::ptr::null_mut(),
            snug_launcher::error_window::ErrorDialog {
                title: &dialogs.launcher.error.title,
                heading: &dialogs.launcher.error.heading,
                subheading: &dialogs.launcher.error.subheading,
                error_content: &content,
                info_icon: snug_launcher::error_window::InfoIcon::Error,
                info_heading: Some(&dialogs.launcher.error.info_heading),
                info_subtext: Some(&dialogs.launcher.error.info_subtext),
                button_label: Some(&dialogs.launcher.error.button_label),
                mascot_hbitmap: 0,
                update_check_url: update_url_ref,
                update_check_label: label_ref,
            },
        );
    }
}

/// Plain `MessageBoxW` fallback for the two narrow cases where we
/// can't reach the custom error window — no payload available, or
/// even finding the current EXE failed.
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
