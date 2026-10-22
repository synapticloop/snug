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
//! Keys that no rendering site supplies a slot for are counted at the
//! end rather than printed with an empty `{}`, because "translate this
//! one literally" is not information worth 85 lines of screen. A key the
//! English copy happens not to use a slot in still gets its own entry if
//! its site supplies one — see [`inventory`].

use std::collections::BTreeSet;

use snug_format::{
    DEFAULT_EN_TAG, DEFAULT_EN_TEXT, Localization, describe, injected_for, placeholders,
};

/// One key in the per-key reference: what it says, and what it may carry.
#[derive(Debug, Clone)]
pub struct Row {
    pub key: String,
    /// The English value, so a translator can see the original.
    pub english: String,
    /// Slots the English copy names, sorted.
    pub used: Vec<String>,
    /// Slots the rendering site supplies *beyond* those: legal in a
    /// translation, absent from the English wording. Sorted.
    pub also: Vec<String>,
}

/// The baseline inventory: every key, and every slot usable in one.
pub struct Inventory {
    /// One row per key whose rendering site supplies at least one slot,
    /// sorted by key.
    pub rows: Vec<Row>,
    /// Every distinct slot a translation may use anywhere: the union of
    /// [`Row::used`] and [`Row::also`] over all rows. Sorted.
    pub available: BTreeSet<String>,
    /// The subset of `available` the English copy names. A slot in
    /// `available` but not here is headroom, not an oversight.
    pub used_by_english: BTreeSet<String>,
    /// Keys no code path fills a slot into, so they are translated
    /// literally. Sorted.
    pub plain: Vec<String>,
}

/// Derive the inventory from a parsed bundle.
///
/// Split out from [`render`] so the shape can be asserted in tests
/// without capturing stdout.
///
/// ## Why the rows are driven by `injected_for`, not by the braces
///
/// The braces in the English copy are not the contract. They are one
/// translation's choice among the slots the rendering site supplies, and
/// `INJECTED_BY_KEY` is deliberately a superset — a translator is
/// allowed to reach for any of it. Keying the table off the braces hid
/// two slots outright: `{sha256}` and `{major}` are not in the English
/// text, and the two keys that may use them (`runtime.prompt.button_download`,
/// `runtime.metadata_failed.subheading`) carry no braces at all, so they
/// fell into `plain` and never reached the output. They were reachable
/// nowhere — not in a `may also use` line, not in the flat token list.
///
/// Driving the rows off the injection table makes every declared slot
/// reachable by construction, which is the property worth having rather
/// than the two slots it rescues.
pub fn inventory(bundle: &Localization) -> Inventory {
    let mut rows = Vec::new();
    let mut plain = Vec::new();
    let mut available = BTreeSet::new();
    let mut used_by_english = BTreeSet::new();

    for (key, value) in &bundle.entries {
        let used: BTreeSet<String> = placeholders(value)
            .into_iter()
            .map(str::to_string)
            .collect();
        let also: BTreeSet<String> = injected_for(key)
            .iter()
            .map(|t| (*t).to_string())
            .filter(|t| !used.contains(t))
            .collect();

        if used.is_empty() && also.is_empty() {
            plain.push(key.clone());
            continue;
        }

        available.extend(used.iter().cloned());
        available.extend(also.iter().cloned());
        used_by_english.extend(used.iter().cloned());

        rows.push(Row {
            key: key.clone(),
            english: value.clone(),
            used: used.into_iter().collect(),
            also: also.into_iter().collect(),
        });
    }

    plain.sort();
    rows.sort_by(|a, b| a.key.cmp(&b.key));
    Inventory {
        rows,
        available,
        used_by_english,
        plain,
    }
}

