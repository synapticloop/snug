//! Build-time handling of user-supplied `snug-localisations.<tag>.txt` files.
//!
//! Each file is a flat `key = value` text file in Java-`.properties`
//! style. The CLI parses every file the user passes via `--localization`
//! (or via `snug.options`), bundles them with the always-embedded built-in
//! English baseline, and writes the result into [`snug_format::SnugPayload`].
//!
//! ## Filename convention
//!
//! The CLI extracts the BCP 47 locale tag from the **filename** —
//
// `snug-localisations.<tag>.txt`. This matches the runtime contract:
// the launcher detects the user's Windows UI language and looks up a
//! bundle whose `tag` matches. If the user's file isn't named correctly,
//! we refuse to embed it (no silent fallback to "en" or similar).
//!
//! ## Built-in baseline
//!
//! Every build embeds at least the built-in English baseline
//! (`DEFAULT_EN_TAG`, parsed at startup from the constant string below).
//! If the user passes `--localization foo.txt` but never passes one for
//! `en`, the built-in baseline is still embedded — so the launcher has
//! a working fallback even when every locale-specific bundle is absent.
//!
//! Passing a bundle of your own tagged `en` *replaces* the built-in
//! baseline in place rather than colliding with it: shipping your own
//! English copy is legitimate, and the scaffolded `localisations/`
//! directory always contains one. A second `en` in the same build is
//! still an error (see [`ensure_unique_tags`]).
//!
//! ## Validation
//!
//! Build-time validation is intentionally non-fatal for missing keys:
//! we emit a warning per missing key on stderr, so the user notices but
//! the build still succeeds (the launcher falls back to the built-in
//! English for any key the user's bundle doesn't define).

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use snug_format::Localization;

/// Canonical BCP 47 tag for the built-in English baseline bundle.
///
/// The launcher includes this string at compile time, parses it at
/// startup as the always-present fallback, and the CLI embeds it into
/// every build so end users always see English even if every other
/// bundle is missing.
pub use snug_format::DEFAULT_EN_TAG;

/// Built-in English baseline text, embedded into every payload.
///
/// Re-exported from `snug-format`, which owns
/// `assets/snug-localisations.en.txt` and holds the single
/// `include_str!` of it in the workspace. The launcher re-exports the
/// same constant for its in-binary fallback, so the copy baked into
/// the payload and the copy compiled into the launcher are the same
/// bytes by construction rather than by convention.
pub use snug_format::DEFAULT_EN_TEXT;

/// Build a `snug-localisations` filename from a locale tag.
///
/// E.g. `"en"` → `"snug-localisations.en.txt"`,
/// `"en-US"` → `"snug-localisations.en-US.txt"`.
pub use snug_format::expected_filename;

/// Extract the BCP 47 tag from a localization filename.
///
/// The expected pattern is `snug-localisations.<tag>.txt`. We strip
/// the `snug-localisations.` prefix and `.txt` suffix; whatever's left
/// is the tag. `pt-BR.txt`, `zh-Hans.txt`, etc. all work. We accept
/// `snug-localisations.en.txt` (tag = `en`) but reject
/// `my-translations.txt` (no recognised prefix).
pub use snug_format::tag_from_path;

/// clap value-parser for the `--localization` flag.
///
/// Validates that the file exists, is named `snug-localisations.<tag>.txt`,
/// and parses cleanly (a malformed file is rejected up front so the user
/// sees a clear error rather than a panic at runtime). The result is
/// the same path the caller passed in — `clap` only cares that we
/// didn't reject it.
pub fn validate_localization_path(s: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(s);
    // Accept either a single file or a directory. A directory entry is
    // expanded by `collect()` at build time; here we just confirm it
    // exists so the user gets a clear error rather than a silent miss.
    let meta = std::fs::metadata(&path)
        .map_err(|e| format!("reading {}: {e}", path.display()))?;
    if meta.is_dir() {
        return Ok(path);
    }
    if !meta.is_file() {
        return Err(format!("not a file or directory: {}", path.display()));
    }
    if tag_from_path(&path).is_none() {
        return Err(format!(
            "expected filename like `snug-localisations.<tag>.txt` (e.g. \
             `snug-localisations.en.txt`), got `{}`",
            path.display()
        ));
    }
    // Pre-parse so the user sees a line-numbered error at the CLI
    // boundary instead of at build time.
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("reading {}: {e}", path.display()))?;
    let tag = tag_from_path(&path).expect("checked above");
    Localization::parse(&tag, &text)
        .map_err(|e| format!("parsing {}: {e}", path.display()))?;
    Ok(path)
}

