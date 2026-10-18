//! Thin wrapper around [`crate::modal_window::show`] for the
//! terminal "something went wrong" dialog. Keeps the public API
//! (`ErrorDialog`, `InfoIcon`, `show`) intact so callers in
//! [`crate::jdk_install::show_error_dialog`] and
//! [`crate::main::show_launcher_error`] don't change.
//!
//! [`show_launcher_error`] is the public, ergonomic entry point used by
//! both `main.rs` (when a `LauncherError` bubbles out of the platform
//! runtime) and the external `snug_preview` binary (one-click dialog
//! testing). It composes the `[launcher.error]` copy from
//! the localization bundle with a caller-supplied localized error string, then
//! delegates to [`show`].
//!
//! All layout / paint / WndProc logic now lives in
//! [`crate::modal_window`] — this module only resolves the per-dialog
//! TOML fallback for info / button copy (the cross-cutting link-label
//! fallback lives in `modal_window::show`).

#![cfg(windows)]

use windows_sys::Win32::Foundation::HWND;

use crate::modal_window;

/// Re-exported so callers can keep importing `error_window::InfoIcon`
/// — they don't have to migrate to `modal_window::InfoIcon`.
pub use modal_window::InfoIcon;

/// Inputs to [`show`]. Same fields the legacy module exposed; the
/// internal `ModalDialog` in [`modal_window`] is the new
/// representation.
pub struct ErrorDialog<'a> {
    pub title: &'a str,
    pub heading: &'a str,
    pub subheading: &'a str,
    pub error_content: &'a str,
    pub info_icon: InfoIcon,
    /// Optional override for the info-box heading. `None` ⇒
    /// `[runtime.failure].info_heading` from the localization bundle,
    /// falling back to the module default.
    pub info_heading: Option<&'a str>,
    /// Optional override for the info-box subtext. `None` ⇒
    /// `[runtime.failure].info_subtext` from the localization bundle,
    /// falling back to the module default.
    pub info_subtext: Option<&'a str>,
    /// Optional third info-box line. None or empty ⇒ no third
    /// line is painted. Defaults to the localization bundle's
    /// info_subtext_2 when this dialog is built from a group.
    pub info_subtext_2: Option<&'a str>,
    /// Optional override for the button label. `None` ⇒
    /// `[runtime.failure].button_label` from the localization bundle,
    /// falling back to the module default.
    pub button_label: Option<&'a str>,
    /// Optional HBITMAP (cast to `isize`) for the mascot slot. `0` ⇒
    /// fall back to the EXE icon resource.
    pub mascot_hbitmap: isize,
    /// When `Some` and non-empty, a second button is added to the
    /// right of the dismissal button; activating it opens the URL
    /// via `ShellExecuteW(..., "open", url, ...)`. `None`, empty, or
    /// whitespace ⇒ no second button, so a build with no update URL
    /// never offers a control that would do nothing.
    pub update_url: Option<&'a str>,
}

/// Show the error dialog modally. Returns
/// [`modal_window::IDYES_I32`] when the user dismisses via the
/// single button (or presses Enter / X). Preserves the legacy
/// `IDOK_I32 = 1` semantic for callers that compare against `1`.
/// # Safety
///
/// Thin wrapper over [`modal_window::show`]; see its `# Safety` section.
/// In short: call on the dialog-owning thread, with `parent` either
/// `NULL` or a live `HWND` owned by that thread, and with any non-zero
/// `mascot_hbitmap` a live handle that outlives the call.
pub unsafe fn show(parent: HWND, dlg: ErrorDialog<'_>) -> i32 {
    use crate::dialogs::{pick, resolve, DialogCopy};

    let all = crate::dialogs::dialogs();
    let failure = &all.runtime.failure;

    // Resolve the single button label. There is no module default any
    // more: an omitted label falls back to this dialog's own catalog
    // key, which is copy a translator chose rather than copy we
    // invented.
    let button_label = pick(
        dlg.button_label.unwrap_or_default(),
        failure.button_label.as_str(),
    );

    // The update button sits to the *right* of the dismissal button.
    // `Primary` is the rightmost slot in `modal_window`'s button row,
    // so it is the update action and `Secondary` is the dismiss -- which
    // also keeps the two results distinct: `IDYES` opens the browser,
    // `IDCANCEL` (Close / X / Alt+F4) just closes.
    //
    // The old clickable link row above the info box is no longer
    // requested: `link_url` / `link_label` go in as `None` below.
    let update_url = dlg.update_url.map(str::trim).filter(|s| !s.is_empty());
    let mut buttons: Vec<modal_window::Button> =
        Vec::with_capacity(if update_url.is_some() { 2 } else { 1 });
    if update_url.is_some() {
        buttons.push(modal_window::Button::Primary(
            &all.launcher.error.button_update,
        ));
    }
    buttons.push(modal_window::Button::Secondary(button_label));
    if update_url.is_none() {
        // With one button it must be Primary, or a lone button would
        // report `IDCANCEL` and read as a dismissal rather than the
        // dialog's only action.
        buttons.clear();
        buttons.push(modal_window::Button::Primary(button_label));
    }

    unsafe {
        modal_window::show(
            parent,
            &modal_window::ModalDialog {
                title: dlg.title,
                heading: pick(dlg.heading, failure.heading()),
                subheading: pick(dlg.subheading, failure.subheading()),
                content: dlg.error_content,
                info_icon: dlg.info_icon,
                info_heading: resolve(dlg.info_heading, failure.info_heading()),
                info_subtext: resolve(dlg.info_subtext, failure.info_subtext()),
                info_subtext_2: resolve(dlg.info_subtext_2, failure.info_subtext_2()),
                buttons: &buttons,
                mascot_hbitmap: dlg.mascot_hbitmap,
                link_url: None,
                link_label: None,
            },
        )
    }
}

