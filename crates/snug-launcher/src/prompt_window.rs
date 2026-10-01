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
//! │ [icon] Java Runtime Required — Snug        — □ ✕           │
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

use windows_sys::Win32::Foundation::HWND;

use crate::dialogs::InstallPromptDialog;
use crate::modal_window;

/// Which action the user took on the install prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptChoice {
	/// User clicked "Download Temurin" / Enter. Launch the
	/// download flow.
	Download,
	/// User clicked "Open the download page in my browser".
	OpenBrowser,
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
/// * `url` — replaces `{url}` in `expanded`.
/// * `sha256` — replaces `{sha256}` in `expanded`.
///
/// Blocks the calling thread until the user dismisses the dialog.
pub fn show(
	parent: HWND,
	mascot_hbitmap: isize,
	prompt: &InstallPromptDialog,
	version: &str,
	size_mb: u32,
	url: &str,
	sha256: &str,
) -> PromptChoice {
	let size_mb_str = size_mb.to_string();

	// Substitute placeholders in `content` and `expanded`, then
	// concatenate. The expanded body has no toggle — it's always
	// visible. Newlines in the TOML `"""..."""` strings are
	// preserved verbatim.
	let content = format!(
		"{}\n\n{}",
		substitute(&prompt.content, version, &size_mb_str, url, sha256),
		substitute(&prompt.expanded, version, &size_mb_str, url, sha256),
	);

	let download_label = substitute(&prompt.button_download, version, &size_mb_str, url, sha256);

	let buttons = [
		modal_window::Button::Primary(&download_label),
		modal_window::Button::Secondary(&prompt.button_open_browser),
		modal_window::Button::Tertiary(&prompt.button_cancel),
	];

	let result = unsafe {
		modal_window::show(
			parent,
			&modal_window::ModalDialog {
				title: prompt.title.as_str(),
				heading: prompt.main.as_str(),
				subheading: "",
				content: &content,
				info_icon: modal_window::InfoIcon::Warning,
				info_heading: None,
				info_subtext: None,
				buttons: &buttons,
				mascot_hbitmap,
				link_url: None,
				link_label: None,
			},
		)
	};

	match result {
		modal_window::IDYES_I32 => PromptChoice::Download,
		modal_window::IDNO_I32 => PromptChoice::OpenBrowser,
		// `IDCANCEL_I32` covers the tertiary button + X + Alt+F4.
		_ => PromptChoice::Cancel,
	}
}

/// Cheap placeholder substitution. Avoids pulling in `format!` for
/// each placeholder so the call site stays readable.
fn substitute(template: &str, version: &str, size_mb: &str, url: &str, sha256: &str) -> String {
	template
		.replace("{version}", version)
		.replace("{size_mb}", size_mb)
		.replace("{url}", url)
		.replace("{sha256}", sha256)
}
