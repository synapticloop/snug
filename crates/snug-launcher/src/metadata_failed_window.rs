//! Thin wrapper around [`crate::modal_window::show`] for the
//! "couldn't reach Adoptium" dialog. Keeps the public API
//! (`MetadataFailedDialog`, `IDYES_I32`, `IDCANCEL_I32`, `show`)
//! intact so [`crate::jdk_install::show_metadata_failed_dialog`]
//! doesn't change.
//!
//! All layout / paint / WndProc logic now lives in
//! [`crate::modal_window`], and the shared frame in [`crate::frame`].
//! This module only maps the two-button `MetadataFailedDialog` onto
//! the modal `&[Button]` slice and resolves the caller-copy-or-catalog
//! choice via [`crate::dialogs::resolve`].

#![cfg(windows)]

use windows_sys::Win32::Foundation::HWND;

use crate::modal_window;

/// Re-exported so callers can keep importing
/// `metadata_failed_window::IDYES_I32` and `IDCANCEL_I32`.
pub use modal_window::{IDCANCEL_I32, IDYES_I32};

/// Inputs to [`show`]. Two-button layout — primary (Open in browser)
/// on the left, secondary (Cancel) on the right. Paints a Warning
/// info-icon because metadata failures are typically transient.
pub struct MetadataFailedDialog<'a> {
    pub title: &'a str,
    pub heading: &'a str,
    pub subheading: &'a str,
    pub error_content: &'a str,
    /// Optional override for the info-box heading. `None` ⇒
    /// `[runtime.metadata_failed].info_heading` from the localization
    /// bundle.
    pub info_heading: Option<&'a str>,
    /// Optional override for the info-box subtext. `None` ⇒ the
    /// bundle's `info_subtext`.
    pub info_subtext: Option<&'a str>,
    /// Optional third info-box line, same override-or-catalog
    /// resolution as info_subtext above.
    pub info_subtext_2: Option<&'a str>,
    /// Label for the primary button (left of the pair).
    pub primary_label: &'a str,
    /// Label for the secondary button (rightmost).
    pub secondary_label: &'a str,
    /// Optional HBITMAP (cast to `isize`) for the mascot slot.
    pub mascot_hbitmap: isize,
}

/// Show the dialog modally. Returns [`modal_window::IDYES_I32`]
/// (primary = Open in browser) or [`modal_window::IDCANCEL_I32`]
/// (X / secondary).
/// # Safety
///
/// Thin wrapper over [`modal_window::show`]; see its `# Safety` section.
/// In short: call on the dialog-owning thread, with `parent` either
/// `NULL` or a live `HWND` owned by that thread, and with any non-zero
/// `mascot_hbitmap` a live handle that outlives the call.
pub unsafe fn show(parent: HWND, dlg: MetadataFailedDialog<'_>) -> i32 {
    use crate::dialogs::{dialogs, pick, resolve, DialogCopy};

    let mfd = &dialogs().runtime.metadata_failed;

    let buttons = [
        modal_window::Button::Primary(dlg.primary_label),
        modal_window::Button::Secondary(dlg.secondary_label),
    ];

    unsafe {
        modal_window::show(
            parent,
            &modal_window::ModalDialog {
                title: dlg.title,
                heading: pick(dlg.heading, mfd.heading()),
                subheading: pick(dlg.subheading, mfd.subheading()),
                content: dlg.error_content,
                info_icon: modal_window::InfoIcon::Warning,
                info_heading: resolve(dlg.info_heading, mfd.info_heading()),
                info_subtext: resolve(dlg.info_subtext, mfd.info_subtext()),
                info_subtext_2: resolve(dlg.info_subtext_2, mfd.info_subtext_2()),
                buttons: &buttons,
                mascot_hbitmap: dlg.mascot_hbitmap,
                link_url: None,
                link_label: None,
            },
        )
    }
}
