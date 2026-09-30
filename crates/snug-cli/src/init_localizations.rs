//! `snug --init-localizations` — write a starter `localisations/`
//! directory of `snug-localisations.<tag>.txt` bundles to disk (or
//! stdout).
//!
//! Sibling of [`crate::init_options`]: that one bootstraps the
//! `snug.options` config file, this one bootstraps the translation
//! bundles a build embeds via `--localization`. Both are
//! "write-a-starter-and-exit" modes with the same three knobs — a
//! target path, `--force`, and `--stdout` — so the two can be passed
//! together (`snug --init-options --init-localizations`) to lay down a
//! whole project skeleton in one go.
//!
//! ## Default target
//!
//! `./localisations`, relative to the **current working directory** —
//! the directory `snug` was invoked from, not the directory the
//! `snug.exe` binary lives in. `cd ~/work/myapp && snug/snug.exe
//! --init-localizations` writes `~/work/myapp/localisations/`.
//!
//! This is deliberately the opposite of the `snug.options` *read*
//! order, which prefers the executable's own directory (see
//! [`crate::options_file::resolve`]) so a shipped `snug.exe` carries
//! its defaults wherever it runs from. Translations are per-project
//! source, not a shipped artefact: they belong in the project you are
//! standing in. `--init-options` also defaults to a CWD-relative
//! path, so both init modes agree with each other.
//!
//! ## What gets written
//!
//! One file per requested tag, named by [`crate::localization`]:
//! `snug-localisations.<tag>.txt`.
//!
//! - `en` (always, first) is the built-in baseline verbatim — the
//!   canonical key inventory, already self-documenting.
//! - Any extra `--init-localizations-tag` gets a *translation
//!   template*: the same key set with a translator-facing header
//!   prepended and the English header block stripped. Pre-filling the
//!   English values (rather than commenting every line out) is what
//!   makes the file loadable as-is, so `--localization localisations`
//!   works immediately and the build-time missing-key warning stays
//!   quiet; the header is loud that every value still needs
//!   translating.
//!
//! Nothing else is written into the directory. That's a hard
//! constraint, not a style choice: `localization::collect` scans a
//! `--localization <dir>` top-level and *fails the build* on any file
//! that doesn't match `snug-localisations.<tag>.txt`, so a `README.md`
//! or a `.bak` sitting next to the bundles would break the build that
//! consumes them. The header inside each generated file says so too.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

// Import order follows the rest of the crate (`bail` first); rustfmt's
// 2024 style-edition sort would reorder it, but the crate is written in
// the established order and a one-line diff here isn't worth a
// repo-wide reformat.
use anyhow::{bail, Context, Result};

use crate::localization::{DEFAULT_EN_TAG, DEFAULT_EN_TEXT, expected_filename};

/// Path snug uses when `--init-localizations` is given with no explicit
/// value. Relative, so it resolves against the current working
/// directory — the directory `snug` was invoked from.
pub const DEFAULT_DIR: &str = "localisations";

/// Write / print starter localization bundles. The single entry point
/// used by `main.rs` after the CLI parses.
///
/// Arguments:
/// - `dir`: the target directory (defaults to `localisations`). Created
///   if missing, including intermediate parents.
/// - `tags`: extra locale tags to scaffold. `en` is always included
///   first regardless of what this contains.
/// - `force`: when `false`, refuse to overwrite any existing bundle.
/// - `to_stdout`: when `true`, print to stdout instead of writing to
///   disk — one `# === <path> ===` banner per file so a multi-tag
///   stream stays splittable by eye.
///
/// Returns `Ok(())` on a successful write / print, `Err` otherwise.
pub fn run(dir: &str, tags: &[String], force: bool, to_stdout: bool) -> Result<()> {
    let tags = effective_tags(tags)?;
    if to_stdout {
        return print_to_stdout(Path::new(dir), &tags);
    }
    write_to_disk(Path::new(dir), &tags, force)
}

