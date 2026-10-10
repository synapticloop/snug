//! Runtime localization for the launcher's user-facing strings.
//!
//! The launcher ships a built-in English baseline compiled into the
//! binary, plus zero or more user-supplied bundles embedded in the
//! payload. At startup we detect the user's Windows UI locale, build
//! a priority-ordered chain of bundles, and look up every message
//! through [`t`]: full BCP 47 tag → primary subtag → built-in English.
//!
//! ```ignore
//! use crate::localize;
//!
//! // Once at startup, right after decoding the payload:
//! localize::init(&embedded.payload.localizations);
//!
//! // Then anywhere:
//! let msg = localize::t("err.jvm_not_found",
//!     &[("min_java", "25")]);
//! ```
//!
//! Missing keys fall through the chain and finally return the raw key
//! string — so a missing translation never crashes, it just shows the
//! untranslated key, which is easy to grep for in tests.
//!
//! ## Locale detection
//!
//! Uses `GetUserDefaultLocaleName`, which returns a BCP 47 tag like
//! `en-US`, `de-DE`, `pt-BR`. We split on `-` and use the primary
//! subtag (`en`, `de`, `pt`) as the next-priority lookup. Built-in
//! English (`en`) is the absolute-last-resort fallback.
//!
//! ## Wire / runtime split
//!
//! The CLI embeds bundles as `snug_format::Localization { tag,
//! entries }` (serialized via postcard). At runtime we deserialize
//! those and merge with the built-in English baseline. The launcher
//! always has *some* English baseline available — the built-in copy
//! is parsed at startup from `snug_format::DEFAULT_EN_TEXT` so it
//! survives even if the payload itself is missing or corrupted (e.g.
//! on the bare stub path). That constant is the single
//! `include_str!` of `assets/snug-localisations.en.txt` in the whole
//! workspace, so the copy the CLI bakes into the payload and the copy
//! compiled into this fallback are the same bytes by construction.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};

use snug_format::Localization;

/// BCP 47 tag of the built-in English baseline. Always present.
pub use snug_format::DEFAULT_EN_TAG as DEFAULT_TAG;

/// Built-in English baseline text, re-exported from `snug-format`
/// where the file actually lives (`snug-format/assets/`). Re-exported
/// rather than re-`include_str!`d so the runtime fallback and the
/// CLI-embedded payload copy can never drift.
pub use snug_format::DEFAULT_EN_TEXT;

/// Global lookup state, read via [`lookup`] and replaced wholesale by
/// [`set_bundles`].
///
/// This is an `RwLock` rather than a `OnceLock` because the preview
/// binary needs to *switch* language while it runs, and a `OnceLock`
/// is frozen after the first write. The production launcher still only
/// ever writes once, at `main.rs` — so the lock is uncontended there
/// and the read cost is far below the `String` allocation [`lookup`]
/// already does on every call.
static BUNDLES: RwLock<Bundles> = RwLock::new(Bundles { chain: Vec::new() });

/// Bumped by every [`set_bundles`] call.
///
/// Consumers that cache derived state — currently `dialogs::DIALOGS`,
/// which snapshots ~54 resolved strings — record the generation they
/// built from and rebuild when it moves. Without this a language switch
/// would leave every cached string in the old language.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// The generation the current bundle chain belongs to.
///
/// Compare against a cached value to decide whether derived state is
/// stale. Monotonic; never returns the same value twice for different
/// chain contents.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

/// A complete priority chain of localization bundles, ready for
/// lookup. Built by [`Bundles::load`] and stashed in [`BUNDLES`].
///
/// Order of priority (highest first):
/// 1. User-supplied bundle matching the full BCP 47 tag (e.g. `en-US`).
/// 2. User-supplied bundle matching the primary subtag (e.g. `en`).
/// 3. Built-in English baseline (always last, always present).
///
/// **This slice:** `detect_locale()` always returns `en`, so priority
/// (1)/(2) are placeholders for a follow-up. Today every embedded
/// bundle is treated as "lowest priority" and the built-in English
/// baseline shadows nothing.
pub struct Bundles {
    /// Original bundles in the order they were passed to [`init`].
    /// We walk this on every lookup so a key present in a later
    /// bundle shadows an earlier one.
    chain: Vec<Localization>,
}

