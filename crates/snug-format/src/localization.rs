//! Localized runtime-error strings.
//!
//! Snug's launcher is a Windows GUI that pops up error dialogs when
//! the JVM can't be located, the embedded JAR can't be read, the
//! Java `main` throws, and so on. The strings for those dialogs come
//! from one or more *localization bundles* embedded in the payload;
//! at runtime the launcher picks the bundle matching the user's
//! Windows UI language and falls back to a built-in English baseline
//! for any key the bundle doesn't cover.
//!
//! ## File format
//!
//! Each bundle ships as a flat `key = value` text file named
//! `snug-localisations.<tag>.txt`, where `<tag>` is a BCP 47 locale
//! tag (`en`, `en-US`, `de`, `pt-BR`, ...). Values are single-line;
//! literal `\n` and `\t` escapes are decoded at lookup time, and
//! placeholder names use `{name}` syntax. See
//! `assets/snug-localisations.en.txt` in this crate for the canonical
//! English baseline that every build embeds — it's exposed as
//! [`DEFAULT_EN_TEXT`] and is the single `include_str!` site in the
//! whole workspace, so the CLI and the launcher cannot drift.
//!
//! ## Priority
//!
//! At runtime the launcher builds an ordered list of bundles and
//! walks them on every lookup. The user's Windows UI locale picks
//! the most specific match — e.g. for `en-US` it tries `en-US`
//! first, then falls back to `en`, then to the built-in English
//! baseline. Higher-priority entries win on key collision; missing
//! keys fall through.
//!
//! ## Why this lives in `snug-format`
//!
//! `Localization` is part of the wire format — both the CLI/builder
//! (writes it) and the launcher (reads it) need to agree on the
//! postcard encoding. Keeping the type here means every consumer of
//! `SnugPayload` sees the same shape.

use serde::{Deserialize, Serialize};

/// BCP 47 tag of the built-in English baseline bundle.
pub const DEFAULT_EN_TAG: &str = "en";

/// The canonical English baseline, embedded at compile time.
///
/// `assets/snug-localisations.en.txt` lives in *this* crate because
/// both consumers of the wire format need it and there is nowhere
/// else they can both reach without one of them reaching across a
/// crate boundary on the filesystem. `snug-cli` embeds the baseline
/// into every payload it builds; `snug-launcher` `include_str!`s it as
/// its in-binary last-resort fallback for the bare-stub path. By
/// embedding once here and re-exporting, drift between those two
/// copies becomes structurally impossible rather than a convention
/// the comments have to police.
///
/// The file's *name* is not consulted when parsing it — the tag is
/// supplied explicitly as [`DEFAULT_EN_TAG`]. The name is kept
/// identical to the bundles `snug --init-localizations` writes so it
/// reads the same as a user-supplied English bundle.
pub const DEFAULT_EN_TEXT: &str = include_str!("../assets/snug-localisations.en.txt");

/// A single localization bundle.
///
/// `tag` is the BCP 47 locale tag the bundle covers (`en`, `en-US`,
/// `pt-BR`, ...). `entries` is the flat `key -> value` map; the
/// order of insertion is preserved for stable builds but not
/// semantically meaningful — lookups ignore order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Localization {
    /// BCP 47 locale tag (`en`, `en-US`, `pt-BR`, ...).
    pub tag: String,
    /// Flat `key -> value` map. Insertion order is preserved for
    /// reproducibility of the encoded payload but does not affect
    /// lookup semantics.
    pub entries: Vec<(String, String)>,
}

