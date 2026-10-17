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
//! [`detect_locales`] returns the user's language preference list as
//! BCP 47 tags, most-preferred first: Windows
//! `GetUserPreferredUILanguages` (falling back to
//! `GetUserDefaultLocaleName`), macOS `+[NSLocale preferredLanguages]`.
//! A machine set to `[de-AT, en-US]` yields exactly that order, which
//! [`Bundles::load`] walks as its lookup chain.
//!
//! Each preferred tag is matched exactly first, then truncated one
//! subtag at a time (`de-AT` → `de`, `zh-Hans-CN` → `zh-Hans` → `zh`),
//! with the built-in English baseline as the absolute-last-resort
//! fallback.
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

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// Whether `{name}` slots get filled with the caller's values.
///
/// `true` everywhere a real launcher runs, and this is the default so the
/// product cannot be affected by leaving it alone. `snug_preview` clears it
/// so its "Replace tokens" checkbox can show every dialog *as authored*.
///
/// This is a global rather than a parameter because substitution happens
/// deep inside the window modules — `jdk_install::show_retry_dialog` and
/// friends fill their own copy before handing it to the painter — and the
/// preview calls those same functions, so there is no call site it could
/// pass a flag to. A parameter would have to be threaded through
/// `prompt_window`, `retry_window`, `metadata_failed_window`,
/// `progress_window`, `error_window` and both platform impls, putting a
/// preview-shaped argument in five shipped signatures to serve a binary
/// that never ships.
///
/// Relaxed is enough: this is a UI preference, not synchronisation. The
/// dialogs are modal, so a dialog already on screen keeps the text it
/// painted and only the next one opened observes the change.
static REPLACE_TOKENS: AtomicBool = AtomicBool::new(true);

/// Turn `{name}` filling on or off process-wide.
///
/// See [`REPLACE_TOKENS`]. Only `snug_preview` calls this; a shipped
/// launcher leaves it at the default.
pub fn set_replace_tokens(replace: bool) {
    REPLACE_TOKENS.store(replace, Ordering::Relaxed);
}

/// Whether `{name}` slots are currently filled.
#[inline]
pub(crate) fn replace_tokens() -> bool {
    REPLACE_TOKENS.load(Ordering::Relaxed)
}

/// A complete priority chain of localization bundles, ready for
/// lookup. Built by [`Bundles::load`] and stashed in [`BUNDLES`].
///
/// Order of priority (highest first), for a user whose preference list
/// is `["de-AT", "en-US"]`:
///
/// 1. A bundle tagged exactly as a preferred tag — `de-AT`, then `en-US`.
/// 2. A bundle tagged as a truncated form of one — `de` (from `de-AT`),
///    then `en` (from `en-US`).
/// 3. The built-in English baseline — always present, always last.
///
/// Bundles matching **no** preferred tag are deliberately left out of
/// the chain. A bundle is an *offer* of a translation, not a default:
/// dropping German and French bundles into a payload must not make a
/// French speaker read German because `fr` happened to sort first.
/// Nothing is lost by excluding them — every bundle is reachable from
/// a machine whose locale it matches.
pub struct Bundles {
    /// The bundles in priority order: most-preferred first, built-in
    /// English baseline last. Every lookup walks this front to back and
    /// takes the first bundle that defines the key.
    chain: Vec<Localization>,
}

/// Push `bundle` onto `chain` unless a bundle with the same tag is
/// already present.
///
/// Locale matching is many-to-one — `de-AT` and `de-DE` both fall back
/// to the same single `de` bundle — so without this the chain would
/// carry one bundle once per preference that reached it.
fn push_unique(chain: &mut Vec<Localization>, bundle: &Localization) {
    if !chain.iter().any(|b| b.tag == bundle.tag) {
        chain.push(bundle.clone());
    }
}