/// Normalise the requested tags into the exact list of files to write:
/// the English baseline first, then each extra tag once, in the order
/// given. A tag repeated on the command line, or repeated as `en`, is
/// written once rather than clobbering itself.
fn effective_tags(tags: &[String]) -> Result<Vec<String>> {
    let mut seen = BTreeSet::new();
    let mut out = vec![DEFAULT_EN_TAG.to_string()];
    seen.insert(DEFAULT_EN_TAG.to_string());
    for tag in tags {
        validate_tag(tag)?;
        if seen.insert(tag.clone()) {
            out.push(tag.clone());
        }
    }
    Ok(out)
}

/// Reject anything that would produce an unusable filename or escape
/// the target directory. The tag is interpolated straight into
/// `snug-localisations.<tag>.txt` and then parsed back out by
/// [`crate::localization::tag_from_path`], so a `.` would silently
/// change the parsed tag and a separator would write outside `dir`.
fn validate_tag(tag: &str) -> Result<()> {
    if tag.is_empty() {
        bail!("locale tag must not be empty (expected e.g. `de`, `en-US`, `pt-BR`)");
    }
    if let Some(bad) = tag
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
    {
        bail!(
            "invalid locale tag `{tag}`: `{bad}` is not allowed\n\
             hint: use a BCP 47 tag of ASCII letters, digits and `-` \
             (e.g. `de`, `en-US`, `pt-BR`, `zh-Hans`)"
        );
    }
    Ok(())
}

/// Build the file body for one tag.
///
/// `en` is the baseline verbatim — its own header already explains the
/// format, the naming rule, and the "copy me to add a language"
/// workflow, so there is nothing to add and rewriting it would only
/// create a second copy to keep in sync. Any other tag gets the
/// baseline's key set under a translator-facing header, with the
/// baseline's own header block (everything up to its closing `# ===`
/// fence) dropped so the file doesn't claim to be the baseline.
fn template_for_tag(tag: &str, dir: &Path) -> String {
    if tag == DEFAULT_EN_TAG {
        return DEFAULT_EN_TEXT.to_string();
    }
    format!("{}\n{}", tag_header(tag, dir), baseline_body())
}

/// The baseline minus its leading prose header.
///
/// The baseline opens with a fenced comment block (`# ===` / title /
/// `# ===`, then the "what is this file" prose, then a closing fence).
/// None of that belongs in a translation template — it describes the
/// *English baseline*, not the file being written — so we cut the whole
/// block, defined structurally rather than by counting fences: the
/// header is everything above the first `key = value` line, and the cut
/// lands just after the last fence before it. Cutting on the key set
/// means the per-section comments a translator navigates by
/// (`# ---- Launcher-runtime errors ----`) survive, and it survives the
/// baseline being reworded or its fence count changing.
///
/// If no entry line is found at all, the whole text is returned: a
/// duplicated comment block is harmless, a truncated key list is not.
fn baseline_body() -> &'static str {
    // (line without its terminator, byte offset just past that line).
    // Byte offsets, not line numbers — indexing the string with a line
    // index would cut mid-line, leaving a bare `====` run that the
    // localization parser then reads as an empty key.
    let mut lines: Vec<(&str, usize)> = Vec::new();
    let mut offset = 0usize;
    for chunk in DEFAULT_EN_TEXT.split_inclusive('\n') {
        offset += chunk.len();
        let line = chunk.trim_end_matches(['\n', '\r']);
        lines.push((line, offset));
    }

    let is_entry = |line: &str| -> bool {
        let trimmed = line.trim();
        !trimmed.is_empty() && !trimmed.starts_with('#') && trimmed.contains('=')
    };

    // Nothing above the first entry is an entry, so the whole prose
    // header lives there. Without one, keep everything.
    let Some(first_entry) = lines.iter().position(|(line, _)| is_entry(line)) else {
        return DEFAULT_EN_TEXT;
    };

    // Cut just past the last fence above it; the per-section comments
    // below that fence are the part worth keeping.
    let body_start = (0..first_entry)
        .rev()
        .find(|&idx| lines[idx].0.trim_start().starts_with("# ==="))
        .map_or(first_entry, |idx| idx + 1);

    &DEFAULT_EN_TEXT[lines[body_start.saturating_sub(1)].1..]
}