impl Localization {
    /// Build a bundle from raw text. Used by both the builder
    /// (parsing user-supplied `.txt` files) and the launcher
    /// (parsing the built-in English baseline at startup).
    ///
    /// See the module-level docs for the grammar. Whitespace around
    /// keys/values is trimmed; comments (`#` to end-of-line) and
    /// blank lines are skipped.
    pub fn parse(tag: impl Into<String>, text: &str) -> Result<Self, LocalizationParseError> {
        let tag = tag.into();
        let mut entries = Vec::new();
        for (idx, raw_line) in text.lines().enumerate() {
            let lineno = idx + 1;
            let line = strip_comment(raw_line).trim();
            if line.is_empty() {
                continue;
            }
            let (key, value) = line.split_once('=').ok_or(LocalizationParseError::NoEquals {
                tag: tag.clone(),
                lineno,
            })?;
            let key = key.trim();
            if key.is_empty() {
                return Err(LocalizationParseError::EmptyKey {
                    tag: tag.clone(),
                    lineno,
                });
            }
            // Reject obvious typos: dot-separated, snake_case, or
            // kebab-case identifiers. Anything else is almost
            // certainly a bug in the contributor's file.
            if !is_valid_key(key) {
                return Err(LocalizationParseError::BadKey {
                    tag: tag.clone(),
                    lineno,
                    key: key.to_string(),
                });
            }
            let value = unescape(value.trim());
            entries.push((key.to_string(), value));
        }
        Ok(Localization { tag, entries })
    }

    /// Look up `key` in this bundle alone. Returns `None` if the
    /// key is not present; does NOT walk any fallback chain.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find_map(|(k, v)| if k == key { Some(v.as_str()) } else { None })
    }
}

/// Errors that can arise while parsing a localization file.
#[derive(Debug, thiserror::Error)]
pub enum LocalizationParseError {
    #[error("localization file for `{tag}` line {lineno}: missing `=` separator")]
    NoEquals { tag: String, lineno: usize },

    #[error("localization file for `{tag}` line {lineno}: key is empty")]
    EmptyKey { tag: String, lineno: usize },

    #[error(
        "localization file for `{tag}` line {lineno}: key `{key}` contains invalid characters \
         (allowed: ASCII letters, digits, `_`, `-`, `.`)"
    )]
    BadKey {
        tag: String,
        lineno: usize,
        key: String,
    },
}

/// Strip a trailing comment, starting at the first `#` that is not
/// escaped.
///
/// A `#` is escaped when it follows an *odd* number of backslashes: `\#` is
/// a literal hash, `\\#` is a literal backslash followed by a real comment.
/// The backslash is left in place for [`unescape`] to consume, which is why
/// this returns the line unchanged up to the comment rather than rebuilding
/// it.
///
/// This used to be `line.find('#')`, which matched the *escaped* hash too.
/// Since `strip_comment` runs before `unescape`, the escape never reached
/// the decoder: a contributor following the documented `\#` convention got
/// their value silently cut at the hash, with a stray backslash left over
/// (`Error \#42` became `Error \`). No parse error, and the missing-key
/// guard could not see it, because the key was present -- it just held the
/// wrong text. The unit test passed because it called `unescape` directly
/// and bypassed the order that broke it.
///
/// Scanning by byte is safe here: `#` and `\` are ASCII, so the only place
/// this ever slices is a position that is necessarily a character boundary.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            // Skip the escape *and* what it escapes. Stepping two is what
            // makes `\\#` read as "an escaped backslash, then a comment"
            // rather than "an escaped hash".
            b'\\' => i += 2,
            b'#' => return &line[..i],
            _ => i += 1,
        }
    }
    line
}

/// Decode the value-side escape sequences: `\\n` → `\n`, `\\t` → `\t`,
/// `\\\\` → `\`, `\\#` → `#`, `\\=` → `=`. Anything else is left
/// as a literal backslash followed by the next character (so
/// contributors can write `\.` etc. without surprises).
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some('#') => out.push('#'),
                Some('=') => out.push('='),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn is_valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.chars().all(|c| {
            c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'
        })
}

// ===========================================================================
//  Locating bundle files on disk
// ===========================================================================
//
//  These live here rather than in the CLI so `snug-preview` can resolve
//  the same paths when a user points it at a localisation directory.
//  Both crates depend on `snug-format`; neither can see the other.

use std::path::{Path, PathBuf};