/// The successively shorter forms of a BCP 47 tag, most specific first:
/// `zh-Hans-CN` → `["zh-Hans", "zh"]`, `de` → `[]`.
///
/// This is the standard truncate-from-the-right lookup that language
/// APIs assume, and it is what makes a `zh-Hans` bundle reachable from a
/// `zh-Hans-CN` machine. Case is deliberately left alone: both APIs
/// return canonically-cased tags (`pt-BR`, `zh-Hans`) and
/// `tag_from_path` preserves the filename's casing, so an exact `find`
/// is right. A bundle filed as `pt-br` will not match a `pt-BR`
/// machine — which is the same trade every toolchain makes, and a
/// mismatch there is a typo worth surfacing rather than hiding.
fn parent_tags(tag: &str) -> Vec<String> {
    let mut parts: Vec<&str> = tag.split('-').collect();
    let mut out = Vec::new();
    while parts.len() > 1 {
        parts.pop();
        out.push(parts.join("-"));
    }
    out
}

impl Bundles {
    /// Build a lookup chain from the payload's embedded bundles, ordered
    /// against this machine's detected language preferences, plus the
    /// always-present built-in English baseline.
    ///
    /// The returned `Bundles` is empty only if `payload_bundles` is
    /// empty *and* the built-in baseline failed to parse (a build-time
    /// problem, not a runtime one — `localization.rs` tests catch it). In
    /// every shipped build the baseline parses, so callers always get at
    /// least one bundle.
    pub fn load(payload_bundles: &[Localization]) -> Self {
        Self::load_for(&detect_locales(), payload_bundles)
    }

