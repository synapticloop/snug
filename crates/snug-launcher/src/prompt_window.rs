//! Custom-painted install prompt. Thin wrapper around
//! [`crate::modal_window::show`] that hard-codes the canonical
//! "Java Runtime Required" dialog (three buttons, mascot, info
//! icon) and concatenates the prompt's `content` + `expanded` body
//! into a single multi-line content area.
//!
//! Replaces the previous `TaskDialogIndirect` + `MessageBoxW`
//! implementation in `jdk_install::Config` /
//! `jdk_install::prompt_messagebox`. By keeping the rendering on
//! the same paint path as `error_window` / `retry_window` /
//! `metadata_failed_window`, the user sees a consistent Snug
//! branded dialog throughout the install flow.
//!
//! The three buttons are right-aligned:
//!
//! ```text
//! ┌────────────────────────────────────────────────────────────┐
//! │ [icon] Java Runtime Required - Snug        - □ ✕           │
//! │                                                            │
//! │  ┌────────────┐  Eclipse Temurin JDK was not found…       │
//! │  │  [mascot]  │  …                                       │
//! │  │            │  [content body…]                          │
//! │  │            │  [expanded body…]                         │
//! │  └────────────┘                                           │
//! │                                                            │
//! │  ┌─────────┐  ┌──────────────────┐  ┌─────────────────┐   │
//! │  │ Cancel  │  │ Open in browser  │  │ Download Temurin│   │
//! │  └─────────┘  └──────────────────┘  └─────────────────┘   │
//! └────────────────────────────────────────────────────────────┘
//! ```
//!
//! The TOML `[jdk_install.prompt.show_details]` /
//! `[jdk_install.prompt.hide_details]` fields are dropped (see
//! the localization bundle): this dialog does not have an expand/collapse
//! toggle — both `content` and `expanded` are always rendered.

#![cfg(windows)]

use windows_sys::Win32::Foundation::HWND;

use crate::dialogs::InstallPromptDialog;
use crate::modal_window;

/// Which action the user took on the install prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptChoice {
	/// User clicked "Download Temurin" / Enter. Launch the
	/// download flow.
	Download,
	/// User clicked Cancel, the X button, or pressed Alt+F4 / Esc.
	Cancel,
}

/// Show the install prompt.
///
/// * `parent` — owning window (may be `std::ptr::null_mut()` when
///   the launcher has no other window yet).
/// * `mascot_hbitmap` — loaded Snug mascot, painted in the
///   upper-left of the dialog. `0` falls back to the EXE main
///   icon. See [`crate::modal_window::ModalDialog::mascot_hbitmap`].
/// * `prompt` — strings from `[jdk_install.prompt]` in
///   the localization bundle.
/// * `version` — replaces `{version}` placeholders in `content`
///   / `expanded` / `button_download`.
/// * `size_mb` — replaces `{size_mb}` in `content` / `expanded`.
/// * `sha256` — replaces `{sha256}` in `expanded`.
///
/// There is deliberately no `url` parameter and no browser button. The
/// prompt used to print a direct download link and offer "Open the
/// download page in my browser", but that path never opened anything —
/// `jdk_install::consent` only tested for `Download`, so the button
/// closed the dialog and stopped. Printing the URL was worse than dead:
/// it read as the supported way to install by hand, on a dialog with
/// no way to act on it.
///
/// Blocks the calling thread until the user dismisses the dialog.
///
/// # Safety
///
/// `parent` must be null or a live window handle; it becomes the
/// dialog's owner HWND in Win32. `mascot_hbitmap`, when non-zero, must
/// be a live HBITMAP, since it is painted into the dialog.
pub unsafe fn show(
	parent: HWND,
	mascot_hbitmap: isize,
	prompt: &InstallPromptDialog,
	version: &str,
	size_mb: &str,
	sha256: &str,
) -> PromptChoice {
	// `content` is the whole body now. It used to be assembled here
	// from a `content` + an `expanded` pair; the baseline carries the
	// digest and the JDK explainer in the one value, joined by literal
	// `\n\n` escapes, so the shape matches every other dialog.
	let content = substitute(&prompt.content, version, size_mb, sha256);

	let download_label = substitute(&prompt.button_download, version, size_mb, sha256);

	// Two buttons. There was a third -- "Open the download page in my
	// browser" -- and it did nothing: `jdk_install::consent` only ever
	// tested for `Download`, so choosing it closed the dialog and
	// neither started the install nor opened anything. The direct URL
	// it referred to went with it. macOS' `appkit::consent` had already
	// dropped that button and offered the same pair.
	let buttons = [
		modal_window::Button::Primary(&download_label),
		modal_window::Button::Secondary(&prompt.button_cancel),
	];

	let result = unsafe {
		modal_window::show(
			parent,
			&modal_window::ModalDialog {
				title: prompt.title.as_str(),
				heading: prompt.heading.as_str(),
				subheading: prompt.subheading.as_str(),
				content: &content,
				// The info box is populated from this dialog's own catalog keys, like
				// every other member of the family. There is no module
				// default any more: what used to fill these `None`s
				// was "What happened?" / "You can try again, or visit
				// the download page manually." -- retry advice about
				// an escape route this dialog does not offer.
				info_icon: modal_window::InfoIcon::Info,
				info_heading: Some(prompt.info_heading.as_str()),
				info_subtext: Some(prompt.info_subtext.as_str()),
				info_subtext_2: Some(prompt.info_subtext_2.as_str()),
				buttons: &buttons,
				mascot_hbitmap,
				link_url: None,
				link_label: None,
			},
		)
	};

	match result {
		modal_window::IDYES_I32 => PromptChoice::Download,
		// The secondary button + X + Alt+F4 all land here.
		_ => PromptChoice::Cancel,
	}
}

/// Cheap placeholder substitution.
///
/// Delegates to [`crate::dialogs::fill`] rather than doing its own
/// replaces, so the three call paths that touch bundle values — this,
/// `dialogs::fill` from the launcher, and `localize::t_positional` —
/// cannot drift apart on what `\{name}` means. It used to be its own
/// pair of `replace` calls, which is how a bundle could render the
/// filled value in a real build and the raw token in the preview.
fn substitute(template: &str, version: &str, size_mb: &str, sha256: &str) -> String {
    crate::dialogs::fill(
        template,
        &[("version", version), ("size_mb", size_mb), ("sha256", sha256)],
    )
}
