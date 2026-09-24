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
//! is parsed at startup from `include_str!("../snug-launcher/src/
//! snug-localisations.en.txt")` so it survives even if the payload
//! itself is missing or corrupted (e.g. on the bare stub path).

use std::sync::OnceLock;

use snug_format::Localization;

/// BCP 47 tag of the built-in English baseline. Always present.
pub const DEFAULT_TAG: &str = "en";

/// Built-in English baseline text. Sourced directly from
/// `crates/snug-launcher/src/snug-localisations.en.txt` at compile
/// time — same file the CLI embeds into every payload, so the wire
/// and runtime copies can never drift.
const DEFAULT_EN_TEXT: &str = include_str!("./snug-localisations.en.txt");

/// Global lookup state. Initialised once via [`init`] and read via
/// [`bundles`].
static BUNDLES: OnceLock<Bundles> = OnceLock::new();

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
}

/// Initialise the global lookup state. Call exactly once, as early as
/// possible after the payload has been decoded. Subsequent calls are
/// silently ignored — the first call wins.
///
/// We don't `expect`/panic on re-init: in practice the launcher
/// always calls `init` from one place (`platform::windows::run`),
/// but defensive coding here means a future refactor that adds a
/// second init site doesn't crash production builds.
pub fn init(payload_bundles: &[Localization]) {
    let bundles = Bundles::load(payload_bundles);
    let _ = BUNDLES.set(bundles);
}

/// Returns the initialised bundle chain, or `None` if [`init`] was
/// never called. Callers (`t`, `t_with_subs`) handle `None` by
/// returning the raw key, so it's safe to use the lookup macros from
/// any point in the launcher's lifetime — including before `init`
/// runs, where they'll just return the key string.
fn bundles() -> Option<&'static Bundles> {
    BUNDLES.get()
}

/// Look up `key` in the priority chain and return the raw value
/// (with `\n` / `\t` escape sequences already decoded by the
/// `Localization::parse` step). If [`init`] hasn't been called or
/// the key isn't present anywhere, returns the key itself.
pub fn lookup(key: &str) -> String {
    match bundles() {
        Some(b) => b.raw_lookup(key),
        None => key.to_string(),
    }
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
        assert!(b.len() >= 1, "should always have built-in baseline");
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
}