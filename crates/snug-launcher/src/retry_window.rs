//! Thin wrapper around [`crate::modal_window::show`] for the
//! "download failed, try again?" dialog. Keeps the public API
//! (`RetryDialog`, `IDYES_I32`, `IDCANCEL_I32`, `show`) intact so
//! [`crate::jdk_install::show_retry_dialog`] doesn't change.
//!
//! All layout / paint / WndProc logic now lives in
//! [`crate::modal_window`] — this module only maps the two-button
//! `RetryDialog` onto the modal `&[Button]` slice and resolves the
//! per-dialog TOML fallback for info copy.

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
    pub primary_label: &'a str,
    pub secondary_label: &'a str,
    pub mascot_hbitmap: isize,
}

/// Show the dialog modally. Returns [`modal_window::IDYES_I32`]
/// (primary = Try again) or [`modal_window::IDCANCEL_I32`] (X /
/// secondary).
pub unsafe fn show(parent: HWND, dlg: RetryDialog<'_>) -> i32 {
    let dialogs = crate::dialogs::dialogs();
    let retry = &dialogs.jdk_install.retry;

    let info_heading = dlg
        .info_heading
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            if retry.info_heading.is_empty() {
                None
            } else {
                Some(retry.info_heading.clone())
            }
        });
    let info_subtext = dlg
        .info_subtext
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            if retry.info_subtext.is_empty() {
                None
            } else {
                Some(retry.info_subtext.clone())
            }
        });

    let heading = if dlg.heading.is_empty() {
        retry.heading.as_str()
    } else {
        dlg.heading
    };
    let subheading = if dlg.subheading.is_empty() {
        retry.subheading.as_str()
    } else {
        dlg.subheading
    };

    let buttons = [
        modal_window::Button::Primary(dlg.primary_label),
        modal_window::Button::Secondary(dlg.secondary_label),
    ];

    unsafe {
        modal_window::show(
            parent,
            &modal_window::ModalDialog {
                title: dlg.title,
                heading,
                subheading,
                content: dlg.error_content,
                info_icon: modal_window::InfoIcon::Error,
                info_heading: info_heading.as_deref(),
                info_subtext: info_subtext.as_deref(),
                buttons: &buttons,
                mascot_hbitmap: dlg.mascot_hbitmap,
                link_url: None,
                link_label: None,
            },
        )
    }
}