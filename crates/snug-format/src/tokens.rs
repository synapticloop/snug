//! The replacement-token contract: which `{slots}` each key is *allowed*
//! to name, and where those values come from.
//!
//! ## The problem this solves
//!
//! A bundle value is a template, and the launcher fills `{name}` slots by
//! looking them up in whatever pair list the *rendering site* handed to
//! [`crate::localization::fill`]-equivalent machinery. Two consequences
//! follow, and only the first is obvious:
//!
//! 1. A key can only meaningfully use tokens its own site supplies.
//!    `runtime.retry.subheading` cannot name `{size_mb}` however it is
//!    spelled, because the retry dialog has no download size to give.
//! 2. **A token that nothing supplies is not an error.** Unknown slot
//!    names are copied through verbatim, by design, so a typo leaves the
//!    text visible instead of silently swallowing it. That is the right
//!    default for a *bundle* — a translator gets a visible marker — but
//!    it means the English baseline can drift into naming a slot nobody
//!    fills, and then every shipped dialog prints `{size_mb}` at the
//!    user.
//!
//! The existing build-time check in `snug-cli` compares a bundle against
//! the English baseline, so it catches a *bundle* drifting. It cannot
//! catch the *baseline* drifting, because the baseline is the reference.
//! That is the hole [`INJECTED_BY_KEY`] closes: it is the declaration of
//! what each site supplies, and a test asserts the baseline never asks
//! for more.
//!
//! ## Why it is a hand-written table
//!
//! The values could in principle be scraped from the call sites, but the
//! call sites are Rust expressions spread across `jdk_install.rs`,
//! `error_window.rs` and `progress_window.rs` in a *different crate* that
//! `snug-cli` cannot depend on. A declared table that a test holds to
//! account is worth more than a derived one nobody can run at build time.
//!
//! ## Keeping it honest
//!
//! Three tests, in [`tests`]:
//!
//! - the baseline never names a token its site does not supply;
//! - no declaration names a key the baseline does not have;
//! - the checked-in `REPLACEMENT_TOKENS.md` says what
//!   [`replacement_tokens_markdown`] says.
//!
//! The third one is the one that matters most. A hand-maintained copy of
//! a table like this drifts silently and nobody compares the two — which
//! is the same failure this crate's localisation catalogue has already
//! produced once.
//!
//! It compares *content* rather than bytes, and it reads the file at run
//! time rather than baking it in with `include_str!`. Both choices exist
//! because the byte-for-byte version of this guard was unreliable. The
//! table lives at the repository root, outside this crate's package
//! directory, so cargo does not rebuild when it changes and a baked-in
//! copy goes stale without anybody noticing. And markdown tooling pads
//! every table cell out to the column width, which changes the bytes of
//! a *generated* file without changing a word of it — the checked-in
//! table was reformatted exactly that way once, and the byte-for-byte
//! guard reported the result as stale.
//!
//! The generator now writes its tables in that same padded form, so the
//! file on disk is already what a formatter would produce, and a
//! formatter that runs over the repository has nothing to change. The
//! comparison stays on content so that a formatter which pads
//! *slightly* differently still cannot break the build.

/// One row of the contract: a key, and the slots its render site fills.
#[derive(Debug)]
pub struct Injection {
    /// The localisation key.
    pub key: &'static str,
    /// Slot names the rendering site actually supplies.
    ///
    /// Deliberately a superset of what the English copy uses today. A
    /// translator is allowed to reach for any of these; anything outside
    /// this list will render literally.
    pub tokens: &'static [&'static str],
}

