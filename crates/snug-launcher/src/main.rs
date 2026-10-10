//! Stub launcher entry point.
//!
//! The compiled binary is reused as `bin/launcher-stub-windows-x86_64.exe`; the snug CLI
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

#[cfg(windows)]
use snug_launcher::error;
use snug_launcher::localize;
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

    let payload = match platform::locate_payload(&self_path)? {
        Some(p) => p,
        None => {
            // Bare stub — no payload appended yet. Be helpful about it.
            // Localized keys are used here too — the built-in English
            // baseline is always available via `localize::lookup`,
            // even on the bare-stub path where the payload is absent.
            let path_str = self_path.display().to_string();
            eprintln!("snug-launcher: {}", localize::t("launcher.bare_stub.no_payload", &[("path", &path_str)]));
            eprintln!("{}", localize::lookup("launcher.bare_stub.hint"));
            eprintln!("    {}", localize::lookup("launcher.bare_stub.command"));
            return Ok(2);
        }
    };

    // Wire up the localization lookup chain once the payload is in
    // hand. From here on every `localize::t(...)` / `localize::lookup`
    // call resolves against the merged user + built-in bundle list.
    localize::init(&payload.payload.localizations);

    // Stash the update-check URL now, while the payload is decoded and in
    // hand. `show_launcher_error` used to re-run `find_in_file` to get it,
    // which re-read the EXE and re-decoded the whole payload -- every JAR
    // byte copied into fresh Vecs -- purely to read one string. On a 300 MB
    // fat jar that re-reads 300 MB and roughly doubles peak memory, on
    // precisely the path where the machine is already struggling.
    //
    // Left unset when the payload never decoded, which is exactly the set of
    // cases where the dialog has no URL to show: `SelfPath` / `Format`
    // errors, a bare stub, a corrupt RCDATA resource. `show_launcher_error`
    // falls back to `None` there, as it always did.
    let _ = UPDATE_CHECK_URL.set(update_check_url(&payload.payload));

    platform::run(&self_path, &payload)
}

/// The payload's `update_check_url`, trimmed, with an empty value treated
/// as absent.
fn update_check_url(payload: &snug_format::SnugPayload) -> Option<String> {
    payload
        .config
        .app
        .update_check_url
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Set once, immediately after a successful payload decode. `None` means
/// "never decoded", not "decoded but has no URL" -- the two look identical
/// to the error dialog, and both render without the link row.
static UPDATE_CHECK_URL: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

/// Surface a [`LauncherError`] to the user via the custom-painted
/// `error_window::show_launcher_error` dialog — the same window the
/// JDK-install failure path uses — rather than a bare `MessageBoxW`.
/// The launcher pulls the optional `update_check_url` from the
/// embedded payload (if it was decodable before the error) and
/// renders it as a clickable "Check for a newer version" link below
/// the info box.
///
/// The dialog body uses [`error::localize_launcher_error`] so the
/// error text is in the user's locale. The rest of the chrome (title,
/// heading, info-box copy, button label) comes from the same bundle
/// via [`crate::dialogs::dialogs`], so a `--localization <tag>` build
/// localizes the whole window rather than just its contents.
///
/// Two narrow cases have no update-check URL to offer, because the payload
/// never decoded: the error happened *before* `locate_payload` succeeded
/// (`SelfPath`, `Format`), or *during* it (a corrupt RCDATA resource).
/// Those still get the full window, just without the link row.
///
/// There used to be a stock `MessageBoxW` fallback for exactly those cases,
/// reachable only because reading the URL required resolving our own path.
/// The URL is now captured at decode time, so there is nothing left that
/// needs the path -- and no reason to degrade the window when it is
/// unavailable. `show_error_box` went with it.
#[cfg(windows)]
fn show_launcher_error(err: &LauncherError) {
    // The update-check URL, captured once at decode time. `None` covers both
    // "the payload never decoded" and "it had no URL"; either way there is
    // nothing to render in the link row, and neither is worth re-reading
    // 300 MB to discover.
    let update_url: Option<String> = UPDATE_CHECK_URL.get().cloned().flatten();

    let localized_error = error::localize_launcher_error(err);
    let update_url_ref = update_url.as_deref();

    // SAFETY: delegating to the public wrapper — `error_window` now
    // owns the `dialogs::fill` + `ErrorDialog` construction. The
    // window is modal and returns before any borrow ends. No parent
    // HWND here (we're the entry point of a console-style binary
    // that just lost its payload); the dialog renders as a free
    // topmost window, same as before.
    unsafe {
        snug_launcher::error_window::show_launcher_error(
            std::ptr::null_mut(),
            &localized_error,
            update_url_ref,
        );
    }
}
