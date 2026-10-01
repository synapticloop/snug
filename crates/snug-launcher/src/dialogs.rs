//! User-facing dialog strings.
//!
//! Sourced from the localization bundle — the same chain the error
//! strings use — rather than a dedicated file. The strings live in
//! `snug-format/assets/snug-localisations.en.txt` under
//! `jdk_install.*`, `generic.*` and `launcher.error.*` keys, which
//! means a `--localization <tag>` bundle translates the whole window
//! rather than just the error text inside it.
//!
//! The [`Dialogs`] struct itself is unchanged and still what every
//! caller reads: [`dialogs`] *assembles* it out of
//! [`crate::localize::lookup`] calls instead of deserializing it.
//! That is deliberate. Flat localization keys are bare string
//! literals, and a typo in one resolves to the key itself at runtime
//! — loud in the UI, but late. Keeping the struct preserves
//! compile-checked field access at every call site
//! (`dialogs().jdk_install.prompt.title`), so key literals appear in
//! exactly one place — this constructor — and the
//! `every_localize_key_is_in_the_baseline` test in
//! [`crate::localize`] checks them against the baseline.
//!
//! Placeholders use `{name}` syntax, substituted by [`fill`] with
//! pre-formatted `&str` values. Format specs (like `{x:.1}`) must be
//! applied to the value in Rust before substitution; placeholders are
//! matched literally.

use std::sync::RwLock;

/// Top-level container. Field names mirror the key prefixes in the
/// localization catalog one-for-one — `dialogs.jdk_install.prompt`
/// is `jdk_install.prompt.*`, and so on.
#[derive(Debug, Clone)]
pub struct Dialogs {
    pub jdk_install: JdkInstallDialogs,
    pub generic: GenericDialogs,
    pub launcher: LauncherDialogs,
}

/// Launcher-runtime error strings (Java launch failures, JNI errors,
/// Main-Class not found, Java `main` exceptions, etc.). Same physical
/// window as `[jdk_install.failure]` — different copy.
#[derive(Debug, Clone)]
pub struct LauncherDialogs {
    pub error: LauncherErrorDialogs,
}

#[derive(Debug, Clone)]
pub struct LauncherErrorDialogs {
    pub title: String,
    pub heading: String,
    pub subheading: String,
    pub content: String,
    pub info_heading: String,
    pub info_subtext: String,
    /// Third line in the light-blue info box. Rendered only when
    /// non-empty, so a dialog can opt in without padding.
    pub info_subtext_2: String,
    pub button_label: String,
    /// Leading label rendered before the optional "Check for a
    /// newer version" link (e.g. `"Check for a newer version:"`).
    /// The URL itself comes from the embedded payload's
    /// `AppMetadata.update_check_url`, not the TOML.
    pub update_check_label: String,
}

#[derive(Debug, Clone)]
pub struct JdkInstallDialogs {
    pub prompt: InstallPromptDialog,
    pub metadata_failed: MetadataFailedDialog,
    pub progress: ProgressDialog,
    pub failure: FailureDialog,
    pub retry: RetryDialog,
}

#[derive(Debug, Clone)]
pub struct InstallPromptDialog {
    pub title: String,
    pub main: String,
    pub content: String,
    pub expanded: String,
    pub button_download: String,
    pub button_open_browser: String,
    pub button_cancel: String,
}

#[derive(Debug, Clone)]
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
    /// Third line in the light-blue info box. Rendered only when
    /// non-empty, so a dialog can opt in without padding.
    pub info_subtext_2: String,
    /// Primary button label — opens the Adoptium download page in
    /// the user's browser.
    pub button_open_browser: String,
    /// Secondary button label — dismisses the dialog, install
    /// flow returns `Ok(None)` and the launcher aborts.
    pub button_cancel: String,
}

#[derive(Debug, Clone)]
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
    /// Third line in the light-blue info box. Rendered only when
    /// non-empty, so a dialog can opt in without padding.
    pub info_subtext_2: String,
    /// Label of the Cancel button while the download phase is
    /// running. Default `"Install"` — the download is part of the
    /// install flow, and labelling the abort button "Install"
    /// signals the user what's about to happen. Swapped to
    /// `prompt.button_cancel` once the verify / extract phases
    /// start.
    pub cancel_button_during_download: String,
}