/// Load, parse, and bundle every user-supplied localization file plus
/// the always-present English baseline.
///
/// The returned vector is in **insertion order** with the English
/// baseline first; the launcher respects payload order during merge
/// so this means the baseline has the lowest priority. Later bundles
/// (in CLI / `snug.options` order) override earlier ones at the key
/// level, with the user's full BCP 47 tag winning over the built-in
/// baseline at runtime.
///
/// A user bundle tagged `en` takes over the baseline slot instead of
/// being appended as a second `en`; see the module docs. A *second*
/// `en` is left in the list so [`ensure_unique_tags`] rejects it.
pub fn collect(user_paths: &[PathBuf]) -> Result<Vec<Localization>> {
    // The English baseline always occupies slot 0 so it stays the
    // lowest priority in the launcher's merge chain. A user-supplied
    // `en` bundle takes that slot over (see below) rather than being
    // appended as a second `en`.
    let mut baseline_override: Option<Localization> = None;
    let mut bundles: Vec<Localization> = Vec::new();

    // 1. Expand user inputs. A directory entry is replaced with the
    //    sorted list of `snug-localisations.<tag>.txt` files directly
    //    inside it (subdirectories are skipped — top-level scan only).
    //    Explicit-file entries pass through unchanged, so callers can
    //    freely mix `--localization <dir>` and `--localization <file>`.
    //    Every file inside a passed directory MUST match the pattern;
    //    a stray file is almost always a typo (e.g. `.bak`, wrong case)
    //    so we fail the build rather than silently skip it.
    let expanded = expand_user_paths(user_paths)?;

    for path in &expanded {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading localization file {}", path.display()))?;
        let tag = tag_from_path(path).with_context(|| {
            format!(
                "extracting locale tag from filename {} (expected `snug-localisations.<tag>.txt`)",
                path.display()
            )
        })?;
        let bundle = Localization::parse(&tag, &text)
            .with_context(|| format!("parsing localization file {}", path.display()))?;

        // A user-supplied `en` bundle *replaces* the built-in
        // baseline instead of colliding with it. Shipping your own
        // English copy is a legitimate thing to want — you may reword
        // the launcher chrome for your product — and the scaffolded
        // `localisations/` directory (`snug --init-localizations`)
        // always contains one, so the naive append would make the
        // obvious `--localization localisations` fail with a
        // duplicate-tag error. A *second* `en` is still pushed and
        // still rejected by `ensure_unique_tags`: that's a genuine
        // ambiguity, not a baseline override.
        if tag == DEFAULT_EN_TAG && baseline_override.is_none() {
            baseline_override = Some(bundle);
            continue;
        }
        bundles.push(bundle);
    }

    // 2. Slot 0: the user's English bundle if they supplied one,
    //    otherwise the built-in baseline.
    let baseline = match baseline_override {
        Some(bundle) => bundle,
        None => Localization::parse(DEFAULT_EN_TAG, DEFAULT_EN_TEXT)
            .context("parsing built-in English baseline")?,
    };
    let mut out = vec![baseline];
    out.extend(bundles);

    // 3. Build-time warning: a user bundle is "missing" any baseline key
    //    we couldn't find. Don't fail the build — the launcher falls
    //    back to the built-in English for those — but make it loud so
    //    the user notices.
    warn_on_missing_keys(&out);

    Ok(out)
}

/// Walk `user_paths`, expanding any directory entries into the sorted
/// list of `snug-localisations.<tag>.txt` files directly inside.
///
/// The walking itself lives in `snug_format::discover_localization_files`
/// so `snug-preview` resolves `--localisation` the same way this CLI
/// resolves `--localization`; this wrapper only adds the `anyhow`
/// context the build path wants.
fn expand_user_paths(user_paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    snug_format::discover_localization_files(user_paths).map_err(anyhow::Error::from)
}