fn tag_header(tag: &str, dir: &Path) -> String {
    let filename = expected_filename(tag);
    let dir = dir.display();
    format!(
        "# ============================================================================\n\
         #  {filename} — translation template for `{tag}`\n\
         # ============================================================================\n\
         #\n\
         #  Scaffolded by `snug --init-localizations --init-localizations-tag {tag}`.\n\
         #\n\
         #  EVERY VALUE BELOW IS STILL THE BUILT-IN ENGLISH BASELINE. Replace each\n\
         #  `value` with its `{tag}` translation, keeping the keys byte-identical — the\n\
         #  launcher looks strings up by key, not by line order. A line you leave as\n\
         #  English just shows English to `{tag}` users, so translate the whole file.\n\
         #\n\
         #  Placeholders (`{{0}}`, `{{path}}`, `{{min_java}}`, ...) are filled in at\n\
         #  lookup time. Keep them in the translated string; move them wherever the\n\
         #  target language's word order needs them.\n\
         #\n\
         #  Building with it\n\
         #  ----------------\n\
         #    snug app.jar --localization {dir} -o App.exe\n\
         #\n\
         #  The whole directory can be passed in one go — `--localization {dir}`\n\
         #  scans it top-level for `snug-localisations.<tag>.txt` files. Nothing else\n\
         #  may live in there: a stray README, `.bak`, or note file fails the build,\n\
         #  because every entry has to match that pattern.\n\
         #\n\
         #  Every key in the English baseline is present below, so the build-time\n\
         #  missing-key warning stays quiet. Add a key later by re-running\n\
         #  `snug --init-localizations --init-localizations-force` and re-applying your\n\
         #  translations to the new lines.\n\
         # ============================================================================\n"
    )
}

fn print_to_stdout(dir: &Path, tags: &[String]) -> Result<()> {
    let mut out = std::io::stdout().lock();
    for tag in tags {
        let name = expected_filename(tag);
        writeln!(out, "# ==== {}/{name} ====", dir.display())
            .context("writing banner to stdout")?;
        out.write_all(template_for_tag(tag, dir).as_bytes())
            .with_context(|| format!("writing {name} template to stdout"))?;
    }
    out.flush().context("flushing stdout")?;
    Ok(())
}

fn write_to_disk(dir: &Path, tags: &[String], force: bool) -> Result<()> {
    // Validate every tag before touching the filesystem so a bad tag
    // can't leave a half-populated directory behind.
    for tag in tags {
        validate_tag(tag)?;
    }

    // Check every target up front, before creating anything, for the
    // same reason: a refusal should be a refusal, not a partial write.
    if !force {
        for tag in tags {
            let path = dir.join(expected_filename(tag));
            if path.exists() {
                bail!(
                    "refusing to overwrite existing file at `{}`\n\
                     hint: pass --init-localizations-force to overwrite, or move the existing file aside",
                    path.display()
                );
            }
        }
    }

    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating localizations directory {}", dir.display()))?;

    for tag in tags {
        let name = expected_filename(tag);
        let path = dir.join(&name);
        std::fs::write(&path, template_for_tag(tag, dir).as_bytes())
            .with_context(|| format!("writing {}", path.display()))?;
        eprintln!("snug: wrote {}", path.display());
    }

    eprintln!(
        "snug: translate every value in each file, then build with `--localization {}`.",
        dir.display()
    );
    Ok(())
}

/// Convenience for tests and callers that want the canonical path of a
/// bundle inside a target directory.
pub fn bundle_path(dir: &Path, tag: &str) -> PathBuf {
    dir.join(expected_filename(tag))
}

#[cfg(test)]
mod tests {
    use super::*;
    use snug_format::Localization;