/// Every key that may carry a replacement token, with the tokens its
/// rendering site supplies.
///
/// Keys absent from this table may not use any token: they are filled by
/// no code path. Two entries below use none today and are listed anyway,
/// because that is exactly the headroom a translator is allowed to use.
pub const INJECTED_BY_KEY: &[Injection] = &[
    // --- err.* -- positional, from thiserror's Display ------------------
    Injection {
        key: "err.format",
        tokens: &["0"],
    },
    Injection {
        key: "err.invalid_state",
        tokens: &["0"],
    },
    Injection {
        key: "err.io",
        tokens: &["0"],
    },
    Injection {
        key: "err.java_exception",
        tokens: &["0"],
    },
    Injection {
        key: "err.jni_attach",
        tokens: &["0"],
    },
    Injection {
        key: "err.jni_create",
        tokens: &["0"],
    },
    Injection {
        key: "err.jni_init",
        tokens: &["0"],
    },
    Injection {
        key: "err.jni_invoke",
        tokens: &["0"],
    },
    Injection {
        key: "err.main_class_not_found",
        tokens: &["0"],
    },
    Injection {
        key: "err.no_main_method",
        tokens: &["0"],
    },
    Injection {
        key: "err.self_path",
        tokens: &["0"],
    },
    Injection {
        key: "err.zip",
        tokens: &["0"],
    },
    // --- err.* -- named, from error.rs ----------------------------------
    Injection {
        key: "err.jvm_not_found",
        tokens: &["min_java"],
    },
    Injection {
        key: "err.jvm_too_old",
        tokens: &["path", "found", "min_java"],
    },
    Injection {
        key: "err.library_load",
        tokens: &["library", "message"],
    },
    Injection {
        key: "err.symbol_not_found",
        tokens: &["symbol", "library"],
    },
    // --- jdk.err.* ------------------------------------------------------
    Injection {
        key: "jdk.err.bad_metadata_shape",
        tokens: &["0"],
    },
    Injection {
        key: "jdk.err.dialog",
        tokens: &["0"],
    },
    Injection {
        key: "jdk.err.download",
        tokens: &["0"],
    },
    Injection {
        key: "jdk.err.extract",
        tokens: &["0"],
    },
    Injection {
        key: "jdk.err.io",
        tokens: &["0"],
    },
    Injection {
        key: "jdk.err.metadata_fetch",
        tokens: &["0"],
    },
    Injection {
        key: "jdk.err.no_java_exe",
        tokens: &["0"],
    },
    Injection {
        key: "jdk.err.no_metadata_for_version",
        tokens: &["0"],
    },
    Injection {
        key: "jdk.err.bad_field",
        tokens: &["name", "value"],
    },
    Injection {
        key: "jdk.err.sha256_mismatch",
        tokens: &["declared", "computed"],
    },
    // --- splash.err.* ---------------------------------------------------
    Injection {
        key: "splash.err.thread_spawn",
        tokens: &["0"],
    },
    Injection {
        key: "splash.err.buffer_length",
        tokens: &["expected", "actual"],
    },
    // --- launcher.* -----------------------------------------------------
    Injection {
        key: "launcher.bare_stub.no_payload",
        tokens: &["path"],
    },
    Injection {
        key: "launcher.error.content",
        tokens: &["error"],
    },
    Injection {
        key: "launcher.error.info_subtext_2",
        tokens: &["update_check_url"],
    },
    // --- runtime.prompt.* -- the install offer ---------------------------
    Injection {
        key: "runtime.prompt.content",
        tokens: &["version", "size_mb", "sha256"],
    },
    // Uses none today; the download button is filled by the same site, so
    // a translator may put the version or size in it.
    Injection {
        key: "runtime.prompt.button_download",
        tokens: &["version", "size_mb", "sha256"],
    },
    // --- runtime.metadata_failed.* ---------------------------------------
    // Uses none today. `major` was here because the site's `fill` passes
    // it, and an earlier draft of the copy wanted the Java major in the
    // subheading.
    Injection {
        key: "runtime.metadata_failed.subheading",
        tokens: &["major"],
    },
    Injection {
        key: "runtime.metadata_failed.content",
        tokens: &["major", "error"],
    },
    // --- runtime.progress.* ---------------------------------------------
    Injection {
        key: "runtime.progress.content",
        tokens: &["version", "size_mb"],
    },
    Injection {
        key: "runtime.progress.phase_label",
        tokens: &["arch"],
    },
    Injection {
        key: "runtime.progress.pct_label",
        tokens: &["pct"],
    },
    Injection {
        key: "runtime.progress.detail_with_size",
        tokens: &["done_mb", "total_mb", "speed_mb_s"],
    },
    Injection {
        key: "runtime.progress.detail_no_size",
        tokens: &["pct"],
    },
    Injection {
        key: "runtime.progress.detail_eta_seconds",
        tokens: &["n"],
    },
    // --- runtime.failure.* / runtime.retry.* -----------------------------
    Injection {
        key: "runtime.failure.content",
        tokens: &["version", "error"],
    },
    Injection {
        key: "runtime.retry.subheading",
        tokens: &["version", "attempt", "max_attempts", "error"],
    },
    Injection {
        key: "runtime.retry.content",
        tokens: &["version", "attempt", "max_attempts", "error"],
    },
];