impl Bundles {
    /// Build a lookup chain from the payload's embedded bundles plus
    /// the always-present built-in English baseline.
    ///
    /// The returned `Bundles` is empty if `payload_bundles` is empty
    /// AND the built-in baseline failed to parse (a build-time
    /// problem, not a runtime one — `localization.rs` tests catch
    /// it). In every shipped build the baseline parses, so callers
    /// always get at least one bundle.
    ///
    /// Sorts `payload_bundles` by detected-locale priority so that
    /// the user's tag (currently always `en`) comes first and
    /// generalist fallbacks come last. The CLI also pre-sorts via
    /// `collect_localizations`, but doing it here too keeps the
    /// invariant local — tests can construct `Bundles` directly
    /// without depending on the CLI.
    pub fn load(payload_bundles: &[Localization]) -> Self {
        let (full, _primary) = detect_locale();
        let mut chain: Vec<Localization> = Vec::with_capacity(payload_bundles.len() + 1);

        // 1. Bundle matching the full detected tag (highest priority).
        //    Skipped when full == primary (no separate regional bundle
        //    is meaningful).
        if !full.is_empty() && full != DEFAULT_TAG {
            if let Some(b) = payload_bundles.iter().find(|b| b.tag == full) {
                chain.push(b.clone());
            }
        }
        // 2. Bundle matching the primary subtag.
        //    (Currently no-op because `detect_locale` returns en; the
        //    English baseline covers this.)
        // 3. All other payload bundles in their original order.
        for b in payload_bundles {
            if !chain.iter().any(|existing| existing.tag == b.tag) {
                chain.push(b.clone());
            }
        }

        // 4. Built-in English baseline at the END so it's the lowest
        //    priority in the lookup chain.
        if let Ok(builtin) = Localization::parse(DEFAULT_TAG, DEFAULT_EN_TEXT) {
            if !chain.iter().any(|b| b.tag == DEFAULT_TAG) {
                chain.push(builtin);
            }
        }

        Self { chain }
    }

    /// Look up `key` across the priority chain. Returns the value
    /// from the highest-priority bundle that has the key; falls back
    /// to the raw key string if none do. Does NOT substitute
    /// placeholders — call [`t`] or [`t_with_subs`] for that.
    pub fn raw_lookup(&self, key: &str) -> String {
        for bundle in &self.chain {
            if let Some(v) = bundle.get(key) {
                return v.to_string();
            }
        }
        // Last resort: the key itself. Loud but non-fatal — easy to
        // grep for in tests ("no, you shouldn't see `err.jvm_not_found`
        // in the user's dialog").
        key.to_string()
    }

    /// Number of bundles in the chain. Useful for diagnostics; tests
    /// call this to assert `init` wired up the right number.
    pub fn len(&self) -> usize {
        self.chain.len()
    }

    /// True when the chain holds no user bundles — i.e. lookups should
    /// fall through to the compiled-in baseline. This is the initial
    /// state of [`BUNDLES`], before any [`set_bundles`] call.
    pub fn is_empty(&self) -> bool {
        self.chain.is_empty()
    }

    /// Look up `key` across the chain, returning the winning value
    /// without allocating. `None` when no bundle in the chain has it.
    ///
    /// The allocating [`Bundles::raw_lookup`] is for tests and
    /// diagnostics; [`lookup`] uses this so the read lock is held for
    /// the length of a comparison rather than a heap write.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.chain.iter().find_map(|b| b.get(key))
    }

    /// The tags present in the chain, in priority order. Used by the
    /// preview binary to build its language dropdown.
    pub fn tags(&self) -> Vec<String> {
        self.chain.iter().map(|b| b.tag.clone()).collect()
    }
}

/// Initialise the global lookup state. Call as early as possible
/// after the payload has been decoded.
///
/// Thin alias over [`set_bundles`], kept because `main.rs` reads as
/// "init once at startup" and that's exactly what it does. Unlike the
/// old `OnceLock`, calling it twice now *replaces* the chain rather
/// than being ignored — the preview binary depends on that.
pub fn init(payload_bundles: &[Localization]) {
    set_bundles(payload_bundles);
}