#[derive(Debug, Clone)]
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
    /// Third line in the light-blue info box. Rendered only when
    /// non-empty, so a dialog can opt in without padding.
    pub info_subtext_2: String,
    /// Single-button label on the dialog. Default `"OK"`.
    pub button_label: String,
}

/// Popped between failed download attempts so the user can choose to
/// retry up to `MAX_DOWNLOAD_ATTEMPTS` times before the terminal
/// `failure` dialog takes over.
#[derive(Debug, Clone)]
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
    /// Third line in the light-blue info box. Rendered only when
    /// non-empty, so a dialog can opt in without padding.
    pub info_subtext_2: String,
    /// Primary button label — retries the install.
    pub button_retry: String,
    /// Secondary button label — aborts the install.
    pub button_cancel: String,
}

#[derive(Debug, Clone)]
pub struct GenericDialogs {
    pub error_dialog_ok: String,
    pub info_dialog_continue: String,
}

/// The assembled copy plus the [`crate::localize::generation`] it was
/// built from.
///
/// The generation is the invalidation key: when `localize::set_bundles`
/// swaps the active language, every field in here is stale, so the
/// cached entry is rebuilt. Without it the preview's language dropdown
/// would switch the chain and every dialog would keep rendering the
/// previous language.
static DIALOGS: RwLock<Option<(u64, &'static Dialogs)>> = RwLock::new(None);

/// Assemble and cache the dialog copy for the lifetime of the process.
///
/// Each field is one `localize::lookup` against the bundle chain, so
/// the first call after `crate::localize::init` picks up the user's
/// `--localization` bundles and the built-in English baseline backs
/// every key they don't cover.
///
/// A key that resolves nowhere comes back as the key string itself
/// (that's [`crate::localize`]'s deliberate "loud but non-fatal"
/// contract), which would paint raw dotted text into a window. The
/// `every_localize_key_is_in_the_baseline` test in
/// [`crate::localize`] is what keeps that from shipping.
///
/// Every dialog is shown from [`crate::platform::run`] or later, which
/// runs *after* `localize::init` — so a `--localization` build resolves
/// translated chrome. Even if something reached this earlier, the
/// `OnceLock` would cache whatever it saw, which is why the
/// pre-`init` path in [`crate::localize::lookup`] falls back to the
/// compiled-in English baseline rather than handing back bare keys:
/// a cached `jdk_install.prompt.title` would be a permanently broken
/// window, and a cached English string is merely untranslated.
pub fn dialogs() -> &'static Dialogs {
    let gen_id = crate::localize::generation();

    // Fast path: generation unchanged, so the cached copy is current.
    {
        let guard = DIALOGS.read().unwrap_or_else(|e| e.into_inner());
        if let Some((cached_gen, dialogs)) = *guard {
            if cached_gen == gen_id {
                return dialogs;
            }
        }
    }

    // Slow path: reassemble. Re-check under the write lock so two
    // threads racing the first call don't both build.
    let mut guard = DIALOGS.write().unwrap_or_else(|e| e.into_inner());
    if let Some((cached_gen, dialogs)) = *guard {
        if cached_gen == gen_id {
            return dialogs;
        }
    }
    // Leaked deliberately: the signature is `&'static Dialogs`, which
    // keeps all ~16 call sites compiling untouched, and a rebuild only
    // happens when the language actually changes. That bounds the leak
    // to "number of language switches × one `Dialogs`" — a handful of
    // kilobytes across a whole preview session. The alternative was
    // changing every call site to take an owned `Dialogs` (or a guard
    // type) to reclaim a few KB that only a dev tool ever leaks.
    let built: &'static Dialogs = Box::leak(Box::new(assemble()));
    *guard = Some((gen_id, built));
    built
}