    fn tmpdir() -> PathBuf {
        let base = std::env::temp_dir();
        let unique = format!(
            "snug-init-loc-{}-{}",
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
    fn default_dir_is_cwd_relative() {
        assert_eq!(DEFAULT_DIR, "localisations");
        // Relative, not absolute and not exe-relative: the directory
        // snug was invoked from wins.
        assert!(Path::new(DEFAULT_DIR).is_relative());
    }

    #[test]
    fn effective_tags_always_leads_with_english() {
        let tags = effective_tags(&[]).unwrap();
        assert_eq!(tags, vec!["en".to_string()]);
    }

    #[test]
    fn effective_tags_dedupes_and_preserves_order() {
        let tags = effective_tags(&[
            "de".to_string(),
            "en".to_string(),
            "de".to_string(),
            "pt-BR".to_string(),
        ])
        .unwrap();
        assert_eq!(tags, vec!["en", "de", "pt-BR"]);
    }

    #[test]
    fn validate_tag_rejects_unsafe_and_malformed_tags() {
        for bad in ["", "de/../en", "en.txt", "en US", "..", "de\\fr", "en\r\nX"] {
            assert!(
                validate_tag(bad).is_err(),
                "expected `{bad}` to be rejected as a locale tag"
            );
        }
    }

    #[test]
    fn validate_tag_accepts_bcp47_shapes() {
        for good in ["en", "en-US", "pt-BR", "zh-Hans", "de_CH", "es-419"] {
            assert!(
                validate_tag(good).is_ok(),
                "expected `{good}` to be accepted as a locale tag"
            );
        }
    }

    #[test]
    fn english_template_is_the_baseline_verbatim() {
        assert_eq!(
            template_for_tag("en", Path::new(DEFAULT_DIR)),
            DEFAULT_EN_TEXT
        );
    }

    #[test]
    fn translated_template_keeps_every_baseline_key() {
        let text = template_for_tag("de", Path::new(DEFAULT_DIR));
        let baseline = Localization::parse(DEFAULT_EN_TAG, DEFAULT_EN_TEXT).unwrap();
        let translated = Localization::parse("de", &text).expect("template parses");
        assert_eq!(translated.tag, "de");
        assert_eq!(
            translated.entries.len(),
            baseline.entries.len(),
            "translation template must carry the full key set"
        );
        for (key, value) in &baseline.entries {
            assert_eq!(
                translated.get(key),
                Some(value.as_str()),
                "template should scaffold `{key}` with its English value"
            );
        }
    }

    #[test]
    fn translated_template_drops_the_baseline_header() {
        let body = baseline_body();
        // The whole prose block goes, not just the fenced title. A
        // partial cut leaves a translation template that still calls
        // itself "the canonical English baseline".
        for leaked in [
            "built-in English baseline",
            "This file is the canonical English baseline",
            "Adding a new language",
        ] {
            assert!(
                !body.contains(leaked),
                "baseline header text `{leaked}` leaked into the translation template"
            );
        }
        // The cut lands on a line boundary, not inside the fence that
        // delimits the header. A stray `====` run parses as an empty
        // key, so this is a correctness check, not a cosmetic one.
        assert!(
            body.lines()
                .next()
                .is_none_or(|l| l.trim().is_empty() || l.starts_with('#')),
            "body should open on a blank or comment line, got: {:?}",
            body.lines().next()
        );
        // ...but the per-section comments a translator navigates by
        // must stay.
        assert!(body.contains("Launcher-runtime errors"));
        // And the entries themselves, from first to last.
        assert!(body.contains("err.zip ="));
        assert!(body.contains("launcher.error.content ="));
    }

    #[test]
    fn baseline_body_keeps_every_entry_and_nothing_else() {
        // The body is exactly the baseline's `key = value` lines plus
        // comments — no entries lost, none invented.
        let body = baseline_body();
        let baseline = Localization::parse(DEFAULT_EN_TAG, DEFAULT_EN_TEXT).unwrap();
        let from_body = Localization::parse("de", body).expect("body parses standalone");
        assert_eq!(from_body.entries.len(), baseline.entries.len());
        for (key, value) in &baseline.entries {
            assert_eq!(from_body.get(key), Some(value.as_str()), "lost `{key}`");
        }
    }

    #[test]
    fn translated_template_header_names_the_tag_and_warns() {
        let text = template_for_tag("pt-BR", Path::new("custom/i18n"));
        assert!(text.contains("snug-localisations.pt-BR.txt"));
        assert!(text.contains("`pt-BR`"));
        assert!(
            text.to_lowercase().contains("english"),
            "header must be explicit that values are still English"
        );
        // The suggested build line points at the actual target dir,
        // not a hardcoded `localisations`.
        assert!(text.contains("--localization custom/i18n"));
    }

    #[test]
    fn run_writes_directory_with_english_baseline() {
        let dir = tmpdir().join("localisations");
        run(dir.to_str().unwrap(), &[], false, false).unwrap();
        let written = std::fs::read_to_string(bundle_path(&dir, "en")).unwrap();
        assert_eq!(written, DEFAULT_EN_TEXT);
    }

    #[test]
    fn run_creates_intermediate_directories() {
        let dir = tmpdir().join("a").join("b").join("localisations");
        run(dir.to_str().unwrap(), &[], false, false).unwrap();
        assert!(bundle_path(&dir, "en").is_file());
    }

    #[test]
    fn run_writes_every_requested_tag() {
        let dir = tmpdir();
        run(
            dir.to_str().unwrap(),
            &["de".to_string(), "ja".to_string()],
            false,
            false,
        )
        .unwrap();
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "snug-localisations.de.txt",
                "snug-localisations.en.txt",
                "snug-localisations.ja.txt",
            ]
        );
    }