    /// The host-independent half of [`Bundles::load`].
    ///
    /// Taking the preferences as an argument is what makes the chain
    /// testable: `load` reads the *test machine's* locale, so a test
    /// asserting "a German bundle wins" would pass on a German box and
    /// fail everywhere else. Tests pass an explicit preference list and
    /// get the same answer on every host.
    pub fn load_for(preferences: &[String], payload_bundles: &[Localization]) -> Self {
        let mut chain: Vec<Localization> = Vec::with_capacity(payload_bundles.len() + 1);

        // 1. Exact tag matches, in the user's stated preference order.
        for tag in preferences {
            if let Some(b) = payload_bundles.iter().find(|b| &b.tag == tag) {
                push_unique(&mut chain, b);
            }
        }

        // 2. Truncated forms, most specific first: `zh-Hans-CN` yields
        //    `zh-Hans` then `zh`. Each preference's parents are walked
        //    before moving to the next language, so a machine set to
        //    `[de-AT, en-US]` resolves its own German before any English.
        for tag in preferences {
            for parent in parent_tags(tag) {
                if let Some(b) = payload_bundles.iter().find(|b| &b.tag == &parent) {
                    push_unique(&mut chain, b);
                }
            }
        }

        // 3. Built-in English baseline at the END so it's the lowest
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
    set_bundles_for(&detect_locales(), payload_bundles)
}

/// Replace the active bundle chain with an **explicit** preference
/// list, and bump [`generation`].
///
/// [`set_bundles`] is the production entry point and correctly filters
/// against `detect_locales()`. This variant is for a caller that has
/// already decided which language it wants regardless of the host —
/// which is exactly what `snug_preview`'s language dropdown is. Picking
/// `de` on an English machine has to show German; routed through
/// `set_bundles` the chosen bundle is filtered straight back out (the
/// machine wants `en`), leaving the chain as the built-in baseline and
/// every dialog silently English no matter what you selected.
pub fn set_bundles_for(preferences: &[String], payload_bundles: &[Localization]) {
    let next = Bundles::load_for(preferences, payload_bundles);
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
/// and returning a bare `runtime.prompt.title` is not.
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
/// Substitute `{name}` slots in `template`, resolving each name through
/// `resolve`.
///
/// ## `\{` is a literal brace
///
/// A bundle author who needs to *show* a slot rather than fill one --
/// a book scaffold documenting which string takes which value, say --
/// writes `\{size_mb}` and gets `{size_mb}` back, unfilled. Only the
/// opening brace needs escaping; the closing `}` is ordinary text.
///
/// This is why substitution cannot be a loop of `str::replace`: the
/// substring `{size_mb}` is present *inside* `\{size_mb}`, so a
/// literal replace would fill it and the escape would mean nothing.
///
/// Unknown names are copied through verbatim rather than dropped, so a
/// typo leaves the text visible instead of silently swallowing it.
///
/// When [`replace_tokens`] is off the slot branch is skipped entirely, which
/// leaves `{name}` exactly as written — and still unescapes `\{name}` to
/// `{name}`, because that branch is not gated. That is deliberate: the
/// escape is about what the *user* sees, so it must be honoured in both
/// modes. Only the filling stops.
///
/// One implementation on purpose. `dialogs::fill` (named) and
/// `t_positional` (indexed) both route through here, and so does
/// `prompt_window::substitute`. When each did its own `replace`, the
/// same bundle rendered three different ways depending on which dialog
/// opened it, and an escape honoured by one would have been silently
/// ignored by the others -- the build and snug_preview disagreeing on
/// the same string.
pub(crate) fn substitute_slots(
    template: &str,
    mut resolve: impl FnMut(&str) -> Option<String>,
) -> String {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // `\{` -> literal `{`, resuming after it so the name that
        // follows is copied as ordinary text.
        if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            out.push('{');
            i += 2;
            continue;
        }
        if bytes[i] == b'{' && replace_tokens() {
            if let Some(close) = template[i + 1..].find('}') {
                let name = &template[i + 1..i + 1 + close];
                if let Some(value) = resolve(name) {
                    out.push_str(&value);
                    i = i + close + 2;
                    continue;
                }
            }
        }
        // Copy one whole char, so a multi-byte sequence is never split.
        let c = template[i..].chars().next().unwrap_or('\u{fffd}');
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// This helper does a single linear pass through the template
/// replacing each `{i}` with `values[i]`. Unknown indices (e.g.
/// `{3}` when only two values were passed) are left as-is so a typo
/// is visible. `\{i}` is a literal `{i}`, not a slot -- see
/// [`substitute_slots`].
pub fn t_positional(key: &str, values: &[&str]) -> String {
    substitute_slots(&lookup(key), |name| {
        name.parse::<usize>()
            .ok()
            .and_then(|i| values.get(i))
            .map(|v| (*v).to_string())
    })
}

/// The user's language preferences as BCP 47 tags, most-preferred
/// first — e.g. `["de-AT", "en-US"]`.
///
/// Never empty: a platform with no detection (Linux, where there are no
/// dialogs to localise yet) or a failed lookup degrades to `["en"]`,
/// which resolves to the built-in baseline plus any user-supplied `en`
/// bundle. A launcher that cannot tell what language to speak still has
/// to start and still has to say something.
///
/// This is deliberately the OS's own answer rather than a computed one.
/// Windows and macOS already agree on what a user's language list
/// means, and re-deriving it from `LANG` or a registry key would be a
/// worse version of the same thing that silently disagrees with the OS
/// on the machines that matter.
pub fn detect_locales() -> Vec<String> {
    let mut out: Vec<String> = platform_locales()
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .collect();
    if out.is_empty() {
        out.push(DEFAULT_TAG.to_string());
    }
    out
}

/// Windows: the user's ordered UI-language preference list.
///
/// `GetUserPreferredUILanguages` is the right call over its two siblings
/// because it is the only one that returns an *ordered list*: a machine
/// configured `[de-AT, en-US]` is stating a fallback order, and a lookup
/// chain needs exactly that. `GetUserDefaultUILanguage` collapses that
/// to a single LANGID and loses it.
///
/// `GetUserDefaultLocaleName` is the second choice, not the first: it
/// returns the user's *regional* locale (`en-AU` for Australian
/// English), which is the wrong notion for UI text — but a far better
/// guess than giving up and defaulting to English.
#[cfg(windows)]
fn platform_locales() -> Vec<String> {
    // `MUI_LANGUAGE` from winnls.h — "the UI language list", as opposed
    // to `MUI_SEARCH_LANGUAGE` (the fallback list for files that carry no
    // MUI resources). windows-sys exports the function but not the flag
    // under a name, so the value is spelled out here.
    const MUI_LANGUAGE: u32 = 0;
    // `LOCALE_NAME_MAX_LENGTH` from winnls.h. Windows bounds each entry
    // of the preferred list at this length; it is also the size of the
    // buffer `GetUserDefaultLocaleName` documents as sufficient.
    const LOCALE_NAME_MAX_LENGTH: i32 = 85;

    let langs = preferred_ui_languages(MUI_LANGUAGE, LOCALE_NAME_MAX_LENGTH);
    if !langs.is_empty() {
        return langs;
    }

    let mut buf = [0u16; LOCALE_NAME_MAX_LENGTH as usize];
    // SAFETY: `buf` holds exactly `LOCALE_NAME_MAX_LENGTH` UTF-16 units
    // and the capacity argument matches, which is this API's only
    // precondition.
    let n = unsafe {
        windows_sys::Win32::Globalization::GetUserDefaultLocaleName(
            buf.as_mut_ptr(),
            LOCALE_NAME_MAX_LENGTH,
        )
    };
    if n <= 0 {
        return Vec::new();
    }
    // `n` counts the terminating NUL — MSDN is explicit about this — so
    // slicing to `n` keeps the NUL and yields a tag like `"en-AU\0"`,
    // which then matches no bundle and truncates to nothing. It degrades
    // quietly to English rather than erroring, so it is worth the
    // explicit `- 1`.
    let name = String::from_utf16_lossy(&buf[..n as usize - 1]);
    if name.is_empty() {
        return Vec::new();
    }
    vec![name]
}

/// The two-call buffer dance `GetUserPreferredUILanguages` requires: the
/// first call asks how long the list is, the second fills it.
///
/// Returns an empty list on any failure rather than propagating one — a
/// launcher with no language detection still has to start, and
/// [`detect_locales`] carries the fallback for exactly this case.
#[cfg(windows)]
fn preferred_ui_languages(flags: u32, max_len: i32) -> Vec<String> {
    use windows_sys::Win32::Globalization::GetUserPreferredUILanguages;

    let mut num_languages: u32 = 0;
    let mut sizing_len: u32 = 0;
    // Sizing call: null buffer. On success `num_languages` comes back
    // holding how many tags the user has queued — which is *not* the
    // same as the character count of the buffer they need. The length
    // out-parameter has to be a real pointer here; passing null for it
    // makes the call fail and leaves `num_languages` at 0, which is
    // indistinguishable from "no languages" downstream.
    // SAFETY: a null buffer is the documented sizing form, and both
    // out-parameters are live `u32`s.
    unsafe {
        GetUserPreferredUILanguages(
            flags,
            &mut num_languages,
            std::ptr::null_mut(),
            &mut sizing_len,
        )
    };
    if num_languages == 0 {
        return Vec::new();
    }

    // The two out-parameters carry different quantities and both are
    // in/out, which is the one genuinely easy thing to get wrong here:
    // `pulNumLanguages` takes the *language count* in and returns the
    // number of strings written, while `pcchLanguagesBuffer` takes the
    // buffer's capacity in *characters* and returns the characters
    // written. Handing it a 0 — or a language count — fails with
    // ERROR_INSUFFICIENT_BUFFER, and because this function swallows
    // failures the symptom is not an error at all: detection quietly
    // degrades to the single-locale path and a multilingual machine
    // only ever hears about one of its languages.
    let capacity = num_languages as usize * max_len.max(1) as usize;
    let mut buf = vec![0u16; capacity];
    let mut written: u32 = capacity as u32;
    // SAFETY: `buf` holds exactly `capacity` UTF-16 units, the
    // documented worst case for `num_languages` tags.
    let ok =
        unsafe { GetUserPreferredUILanguages(flags, &mut num_languages, buf.as_mut_ptr(), &mut written) };
    if ok == 0 || written == 0 {
        return Vec::new();
    }
    buf.truncate(written as usize);

    // The whole list arrives as one buffer of NUL-separated,
    // double-NUL-terminated strings, so splitting on NUL and dropping
    // the empties the trailing terminator leaves behind is the parse.
    String::from_utf16_lossy(&buf)
        .split('\0')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// macOS: `+[NSLocale preferredLanguages]`, the same ordered list macOS
/// uses to pick an app's `.lproj` — which is exactly the answer we want,
/// since that is the list the user's own Applications are resolving
/// against.
#[cfg(target_os = "macos")]
fn platform_locales() -> Vec<String> {
    use objc2_foundation::NSLocale;
    NSLocale::preferredLanguages()
        .iter()
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
        .collect()
}

/// No detection on this platform. `platform::other` is a deliberate no-op
/// stub for the same reason — Linux has no launcher dialogs to localise
/// yet — so this keeps the shape consistent rather than leaving a hole
/// the next platform has to special-case.
#[cfg(not(any(windows, target_os = "macos")))]
fn platform_locales() -> Vec<String> {
    Vec::new()
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

    /// Serialises tests that swap the process-global bundle chain.
///
/// `set_bundles_for` installs into `BUNDLES`, which every other
/// `lookup`-based test reads, and cargo runs tests in threads — so
/// without this a chain swap lands mid-run and a sibling test asserts
/// against the wrong language. Same pattern as `appkit`'s test seam.
static CHAIN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn the_preview_switch_shows_slots_as_authored() {
    // The switch is process-global, so this needs the same guard the
    // bundle tests use: without it a parallel test that expects
    // substitution would see whatever this one last set.
    let _guard = CHAIN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let previous = replace_tokens();

    set_replace_tokens(false);
    let raw = substitute_slots(r"\{size_mb} {size_mb}", |n| Some(n.to_uppercase()));
    // The escaped half loses only its backslash. That is the whole
    // reason the `\{` branch is not gated: the escape is about what the
    // user sees, so it must be honoured whether or not tokens fill.
    assert_eq!(raw, "{size_mb} {size_mb}", "escaped slot must still unescape");

    set_replace_tokens(true);
    let filled = substitute_slots("{size_mb}", |n| Some(n.to_uppercase()));
    assert_eq!(filled, "SIZE_MB");

    set_replace_tokens(previous);
}

#[test]
fn replacing_is_on_by_default() {
    // A shipped launcher never calls `set_replace_tokens`, so if the
    // default ever flips, a user's dialogs silently stop interpolating
    // and `{size_mb}` appears in front of them.
    assert!(replace_tokens(), "the product default must stay 'replace'");
}

#[test]
    fn set_bundles_for_overrides_the_machine_locale() {
        let _guard = CHAIN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // The regression behind `snug_preview`'s language dropdown
        // looking dead: `set_bundles` filters against
        // `detect_locales()`, so on an English host a bundle tagged
        // `de` is dropped and every dialog stays English. The preview
        // needs the selection to win regardless of host.
        //
        // This asserts the *mechanism* rather than the symptom, so it
        // holds on a German machine too — which a test that just
        // checked "German text appears" would not.
        let de = bundle("de", &[("err.jvm_not_found", "Keine JVM gefunden.")]);
        set_bundles_for(&["de".to_string()], std::slice::from_ref(&de));
        assert_eq!(lookup("err.jvm_not_found"), "Keine JVM gefunden.");

        // And the baseline still backs untranslated keys.
        assert_eq!(
            lookup("err.java_exception"),
            "Java `main` threw an exception: {0}"
        );

        // Restore the default chain so a later test cannot inherit
        // German, even though the lock is released at the end.
        set_bundles(&[]);
    }

    #[test]
    fn load_appends_user_bundles_above_baseline() {
        let de = bundle("de", &[("err.jvm_not_found", "Keine JVM gefunden.")]);
        let b = Bundles::load_for(&["de".to_string()], &[de]);
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
        let b = Bundles::load_for(&["de".to_string()], &[de]);
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
        // Preferred tag is `en-US`, so this also exercises the
        // truncation path reaching the user's own `en` bundle.
        let cli_en = bundle("en", &[("err.foo", "from CLI")]);
        let b = Bundles::load_for(&["en-US".to_string()], &[cli_en]);
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
        // a one-shot per-process), and pin the preference list so the
        // assertion cannot depend on the test machine's own locale.
        let b = Bundles::load_for(
            &["en-US".to_string()],
            &[bundle(
                "en",
                &[("err.demo", "Found Java {min_java}, need {wanted}")],
            )],
        );
        let resolved = b.raw_lookup("err.demo");
        let out = fill_placeholders(&resolved, &[("min_java", "17"), ("wanted", "25")]);
        assert_eq!(out, "Found Java 17, need 25");
    }

    #[test]
    fn detect_locales_always_yields_a_usable_tag() {
        let langs = detect_locales();
        assert!(!langs.is_empty(), "detection must degrade, never to nothing");
        for tag in &langs {
            assert!(!tag.trim().is_empty());
            // `_` is the POSIX/`LANG` spelling of the same idea. Neither
            // `GetUserPreferredUILanguages` nor `+[NSLocale
            // preferredLanguages]` emits it, so this is a real assertion
            // about the API contract rather than a defensive one — and
            // `parent_tags` splits on `-`, so an underscore here would
            // quietly defeat every fallback.
            assert!(!tag.contains('_'), "not a BCP 47 tag: {tag}");
        }
    }

    #[test]
    fn parent_tags_truncates_from_the_right() {
        assert_eq!(parent_tags("de"), Vec::<String>::new());
        assert_eq!(parent_tags("de-AT"), vec!["de".to_string()]);
        assert_eq!(parent_tags("pt-BR"), vec!["pt".to_string()]);
        assert_eq!(
            parent_tags("zh-Hans-CN"),
            vec!["zh-Hans".to_string(), "zh".to_string()],
            "a script subtag must survive the first truncation"
        );
    }

    #[test]
    fn exact_tag_wins_over_its_own_parent() {
        // A `de-AT` machine shipping both `de-AT` and `de` must read the
        // regional bundle first; `de` is the fallback for whatever the
        // regional one left untranslated.
        let generic = bundle("de", &[("k", "generic"), ("k2", "generic2")]);
        let regional = bundle("de-AT", &[("k", "regional")]);
        let b = Bundles::load_for(&["de-AT".to_string()], &[generic, regional]);
        assert_eq!(
            b.tags(),
            vec!["de-AT".to_string(), "de".to_string(), "en".to_string()]
        );
        assert_eq!(b.raw_lookup("k"), "regional");
        assert_eq!(
            b.raw_lookup("k2"),
            "generic2",
            "keys the regional bundle omits fall back to its parent"
        );
    }

    #[test]
    fn user_preference_order_beats_payload_order() {
        // The entire reason for asking for an ordered *list*. Payload
        // order is `fr` before `de`; the user's order is `de` before
        // `fr`, and the user's order is the one that counts.
        let fr = bundle("fr", &[("k", "francais")]);
        let de = bundle("de", &[("k", "deutsch")]);
        let prefs = vec!["de-AT".to_string(), "fr-FR".to_string()];
        let b = Bundles::load_for(&prefs, &[fr, de]);
        assert_eq!(b.tags(), vec!["de".to_string(), "fr".to_string(), "en".to_string()]);
        assert_eq!(b.raw_lookup("k"), "deutsch");
    }

    #[test]
    fn script_subtag_bundle_is_reachable_from_a_regional_locale() {
        // The case primary-subtag-only matching gets wrong: `zh` is a
        // poor stand-in for `zh-Hans` on a Simplified Chinese machine.
        let script = bundle("zh-Hans", &[("k", "simplified")]);
        let b = Bundles::load_for(&["zh-Hans-CN".to_string()], &[script]);
        assert_eq!(b.raw_lookup("k"), "simplified");
    }

    #[test]
    fn bundles_the_user_cannot_read_are_left_out_of_the_chain() {
        // Regression guard for the old payload-order chain, which put
        // *every* bundle in the lookup and so let `de` shadow `fr` for
        // every user regardless of their locale.
        let de = bundle("de", &[("err.jvm_not_found", "Keine JVM gefunden.")]);
        let b = Bundles::load_for(&["en-US".to_string()], &[de]);
        assert!(
            !b.tags().contains(&"de".to_string()),
            "unreadable bundle leaked into the chain: {:?}",
            b.tags()
        );
        assert_eq!(
            b.raw_lookup("err.jvm_not_found"),
            "Could not locate a Java {min_java}+ JVM in any of the configured discovery locations"
        );
    }

    #[test]
    fn a_single_locale_payload_still_works_on_an_english_machine() {
        // A `--localization de.txt` build whose only non-English bundle
        // matches nothing here: English is still in the chain, so every
        // key resolves to real copy rather than a raw key string.
        let de = bundle("de", &[("err.jvm_not_found", "Keine JVM gefunden.")]);
        let b = Bundles::load_for(&["en-GB".to_string()], &[de]);
        assert_eq!(b.tags(), vec!["en".to_string()]);
        assert_eq!(b.raw_lookup("splash.title"), "Snug Splash");
    }

    #[test]
    fn chain_has_no_duplicate_tags_when_preferences_share_a_parent() {
        // `de-AT` and `de-DE` both truncate to `de`; without the dedup
        // the same bundle would be walked twice per lookup.
        let de = bundle("de", &[("k", "deutsch")]);
        let prefs = vec!["de-AT".to_string(), "de-DE".to_string()];
        let b = Bundles::load_for(&prefs, &[de]);
        assert_eq!(b.tags(), vec!["de".to_string(), "en".to_string()]);
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
            "runtime.prompt.title",
            "runtime.prompt.heading",
            "runtime.prompt.subheading",
            "runtime.prompt.content",
            "runtime.prompt.info_heading",
            "runtime.prompt.info_subtext",
            "runtime.prompt.info_subtext_2",
            "runtime.prompt.button_download",
            "runtime.prompt.button_cancel",
            "runtime.metadata_failed.title",
            "runtime.metadata_failed.heading",
            "runtime.metadata_failed.subheading",
            "runtime.metadata_failed.content",
            "runtime.metadata_failed.info_heading",
            "runtime.metadata_failed.info_subtext",
            "runtime.metadata_failed.info_subtext_2",
            "runtime.metadata_failed.button_open_browser",
            "runtime.metadata_failed.button_cancel",
            "runtime.progress.title",
            "runtime.progress.content",
            "runtime.progress.heading",
            "runtime.progress.subheading",
            "runtime.progress.pct_label",
            "runtime.progress.phase_label",
            "runtime.progress.detail_with_size",
            "runtime.progress.detail_no_size",
            "runtime.progress.detail_eta_seconds",
            "runtime.progress.detail_eta_second",
            "runtime.progress.detail_eta_done",
            "runtime.progress.info_heading",
            "runtime.progress.info_subtext",
            "runtime.progress.info_subtext_2",
            "runtime.progress.cancel_button_during_download",
            "runtime.progress.arch_macos_arm64",
            "runtime.progress.arch_macos_x86_64",
            "runtime.progress.arch_windows_x86_64",
            "runtime.failure.title",
            "runtime.failure.heading",
            "runtime.failure.subheading",
            "runtime.failure.content",
            "runtime.failure.info_heading",
            "runtime.failure.info_subtext",
            "runtime.failure.info_subtext_2",
            "runtime.failure.button_label",
            "runtime.retry.title",
            "runtime.retry.heading",
            "runtime.retry.subheading",
            "runtime.retry.content",
            "runtime.retry.info_heading",
            "runtime.retry.info_subtext",
            "runtime.retry.info_subtext_2",
            "runtime.retry.button_retry",
            "runtime.retry.button_cancel",
            "generic.error_dialog_ok",
            "generic.info_dialog_continue",
            "launcher.error.title",
            "launcher.error.heading",
            "launcher.error.subheading",
            "launcher.error.content",
            "launcher.error.info_heading",
            "launcher.error.info_subtext",
            "launcher.error.info_subtext_2",
            "launcher.error.button_exit",
            "launcher.error.button_update",
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

    /// The checked-in translation bundles must track the baseline.
    ///
    /// `test/localisations/*.txt` are hand-maintained copies of the built-in
    /// English, not fixtures anything reads at runtime — they exist so a
    /// bundle can be dropped into `--localization <path>` and eyeballed.
    /// Nothing compared them, which is how `fr` and `de` drifted a full
    /// release behind (still telling users to visit adoptium.net) without
    /// anything going red. Every bundle in the directory is checked, so
    /// adding `es.txt` is covered the day it lands rather than the day
    /// someone remembers.
    ///
    /// Deliberately only the two things a machine can judge: the key set
    /// and the placeholder set. Whether a translation is *accurate* needs
    /// someone who reads the language, and that is not attempted here.
    ///
    /// Placeholder drift is the half that bites. `fill` leaves unknown
    /// placeholders as-is by design — "so a typo doesn't silently swallow
    /// text" — so a bundle carrying `{major}` for a key the baseline no
    /// longer supplies `major` for renders a literal `{major}` in a dialog.
    /// And it renders nothing at all until someone tidies the call site,
    /// long after the drift appeared. Equality is checked in both
    /// directions: a bundle *missing* a slot the baseline has is the
    /// quieter failure, reading as a silently wrong sentence with no braces
    /// to give it away.
    ///
    /// The scanner itself is `snug_format::placeholders`, shared with the
    /// builder's build-time warning — one definition of "which slots does
    /// this value use", so the test and the warning cannot disagree about
    /// what a placeholder even is.
    #[test]
    fn the_checked_in_bundles_track_the_baseline() {
        use std::collections::BTreeMap;
        use std::path::{Path, PathBuf};

        let slots = snug_format::placeholders;

        let baseline_bundle =
            Localization::parse(DEFAULT_TAG, DEFAULT_EN_TEXT).expect("built-in baseline parses");
        let baseline: BTreeMap<&str, &str> = baseline_bundle
            .entries
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test/localisations");
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
            .map(|e| e.expect("dir entry").path())
            .filter(|p| {
                let n = p.file_name().unwrap_or_default().to_string_lossy();
                n.starts_with("snug-localisations.") && n.ends_with(".txt")
            })
            .collect();
        // Sorted so a failure names the same bundle first every run.
        paths.sort();

        let mut checked = 0usize;
        for path in &paths {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let tag = name
                .trim_start_matches("snug-localisations.")
                .trim_end_matches(".txt")
                .to_string();
            let text = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
            let parsed =
                Localization::parse(&tag, &text).unwrap_or_else(|e| panic!("{name}: {e}"));
            let copy: BTreeMap<&str, &str> =
                parsed.entries.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

            let missing: Vec<&str> = baseline
                .keys()
                .filter(|k| !copy.contains_key(*k))
                .copied()
                .collect();
            assert!(
                missing.is_empty(),
                "{name} is missing {} baseline key(s), so end users fall back to \
                 English for them: {missing:?}",
                missing.len()
            );
            let extra: Vec<&str> = copy
                .keys()
                .filter(|k| !baseline.contains_key(*k))
                .copied()
                .collect();
            assert!(
                extra.is_empty(),
                "{name} has {} key(s) the baseline does not define, so nothing will \
                 ever look them up: {extra:?}",
                extra.len()
            );

            let mismatched: Vec<String> = baseline
                .iter()
                .filter_map(|(k, v)| {
                    let want = slots(v);
                    let got = slots(copy[*k]);
                    (want != got).then(|| format!("\n    {k}: baseline {want:?} vs {got:?}"))
                })
                .collect();
            assert!(
                mismatched.is_empty(),
                "{name} disagrees with the baseline on {} placeholder set(s):{}\n  \
                 A slot the baseline dropped renders as literal `{{name}}` once the call \
                 site stops supplying it. A slot it gained leaves the sentence silently \
                 wrong, with no braces to give it away.",
                mismatched.len(),
                mismatched.concat()
            );
            checked += 1;
        }
        assert!(
            checked > 0,
            "no snug-localisations.*.txt found in {} — the drift this guards against \
             would be invisible again",
            dir.display()
        );
    }
}