/// Ergonomic public entry point for the launcher-runtime error dialog.
///
/// Composes the `[launcher.error]` copy from the localization bundle with a
/// caller-supplied `localized_error` string (which `main.rs` builds via
/// `error::localize_launcher_error`, but external callers — e.g. the
/// `snug_preview` bin — can pass any already-localized string) and
/// delegates to [`show`].
///
/// `update_check_url`, when `Some` and non-empty, renders the
/// "Check for a newer version: <url>" link row below the info box.
/// `None` skips the row entirely — matches the production path when
/// the embedded payload was unreadable (so the URL field isn't
/// knowable).
///
/// Blocks until the user dismisses the dialog. Returns the same
/// `i32` that [`show`] does — `IDOK_I32` (=1) on primary dismissal.
/// # Safety
///
/// As [`show`]: call on the thread that will own the dialog's message
/// loop, and pass `parent` as `NULL` or a live `HWND` owned by it.
pub unsafe fn show_launcher_error(
    parent: HWND,
    localized_error: &str,
    update_check_url: Option<&str>,
) {
    let dialogs = crate::dialogs::dialogs();
    let content = crate::dialogs::fill(
        dialogs.launcher.error.content.as_str(),
        &[("error", localized_error)],
    );

    // The update-check URL rides the info box like every other dialog's
    // copy: `info_subtext` is the prompt, `info_subtext_2` holds
    // `{update_check_url}` and is filled here. It was previously a separate
    // clickable row below the box, driven by its own catalog key.
    //
    // An absent URL — the production path when the embedded payload was
    // unreadable, so the field is not knowable — leaves the token
    // unresolved, and `fill` would print `{update_check_url}` literally. An
    // empty string renders as an absent third line instead, because
    // `info_subtext_2` is only painted when non-empty.
    let info_subtext_2 = crate::dialogs::fill(
        dialogs.launcher.error.info_subtext_2.as_str(),
        &[("update_check_url", update_check_url.unwrap_or(""))],
    );

    // SAFETY: every borrowed string is live for the duration of the
    // call (dialogs is `&'static`, `content` and `info_subtext_2` are
    // local `String`s). The dialog runs modally and returns before any
    // borrow ends.
    let result = unsafe {
        show(
            parent,
            ErrorDialog {
                title: &dialogs.launcher.error.title,
                heading: &dialogs.launcher.error.heading,
                subheading: &dialogs.launcher.error.subheading,
                error_content: &content,
                info_icon: InfoIcon::Error,
                info_heading: Some(&dialogs.launcher.error.info_heading),
                info_subtext: Some(&dialogs.launcher.error.info_subtext),
                info_subtext_2: Some(&info_subtext_2),
                button_label: Some(&dialogs.launcher.error.button_exit),
                mascot_hbitmap: 0,
                // Same normalisation `show` applies, so the button
                // and the `IDYES` guard below agree on whether there
                // is a URL to open.
                update_url: update_check_url.map(str::trim).filter(|s| !s.is_empty()),
            },
        )
    };

    // `IDYES` is the update button. Without a URL there is no update
    // button and the lone button reports `IDCANCEL`, so this is
    // unreachable — but it is guarded anyway, because "open a
    // browser" is not a thing to trigger on a guess.
    if result == modal_window::IDYES_I32 {
        if let Some(url) = update_check_url.map(str::trim).filter(|s| !s.is_empty()) {
            if let Err(e) = crate::jdk_install::open_in_browser(url) {
                crate::log::log(&format!("error_window: opening update URL failed: {e}"));
            }
        }
    }
}
