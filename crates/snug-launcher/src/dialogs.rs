//! User-facing dialog strings.
//!
//! Sourced from the localization bundle — the same chain the error
//! strings use — rather than a dedicated file. The strings live in
//! `snug-format/assets/snug-localisations.en.txt` under
//! `runtime.*`, `generic.*` and `launcher.error.*` keys, which
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
//! (`dialogs().runtime.prompt.title`), so key literals appear in
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
/// localization catalog one-for-one — `dialogs.runtime.prompt`
/// is `runtime.prompt.*`, and so on.
#[derive(Debug, Clone)]
pub struct Dialogs {
    pub runtime: RuntimeDialogs,
    pub generic: GenericDialogs,
    pub launcher: LauncherDialogs,
}

/// Launcher-runtime error strings (Java launch failures, JNI errors,
/// Main-Class not found, Java `main` exceptions, etc.). Same physical
/// window as `[runtime.failure]` — different copy.
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
    /// Single-button dismissal label.
    pub button_exit: String,
    /// Label for the update button. Rendered only when the payload
    /// carries an `update_check_url`, so a build with no update URL
    /// never shows a button that would do nothing.
    pub button_update: String,
}

#[derive(Debug, Clone)]
pub struct RuntimeDialogs {
    pub prompt: InstallPromptDialog,
    pub metadata_failed: MetadataFailedDialog,
    pub progress: ProgressDialog,
    pub failure: FailureDialog,
    pub retry: RetryDialog,
}

#[derive(Debug, Clone)]
pub struct InstallPromptDialog {
    pub title: String,
    pub heading: String,
    /// Sentence below the headline. The baseline splits the prompt's
    /// one-line `main` into a headline and this; the modal's
    /// `subheading` slot was already there and empty.
    pub subheading: String,
    pub content: String,
    /// Info-box heading line (light-blue box, bottom-left). Every
    /// dialog in the family carries these three, so the prompt has
    /// them too rather than rendering an empty box.
    pub info_heading: String,
    pub info_subtext: String,
    pub info_subtext_2: String,
    pub button_download: String,
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
    pub heading: String,
    pub subheading: String,
    /// The runtime line under the subheading — "Runtime 25.0.4.1
    /// (~192 MB)". Was `.main`; renamed so every dialog names its
    /// body `.content` and nothing carries a second spelling.
    pub content: String,
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
    /// Platform names for the `{arch}` slot of `phase_label`.
    ///
    /// `phase_label` is a *template*, not a finished string. Every
    /// platform that shows it has to substitute its own name, or the user
    /// reads a literal `{arch}`.
    ///
    /// The macOS pair feeds `appkit::adoptium_arch_label`, which needed
    /// splitting out because a hardcoded "Windows x64" in the shared
    /// template had macOS announcing itself as a Windows download. See
    /// that function for the full story.
    ///
    /// The Windows value had no key at all until now: the Win32 progress
    /// window passed the template straight to its `STATIC` control, so
    /// `{arch}` was never filled and the dialog showed the placeholder.
    /// There is one Windows key rather than an arch branch because snug
    /// targets x86_64 Windows only - one shipping target, one spelling.
    /// An arm64 slice adds its key and its branch together.
    pub arch_macos_arm64: String,
    pub arch_macos_x86_64: String,
    pub arch_windows_x86_64: String,
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
/// a cached `runtime.prompt.title` would be a permanently broken
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
            runtime: RuntimeDialogs {
                prompt: InstallPromptDialog {
                    title: lookup("runtime.prompt.title"),
                    heading: lookup("runtime.prompt.heading"),
                    subheading: lookup("runtime.prompt.subheading"),
                    content: lookup("runtime.prompt.content"),
                    info_heading: lookup("runtime.prompt.info_heading"),
                    info_subtext: lookup("runtime.prompt.info_subtext"),
                    info_subtext_2: lookup("runtime.prompt.info_subtext_2"),
                    button_download: lookup("runtime.prompt.button_download"),
                    button_cancel: lookup("runtime.prompt.button_cancel"),
                },
                metadata_failed: MetadataFailedDialog {
                    title: lookup("runtime.metadata_failed.title"),
                    heading: lookup("runtime.metadata_failed.heading"),
                    subheading: lookup("runtime.metadata_failed.subheading"),
                    content: lookup("runtime.metadata_failed.content"),
                    info_heading: lookup("runtime.metadata_failed.info_heading"),
                    info_subtext: lookup("runtime.metadata_failed.info_subtext"),
                    info_subtext_2: lookup("runtime.metadata_failed.info_subtext_2"),
                    button_open_browser: lookup("runtime.metadata_failed.button_open_browser"),
                    button_cancel: lookup("runtime.metadata_failed.button_cancel"),
                },
                progress: ProgressDialog {
                    title: lookup("runtime.progress.title"),
                    heading: lookup("runtime.progress.heading"),
                    subheading: lookup("runtime.progress.subheading"),
                    content: lookup("runtime.progress.content"),
                    pct_label: lookup("runtime.progress.pct_label"),
                    phase_label: lookup("runtime.progress.phase_label"),
                    detail_with_size: lookup("runtime.progress.detail_with_size"),
                    detail_no_size: lookup("runtime.progress.detail_no_size"),
                    detail_eta_seconds: lookup("runtime.progress.detail_eta_seconds"),
                    detail_eta_second: lookup("runtime.progress.detail_eta_second"),
                    detail_eta_done: lookup("runtime.progress.detail_eta_done"),
                    info_heading: lookup("runtime.progress.info_heading"),
                    info_subtext: lookup("runtime.progress.info_subtext"),
                    info_subtext_2: lookup("runtime.progress.info_subtext_2"),
                    cancel_button_during_download: {
                        lookup("runtime.progress.cancel_button_during_download")
                    },
                    arch_macos_arm64: lookup("runtime.progress.arch_macos_arm64"),
                    arch_macos_x86_64: lookup("runtime.progress.arch_macos_x86_64"),
                    arch_windows_x86_64: lookup("runtime.progress.arch_windows_x86_64"),
                },
                failure: FailureDialog {
                    title: lookup("runtime.failure.title"),
                    heading: lookup("runtime.failure.heading"),
                    subheading: lookup("runtime.failure.subheading"),
                    // Intentionally empty in the baseline: `error_window`
                    // treats an empty body as "use the module default".
                    content: lookup("runtime.failure.content"),
                    info_heading: lookup("runtime.failure.info_heading"),
                    info_subtext: lookup("runtime.failure.info_subtext"),
                    info_subtext_2: lookup("runtime.failure.info_subtext_2"),
                    button_label: lookup("runtime.failure.button_label"),
                },
                retry: RetryDialog {
                    title: lookup("runtime.retry.title"),
                    heading: lookup("runtime.retry.heading"),
                    subheading: lookup("runtime.retry.subheading"),
                    content: lookup("runtime.retry.content"),
                    info_heading: lookup("runtime.retry.info_heading"),
                    info_subtext: lookup("runtime.retry.info_subtext"),
                    info_subtext_2: lookup("runtime.retry.info_subtext_2"),
                    button_retry: lookup("runtime.retry.button_retry"),
                    button_cancel: lookup("runtime.retry.button_cancel"),
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
                    button_exit: lookup("launcher.error.button_exit"),
            button_update: lookup("launcher.error.button_update"),
                        },
            },
        }
}