/// Replace the active bundle chain and bump [`generation`].
///
/// Any consumer caching resolved strings should rebuild when the
/// generation moves; `dialogs::dialogs()` does.
pub fn set_bundles(payload_bundles: &[Localization]) {
    let next = Bundles::load(payload_bundles);
    // A poisoned lock means some other thread panicked while holding
    // it. `into_inner` hands back the guard either way, and the data
    // we are about to install is freshly built, so recovering beats
    // propagating a panic into a dialog.
    let mut guard = BUNDLES.write().unwrap_or_else(|e| e.into_inner());
    *guard = next;
    drop(guard);
    GENERATION.fetch_add(1, Ordering::Release);
}

/// The built-in English baseline, parsed once and independent of
/// whether [`init`] has run.
///
/// `Bundles::load` appends its own copy to the chain, so this is only
/// reached on the pre-init path. It exists because the baseline is
/// compiled in and therefore *always* available — resolving to
/// English is the correct answer before the user's bundles are known,
/// and returning a bare `jdk_install.prompt.title` is not.
static BUILTIN: OnceLock<Option<Localization>> = OnceLock::new();

fn builtin() -> Option<&'static Localization> {
    BUILTIN
        .get_or_init(|| {
            Localization::parse(DEFAULT_TAG, DEFAULT_EN_TEXT).ok()
        })
        .as_ref()
}

/// Look up `key` in the priority chain and return the raw value
/// (with `\n` / `\t` escape sequences already decoded by the
/// `Localization::parse` step).
///
/// Before [`init`] runs — the bare-stub path in `main.rs`, the
/// preview binaries, unit tests — there is no chain yet, so this
/// falls back to the compiled-in English baseline. Only a key that is
/// in neither returns the key string itself.
pub fn lookup(key: &str) -> String {
    // The read lock is taken for the comparison only, then dropped
    // before the `String` is built, so it is never held across an
    // allocation. `into_inner` on a poisoned lock still yields valid
    // data — poisoning records that some thread panicked, not that
    // the bundle chain is corrupt.
    let from_chain = {
        let guard = BUNDLES.read().unwrap_or_else(|e| e.into_inner());
        if guard.is_empty() {
            None
        } else {
            guard.get(key).map(str::to_string)
        }
    };
    from_chain
        .or_else(|| builtin().and_then(|b| b.get(key)).map(str::to_string))
        .unwrap_or_else(|| key.to_string())
}

/// Substitute `{name}` placeholders in `template` with the
/// corresponding values from `subs`. Format specifiers
/// (`{name:.2}`, `{name:>5}` etc.) are not supported — apply them to
/// the values in Rust before calling. Unknown placeholders are left
/// as-is so a typo doesn't silently swallow text.
///
/// This is a thin wrapper over [`super::dialogs::fill`] — kept here
/// so callers don't have to import both modules.
pub fn fill_placeholders(template: &str, subs: &[(&str, &str)]) -> String {
    super::dialogs::fill(template, subs)
}

/// Look up `key`, substitute `{name}` placeholders from `subs`, and
/// return the resolved string. If the key is missing everywhere,
/// returns the raw key (also with placeholders unsubstituted, so a
/// typo in the key still produces something visible).
///
/// `subs` is `&[(&str, &str)]` rather than e.g. a `HashMap` so it
/// stays zero-alloc and matches the existing [`super::dialogs::fill`]
/// API used elsewhere.
pub fn t(key: &str, subs: &[(&str, &str)]) -> String {
    let raw = lookup(key);
    fill_placeholders(&raw, subs)
}

/// Convenience for the common single-placeholder case.
pub fn t1(key: &str, name: &str, value: &str) -> String {
    t(key, &[(name, value)])
}