    #[test]
    fn run_refuses_to_overwrite_without_force() {
        let dir = tmpdir();
        run(dir.to_str().unwrap(), &[], false, false).unwrap();
        let err = run(dir.to_str().unwrap(), &["de".to_string()], false, false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("refusing to overwrite") && msg.contains("--init-localizations-force"),
            "unexpected error: {msg}"
        );
        // The refusal happened before any new file was written.
        assert!(!bundle_path(&dir, "de").exists());
    }

    #[test]
    fn run_overwrites_with_force() {
        let dir = tmpdir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(bundle_path(&dir, "en"), "stale").unwrap();
        run(dir.to_str().unwrap(), &[], true, false).unwrap();
        assert_eq!(
            std::fs::read_to_string(bundle_path(&dir, "en")).unwrap(),
            DEFAULT_EN_TEXT
        );
    }

    #[test]
    fn run_rejects_bad_tag_before_writing_anything() {
        let dir = tmpdir().join("localisations");
        let err = run(
            dir.to_str().unwrap(),
            &["../escape".to_string()],
            false,
            false,
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("invalid locale tag"),
            "unexpected error: {err:#}"
        );
        // Nothing created, and in particular nothing outside the dir.
        assert!(!dir.exists());
        assert!(!tmpdir_sibling_escaped());
    }

    /// Guards the traversal rejection: a tag like `../../evil` must
    /// never have produced a file above the temp dir we handed out.
    fn tmpdir_sibling_escaped() -> bool {
        std::env::temp_dir()
            .join("snug-localisations.escape.txt")
            .exists()
    }

    #[test]
    fn written_directory_is_directly_consumable_by_localization_collect() {
        // The whole point of the default layout: the generated
        // directory can be handed straight to `--localization` without
        // the build tripping over a non-matching file.
        let dir = tmpdir();
        run(dir.to_str().unwrap(), &["de".to_string()], false, false).unwrap();
        let bundles = crate::localization::collect(&[dir]).expect("collect accepts the scaffold");
        let tags: Vec<&str> = bundles.iter().map(|b| b.tag.as_str()).collect();
        assert_eq!(tags, vec!["en", "de"]);
        crate::localization::ensure_unique_tags(&bundles).unwrap();
    }

    #[test]
    fn run_to_stdout_does_not_touch_disk() {
        let dir = tmpdir().join("localisations");
        // Smoke: the stdout path writes to the process stdout, which
        // isn't capturable without a `gag`-style crate. Assert the
        // important half — no directory is created.
        run(dir.to_str().unwrap(), &["de".to_string()], false, true).unwrap();
        assert!(!dir.exists());
    }
}
