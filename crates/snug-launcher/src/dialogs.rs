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
    pub main: String,
    pub content: String,
    pub button_open_browser: String,
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
    pub title: String,
    pub content: String,
    pub button_ok: String,
    pub button_continue: String,
}

/// Popped between failed download attempts so the user can choose to
/// retry up to `MAX_DOWNLOAD_ATTEMPTS` times before the terminal
/// `failure` dialog takes over.
#[derive(Debug, Clone, Deserialize)]
pub struct RetryDialog {
    pub title: String,
    pub main: String,
    pub content: String,
    pub button_retry: String,
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