/// Build a `snug-localisations` filename from a locale tag.
///
/// E.g. `"en"` → `"snug-localisations.en.txt"`, `"en-US"` →
/// `"snug-localisations.en-US.txt"`.
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

/// Errors from walking the paths a user pointed us at.
#[derive(Debug, thiserror::Error)]
pub enum LocalizationLoadError {
    #[error("stat-ing localization entry `{path}`: {source}")]
    Stat {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("reading localization directory `{path}`: {source}")]
    ReadDir {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "localization directory `{0}` contains no `snug-localisations.<tag>.txt` files \
         (top-level scan; subdirectories are not searched)"
    )]
    EmptyDir(String),

    #[error(
        "localization directory `{dir}` contains non-matching file `{path}` \
         (expected `snug-localisations.<tag>.txt`)"
    )]
    NonMatchingFile { dir: String, path: String },
}

/// Walk `entries`, expanding any directory into the sorted list of
/// `snug-localisations.<tag>.txt` files directly inside it.
///
/// A directory containing zero matching files, or any non-matching
/// file, is an error — directory mode is opt-in and a stray `.bak`
/// next to the bundles is almost always a typo, so we fail loudly
/// rather than silently ignoring it. Subdirectories are not descended
/// into; this is a top-level scan.
///
/// The output is sorted per directory so callers get a deterministic
/// order across platforms (`read_dir` order is OS-specific).
pub fn discover_localization_files(
    entries: &[PathBuf],
) -> Result<Vec<PathBuf>, LocalizationLoadError> {
    let mut expanded: Vec<PathBuf> = Vec::with_capacity(entries.len());
    for path in entries {
        let meta = std::fs::metadata(path).map_err(|source| {
            LocalizationLoadError::Stat {
                path: path.display().to_string(),
                source,
            }
        })?;
        if !meta.is_dir() {
            expanded.push(path.clone());
            continue;
        }

        let dir_display = path.display().to_string();
        let mut found: Vec<PathBuf> = std::fs::read_dir(path)
            .map_err(|source| LocalizationLoadError::ReadDir {
                path: dir_display.clone(),
                source,
            })?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| p.is_file())
            .collect();
        found.sort();
        if found.is_empty() {
            return Err(LocalizationLoadError::EmptyDir(dir_display));
        }
        for entry in &found {
            if tag_from_path(entry).is_none() {
                return Err(LocalizationLoadError::NonMatchingFile {
                    dir: dir_display.clone(),
                    path: entry.display().to_string(),
                });
            }
        }
        expanded.extend(found);
    }
    Ok(expanded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_bundle() {
        let text = "\
# A comment
err.foo = hello

err.bar = multi\\nline
";
        let bundle = Localization::parse("en", text).unwrap();
        assert_eq!(bundle.tag, "en");
        assert_eq!(bundle.get("err.foo"), Some("hello"));
        assert_eq!(bundle.get("err.bar"), Some("multi\nline"));
    }

    #[test]
    fn rejects_missing_equals() {
        let err = Localization::parse("en", "key no equals\n").unwrap_err();
        match err {
            LocalizationParseError::NoEquals { lineno, .. } => assert_eq!(lineno, 1),
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn rejects_empty_key() {
        let err = Localization::parse("en", " = value\n").unwrap_err();
        match err {
            LocalizationParseError::EmptyKey { lineno, .. } => assert_eq!(lineno, 1),
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn rejects_bad_key() {
        let err = Localization::parse("en", "bad key! = x\n").unwrap_err();
        match err {
            LocalizationParseError::BadKey { lineno, key, .. } => {
                assert_eq!(lineno, 1);
                assert_eq!(key, "bad key!");
            }
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn get_returns_none_for_missing_key() {
        let bundle = Localization::parse("en", "err.foo = x\n").unwrap();
        assert_eq!(bundle.get("err.foo"), Some("x"));
        assert_eq!(bundle.get("err.missing"), None);
    }

    #[test]
    fn unescape_handles_common_sequences() {
        assert_eq!(unescape(r"hello\nworld"), "hello\nworld");
        assert_eq!(unescape(r"tab\there"), "tab\there");
        assert_eq!(unescape(r"back\\slash"), "back\\slash");
        assert_eq!(unescape(r"hash\#sign"), "hash#sign");
        assert_eq!(unescape(r"eq\=sign"), "eq=sign");
        // Unknown escape → preserved verbatim.
        assert_eq!(unescape(r"x\yz"), "x\\yz");
    }

    // --- Escaped hashes (F-12) ------------------------------------------
    //
    // These deliberately go through `parse`, not `unescape` directly. The
    // bug was an ordering problem between the two, so a test that skips
    // `strip_comment` cannot see it -- which is exactly what
    // `unescape_handles_common_sequences` was doing.

    #[test]
    fn an_escaped_hash_survives_comment_stripping() {
        let bundle = Localization::parse("en", concat!(r"err.code = Error \#42", "\n")).unwrap();
        assert_eq!(
            bundle.get("err.code"),
            Some("Error #42"),
            "the documented `\\#` must reach the value, not start a comment"
        );
    }

    #[test]
    fn a_real_comment_is_still_stripped() {
        let bundle = Localization::parse("en", "err.a = kept  # trailing note\n").unwrap();
        assert_eq!(bundle.get("err.a"), Some("kept"));
    }

    #[test]
    fn an_escaped_backslash_does_not_escape_the_next_hash() {
        // `\\#` is a literal backslash followed by a real comment. Getting
        // this backwards would let a value swallow the rest of the file's
        // comments, or cut a legitimate value short.
        let bundle =
            Localization::parse("en", "err.a = back\\\\  # note\nerr.b = second\n").unwrap();
        assert_eq!(bundle.get("err.a"), Some("back\\"));
        assert_eq!(bundle.get("err.b"), Some("second"));
    }

    #[test]
    fn a_lone_trailing_backslash_does_not_panic() {
        // The scan steps two bytes for an escape; a `\` at the very end has
        // nothing to step over. It must not slice mid-character or panic.
        let bundle = Localization::parse("en", "err.a = ends with \\\n").unwrap();
        assert_eq!(bundle.get("err.a"), Some("ends with \\"));
    }

    #[test]
    fn escapes_and_comments_interleave_correctly() {
        // The realistic composite: a literal hash, an escaped equals, a real
        // comment, and a second key afterwards.
        //
        // Note the path avoids `\t` and `\n` on purpose: those ARE decoded,
        // so a Windows path through a `temp` or `notepad` directory becomes
        // real control characters. That is documented, not a bug, but it is
        // the first thing a contributor trips over.
        let text = concat!(
            "err.a = C:\\Users\\admin \\# not a comment\n",
            "err.b = k\\=v   # this part is a comment\n",
            "err.c = plain\n",
        );
        let bundle = Localization::parse("en", text).unwrap();
        assert_eq!(bundle.get("err.a"), Some(r"C:\Users\admin # not a comment"));
        assert_eq!(bundle.get("err.b"), Some("k=v"));
        assert_eq!(bundle.get("err.c"), Some("plain"));
    }

    #[test]
    fn backslash_t_in_a_value_is_a_tab_not_a_path_separator() {
        // Pinned because it is the other half of why a Windows path needs
        // doubling: `\t` and `\n` are decoded, so `C:\temp` cannot be
        // written as-is. Worth stating explicitly rather than leaving a
        // contributor to discover it as a mangled dialog.
        let bundle = Localization::parse("en", concat!(r"err.a = C:\temp", "\n")).unwrap();
        assert_eq!(bundle.get("err.a"), Some("C:\temp"));
    }

    #[test]
    fn roundtrip_postcard() {
        let bundle = Localization {
            tag: "en-US".into(),
            entries: vec![
                ("err.foo".into(), "hello".into()),
                ("err.bar".into(), "multi\nline".into()),
            ],
        };
        let bytes = postcard::to_allocvec(&bundle).unwrap();
        let decoded: Localization = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, bundle);
    }
}