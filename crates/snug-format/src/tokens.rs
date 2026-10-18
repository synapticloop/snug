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
//! - the checked-in `REPLACEMENT_TOKENS.md` is byte-identical (modulo
//!   line endings) to [`replacement_tokens_markdown`].
//!
//! The third one is the one that matters most. A hand-maintained copy of
//! a table like this drifts silently and nobody compares the two — which
//! is the same failure this crate's localisation catalogue has already
//! produced once.

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
    Injection { key: "err.format", tokens: &["0"] },
    Injection { key: "err.invalid_state", tokens: &["0"] },
    Injection { key: "err.io", tokens: &["0"] },
    Injection { key: "err.java_exception", tokens: &["0"] },
    Injection { key: "err.jni_attach", tokens: &["0"] },
    Injection { key: "err.jni_create", tokens: &["0"] },
    Injection { key: "err.jni_init", tokens: &["0"] },
    Injection { key: "err.jni_invoke", tokens: &["0"] },
    Injection { key: "err.main_class_not_found", tokens: &["0"] },
    Injection { key: "err.no_main_method", tokens: &["0"] },
    Injection { key: "err.self_path", tokens: &["0"] },
    Injection { key: "err.zip", tokens: &["0"] },
    // --- err.* -- named, from error.rs ----------------------------------
    Injection { key: "err.jvm_not_found", tokens: &["min_java"] },
    Injection { key: "err.jvm_too_old", tokens: &["path", "found", "min_java"] },
    Injection { key: "err.library_load", tokens: &["library", "message"] },
    Injection { key: "err.symbol_not_found", tokens: &["symbol", "library"] },
    // --- jdk.err.* ------------------------------------------------------
    Injection { key: "jdk.err.bad_metadata_shape", tokens: &["0"] },
    Injection { key: "jdk.err.dialog", tokens: &["0"] },
    Injection { key: "jdk.err.download", tokens: &["0"] },
    Injection { key: "jdk.err.extract", tokens: &["0"] },
    Injection { key: "jdk.err.io", tokens: &["0"] },
    Injection { key: "jdk.err.metadata_fetch", tokens: &["0"] },
    Injection { key: "jdk.err.no_java_exe", tokens: &["0"] },
    Injection { key: "jdk.err.no_metadata_for_version", tokens: &["0"] },
    Injection { key: "jdk.err.bad_field", tokens: &["name", "value"] },
    Injection { key: "jdk.err.sha256_mismatch", tokens: &["declared", "computed"] },
    // --- splash.err.* ---------------------------------------------------
    Injection { key: "splash.err.thread_spawn", tokens: &["0"] },
    Injection { key: "splash.err.buffer_length", tokens: &["expected", "actual"] },
    // --- launcher.* -----------------------------------------------------
    Injection { key: "launcher.bare_stub.no_payload", tokens: &["path"] },
    Injection { key: "launcher.error.content", tokens: &["error"] },
    Injection { key: "launcher.error.info_subtext_2", tokens: &["update_check_url"] },
    // --- runtime.prompt.* -- the install offer ---------------------------
    Injection { key: "runtime.prompt.content", tokens: &["version", "size_mb", "sha256"] },
    // Uses none today; the download button is filled by the same site, so
    // a translator may put the version or size in it.
    Injection { key: "runtime.prompt.button_download", tokens: &["version", "size_mb", "sha256"] },
    // --- runtime.metadata_failed.* ---------------------------------------
    // Uses none today. `major` was here because the site's `fill` passes
    // it, and an earlier draft of the copy wanted the Java major in the
    // subheading.
    Injection { key: "runtime.metadata_failed.subheading", tokens: &["major"] },
    Injection { key: "runtime.metadata_failed.content", tokens: &["major", "error"] },
    // --- runtime.progress.* ---------------------------------------------
    Injection { key: "runtime.progress.content", tokens: &["version", "size_mb"] },
    Injection { key: "runtime.progress.phase_label", tokens: &["arch"] },
    Injection { key: "runtime.progress.pct_label", tokens: &["pct"] },
    Injection { key: "runtime.progress.detail_with_size", tokens: &["done_mb", "total_mb", "speed_mb_s"] },
    Injection { key: "runtime.progress.detail_no_size", tokens: &["pct"] },
    Injection { key: "runtime.progress.detail_eta_seconds", tokens: &["n"] },
    // --- runtime.failure.* / runtime.retry.* -----------------------------
    Injection { key: "runtime.failure.content", tokens: &["version", "error"] },
    Injection { key: "runtime.retry.subheading", tokens: &["version", "attempt", "max_attempts", "error"] },
    Injection { key: "runtime.retry.content", tokens: &["version", "attempt", "max_attempts", "error"] },
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

/// Render the checked-in `REPLACEMENT_TOKENS.md`.
///
/// Generated from [`INJECTED_BY_KEY`] and the live baseline so the
/// "used by the English copy" column cannot fall out of date. A test
/// asserts the file on disk equals this, modulo line endings — the
/// working tree is CRLF under `core.autocrlf` while this emits LF, and
/// comparing the two raw would fail on a fresh clone for a reason that
/// has nothing to do with the content.
pub fn replacement_tokens_markdown() -> String {
    use crate::localization::{placeholders, Localization, TOKEN_DESCRIPTIONS};

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
    out.push_str("| Token | Meaning |\n|---|---|\n");
    let mut names: Vec<&str> = TOKEN_DESCRIPTIONS.iter().map(|(n, _)| *n).collect();
    names.sort_unstable();
    for name in names {
        let desc = TOKEN_DESCRIPTIONS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, d)| *d)
            .unwrap_or("undocumented");
        out.push_str(&format!(
            "| `{{{name}}}` | {desc} |\n",
            desc = escape_cell(desc)
        ));
    }

    out.push_str("\n## Keys and the slots they may use\n\n");
    out.push_str("| Key | May use | English copy uses |\n|---|---|---|\n");
    for inj in INJECTED_BY_KEY {
        let used: Vec<&str> = baseline
            .get(inj.key)
            .map(placeholders)
            .unwrap_or_default()
            .into_iter()
            .collect();
        out.push_str(&format!(
            "| `{}` | {} | {} |\n",
            inj.key,
            brace_list(inj.tokens),
            if used.is_empty() {
                "—".to_string()
            } else {
                brace_list(&used)
            }
        ));
    }

    out.push_str("\n## Keys that may use nothing\n\n");
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
    use crate::localization::{placeholders, Localization};

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

    const CHECKED_IN: &str = include_str!("../../../REPLACEMENT_TOKENS.md");

    /// Line endings are normalised on both sides: `core.autocrlf` hands
    /// out CRLF while the generator emits LF, and a raw comparison would
    /// fail on every fresh clone for a reason unrelated to content.
    fn normalise(s: &str) -> String {
        s.replace("\r\n", "\n")
    }

    #[test]
    fn the_checked_in_table_matches_the_generator() {
        assert_eq!(
            normalise(CHECKED_IN),
            normalise(&replacement_tokens_markdown()),
            "REPLACEMENT_TOKENS.md is stale. Run \
             `cargo test -p snug-format regenerate_the_checked_in_table -- --ignored`"
        );
    }

    /// Not run by default: it writes into the repository. It exists so
    /// the file can be rebuilt from the table rather than hand-edited,
    /// which is the whole point of generating it.
    #[test]
    #[ignore = "writes REPLACEMENT_TOKENS.md into the repository root"]
    fn regenerate_the_checked_in_table() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../REPLACEMENT_TOKENS.md");
        std::fs::write(&path, replacement_tokens_markdown()).expect("write table");
        println!("wrote {}", path.display());
    }
}