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
//! ## Validation
//!
//! Build-time validation is intentionally non-fatal for missing keys:
//! we emit a warning per missing key on stderr, so the user notices but
//! the build still succeeds (the launcher falls back to the built-in
//! English for any key the user's bundle doesn't define).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use snug_format::Localization;

/// Canonical BCP 47 tag for the built-in English baseline bundle.
///
/// The launcher includes this string at compile time, parses it at
/// startup as the always-present fallback, and the CLI embeds it into
/// every build so end users always see English even if every other
/// bundle is missing.
pub const DEFAULT_EN_TAG: &str = "en";

/// Built-in English baseline text, embedded into every payload.
///
/// Sourced directly from `crates/snug-launcher/src/snug-localisations.en.txt`
/// via `include_str!`. The launcher also `include_str!`s the same file
/// at compile time to keep its in-binary fallback in lock-step — single
/// source of truth, no drift risk.
pub const DEFAULT_EN_TEXT: &str =
    include_str!("../../snug-launcher/src/snug-localisations.en.txt");

/// Build a `snug-localisations` filename from a locale tag.
///
/// E.g. `"en"` → `"snug-localisations.en.txt"`,
/// `"en-US"` → `"snug-localisations.en-US.txt"`.
pub fn expected_filename(tag: &str) -> String {
    format!("snug-localisations.{tag}.txt")
}

/// Extract the BCP 47 tag from a localization filename.
///
/// The expected pattern is `snug-localisations.<tag>.txt`. We strip
/// the `snug-localisations.` prefix and `.txt` suffix; whatever's left
/// is the tag. `pt-BR.txt`, `zh-Hans.txt`, etc. all work. We accept
/// `snug-localisations.en.txt` (tag = `en`) but reject
/// `my-translations.txt` (no recognised prefix).
pub fn tag_from_path(path: &Path) -> Option<String> {
    let stem = path.file_name()?.to_str()?;
    let after = stem.strip_prefix("snug-localisations.")?;
    let tag = after.strip_suffix(".txt")?;
    if tag.is_empty() {
        return None;
    }
    Some(tag.to_string())
}

/// clap value-parser for the `--localization` flag.
///
/// Validates that the file exists, is named `snug-localisations.<tag>.txt`,
/// and parses cleanly (a malformed file is rejected up front so the user
/// sees a clear error rather than a panic at runtime). The result is
/// the same path the caller passed in — `clap` only cares that we
/// didn't reject it.
pub fn validate_localization_path(s: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(s);
    if !path.is_file() {
        return Err(format!("not a file: {}", path.display()));
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
pub fn collect(user_paths: &[PathBuf]) -> Result<Vec<Localization>> {
    let mut bundles: Vec<Localization> = Vec::new();

    // 1. Built-in English baseline — always first so it's the lowest
    //    priority in the bundle chain.
    let baseline = Localization::parse(DEFAULT_EN_TAG, DEFAULT_EN_TEXT)
        .context("parsing built-in English baseline")?;
    bundles.push(baseline);

    // 2. User bundles, in CLI / options-file order.
    for path in user_paths {
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
        bundles.push(bundle);
    }

    // 3. Build-time warning: a user bundle is "missing" any baseline key
    //    we couldn't find. Don't fail the build — the launcher falls
    //    back to the built-in English for those — but make it loud so
    //    the user notices.
    warn_on_missing_keys(&bundles);

    Ok(bundles)
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

    fn tmpdir() -> PathBuf {
        let base = std::env::temp_dir();
        let unique = format!(
            "snug-localize-{}-{}",
            std::process::id(),
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
    fn validate_localization_path_rejects_missing_file() {
        let err = validate_localization_path("C:/does/not/exist.txt").unwrap_err();
        assert!(err.contains("not a file"), "unexpected error: {err}");
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
}