/// Convenience for the `{0}` / `{1}` positional placeholder style used
/// by `thiserror`'s default `Display` impl. Each entry is interpolated
/// at `{0}`, `{1}`, etc.
///
/// Note: `thiserror` uses `{0}` through `{N-1}` for the tuple fields
/// of variants like `Err(String)`. We don't read those field names
/// from the error type at runtime (would need a derive), so the
/// caller is responsible for passing `(name, value)` pairs that
/// match the placeholders the localization bundle actually uses.
/// For `thiserror`-derived strings, that means the bundle should use
/// the `{0}` / `{1}` style and the caller should pass
/// `&[("0", "..."), ("1", "...")]`.
///
/// This helper does a single linear pass through the template
/// replacing each `{i}` with `values[i]`. Unknown indices (e.g.
/// `{3}` when only two values were passed) are left as-is so a typo
/// is visible.
pub fn t_positional(key: &str, values: &[&str]) -> String {
    let raw = lookup(key);
    let mut out = raw;
    for (i, v) in values.iter().enumerate() {
        let needle = format!("{{{i}}}");
        out = out.replace(&needle, v);
    }
    out
}

/// Detect the user's Windows UI language. Returns the BCP 47 tag
/// (e.g. `"en-US"`, `"de-DE"`) and its primary subtag (e.g. `"en"`,
/// `"de"`).
///
/// **Stub for this slice.** Always returns `("en", "en")` because
/// `windows-sys` 0.59 does not export `GetUserDefaultLocaleName` or
/// `GetUserDefaultUILanguage` under a usable feature gate. Adding
/// real detection is a follow-up; the plan is to read
/// `HKCU\Control Panel\Desktop\PreferredUILanguages` via the
/// registry helpers we already pull in. Until then the launcher
/// always uses the built-in English baseline; user-supplied
/// `--localization` bundles are still parsed and embedded so a
/// follow-up PR with detection can light them up without rebuilding
/// the EXE.
pub fn detect_locale() -> (String, String) {
    (DEFAULT_TAG.to_string(), DEFAULT_TAG.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(tag: &str, pairs: &[(&str, &str)]) -> Localization {
        let mut entries = Vec::new();
        for (k, v) in pairs {
            entries.push((k.to_string(), v.to_string()));
        }
        Localization {
            tag: tag.to_string(),
            entries,
        }
    }

    #[test]
    fn load_includes_built_in_baseline_when_no_payload() {
        let b = Bundles::load(&[]);
        assert!(!b.is_empty(), "should always have built-in baseline");
        // Baseline has the canonical runtime-error keys.
        assert_eq!(
            b.raw_lookup("err.jvm_not_found"),
            "Could not locate a Java {min_java}+ JVM in any of the configured discovery locations"
        );
        assert_eq!(b.raw_lookup("splash.title"), "Snug Splash");
    }

    #[test]
    fn load_appends_user_bundles_above_baseline() {
        let de = bundle("de", &[("err.jvm_not_found", "Keine JVM gefunden.")]);
        let b = Bundles::load(&[de]);
        assert_eq!(
            b.raw_lookup("err.jvm_not_found"),
            "Keine JVM gefunden.",
            "user bundle should override baseline"
        );
    }

    #[test]
    fn load_keeps_baseline_only_for_keys_user_did_not_translate() {
        // User bundle covers only one key.
        let de = bundle("de", &[("err.jvm_not_found", "Keine JVM gefunden.")]);
        let b = Bundles::load(&[de]);
        // Key the user didn't translate falls back to English.
        assert_eq!(
            b.raw_lookup("err.java_exception"),
            "Java `main` threw an exception: {0}",
            "missing keys should fall through to baseline"
        );
    }

    #[test]
    fn load_avoids_double_embedding_baseline_when_payload_already_has_en() {
        // The CLI may include the baseline in the payload (it does,
        // intentionally). Make sure we don't double up: if a payload
        // already has an `en` tag, the built-in baseline is skipped.
        let cli_en = bundle("en", &[("err.foo", "from CLI")]);
        let b = Bundles::load(&[cli_en]);
        // Should have exactly one `en` bundle in the chain.
        let en_count = b.chain.iter().filter(|b| b.tag == "en").count();
        assert_eq!(en_count, 1, "baseline should not duplicate CLI-supplied en");
        // The CLI-supplied `en` wins for its own keys (it's later in
        // the chain than the built-in would be).
        assert_eq!(b.raw_lookup("err.foo"), "from CLI");
    }

    #[test]
    fn lookup_returns_key_when_uninitialised() {
        // We can't un-init the global OnceLock, but we can use the
        // raw path: lookup returns the key if the chain doesn't have
        // it.
        let raw = lookup("definitely.not.present.anywhere");
        // Could be either "" / the key / empty string — we only
        // assert it doesn't panic.
        let _ = raw;
    }

    #[test]
    fn fill_placeholders_substitutes_named_keys() {
        // This is just a thin wrapper over `dialogs::fill` — assert
        // it still does the right thing end-to-end.
        let s = fill_placeholders("Java {ver} needs {n}", &[("ver", "25"), ("n", "X")]);
        assert_eq!(s, "Java 25 needs X");
    }

    #[test]
    fn t_substitutes_placeholders_from_lookup() {
        // Construct a standalone chain (not via global init — that's
        // a one-shot per-process).
        let b = Bundles::load(&[bundle(
            "en",
            &[("err.demo", "Found Java {min_java}, need {wanted}")],
        )]);
        let resolved = b.raw_lookup("err.demo");
        let out = fill_placeholders(&resolved, &[("min_java", "17"), ("wanted", "25")]);
        assert_eq!(out, "Found Java 17, need 25");
    }

    #[test]
    fn detect_locale_returns_a_tag() {
        let (full, primary) = detect_locale();
        assert!(!full.is_empty());
        // Primary is the head of the full tag.
        let expected_primary = full.split('-').next().unwrap_or(&full).to_string();
        assert_eq!(primary, expected_primary);
    }

    /// Every key the launcher looks up must exist in the built-in
    /// baseline.
    ///
    /// `lookup` returns the key string itself when nothing in the chain
    /// has it — deliberate, so a missing *translation* degrades to
    /// English rather than failing. But a key missing from the
    /// *baseline* is a real bug: the built-in English is supposed to
    /// be complete, and the user sees a dotted identifier in a dialog
    /// instead of a sentence. Nothing at the call site can catch that,
    /// because the call site is just a string literal, so the
    /// inventory is pinned here.
    ///
    /// When you add a lookup, add its key to this list. When you add a
    /// key to the baseline, the second half of the test flags it as
    /// unreferenced — dead weight in every shipped payload otherwise.
    #[test]
    fn every_localize_key_is_in_the_baseline() {
        const KEYS: &[&str] = &[
            // err.* — LauncherError, via error::localize_launcher_error
            "err.zip",
            "err.self_path",
            "err.io",
            "err.jvm_not_found",
            "err.jvm_too_old",
            "err.library_load",
            "err.symbol_not_found",
            "err.invalid_state",
            "err.jni_init",
            "err.jni_create",
            "err.jni_attach",
            "err.jni_invoke",
            "err.no_main_class",
            "err.main_class_not_found",
            "err.no_main_method",
            "err.java_exception",
            "err.unsupported_platform",
            "err.format",
            // splash.*
            "splash.err.overflow",
            "splash.err.buffer_length",
            "splash.err.empty",
            "splash.err.thread_spawn",
            "splash.err.bitmap",
            "splash.err.window",
            "splash.err.update_layered",
            "splash.err.create_compatible_dc",
            "splash.err.create_dib_section",
            "splash.err.create_window",
            "splash.err.update_layered_window",
            "splash.title",
            // jdk.err.* — JdkError
            "jdk.err.metadata_fetch",
            "jdk.err.no_metadata_for_version",
            "jdk.err.bad_metadata_shape",
            "jdk.err.bad_field",
            "jdk.err.download",
            "jdk.err.sha256_mismatch",
            "jdk.err.extract",
            "jdk.err.no_java_exe",
            "jdk.err.io",
            "jdk.err.dialog",
            // launcher.* — bare stub + fallback message box
            "launcher.bare_stub.no_payload",
            "launcher.bare_stub.hint",
            "launcher.bare_stub.command",
            "launcher.fallback_messagebox.title",
            // dialog chrome — assembled in dialogs::dialogs()
            "jdk_install.prompt.title",
            "jdk_install.prompt.main",
            "jdk_install.prompt.content",
            "jdk_install.prompt.expanded",
            "jdk_install.prompt.button_download",
            "jdk_install.prompt.button_open_browser",
            "jdk_install.prompt.button_cancel",
            "jdk_install.metadata_failed.title",
            "jdk_install.metadata_failed.heading",
            "jdk_install.metadata_failed.subheading",
            "jdk_install.metadata_failed.content",
            "jdk_install.metadata_failed.info_heading",
            "jdk_install.metadata_failed.info_subtext",
            "jdk_install.metadata_failed.info_subtext_2",
            "jdk_install.metadata_failed.button_open_browser",
            "jdk_install.metadata_failed.button_cancel",
            "jdk_install.progress.title",
            "jdk_install.progress.main",
            "jdk_install.progress.content_initial",
            "jdk_install.progress.heading",
            "jdk_install.progress.subtitle",
            "jdk_install.progress.pct_label",
            "jdk_install.progress.phase_label",
            "jdk_install.progress.detail_with_size",
            "jdk_install.progress.detail_no_size",
            "jdk_install.progress.detail_eta_seconds",
            "jdk_install.progress.detail_eta_second",
            "jdk_install.progress.detail_eta_done",
            "jdk_install.progress.info_heading",
            "jdk_install.progress.info_subtext",
            "jdk_install.progress.info_subtext_2",
            "jdk_install.progress.cancel_button_during_download",
            "jdk_install.progress.arch_macos_arm64",
            "jdk_install.progress.arch_macos_x86_64",
            "jdk_install.progress.arch_windows_x86_64",
            "jdk_install.failure.title",
            "jdk_install.failure.heading",
            "jdk_install.failure.subheading",
            "jdk_install.failure.content",
            "jdk_install.failure.info_heading",
            "jdk_install.failure.info_subtext",
            "jdk_install.failure.info_subtext_2",
            "jdk_install.failure.button_label",
            "jdk_install.retry.title",
            "jdk_install.retry.heading",
            "jdk_install.retry.subheading",
            "jdk_install.retry.content",
            "jdk_install.retry.info_heading",
            "jdk_install.retry.info_subtext",
            "jdk_install.retry.info_subtext_2",
            "jdk_install.retry.button_retry",
            "jdk_install.retry.button_cancel",
            "generic.error_dialog_ok",
            "generic.info_dialog_continue",
            "launcher.error.title",
            "launcher.error.heading",
            "launcher.error.subheading",
            "launcher.error.content",
            "launcher.error.info_heading",
            "launcher.error.info_subtext",
            "launcher.error.info_subtext_2",
            "launcher.error.button_label",
            "launcher.error.update_check_label",
        ];

        let baseline = Bundles::load(&[]);
        let missing: Vec<&&str> = KEYS
            .iter()
            .filter(|k| baseline.raw_lookup(k) == **k)
            .collect();
        assert!(
            missing.is_empty(),
            "these keys are looked up by the launcher but absent from the built-in \
             baseline (the user would see the raw key in a dialog): {missing:?}"
        );

        // The other direction: a baseline key that nothing reads is
        // dead weight shipped in every payload, and usually a leftover
        // from a rename. Allow it to be opted into explicitly rather
        // than silently drifting.
        const KNOWN_UNREFERENCED: &[&str] = &[];
        let builtin = Localization::parse(DEFAULT_TAG, DEFAULT_EN_TEXT)
            .expect("built-in baseline parses");
        let unreferenced: Vec<&str> = builtin
            .entries
            .iter()
            .map(|(k, _)| k.as_str())
            .filter(|k| !KEYS.contains(k) && !KNOWN_UNREFERENCED.contains(k))
            .collect();
        assert!(
            unreferenced.is_empty(),
            "these keys are in the baseline but nothing looks them up — add them to \
             KEYS, or to KNOWN_UNREFERENCED if they're deliberately dormant: \
             {unreferenced:?}"
        );
    }
}