/// Format the inventory as the text `--list-tokens` prints.
///
/// Wrapped into a function taking a `&mut impl Write` so a test can read
/// it back; `run` hands it a locked stdout.
pub fn render(inv: &Inventory, out: &mut impl std::io::Write) -> std::io::Result<()> {
    let total = inv.rows.len() + inv.plain.len();
    writeln!(
        out,
        "snug: localisation inventory for the built-in `{DEFAULT_EN_TAG}` baseline"
    )?;
    writeln!(
        out,
        "      {total} keys, {} may take placeholders, {} distinct tokens available",
        inv.rows.len(),
        inv.available.len()
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "Placeholders such as {{version}} are values that Snug inserts when a"
    )?;
    writeln!(out, "message is displayed.")?;
    writeln!(out)?;
    writeln!(
        out,
        "For each key, you may use the placeholders shown. Some keys also list"
    )?;
    writeln!(
        out,
        "additional placeholders under \"may also use\". These are available to"
    )?;
    writeln!(
        out,
        "translations even though the built-in English message does not use them."
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "Do not add placeholders that are not listed for that key. Snug cannot"
    )?;
    writeln!(
        out,
        "supply a value for them, so they will appear literally in the message."
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "The token list at the end names every placeholder any key may use, not"
    )?;
    writeln!(
        out,
        "only the ones the English messages happen to carry themselves."
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "For the complete per-key reference, see REPLACEMENT_TOKENS.md in the"
    )?;
    writeln!(
        out,
        "https://github.com/synapticloop/snug/ repository root."
    )?;
    writeln!(out)?;

    for row in &inv.rows {
        // A key whose English copy needs no slot prints its bare name: the
        // `may also use` line below is then the whole contract for it, and
        // an empty `{}` would read as "this key takes no arguments".
        if row.used.is_empty() {
            writeln!(out, "  {}", row.key)?;
        } else {
            writeln!(out, "  {}  {{{}}}", row.key, row.used.join(", "))?;
        }
        writeln!(out, "      {}", row.english)?;
        if !row.also.is_empty() {
            let extra: Vec<String> = row.also.iter().map(|t| format!("{{{t}}}")).collect();
            writeln!(out, "      may also use: {}", extra.join(" "))?;
        }
    }

    writeln!(out)?;
    writeln!(
        out,
        "Every distinct token any translation may use ({}):",
        inv.available.len()
    )?;
    // Column width from the widest name, floored so a future rename to
    // something two characters long doesn't produce a 4-space gutter.
    let width = inv
        .available
        .iter()
        .map(String::len)
        .max()
        .unwrap_or(0)
        .max(12);
    for token in &inv.available {
        // A gap prints as a gap rather than being hidden. It cannot
        // happen while snug-format's coverage test passes, but if it
        // ever did, the person reading this is exactly the one who can
        // do something about it.
        let described = describe(token)
            .unwrap_or("(UNDOCUMENTED -- see the baseline and fix TOKEN_DESCRIPTIONS)");
        if inv.used_by_english.contains(token) {
            writeln!(out, "  {:<width$}  {}", token, described, width = width)?;
        } else {
            // Legal in a translation, absent from the shipped wording. The
            // marker is the difference between "spare slot you may use" and
            // "token this build forgot to describe".
            writeln!(
                out,
                "  {:<width$}  {}  <- not used by the English copy; see \"may also use\"",
                token,
                described,
                width = width
            )?;
        }
    }

    writeln!(out)?;
    writeln!(
        out,
        "{} other keys use no placeholders. Their translated values are",
        inv.plain.len()
    )?;
    writeln!(out, "displayed as written.")?;
    writeln!(out)?;
    writeln!(out, "Next: create a localisation file:")?;
    writeln!(
        out,
        "  snug --init-localizations --init-localizations-tag <tag>"
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

    /// The real thing. Synthetic bundles cannot test the supply side,
    /// because `injected_for` is keyed by real key names.
    fn baseline_inv() -> Inventory {
        let b = Localization::parse(DEFAULT_EN_TAG, DEFAULT_EN_TEXT).expect("baseline parses");
        inventory(&b)
    }

    #[test]
    fn keyed_and_plain_keys_are_separated() {
        let inv = inv_of("a = plain text\nb = has {version}\n");
        assert_eq!(inv.rows.len(), 1);
        assert_eq!(inv.rows[0].key, "b");
        assert_eq!(inv.plain, vec!["a".to_string()]);
    }

    #[test]
    fn a_key_with_no_braces_and_no_slots_is_never_given_a_row() {
        // The failure this guards: an empty `{}` printed against a key
        // that just needs translating literally, which reads as "this key
        // takes no arguments" rather than "this key has no braces".
        // `title` is absent from INJECTED_BY_KEY, so nothing fills it.
        let inv = inv_of("title = Application failed to launch - Snug\n");
        assert!(inv.rows.is_empty());
        assert_eq!(inv.plain.len(), 1);
    }

    #[test]
    fn a_key_with_no_english_braces_still_gets_a_row_if_its_site_supplies_one() {
        // The other half of the same rule, and the one that was wrong:
        // `runtime.prompt.button_download` needs no slot in the English
        // copy, but the site fills version/size_mb/sha256 into it. It used
        // to land in `plain`, which prints no headroom, so those three
        // slots were described nowhere.
        let inv = baseline_inv();
        let row = inv
            .rows
            .iter()
            .find(|r| r.key == "runtime.prompt.button_download")
            .expect("headroom-only key must still be listed");
        assert!(row.used.is_empty());
        assert_eq!(
            row.also,
            vec![
                "sha256".to_string(),
                "size_mb".to_string(),
                "version".to_string()
            ]
        );
        assert!(
            !inv.plain
                .contains(&"runtime.prompt.button_download".to_string())
        );
    }

    #[test]
    fn tokens_are_the_union_across_keys_and_sorted() {
        let inv = inv_of("a = {zebra}\nb = {apple} {zebra}\n");
        assert_eq!(
            inv.available.into_iter().collect::<Vec<_>>(),
            vec!["apple".to_string(), "zebra".to_string()]
        );
    }

    /// The invariant the old shape broke, and the reason this module
    /// exists in its current form: every slot a translation may use must
    /// be reachable from some row. `{sha256}` and `{major}` were not,
    /// because nothing in the English text used them and their only two
    /// keys printed no headroom.
    #[test]
    fn every_available_token_is_reachable_from_some_row() {
        let inv = baseline_inv();
        let reachable: BTreeSet<String> = inv
            .rows
            .iter()
            .flat_map(|r| r.used.iter().chain(r.also.iter()).cloned())
            .collect();
        let orphans: Vec<&String> = inv.available.difference(&reachable).collect();
        assert!(
            orphans.is_empty(),
            "token(s) listed as available but named against no key: {orphans:?}\n  \
             every INJECTED_BY_KEY slot must reach a row"
        );
    }

    #[test]
    fn available_is_a_superset_of_what_the_english_copy_uses() {
        let inv = baseline_inv();
        let escaped: Vec<&String> = inv.used_by_english.difference(&inv.available).collect();
        assert!(
            escaped.is_empty(),
            "English copy uses slot(s) the inventory does not offer: {escaped:?}"
        );
    }

    /// Pinned on `available`, not on the English copy's own braces: the
    /// question a translator asks is "may I use `{sha256}`?", and the
    /// answer is yes even though no shipped message does.
    #[test]
    fn the_shipped_baseline_has_the_tokens_it_is_documented_to_have() {
        let inv = baseline_inv();
        for expected in [
            "version",
            "size_mb",
            "attempt",
            "max_attempts",
            "error",
            "pct",
            "arch",
            "min_java",
            "update_check_url",
            // Deliberately headroom: declared available, unused in English.
            "sha256",
            "major",
        ] {
            assert!(
                inv.available.contains(expected),
                "`{{{expected}}}` is no longer offered to translations -- update this test and \
                 the README token table if that was intentional"
            );
        }
    }

    #[test]
    fn render_mentions_every_key_that_carries_a_token() {
        let inv = baseline_inv();
        let mut buf = Vec::new();
        render(&inv, &mut buf).expect("render");
        let text = String::from_utf8(buf).expect("utf-8");
        for row in &inv.rows {
            assert!(
                text.contains(row.key.as_str()),
                "`{}` missing from output",
                row.key
            );
        }
    }

    /// The two slots the English copy never uses must survive all the way
    /// into the rendered text, marked as headroom rather than presented as
    /// if the English wording used them.
    #[test]
    fn headroom_tokens_reach_the_rendered_output_and_are_marked() {
        let inv = baseline_inv();
        let mut buf = Vec::new();
        render(&inv, &mut buf).expect("render");
        let text = String::from_utf8(buf).expect("utf-8");
        for token in ["sha256", "major"] {
            let line = text
                .lines()
                .find(|l| l.trim_start().starts_with(token))
                .unwrap_or_else(|| panic!("`{token}` missing from the token list"));
            assert!(
                line.contains("not used by the English copy"),
                "`{token}` is headroom and must say so, got: {line}"
            );
        }
        // And the per-key table has to say where each one is usable.
        assert!(text.contains("may also use: {sha256}"));
        assert!(text.contains("may also use: {major}"));
    }
}
