//! `snug --list-tokens` — print the placeholder contract for the
//! built-in English baseline.
//!
//! ## Why this exists
//!
//! A translation bundle is a flat `key = value` file, and the keys are
//! only half the contract. The other half is *which `{slots}` each
//! value uses*: `runtime.prompt.content` needs `{size_mb}` and
//! `{version}`, `runtime.retry.subheading` needs `{attempt}` and
//! `{max_attempts}`. Nothing documented that relationship outside the
//! English file itself, so a translator had to read 107 lines and spot
//! the braces by eye.
//!
//! Getting it wrong is not cosmetic. The launcher substitutes by
//! literal replace and leaves unknown slots alone, so a bundle that
//! keeps `{version}` for a key that dropped it renders a literal
//! `{version}` in a dialog once the call site stops supplying one. The
//! build-time counterpart to this is
//! [`crate::localization`]'s placeholder warning.
//!
//! ## Scope
//!
//! Reads the compiled-in baseline only. It never touches the input JAR,
//! an options file, or any user bundle, so it runs in microseconds and
//! works with no build context at all — `snug --list-tokens` is enough.
//! Keys carrying no placeholders are counted and listed at the end
//! rather than printed with an empty `{}`, because "translate this one
//! literally" is not information worth 85 lines of screen.

use std::collections::BTreeSet;

use snug_format::{placeholders, Localization, DEFAULT_EN_TAG, DEFAULT_EN_TEXT};

/// The baseline inventory: keys with slots, and the flat token list.
pub struct Inventory {
    /// `(key, english value, sorted slot names)` for every key that
    /// carries at least one placeholder.
    pub keyed: Vec<(String, String, Vec<String>)>,
    /// Every distinct slot name across the bundle, sorted.
    pub tokens: BTreeSet<String>,
    /// Keys with no placeholders, sorted.
    pub plain: Vec<String>,
}

/// Derive the inventory from a parsed bundle.
///
/// Split out from [`render`] so the shape can be asserted in tests
/// without capturing stdout.
pub fn inventory(bundle: &Localization) -> Inventory {
    let mut keyed = Vec::new();
    let mut plain = Vec::new();
    let mut tokens = BTreeSet::new();
    for (key, value) in &bundle.entries {
        let slots = placeholders(value);
        if slots.is_empty() {
            plain.push(key.clone());
            continue;
        }
        for name in &slots {
            tokens.insert((*name).to_string());
        }
        keyed.push((
            key.clone(),
            value.clone(),
            slots.into_iter().map(str::to_string).collect(),
        ));
    }
    plain.sort();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    Inventory {
        keyed,
        tokens,
        plain,
    }
}

/// Format the inventory as the text `--list-tokens` prints.
///
/// Wrapped into a function taking a `&mut impl Write` so a test can read
/// it back; `run` hands it a locked stdout.
pub fn render(inv: &Inventory, out: &mut impl std::io::Write) -> std::io::Result<()> {
    let total = inv.keyed.len() + inv.plain.len();
    writeln!(
        out,
        "snug: localisation inventory for the built-in `{DEFAULT_EN_TAG}` baseline"
    )?;
    writeln!(
        out,
        "      {total} keys, {} carry placeholders, {} distinct tokens",
        inv.keyed.len(),
        inv.tokens.len()
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "Use exactly these {{slots}} in a translated value. A slot you drop, or"
    )?;
    writeln!(
        out,
        "invent one the English does not have, and the launcher prints it literally."
    )?;
    writeln!(out)?;

    for (key, english, slots) in &inv.keyed {
        writeln!(out, "  {key}  {{{}}}", slots.join(", "))?;
        writeln!(out, "      {english}")?;
    }

    writeln!(out)?;
    writeln!(out, "Every distinct token ({}):", inv.tokens.len())?;
    // Column width from the widest name, floored so a future rename to
    // something two characters long doesn't produce a 4-space gutter.
    let width = inv
        .tokens
        .iter()
        .map(String::len)
        .max()
        .unwrap_or(0)
        .max(12);
    for token in &inv.tokens {
        // A gap prints as a gap rather than being hidden. It cannot
        // happen while snug-format's coverage test passes, but if it
        // ever did, the person reading this is exactly the one who can
        // do something about it.
        let described = snug_format::describe(token)
            .unwrap_or("(UNDOCUMENTED -- see the baseline and fix TOKEN_DESCRIPTIONS)");
        writeln!(out, "  {:<width$}  {}", token, described, width = width)?;
    }

    writeln!(out)?;
    writeln!(
        out,
        "{} more key(s) take no placeholders and are translated literally.",
        inv.plain.len()
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "Next: snug --init-localizations --init-localizations-tag <tag>"
    )?;
    Ok(())
}

/// Entry point for `snug --list-tokens`.
pub fn run() -> anyhow::Result<()> {
    let bundle = Localization::parse(DEFAULT_EN_TAG, DEFAULT_EN_TEXT)
        .map_err(|e| anyhow::anyhow!("built-in {DEFAULT_EN_TAG} baseline is unparseable: {e}"))?;
    let inv = inventory(&bundle);
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    render(&inv, &mut lock)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inv_of(text: &str) -> Inventory {
        let b = Localization::parse("xx", text).expect("parses");
        inventory(&b)
    }

    #[test]
    fn keyed_and_plain_keys_are_separated() {
        let inv = inv_of("a = plain text\nb = has {version}\n");
        assert_eq!(inv.keyed.len(), 1);
        assert_eq!(inv.keyed[0].0, "b");
        assert_eq!(inv.plain, vec!["a".to_string()]);
    }

    #[test]
    fn a_key_with_no_braces_is_never_counted_as_keyed() {
        // The failure this guards: an empty `{}` printed against a key
        // that just needs translating literally, which reads as "this key
        // takes no arguments" rather than "this key has no braces".
        let inv = inv_of("title = Application failed to launch - Snug\n");
        assert!(inv.keyed.is_empty());
        assert_eq!(inv.plain.len(), 1);
    }

    #[test]
    fn tokens_are_the_union_across_keys_and_sorted() {
        let inv = inv_of("a = {zebra}\nb = {apple} {zebra}\n");
        assert_eq!(
            inv.tokens.into_iter().collect::<Vec<_>>(),
            vec!["apple".to_string(), "zebra".to_string()]
        );
    }

    #[test]
    fn the_shipped_baseline_has_the_tokens_it_is_documented_to_have() {
        let bundle = Localization::parse(DEFAULT_EN_TAG, DEFAULT_EN_TEXT).expect("baseline parses");
        let inv = inventory(&bundle);
        // Pinned rather than fuzzy: these are the ones a translator hits
        // most, and a baseline edit that removed one should be a
        // deliberate, visible change rather than a quiet drift.
        for expected in [
            "version",
            "size_mb",
            "attempt",
            "max_attempts",
            "error",
            "pct",
            "arch",
            "min_java",
            "update_link",
        ] {
            assert!(
                inv.tokens.contains(expected),
                "baseline no longer uses `{{{expected}}}` -- update this test and the README \
                 token table if that was intentional"
            );
        }
    }

    #[test]
    fn render_mentions_every_key_that_carries_a_token() {
        let inv = inventory(
            &Localization::parse(DEFAULT_EN_TAG, DEFAULT_EN_TEXT).expect("baseline parses"),
        );
        let mut buf = Vec::new();
        render(&inv, &mut buf).expect("render");
        let text = String::from_utf8(buf).expect("utf-8");
        for (key, _, _) in &inv.keyed {
            assert!(text.contains(key.as_str()), "`{key}` missing from output");
        }
    }
}