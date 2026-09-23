//! User-facing dialog strings.
//!
//! Loaded from `dialogs.toml` at compile time via `include_str!` and
//! parsed once into a `Dialogs` value. Edit the TOML file to change
//! every user-visible string the launcher shows — no Rust
//! recompilation logic needs to change.
//!
//! Callers substitute `{name}` placeholders with [`fill`], passing
//! pre-formatted `&str` values. Format specs (like `{x:.1}`) must
//! be applied to the value in Rust before substitution; the
//! placeholders are matched literally as `{name}`.
//!
//! If the TOML is malformed the launcher panics at first use with a
//! clear error from `toml::de::Error` — fail loud, never silently
//! drop a key.

use std::sync::OnceLock;

use serde::Deserialize;

/// Top-level container. Mirrors the `[...]` table structure of
/// `dialogs.toml` exactly.
#[derive(Debug, Clone, Deserialize)]
pub struct Dialogs {
    pub jdk_install: JdkInstallDialogs,
    pub generic: GenericDialogs,
    pub launcher: LauncherDialogs,
}

/// Launcher-runtime error strings (Java launch failures, JNI errors,
/// Main-Class not found, Java `main` exceptions, etc.). Same physical
/// window as `[jdk_install.failure]` — different copy.
#[derive(Debug, Clone, Deserialize)]
pub struct LauncherDialogs {
    pub error: LauncherErrorDialogs,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LauncherErrorDialogs {
    pub title: String,
    pub heading: String,
    pub subheading: String,
    pub content: String,
    pub info_heading: String,
    pub info_subtext: String,
    pub button_label: String,
    /// Leading label rendered before the optional "Check for a
    /// newer version" link (e.g. `"Check for a newer version:"`).
    /// The URL itself comes from the embedded payload's
    /// `AppMetadata.update_check_url`, not the TOML.
    pub update_check_label: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JdkInstallDialogs {
    pub prompt: InstallPromptDialog,
    pub metadata_failed: MetadataFailedDialog,
    pub progress: ProgressDialog,
    pub success: SuccessDialog,
    pub failure: FailureDialog,
    pub retry: RetryDialog,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InstallPromptDialog {
    pub title: String,
    pub main: String,
    pub content: String,
    pub expanded: String,
    pub button_download: String,
    pub button_open_browser: String,
    pub button_cancel: String,
    pub show_details: String,
    pub hide_details: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MetadataFailedDialog {
    pub title: String,
    /// Heading — large bold line at the top of the dialog body.
    pub heading: String,
    /// Subheading — smaller grey line below the heading.
    pub subheading: String,
    /// Error content — multi-line body text. The launcher's
    /// caller fills placeholders like `{major}` / `{error}`
    /// before showing.
    pub content: String,
    /// Info-box heading line (light-blue box, bottom-left).
    pub info_heading: String,
    /// Info-box subtext (second line in the light-blue box).
    pub info_subtext: String,
    /// Primary button label — opens the Adoptium download page in
    /// the user's browser.
    pub button_open_browser: String,
    /// Secondary button label — dismisses the dialog, install
    /// flow returns `Ok(None)` and the launcher aborts.
    pub button_cancel: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProgressDialog {
    pub title: String,
    pub main: String,
    pub content_initial: String,

    // Strings used by the custom-painted v5 fallback window
    // (`progress_window.rs`). Mirrors the user-facing mockup layout
    // — large heading, subtitle, percent label, phase description,
    // transfer speed + ETA, info-box copy.
    pub heading: String,
    pub subtitle: String,
    pub pct_label: String,
    pub phase_label: String,
    pub detail_with_size: String,
    pub detail_no_size: String,
    pub detail_eta_seconds: String,
    pub detail_eta_second: String,
    pub detail_eta_done: String,
    pub info_heading: String,
    pub info_subtext: String,
    /// Label of the Cancel button while the download phase is
    /// running. Default `"Install"` — the download is part of the
    /// install flow, and labelling the abort button "Install"
    /// signals the user what's about to happen. Swapped to
    /// `prompt.button_cancel` once the verify / extract phases
    /// start.
    pub cancel_button_during_download: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SuccessDialog {
    pub title: String,
    pub main: String,
    pub content: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FailureDialog {
    /// Title-bar text.
    pub title: String,
    /// Heading — large bold line at the top of the dialog body.
    pub heading: String,
    /// Subheading — smaller grey line below the heading.
    pub subheading: String,
    /// Error content — multi-line body text. The launcher's caller
    /// `fill`s placeholders like `{version}` / `{error}` before
    /// passing to the dialog renderer.
    pub content: String,
    /// Info-box heading line (light-blue box, bottom-left).
    pub info_heading: String,
    /// Info-box subtext (second line in the light-blue box).
    pub info_subtext: String,
    /// Single-button label on the dialog. Default `"OK"`.
    pub button_label: String,
}

/// Popped between failed download attempts so the user can choose to
/// retry up to `MAX_DOWNLOAD_ATTEMPTS` times before the terminal
/// `failure` dialog takes over.
#[derive(Debug, Clone, Deserialize)]
pub struct RetryDialog {
    pub title: String,
    /// Heading — large bold line at the top of the dialog body.
    pub heading: String,
    /// Subheading — smaller grey line below the heading.
    pub subheading: String,
    /// Error content — multi-line body text. The launcher fills
    /// placeholders like `{attempt}` / `{max_attempts}` /
    /// `{version}` / `{error}` before showing.
    pub content: String,
    /// Info-box heading line (light-blue box, bottom-left).
    pub info_heading: String,
    /// Info-box subtext (second line in the light-blue box).
    pub info_subtext: String,
    /// Primary button label — retries the install.
    pub button_retry: String,
    /// Secondary button label — aborts the install.
    pub button_cancel: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GenericDialogs {
    pub error_dialog_ok: String,
    pub info_dialog_continue: String,
}

static DIALOGS: OnceLock<Dialogs> = OnceLock::new();

/// Lazily parsed and cached. The first call embeds the TOML via
/// `include_str!`, parses it, and stores it for the lifetime of the
/// process.
pub fn dialogs() -> &'static Dialogs {
    DIALOGS.get_or_init(|| {
        let text = include_str!("../dialogs.toml");
        toml::from_str(text).unwrap_or_else(|e| {
            panic!(
                "crates/snug-launcher/dialogs.toml is malformed: {e}\n\
                 Fix the TOML and rebuild."
            )
        })
    })
}

/// Substitute `{name}` placeholders in `template` with the
/// corresponding values from `subs`. Format specifiers (`{name:.2}`,
/// `{name:>5}` etc.) are not supported — apply them to the values
/// in Rust before calling. Unknown placeholders are left as-is so
/// a typo in the TOML doesn't silently swallow text.
pub fn fill(template: &str, subs: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (key, val) in subs {
        let needle = format!("{{{key}}}");
        out = out.replace(&needle, val);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialogs_parse_cleanly() {
        // Touching the lazy static forces parsing. If the TOML is
        // broken (missing key, bad type, etc.) the `unwrap_or_else`
        // inside `dialogs()` panics with the location of the failure.
        let d = dialogs();
        assert!(!d.jdk_install.prompt.title.is_empty());
        assert!(!d.jdk_install.prompt.button_download.is_empty());
        assert!(d.jdk_install.prompt.content.contains("{version}"));
    }

    #[test]
    fn fill_substitutes_named_placeholders() {
        let s = fill(
            "Downloaded {done_mb} MB of {total_mb} MB ({pct}%)",
            &[("done_mb", "47.2"), ("total_mb", "141.0"), ("pct", "33")],
        );
        assert_eq!(s, "Downloaded 47.2 MB of 141.0 MB (33%)");
    }

    #[test]
    fn fill_leaves_unknown_placeholders_alone() {
        let s = fill("hello {name}, you are {age}", &[("name", "world")]);
        assert_eq!(s, "hello world, you are {age}");
    }
}