/// Build a `Dialogs` from the current bundle chain.
fn assemble() -> Dialogs {
    use crate::localize::lookup;
    Dialogs {
            jdk_install: JdkInstallDialogs {
                prompt: InstallPromptDialog {
                    title: lookup("jdk_install.prompt.title"),
                    main: lookup("jdk_install.prompt.main"),
                    content: lookup("jdk_install.prompt.content"),
                    expanded: lookup("jdk_install.prompt.expanded"),
                    button_download: lookup("jdk_install.prompt.button_download"),
                    button_open_browser: lookup("jdk_install.prompt.button_open_browser"),
                    button_cancel: lookup("jdk_install.prompt.button_cancel"),
                },
                metadata_failed: MetadataFailedDialog {
                    title: lookup("jdk_install.metadata_failed.title"),
                    heading: lookup("jdk_install.metadata_failed.heading"),
                    subheading: lookup("jdk_install.metadata_failed.subheading"),
                    content: lookup("jdk_install.metadata_failed.content"),
                    info_heading: lookup("jdk_install.metadata_failed.info_heading"),
                    info_subtext: lookup("jdk_install.metadata_failed.info_subtext"),
                    info_subtext_2: lookup("jdk_install.metadata_failed.info_subtext_2"),
                    button_open_browser: lookup("jdk_install.metadata_failed.button_open_browser"),
                    button_cancel: lookup("jdk_install.metadata_failed.button_cancel"),
                },
                progress: ProgressDialog {
                    title: lookup("jdk_install.progress.title"),
                    main: lookup("jdk_install.progress.main"),
                    content_initial: lookup("jdk_install.progress.content_initial"),
                    heading: lookup("jdk_install.progress.heading"),
                    subtitle: lookup("jdk_install.progress.subtitle"),
                    pct_label: lookup("jdk_install.progress.pct_label"),
                    phase_label: lookup("jdk_install.progress.phase_label"),
                    detail_with_size: lookup("jdk_install.progress.detail_with_size"),
                    detail_no_size: lookup("jdk_install.progress.detail_no_size"),
                    detail_eta_seconds: lookup("jdk_install.progress.detail_eta_seconds"),
                    detail_eta_second: lookup("jdk_install.progress.detail_eta_second"),
                    detail_eta_done: lookup("jdk_install.progress.detail_eta_done"),
                    info_heading: lookup("jdk_install.progress.info_heading"),
                    info_subtext: lookup("jdk_install.progress.info_subtext"),
                    info_subtext_2: lookup("jdk_install.progress.info_subtext_2"),
                    cancel_button_during_download: {
                        lookup("jdk_install.progress.cancel_button_during_download")
                    },
                },
                failure: FailureDialog {
                    title: lookup("jdk_install.failure.title"),
                    heading: lookup("jdk_install.failure.heading"),
                    subheading: lookup("jdk_install.failure.subheading"),
                    // Intentionally empty in the baseline: `error_window`
                    // treats an empty body as "use the module default".
                    content: lookup("jdk_install.failure.content"),
                    info_heading: lookup("jdk_install.failure.info_heading"),
                    info_subtext: lookup("jdk_install.failure.info_subtext"),
                    info_subtext_2: lookup("jdk_install.failure.info_subtext_2"),
                    button_label: lookup("jdk_install.failure.button_label"),
                },
                retry: RetryDialog {
                    title: lookup("jdk_install.retry.title"),
                    heading: lookup("jdk_install.retry.heading"),
                    subheading: lookup("jdk_install.retry.subheading"),
                    content: lookup("jdk_install.retry.content"),
                    info_heading: lookup("jdk_install.retry.info_heading"),
                    info_subtext: lookup("jdk_install.retry.info_subtext"),
                    info_subtext_2: lookup("jdk_install.retry.info_subtext_2"),
                    button_retry: lookup("jdk_install.retry.button_retry"),
                    button_cancel: lookup("jdk_install.retry.button_cancel"),
                },
            },
            generic: GenericDialogs {
                error_dialog_ok: lookup("generic.error_dialog_ok"),
                info_dialog_continue: lookup("generic.info_dialog_continue"),
            },
            launcher: LauncherDialogs {
                error: LauncherErrorDialogs {
                    title: lookup("launcher.error.title"),
                    heading: lookup("launcher.error.heading"),
                    subheading: lookup("launcher.error.subheading"),
                    // `{error}` — the launcher fills this from the
                    // matching `err.*` key, which is how a localized
                    // error body lands in a localized frame.
                    content: lookup("launcher.error.content"),
                    info_heading: lookup("launcher.error.info_heading"),
                    info_subtext: lookup("launcher.error.info_subtext"),
                    info_subtext_2: lookup("launcher.error.info_subtext_2"),
                    button_label: lookup("launcher.error.button_label"),
                    update_check_label: lookup("launcher.error.update_check_label"),
                },
            },
        }
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
    fn dialogs_resolve_from_the_baseline() {
        // Touching the lazy static forces assembly. If a key is missing
        // from the baseline the value comes back as the key literal
        // itself, so these assertions are really "no dotted key text
        // leaked into a field".
        let d = dialogs();
        assert!(!d.jdk_install.prompt.title.is_empty());
        assert!(!d.jdk_install.prompt.button_download.is_empty());
        assert!(d.jdk_install.prompt.content.contains("{version}"));
        assert_eq!(d.generic.error_dialog_ok, "OK");
        assert!(d.launcher.error.title.ends_with("— Snug"));
    }

    #[test]
    fn no_field_fell_back_to_a_bare_key() {
        // A key that resolves nowhere comes back from `localize::lookup`
        // as the key literal itself, so "this field is some known
        // localization key" is an exact test for "this field is
        // missing" — not a heuristic about dots and spaces.
        let baseline = snug_format::Localization::parse(
            snug_format::DEFAULT_EN_TAG,
            snug_format::DEFAULT_EN_TEXT,
        )
        .expect("built-in baseline parses");
        let is_a_key = |value: &str| baseline.get(value.trim()).is_some();

        let d = dialogs();
        for (name, value) in [
            ("prompt.title", &d.jdk_install.prompt.title),
            ("prompt.main", &d.jdk_install.prompt.main),
            ("prompt.content", &d.jdk_install.prompt.content),
            ("prompt.expanded", &d.jdk_install.prompt.expanded),
            ("prompt.button_download", &d.jdk_install.prompt.button_download),
            ("prompt.button_cancel", &d.jdk_install.prompt.button_cancel),
            ("metadata_failed.title", &d.jdk_install.metadata_failed.title),
            ("metadata_failed.content", &d.jdk_install.metadata_failed.content),
            ("metadata_failed.button_cancel", &d.jdk_install.metadata_failed.button_cancel),
            ("progress.title", &d.jdk_install.progress.title),
            ("progress.pct_label", &d.jdk_install.progress.pct_label),
            ("progress.detail_with_size", &d.jdk_install.progress.detail_with_size),
            ("progress.cancel_during_download", &d.jdk_install.progress.cancel_button_during_download),
            ("failure.title", &d.jdk_install.failure.title),
            ("failure.button_label", &d.jdk_install.failure.button_label),
            ("retry.title", &d.jdk_install.retry.title),
            ("retry.content", &d.jdk_install.retry.content),
            ("retry.button_retry", &d.jdk_install.retry.button_retry),
            ("generic.error_dialog_ok", &d.generic.error_dialog_ok),
            ("generic.info_dialog_continue", &d.generic.info_dialog_continue),
            ("launcher.error.title", &d.launcher.error.title),
            ("launcher.error.content", &d.launcher.error.content),
            ("launcher.error.button_label", &d.launcher.error.button_label),
            (
                "launcher.error.update_check_label",
                &d.launcher.error.update_check_label,
            ),
        ] {
            assert!(
                !is_a_key(value),
                "field `{name}` fell back to a raw localization key: {value:?}"
            );
        }
    }

    #[test]
    fn multi_line_bodies_decoded_from_escapes() {
        // The old `dialogs.toml` used TOML triple-quoted strings; the
        // catalog is single-line with `\n` escapes. This is the
        // regression guard for that conversion.
        let d = dialogs();
        let prompt = &d.jdk_install.prompt.content;
        assert!(
            prompt.contains("\n\n"),
            "prompt body lost its paragraph break: {prompt:?}"
        );
        assert!(prompt.contains("Java {version} or higher"));
        assert!(!prompt.contains("\\n"), "escapes were not decoded");

        let expanded = &d.jdk_install.prompt.expanded;
        assert!(expanded.contains("\n{url}\n"), "url line lost: {expanded:?}");
        assert!(expanded.contains("SHA-256: {sha256}"));

        let failed = &d.jdk_install.metadata_failed.content;
        assert!(failed.starts_with("Technical detail:\n{error}"), "{failed:?}");
    }

    #[test]
    fn failure_content_is_intentionally_empty() {
        // `error_window` treats an empty body as "fall back to the
        // module default". If this ever stops being empty, that
        // fallback path goes quiet — so pin it deliberately rather
        // than by accident.
        assert_eq!(dialogs().jdk_install.failure.content, "");
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