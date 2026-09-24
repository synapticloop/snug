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
//! `assets/snug-localisations.en.txt` for the canonical English
//! baseline that every build embeds.
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

/// Strip a `#`-prefixed trailing comment, respecting nothing —
/// comments are not allowed inside values. Callers that need
/// literal `#` in a value should write `\#` (the backslash is
/// stripped by `unescape`).
fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(i) => &line[..i],
        None => line,
    }
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