/// Substitute `{name}` placeholders in `template` with the
/// corresponding values from `subs`. Format specifiers (`{name:.2}`,
/// `{name:>5}` etc.) are not supported — apply them to the values
/// in Rust before calling. Unknown placeholders are left as-is so
/// a typo in the TOML doesn't silently swallow text.
///
/// `\{name}` escapes to a literal `{name}` and is **not** substituted,
/// so a bundle can show a slot instead of filling one. See
/// [`crate::localize::substitute_slots`], which is also what makes
/// this and `prompt_window`'s substitution agree byte for byte.
pub fn fill(template: &str, subs: &[(&str, &str)]) -> String {
    crate::localize::substitute_slots(template, |name| {
        subs.iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| (*v).to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_escaped_brace_is_a_literal_brace_not_a_slot() {
        // The whole point: a bundle can *show* a slot instead of
        // filling one, and it must do so identically in a shipped
        // build and in snug_preview. `str::replace` would have filled
        // this, because `{size_mb}` is present inside `\{size_mb}` --
        // which is why substitution is a scan and not a replace.
        assert_eq!(
            fill(r"needs \{size_mb} MB", &[("size_mb", "192")]),
            "needs {size_mb} MB"
        );
        // Only the opening brace is escaped; the closing one is
        // ordinary text.
        assert_eq!(fill(r"\{version}", &[("version", "25")]), "{version}");
    }

    #[test]
    fn the_rule_is_a_brace_that_starts_with_a_backslash_is_not_replaced() {
        // The contract, in the form the book scaffold uses it: a token
        // whose opening brace is escaped shows itself, a bare one is
        // filled. Order does not matter -- each brace decides for
        // itself and only for itself.
        assert_eq!(
            fill(
                r"prompt.content \{size_mb} {size_mb}",
                &[("size_mb", "192")],
            ),
            "prompt.content {size_mb} 192"
        );
        // Same value, reversed, to pin that neither position wins.
        assert_eq!(
            fill(
                r"prompt.content {size_mb} \{size_mb}",
                &[("size_mb", "192")],
            ),
            "prompt.content 192 {size_mb}"
        );
    }

    #[test]
    fn an_escape_is_local_to_its_own_brace() {
        let subs = [("size_mb", "7"), ("total_mb", "9")];
        // Escaped, live: the escape affects only its own brace.
        assert_eq!(
            fill(r"\{size_mb} of {total_mb}", &subs),
            "{size_mb} of 9"
        );
        // Both escaped: both survive verbatim, which is what a book
        // scaffold wants when it is documenting slots rather than
        // filling them.
        assert_eq!(
            fill(r"\{size_mb} of \{total_mb}", &subs),
            "{size_mb} of {total_mb}"
        );
    }

    #[test]
    fn an_unknown_slot_is_still_left_visible() {
        // Escape support must not weaken the existing contract: a typo
        // shows rather than swallowing text.
        assert_eq!(fill("{nope}", &[("size_mb", "192")]), "{nope}");
    }

    #[test]
    fn substitution_never_splits_a_multibyte_character() {
        assert_eq!(fill("Größe {n}", &[("n", "1")]), "Größe 1");
    }

    #[test]
    fn dialogs_resolve_from_the_baseline() {
        // Touching the lazy static forces assembly. If a key is missing
        // from the baseline the value comes back as the key literal
        // itself, so these assertions are really "no dotted key text
        // leaked into a field".
        let d = dialogs();
        assert!(!d.runtime.prompt.title.is_empty());
        assert!(!d.runtime.prompt.button_download.is_empty());
        assert!(d.runtime.prompt.content.contains("{size_mb}"));
        assert_eq!(d.generic.error_dialog_ok, "OK");
        // The brand suffix is defined by the baseline, and the
        // separator in front of it has changed once already -- it was
        // an em dash until the baseline was normalised to ASCII. If
        // this assertion breaks, read the convention note on
        // `launcher.fallback_messagebox.title` in the baseline rather
        // than assuming the value regressed.
        assert!(d.launcher.error.title.ends_with("- Snug"));
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
            ("prompt.title", &d.runtime.prompt.title),
            ("prompt.heading", &d.runtime.prompt.heading),
            ("prompt.subheading", &d.runtime.prompt.subheading),
            ("prompt.content", &d.runtime.prompt.content),
            ("prompt.button_download", &d.runtime.prompt.button_download),
            ("prompt.button_cancel", &d.runtime.prompt.button_cancel),
            ("metadata_failed.title", &d.runtime.metadata_failed.title),
            ("metadata_failed.content", &d.runtime.metadata_failed.content),
            ("metadata_failed.button_cancel", &d.runtime.metadata_failed.button_cancel),
            ("progress.title", &d.runtime.progress.title),
            ("progress.heading", &d.runtime.progress.heading),
            ("progress.subheading", &d.runtime.progress.subheading),
            ("progress.content", &d.runtime.progress.content),
            ("progress.pct_label", &d.runtime.progress.pct_label),
            ("progress.detail_with_size", &d.runtime.progress.detail_with_size),
            ("progress.cancel_during_download", &d.runtime.progress.cancel_button_during_download),
            ("failure.title", &d.runtime.failure.title),
            ("failure.button_label", &d.runtime.failure.button_label),
            ("retry.title", &d.runtime.retry.title),
            ("retry.content", &d.runtime.retry.content),
            ("retry.button_retry", &d.runtime.retry.button_retry),
            ("generic.error_dialog_ok", &d.generic.error_dialog_ok),
            ("generic.info_dialog_continue", &d.generic.info_dialog_continue),
            ("launcher.error.title", &d.launcher.error.title),
            ("launcher.error.content", &d.launcher.error.content),
            ("launcher.error.button_exit", &d.launcher.error.button_exit),
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
        // The prompt body is one paragraph. The paragraph-break and
        // SHA-256 guards moved to `metadata_failed.content`, which is
        // still a multi-line value — the regression this test exists
        // for is about `\n` being decoded at all, not about which key
        // happens to carry one today.
        let prompt = &d.runtime.prompt.content;
        assert!(
            prompt.contains("a runtime to launch"),
            "prompt body changed shape: {prompt:?}"
        );
        assert!(!prompt.contains("\\n"), "escapes were not decoded");

        let failed = &d.runtime.metadata_failed.content;
        assert!(failed.contains("\n"), "metadata body lost its line break: {failed:?}");
        assert!(failed.starts_with("Technical detail:\n{error}"), "{failed:?}");
        assert!(!failed.contains("\\n"), "escapes were not decoded");
    }

    #[test]
    fn failure_content_carries_the_actual_error() {
        // This used to be deliberately empty, on the belief that
        // `error_window` treats an empty body as "use the module default".
        // No such fallback existed -- `error_window::show` documents content
        // as "always caller-supplied" and passes it straight through -- so
        // every user who exhausted the download attempts got a dialog
        // titled "Runtime Installation Failure" with a blank body and no
        // clue what had failed. Worse, it was silent: the key existed, so
        // the missing-key guard had nothing to report.
        //
        // The text lives here rather than as a hard-coded fallback in
        // `error_window` so that a `--localization` bundle can translate it
        // by default, the same as every other key in the dialog.
        let content = crate::dialogs::fill(
            dialogs().runtime.failure.content.as_str(),
            &[("version", "25.0.1+9"), ("error", "HTTP 503 from api.adoptium.net")],
        );
        assert!(
            content.contains("HTTP 503 from api.adoptium.net"),
            "the terminal failure dialog must show what went wrong, got {content:?}"
        );
        assert!(
            content.contains("25.0.1+9"),
            "and which runtime it was trying to install, got {content:?}"
        );
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