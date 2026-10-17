//! Thin wrapper around [`crate::modal_window::show`] for the
//! "download failed, try again?" dialog. Keeps the public API
//! (`RetryDialog`, `IDYES_I32`, `IDCANCEL_I32`, `show`) intact so
//! [`crate::jdk_install::show_retry_dialog`] doesn't change.
//!
//! All layout / paint / WndProc logic now lives in
//! [`crate::modal_window`], and the shared frame in [`crate::frame`].
//! This module only maps the two-button `RetryDialog` onto the modal
//! `&[Button]` slice and resolves the caller-copy-or-catalog choice
//! via [`crate::dialogs::resolve`].

#![cfg(windows)]

use windows_sys::Win32::Foundation::HWND;

use crate::modal_window;

/// Re-exported so callers can keep importing `retry_window::IDYES_I32`
/// and `retry_window::IDCANCEL_I32`.
pub use modal_window::{IDCANCEL_I32, IDYES_I32};

/// Inputs to [`show`]. Two-button layout — primary (Try again) on
/// the left, secondary (Cancel) on the right.
pub struct RetryDialog<'a> {
    pub title: &'a str,
    pub heading: &'a str,
    pub subheading: &'a str,
    pub error_content: &'a str,
    pub info_heading: Option<&'a str>,
    pub info_subtext: Option<&'a str>,
    /// Optional third info-box line, same override-or-catalog
    /// resolution as info_subtext above.
    pub info_subtext_2: Option<&'a str>,
    pub primary_label: &'a str,
    pub secondary_label: &'a str,
    pub mascot_hbitmap: isize,
}

/// Show the dialog modally. Returns [`modal_window::IDYES_I32`]
/// (primary = Try again) or [`modal_window::IDCANCEL_I32`] (X /
/// secondary).
/// # Safety
///
/// Thin wrapper over [`modal_window::show`]; see its `# Safety` section.
/// In short: call on the dialog-owning thread, with `parent` either
/// `NULL` or a live `HWND` owned by that thread, and with any non-zero
/// `mascot_hbitmap` a live handle that outlives the call.
pub unsafe fn show(parent: HWND, dlg: RetryDialog<'_>) -> i32 {
    use crate::dialogs::{dialogs, pick, resolve, DialogCopy};

    let retry = &dialogs().runtime.retry;

    let buttons = [
        modal_window::Button::Primary(dlg.primary_label),
        modal_window::Button::Secondary(dlg.secondary_label),
    ];

    unsafe {
        modal_window::show(
            parent,
            &modal_window::ModalDialog {
                title: dlg.title,
                heading: pick(dlg.heading, retry.heading()),
                subheading: pick(dlg.subheading, retry.subheading()),
                content: dlg.error_content,
                info_icon: modal_window::InfoIcon::Error,
                info_heading: resolve(dlg.info_heading, retry.info_heading()),
                info_subtext: resolve(dlg.info_subtext, retry.info_subtext()),
                info_subtext_2: resolve(dlg.info_subtext_2, retry.info_subtext_2()),
                buttons: &buttons,
                mascot_hbitmap: dlg.mascot_hbitmap,
                link_url: None,
                link_label: None,
            },
        )
    }
}