/// The tokens a rendering site supplies for `key`, or an empty slice.
///
/// A key that is absent from [`INJECTED_BY_KEY`] may use nothing.
pub fn injected_for(key: &str) -> &'static [&'static str] {
    INJECTED_BY_KEY
        .iter()
        .find(|i| i.key == key)
        .map(|i| i.tokens)
        .unwrap_or(&[])
}

fn escape_cell(s: &str) -> String {
    s.replace('|', "\\|")
}

fn brace_list(tokens: &[&str]) -> String {
    if tokens.is_empty() {
        "(none)".to_string()
    } else {
        tokens
            .iter()
            .map(|t| format!("`{{{t}}}`"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Render one table row, with every cell padded to its column's width.
///
/// A cell is one space, the content, and one more space, so a column
/// whose widest cell is `n` characters across is `n + 2` characters wide.
fn padded_row(cells: &[String], widths: &[usize]) -> String {
    let mut line = String::from("|");
    for (cell, width) in cells.iter().zip(widths) {
        line.push(' ');
        line.push_str(cell);
        for _ in cell.chars().count()..*width {
            line.push(' ');
        }
        line.push(' ');
        line.push('|');
    }
    line
}

/// Render a markdown table: header, separator, rows.
///
/// Every cell is padded to the width of the widest cell in its column,
/// and the separator row under the header is a run of dashes exactly as
/// wide as the cells above it. That is the shape markdown tooling
/// produces when it reformats a table, and the checked-in file is kept
/// in it — so a formatter that runs over the repository has nothing to
/// change, and the file on disk stays byte-for-byte what this function
/// produced.
///
/// Widths are counted in characters rather than bytes, so a multi-byte
/// character counts once. Every character in these tables is narrow
/// anyway; the em dash in a description is the only non-ASCII one.
fn render_table(header: &[&str], rows: &[Vec<String>]) -> String {
    let widths: Vec<usize> = (0..header.len())
        .map(|column| {
            std::iter::once(header[column].chars().count())
                .chain(rows.iter().map(|row| row[column].chars().count()))
                .max()
                .unwrap_or(0)
        })
        .collect();

    let mut out = String::new();
    let header_cells: Vec<String> = header.iter().map(|cell| cell.to_string()).collect();
    out.push_str(&padded_row(&header_cells, &widths));
    out.push('\n');

    out.push('|');
    for width in &widths {
        for _ in 0..(width + 2) {
            out.push('-');
        }
        out.push('|');
    }
    out.push('\n');

    for row in rows {
        out.push_str(&padded_row(row, &widths));
        out.push('\n');
    }
    out
}

/// Render the checked-in `REPLACEMENT_TOKENS.md`.
///
/// Generated from [`INJECTED_BY_KEY`] and the live baseline so the
/// "used by the English copy" column cannot fall out of date.
///
/// The tables are written in the padded form markdown tooling produces
/// when it reformats a table — every cell as wide as the widest cell in
/// its column — so the checked-in file is already in the shape a
/// formatter would leave it in, and a formatter that runs over the
/// repository has nothing to change. A test asserts the file on disk
/// says what this says; it compares content rather than bytes, so a
/// formatter that pads slightly differently cannot break the build
/// either.
pub fn replacement_tokens_markdown() -> String {
    use crate::localization::{Localization, TOKEN_DESCRIPTIONS, placeholders};

    let baseline = Localization::parse(crate::DEFAULT_EN_TAG, crate::DEFAULT_EN_TEXT)
        .expect("the built-in baseline must parse");

    let mut out = String::new();
    out.push_str("# Replacement tokens\n\n");
    out.push_str("Every `{name}` in a localisation value is a slot the launcher fills at\n");
    out.push_str("render time. **This file is the contract for which slots each key may\n");
    out.push_str("use** — generated by `snug_format::tokens::replacement_tokens_markdown()`\n");
    out.push_str("and held to that function by a test, so it cannot drift.\n\n");
    out.push_str("## The rule that matters\n\n");
    out.push_str(
        "Tokens are **scoped to the dialog that owns the data**. Substitution does not\n\
         look a token up globally; each rendering site hands the filler a fixed list of\n\
         `(name, value)` pairs, and a key can only name slots from that list.\n\n\
         `runtime.retry.subheading` cannot say `{size_mb}` however it is spelled, because\n\
         the retry dialog has no download size to give. The list is a superset of what\n\
         today's copy uses, so there is room to write around the values you have.\n\n\
         A slot nobody supplies is **not an error**: unknown names are copied through\n\
         verbatim, on purpose, so a typo leaves a visible marker rather than silently\n\
         blank text. That is the right behaviour for a bundle and the wrong one for the\n\
         shipped baseline, which is why `snug_format::tokens::INJECTED_BY_KEY` exists and\n\
         a test holds the baseline to it.\n\n",
    );

    out.push_str("## Token meanings\n\n");
    let mut names: Vec<&str> = TOKEN_DESCRIPTIONS.iter().map(|(n, _)| *n).collect();
    names.sort_unstable();
    let token_rows: Vec<Vec<String>> = names
        .into_iter()
        .map(|name| {
            let desc = TOKEN_DESCRIPTIONS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, d)| *d)
                .unwrap_or("undocumented");
            vec![escape_cell(&format!("`{{{name}}}`")), escape_cell(desc)]
        })
        .collect();
    out.push_str(&render_table(&["Token", "Meaning"], &token_rows));
    out.push('\n');

    out.push_str("## Keys and the slots they may use\n\n");
    let key_rows: Vec<Vec<String>> = INJECTED_BY_KEY
        .iter()
        .map(|inj| {
            let used: Vec<&str> = baseline
                .get(inj.key)
                .map(placeholders)
                .unwrap_or_default()
                .into_iter()
                .collect();
            vec![
                escape_cell(&format!("`{}`", inj.key)),
                escape_cell(&brace_list(inj.tokens)),
                escape_cell(&if used.is_empty() {
                    "—".to_string()
                } else {
                    brace_list(&used)
                }),
            ]
        })
        .collect();
    out.push_str(&render_table(
        &["Key", "May use", "English copy uses"],
        &key_rows,
    ));
    out.push('\n');

    out.push_str("## Keys that may use nothing\n\n");
    out.push_str(
        "Every other key in the baseline is filled by no code path. A `{token}` in one\n\
         of these renders literally.\n\n",
    );
    let mut plain: Vec<&str> = baseline
        .entries
        .iter()
        .map(|(k, _)| k.as_str())
        .filter(|k| !INJECTED_BY_KEY.iter().any(|i| i.key == *k))
        .collect();
    plain.sort_unstable();
    for key in plain {
        out.push_str(&format!("- `{key}`\n"));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::localization::{Localization, placeholders};

    fn baseline() -> Localization {
        Localization::parse(crate::DEFAULT_EN_TAG, crate::DEFAULT_EN_TEXT)
            .expect("built-in baseline must parse")
    }

    /// The hole this module exists to close: the shipped English copy
    /// must never name a slot that no rendering site supplies.
    #[test]
    fn the_baseline_never_names_a_token_nobody_fills() {
        let b = baseline();
        let mut offenders = Vec::new();
        for (key, value) in &b.entries {
            for slot in placeholders(value) {
                let allowed = injected_for(key);
                if !allowed.contains(&slot) {
                    offenders.push(format!("{key} asks for {{{slot}}}"));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "the baseline asks for slots no call site supplies, so they would print \
             literally in shipped English:\n  {}",
            offenders.join("\n  ")
        );
    }

    /// Guards the other direction: a declaration for a key the baseline
    /// does not have is either a typo here or a key that was renamed, and
    /// both should fail loudly rather than quietly document nothing.
    #[test]
    fn no_declared_key_is_missing_from_the_baseline() {
        let b = baseline();
        let missing: Vec<&str> = INJECTED_BY_KEY
            .iter()
            .map(|i| i.key)
            .filter(|k| b.get(k).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "INJECTED_BY_KEY names keys the baseline does not have: {missing:?}"
        );
    }

    /// Every declared token should be a real, documented token. Catches a
    /// basket that invents a slot nobody will ever fill, which is the
    /// table lying to a translator.
    #[test]
    fn every_declared_token_is_documented() {
        use crate::localization::TOKEN_DESCRIPTIONS;
        for inj in INJECTED_BY_KEY {
            for token in inj.tokens {
                assert!(
                    *token == "0" || TOKEN_DESCRIPTIONS.iter().any(|(n, _)| n == token),
                    "{} declares {{{token}}}, which TOKEN_DESCRIPTIONS does not describe",
                    inj.key
                );
            }
        }
    }

    /// Where the checked-in table lives.
    fn checked_in_table_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../REPLACEMENT_TOKENS.md")
    }

    /// The table as it sits on disk *right now*.
    ///
    /// Read at run time on purpose. `include_str!` would bake the file
    /// in at compile time, and the table lives at the repository root —
    /// outside this crate's package directory — so cargo does not treat
    /// a change to it as a reason to rebuild. A baked-in copy is
    /// therefore stale exactly when it matters, and a guard that
    /// compares a stale copy is worse than no guard at all: it goes
    /// green while the file on disk has drifted.
    fn checked_in_table() -> String {
        let path = checked_in_table_path();
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    /// Split a table row into its cells.
    ///
    /// The generator escapes a literal pipe inside a cell as `\|`, so the
    /// split has to know about the escape; otherwise a description
    /// containing a pipe would look like two cells.
    fn split_row_cells(row: &str) -> Vec<String> {
        let mut cells: Vec<String> = Vec::new();
        let mut current = String::new();
        let mut escaped = false;
        for ch in row.chars() {
            if escaped {
                current.push(ch);
                escaped = false;
            } else if ch == '\\' {
                current.push(ch);
                escaped = true;
            } else if ch == '|' {
                cells.push(std::mem::take(&mut current));
            } else {
                current.push(ch);
            }
        }
        cells.push(current);
        // The pipes at either end of the row delimit the row; they are
        // not cells of their own.
        if cells.len() > 1 && cells.first().is_some_and(|c| c.trim().is_empty()) {
            cells.remove(0);
        }
        if cells.len() > 1 && cells.last().is_some_and(|c| c.trim().is_empty()) {
            cells.pop();
        }
        cells.into_iter().map(|c| c.trim().to_string()).collect()
    }

    /// Reduce one line of the table to what it *says*, discarding how it
    /// is spaced.
    ///
    /// The table is machine-generated, which makes it exactly the kind
    /// of file an editor reformats: markdown tooling pads every table
    /// cell out to the column width and widens the separator row to
    /// match. That changes the bytes without changing a word.
    ///
    /// So a table row is reduced to its trimmed cells, and a separator
    /// row — however many dashes the formatter chose — to one canonical
    /// marker. Everything else is compared trimmed, which covers the
    /// prose and the list of keys that may use nothing.
    fn canonical_line(line: &str) -> String {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            return trimmed.to_string();
        }
        let cells = split_row_cells(trimmed);
        let is_separator = !cells.is_empty()
            && cells
                .iter()
                .all(|c| !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':'));
        if is_separator {
            return format!("|{}|", vec!["---"; cells.len()].join("|"));
        }
        format!("|{}|", cells.join("|"))
    }

    /// The table's content, line by line, in a form that ignores
    /// formatting.
    fn canonical_lines(markdown: &str) -> Vec<String> {
        // `str::lines` already folds CRLF into LF and `canonical_line`
        // trims what is left, so the working tree's CRLF under
        // `core.autocrlf` and the generator's LF compare equal.
        markdown.lines().map(canonical_line).collect()
    }

    #[test]
    fn the_checked_in_table_matches_the_generator() {
        let checked_in = canonical_lines(&checked_in_table());
        let generated = canonical_lines(&replacement_tokens_markdown());
        let detail = match checked_in
            .iter()
            .zip(generated.iter())
            .enumerate()
            .find(|(_, (a, b))| a != b)
        {
            Some((index, (a, b))) => format!(
                "first difference at line {}:\n  checked in: {a}\n  generated:  {b}",
                index + 1
            ),
            None => format!(
                "the checked-in table has {} lines, the generator produces {}",
                checked_in.len(),
                generated.len()
            ),
        };
        assert!(
            checked_in == generated,
            "REPLACEMENT_TOKENS.md does not match the generator. {detail}\n\
             Run `cargo test -p snug-format regenerate_the_checked_in_table -- --ignored` to rewrite it."
        );
    }

    /// The line endings the checked-in table uses in this working tree.
    ///
    /// `core.autocrlf` hands out CRLF on Windows and LF everywhere else,
    /// so the file on disk is not always the LF the generator emits.
    /// Writing back the endings the file already has keeps `git status`
    /// quiet: a table rewritten with the other convention looks modified
    /// even when its content is identical to what is committed.
    fn working_tree_line_ending(existing: &str) -> &'static str {
        if existing.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        }
    }

    /// Not run by default: it writes into the repository. It exists so
    /// the file can be rebuilt from the table rather than hand-edited,
    /// which is the whole point of generating it.
    #[test]
    #[ignore = "writes REPLACEMENT_TOKENS.md into the repository root"]
    fn regenerate_the_checked_in_table() {
        let path = checked_in_table_path();
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let table =
            replacement_tokens_markdown().replace('\n', working_tree_line_ending(&existing));
        std::fs::write(&path, table).expect("write table");
        println!("wrote {}", path.display());
    }
}
