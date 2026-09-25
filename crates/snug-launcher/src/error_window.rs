//! Thin wrapper around [`crate::modal_window::show`] for the
//! terminal "something went wrong" dialog. Keeps the public API
//! (`ErrorDialog`, `InfoIcon`, `show`) intact so callers in
//! [`crate::jdk_install::show_error_dialog`] and
//! [`crate::main::show_launcher_error`] don't change.
//!
//! All layout / paint / WndProc logic now lives in
//! [`crate::modal_window`] — this module only resolves the per-dialog
//! TOML fallback for info / button copy (the cross-cutting link-label
//! fallback lives in `modal_window::show`).

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
    /// `[jdk_install.failure].info_heading` from `dialogs.toml`,
    /// falling back to the module default.
    pub info_heading: Option<&'a str>,
    /// Optional override for the info-box subtext. `None` ⇒
    /// `[jdk_install.failure].info_subtext` from `dialogs.toml`,
    /// falling back to the module default.
    pub info_subtext: Option<&'a str>,
    /// Optional override for the button label. `None` ⇒
    /// `[jdk_install.failure].button_label` from `dialogs.toml`,
    /// falling back to the module default.
    pub button_label: Option<&'a str>,
    /// Optional HBITMAP (cast to `isize`) for the mascot slot. `0` ⇒
    /// fall back to the EXE icon resource.
    pub mascot_hbitmap: isize,
    /// Optional "Check for a newer version" URL. When `Some` and
    /// non-empty, the dialog paints a clickable link below the info
    /// box that opens via `ShellExecuteW(..., "open", url, ...)`.
    /// `None` or empty → the link row is hidden.
    pub update_check_url: Option<&'a str>,
    /// Optional override for the label rendered before the URL
    /// (e.g. `"Check for a newer version:"`). `None` or empty ⇒
    /// the TOML `[launcher.error].update_check_label` (resolved
    /// inside [`modal_window::show`]).
    pub update_check_label: Option<&'a str>,
}

/// Show the error dialog modally. Returns
/// [`modal_window::IDYES_I32`] when the user dismisses via the
/// single button (or presses Enter / X). Preserves the legacy
/// `IDOK_I32 = 1` semantic for callers that compare against `1`.
pub unsafe fn show(parent: HWND, dlg: ErrorDialog<'_>) -> i32 {
    let dialogs = crate::dialogs::dialogs();
    let failure = &dialogs.jdk_install.failure;

    // Resolve per-dialog info / button fallback (caller → TOML →
    // module default). `modal_window::show` handles an empty `None`
    // by using its own defaults; here we substitute TOML-derived
    // values up-front so the per-dialog copy from
    // `[jdk_install.failure]` is what the user actually sees.
    let info_heading = dlg
        .info_heading
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            if failure.info_heading.is_empty() {
                None
            } else {
                Some(failure.info_heading.clone())
            }
        });
    let info_subtext = dlg
        .info_subtext
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            if failure.info_subtext.is_empty() {
                None
            } else {
                Some(failure.info_subtext.clone())
            }
        });
    let button_label: Option<String> = dlg
        .button_label
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            if failure.button_label.is_empty() {
                None
            } else {
                Some(failure.button_label.clone())
            }
        });

    // Heading / subheading use the TOML value when the caller
    // passes an empty string (the existing convention); content is
    // always caller-supplied.
    let heading = if dlg.heading.is_empty() {
        failure.heading.as_str()
    } else {
        dlg.heading
    };
    let subheading = if dlg.subheading.is_empty() {
        failure.subheading.as_str()
    } else {
        dlg.subheading
    };

    // Resolve the single button label once. `unwrap_or_else` keeps
    // the lifetimes independent — the `&'static str` fallback is
    // only constructed when needed, so the resolved label can have
    // either the caller's lifetime (when present) or `'static` (when
    // falling back to the module default).
    let button_label_str: &str = match button_label.as_deref() {
        Some(s) => s,
        None => modal_window::default_button_label(),
    };
    let button = modal_window::Button::Primary(button_label_str);

    unsafe {
        modal_window::show(
            parent,
            &modal_window::ModalDialog {
                title: dlg.title,
                heading,
                subheading,
                content: dlg.error_content,
                info_icon: dlg.info_icon,
                info_heading: info_heading.as_deref(),
                info_subtext: info_subtext.as_deref(),
                buttons: std::slice::from_ref(&button),
                mascot_hbitmap: dlg.mascot_hbitmap,
                link_url: dlg.update_check_url,
                link_label: dlg.update_check_label,
            },
        )
    }
}