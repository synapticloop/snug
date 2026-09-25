//! Thin wrapper around [`crate::modal_window::show`] for the
//! "couldn't reach Adoptium" dialog. Keeps the public API
//! (`MetadataFailedDialog`, `IDYES_I32`, `IDCANCEL_I32`, `show`)
//! intact so [`crate::jdk_install::show_metadata_failed_dialog`]
//! doesn't change.
//!
//! All layout / paint / WndProc logic now lives in
//! [`crate::modal_window`] — this module only maps the two-button
//! `MetadataFailedDialog` onto the modal `&[Button]` slice and
//! resolves the per-dialog TOML fallback for info copy.

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
    /// `[jdk_install.metadata_failed].info_heading` from
    /// `dialogs.toml`, falling back to the module default.
    pub info_heading: Option<&'a str>,
    /// Optional override for the info-box subtext. `None` ⇒ TOML,
    /// falling back to the module default.
    pub info_subtext: Option<&'a str>,
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
pub unsafe fn show(parent: HWND, dlg: MetadataFailedDialog<'_>) -> i32 {
    let dialogs = crate::dialogs::dialogs();
    let mfd = &dialogs.jdk_install.metadata_failed;

    let info_heading = dlg
        .info_heading
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            if mfd.info_heading.is_empty() {
                None
            } else {
                Some(mfd.info_heading.clone())
            }
        });
    let info_subtext = dlg
        .info_subtext
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            if mfd.info_subtext.is_empty() {
                None
            } else {
                Some(mfd.info_subtext.clone())
            }
        });

    let heading = if dlg.heading.is_empty() {
        mfd.heading.as_str()
    } else {
        dlg.heading
    };
    let subheading = if dlg.subheading.is_empty() {
        mfd.subheading.as_str()
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
                info_icon: modal_window::InfoIcon::Warning,
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