/// Emit a stderr warning for every key in the built-in English
/// baseline that no user-supplied bundle covers. The launcher's
/// fallback chain will use English for the gap, so this is advisory
/// rather than fatal.
fn warn_on_missing_keys(bundles: &[Localization]) {
    let baseline = &bundles[0];
    let baseline_keys: std::collections::HashSet<&str> = baseline
        .entries
        .iter()
        .map(|(k, _)| k.as_str())
        .collect();

    let user_bundles = &bundles[1..];
    if user_bundles.is_empty() {
        // No user bundles → no key coverage to compare against.
        return;
    }

    for bundle in user_bundles {
        let covered: std::collections::HashSet<&str> = bundle
            .entries
            .iter()
            .map(|(k, _)| k.as_str())
            .collect();
        let missing: Vec<&str> = baseline_keys.difference(&covered).copied().collect();
        if missing.is_empty() {
            continue;
        }
        // Sort for stable, easy-to-diff output.
        let mut missing = missing;
        missing.sort_unstable();
        eprintln!(
            "snug: warning: localization file `{}` (tag `{}`) is missing {} key(s) compared to the built-in English baseline:",
            bundle
                .entries
                .first()
                .map(|_| bundle.tag.as_str())
                .unwrap_or("?"),
            bundle.tag,
            missing.len(),
        );
        for key in missing {
            eprintln!("snug:   - {key}");
        }
        eprintln!(
            "snug:   (end users will see the built-in English text for these keys; \
             update the bundle to localise them)"
        );
    }
}

/// Reject duplicate tags across the supplied bundle list.
///
/// Two bundles tagged `en-US` would silently shadow each other in the
/// launcher's lookup chain — the later one always wins. Better to fail
/// at build time so the user knows to drop one.
pub fn ensure_unique_tags(bundles: &[Localization]) -> Result<()> {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for bundle in bundles {
        if !seen.insert(bundle.tag.as_str()) {
            bail!(
                "duplicate localization tag `{}` — pass each tag exactly once",
                bundle.tag
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // `Path` is only referenced by the tests now that `tag_from_path`
    // itself lives in `snug-format`.
    use std::path::Path;

    fn tmpdir() -> PathBuf {
        let base = std::env::temp_dir();
        // The counter is load-bearing: pid + nanos is not unique when
        // tests run in parallel on a coarse clock (macOS). See the same
        // fix in options_file's tempdir().
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = format!(
            "snug-localize-{}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let dir = base.join(unique);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn tag_from_path_accepts_canonical_names() {
        assert_eq!(
            tag_from_path(Path::new("snug-localisations.en.txt")),
            Some("en".to_string())
        );
        assert_eq!(
            tag_from_path(Path::new("snug-localisations.en-US.txt")),
            Some("en-US".to_string())
        );
        assert_eq!(
            tag_from_path(Path::new("snug-localisations.pt-BR.txt")),
            Some("pt-BR".to_string())
        );
        assert_eq!(
            tag_from_path(Path::new("snug-localisations.zh-Hans.txt")),
            Some("zh-Hans".to_string())
        );
    }

    #[test]
    fn tag_from_path_rejects_non_canonical_names() {
        assert_eq!(tag_from_path(Path::new("localisations.en.txt")), None);
        assert_eq!(tag_from_path(Path::new("snug-localisations.txt")), None);
        assert_eq!(tag_from_path(Path::new("snug-localisations..txt")), None);
        assert_eq!(tag_from_path(Path::new("translations.txt")), None);
    }

    #[test]
    fn expected_filename_round_trips() {
        assert_eq!(expected_filename("en"), "snug-localisations.en.txt");
        assert_eq!(expected_filename("en-US"), "snug-localisations.en-US.txt");
        assert_eq!(expected_filename("pt-BR"), "snug-localisations.pt-BR.txt");
    }

    #[test]
    fn collect_with_no_user_bundles_returns_baseline_only() {
        let bundles = collect(&[]).unwrap();
        assert_eq!(bundles.len(), 1);
        assert_eq!(bundles[0].tag, DEFAULT_EN_TAG);
        // The baseline must contain the canonical keys we ship.
        assert!(bundles[0].get("err.jvm_not_found").is_some());
        assert!(bundles[0].get("splash.title").is_some());
        assert!(bundles[0].get("launcher.bare_stub.hint").is_some());
    }

    // ------------------------------------------------------------------
    // User-supplied `en` replaces the built-in baseline
    // ------------------------------------------------------------------

    #[test]
    fn collect_user_english_replaces_the_builtin_baseline() {
        let dir = tmpdir();
        let path = dir.join("snug-localisations.en.txt");
        std::fs::write(&path, "err.zip = Our own English copy\n").unwrap();
        let bundles = collect(&[path]).unwrap();
        // One `en`, not two: the duplicate-tag check in the build path
        // would otherwise reject this outright.
        assert_eq!(bundles.len(), 1);
        assert_eq!(bundles[0].tag, DEFAULT_EN_TAG);
        assert_eq!(bundles[0].get("err.zip"), Some("Our own English copy"));
        assert!(ensure_unique_tags(&bundles).is_ok());
    }

    #[test]
    fn collect_user_english_sits_alongside_other_locales() {
        let dir = tmpdir();
        std::fs::write(
            dir.join("snug-localisations.en.txt"),
            "err.zip = Our own English copy\n",
        )
        .unwrap();
        write_full_bundle(&dir, "de");
        let bundles = collect(std::slice::from_ref(&dir)).unwrap();
        let tags: std::collections::HashSet<&str> =
            bundles.iter().map(|b| b.tag.as_str()).collect();
        assert_eq!(tags, ["en", "de"].into_iter().collect());
        // Slot 0 is still the English one — lowest merge priority.
        assert_eq!(bundles[0].tag, DEFAULT_EN_TAG);
        assert!(ensure_unique_tags(&bundles).is_ok());
    }

    #[test]
    fn collect_rejects_a_second_english_bundle() {
        // The override is a baseline replacement, not a free pass for
        // duplicates: two `en` files reached by explicit flags (a
        // single directory can't hold two files of the same name) are
        // a real ambiguity, so the first replaces the baseline and the
        // second still collides.
        let a = tmpdir();
        let b = tmpdir();
        let first = a.join("snug-localisations.en.txt");
        let second = b.join("snug-localisations.en.txt");
        std::fs::write(&first, "err.zip = one\n").unwrap();
        std::fs::write(&second, "err.zip = two\n").unwrap();

        let bundles = collect(&[first, second]).unwrap();
        assert_eq!(bundles.len(), 2);
        assert_eq!(bundles[0].tag, DEFAULT_EN_TAG);
        assert_eq!(bundles[0].get("err.zip"), Some("one"));
        let err = ensure_unique_tags(&bundles).unwrap_err();
        assert!(
            format!("{err:#}").contains("duplicate localization tag `en`"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn collect_with_user_bundles_appends_them() {
        let dir = tmpdir();
        let path = dir.join("snug-localisations.de.txt");
        std::fs::write(
            &path,
            "# German translation\nerr.jvm_not_found = Keine passende Java-Laufzeit gefunden.\n",
        )
        .unwrap();
        let bundles = collect(&[path]).unwrap();
        assert_eq!(bundles.len(), 2);
        assert_eq!(bundles[0].tag, DEFAULT_EN_TAG);
        assert_eq!(bundles[1].tag, "de");
        assert_eq!(
            bundles[1].get("err.jvm_not_found"),
            Some("Keine passende Java-Laufzeit gefunden.")
        );
    }

    #[test]
    fn collect_rejects_malformed_user_bundle() {
        let dir = tmpdir();
        let path = dir.join("snug-localisations.fr.txt");
        std::fs::write(&path, "key no equals sign\n").unwrap();
        let err = collect(&[path]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("missing `=` separator"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn ensure_unique_tags_rejects_duplicates() {
        let a = Localization::parse("en", "x = 1\n").unwrap();
        let b = Localization::parse("en", "y = 2\n").unwrap();
        assert!(ensure_unique_tags(&[a, b]).is_err());
    }

    #[test]
    fn ensure_unique_tags_accepts_distinct_tags() {
        let a = Localization::parse("en", "x = 1\n").unwrap();
        let b = Localization::parse("de", "x = 2\n").unwrap();
        assert!(ensure_unique_tags(&[a, b]).is_ok());
    }

    #[test]
    fn validate_localization_path_accepts_canonical() {
        let dir = tmpdir();
        let path = dir.join("snug-localisations.en.txt");
        std::fs::write(&path, "err.foo = bar\n").unwrap();
        let validated = validate_localization_path(path.to_str().unwrap()).unwrap();
        assert_eq!(validated, path);
    }

    #[test]
    fn validate_localization_path_rejects_non_canonical_name() {
        let dir = tmpdir();
        let path = dir.join("localisations.txt");
        std::fs::write(&path, "err.foo = bar\n").unwrap();
        let err = validate_localization_path(path.to_str().unwrap()).unwrap_err();
        assert!(
            err.contains("expected filename like"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_localization_path_rejects_missing_path() {
        let err = validate_localization_path("C:/does/not/exist.txt").unwrap_err();
        assert!(err.contains("reading"), "unexpected error: {err}");
    }

    #[test]
    fn validate_localization_path_accepts_directory() {
        // A directory entry passes the validator without per-file parsing;
        // `collect()` is responsible for expanding and validating the
        // contents.
        let dir = tmpdir();
        let validated = validate_localization_path(dir.to_str().unwrap()).unwrap();
        assert_eq!(validated, dir);
    }

    #[test]
    fn validate_localization_path_rejects_malformed_contents() {
        let dir = tmpdir();
        let path = dir.join("snug-localisations.de.txt");
        std::fs::write(&path, "no equals sign\n").unwrap();
        let err = validate_localization_path(path.to_str().unwrap()).unwrap_err();
        assert!(
            err.contains("missing `=` separator"),
            "unexpected error: {err}"
        );
    }

    // ------------------------------------------------------------------
    // Directory expansion (--localization <dir>)
    // ------------------------------------------------------------------

    /// Helper: write a minimal-but-valid `snug-localisations.<tag>.txt`
    /// file into `dir` covering every key in the built-in English
    /// baseline, so `collect()` doesn't emit missing-key warnings during
    /// the test. Returns the path of the written file.
    fn write_full_bundle(dir: &Path, tag: &str) -> PathBuf {
        // Pull the canonical key set from the baseline itself so the
        // test stays in sync if the baseline grows.
        let baseline = Localization::parse(DEFAULT_EN_TAG, DEFAULT_EN_TEXT).unwrap();
        let mut text = String::new();
        for (k, _) in &baseline.entries {
            // Value is irrelevant for these tests — we only care about
            // tag presence and file-parsing success.
            text.push_str(&format!("{k} = x\n"));
        }
        let path = dir.join(format!("snug-localisations.{tag}.txt"));
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn collect_expands_directory_of_canonical_files() {
        let dir = tmpdir();
        write_full_bundle(&dir, "de");
        write_full_bundle(&dir, "ja");
        write_full_bundle(&dir, "pt-BR");
        let bundles = collect(std::slice::from_ref(&dir)).unwrap();
        // baseline + 3 expanded files
        assert_eq!(bundles.len(), 4);
        assert_eq!(bundles[0].tag, DEFAULT_EN_TAG);
        let user_tags: std::collections::HashSet<&str> =
            bundles[1..].iter().map(|b| b.tag.as_str()).collect();
        assert_eq!(
            user_tags,
            ["de", "ja", "pt-BR"].into_iter().collect()
        );
    }

    #[test]
    fn collect_directory_errors_on_non_matching_file() {
        let dir = tmpdir();
        write_full_bundle(&dir, "de");
        // A stray README next to the canonical bundle must fail the build.
        std::fs::write(dir.join("README.md"), "hello\n").unwrap();
        let err = collect(std::slice::from_ref(&dir)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("non-matching file") && msg.contains("README.md"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn collect_directory_errors_on_empty_directory() {
        let dir = tmpdir();
        let err = collect(std::slice::from_ref(&dir)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("contains no `snug-localisations"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn collect_directory_skips_subdirectories() {
        // Subdirectories must be ignored (top-level scan only); the
        // nested canonical file is NOT picked up.
        let dir = tmpdir();
        let nested = dir.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        write_full_bundle(&nested, "de");
        let err = collect(std::slice::from_ref(&dir)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("contains no `snug-localisations"),
            "subdir should be skipped; unexpected error: {msg}"
        );
    }

    #[test]
    fn collect_mixes_directory_and_explicit_file() {
        // Directory entries combine with explicit-file entries.
        let dir = tmpdir();
        write_full_bundle(&dir, "ja");
        let extra = tmpdir();
        let extra_file = write_full_bundle(&extra, "de");
        let bundles = collect(&[dir.clone(), extra_file.clone()]).unwrap();
        assert_eq!(bundles.len(), 3); // baseline + ja + de
        let tags: std::collections::HashSet<&str> =
            bundles[1..].iter().map(|b| b.tag.as_str()).collect();
        assert_eq!(tags, ["ja", "de"].into_iter().collect());
    }
}