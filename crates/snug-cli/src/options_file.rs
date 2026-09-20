//! Support for a `snug.options` configuration file.
//!
//! The CLI accepts an optional `--options <path>` flag pointing at a
//! file containing one option per line. If no flag is given, `snug`
//! looks for `snug.options` in the current working directory.
//!
//! The file is parsed line by line, with each line tokenised as if it
//! were supplied on the command line (via `shell_words`). Lines starting
//! with `#` (after trimming) are comments; blank lines are skipped.
//!
//! Tokens from the file are prepended to the actual command-line args
//! before clap sees them, so any value on the command line overrides
//! the value in the file (clap's "last wins" semantics for non-repeatable
//! flags, "all collected" for repeatable ones like `--jvm-arg`).
//!
//! Example `snug.options`:
//!
//! ```text
//! # Default metadata for this project
//! --name "My App"
//! --company "Acme Corp"
//! --version "1.2.3"
//! --min-java 25
//! --main-class "com.example.Main"
//!
//! # JVM options applied in order
//! --jvm-arg=-Xms256m
//! --jvm-arg=-Xmx2g
//! --jvm-arg=-Dfile.encoding=UTF-8
//! ```

use std::path::{Path, PathBuf};

use thiserror::Error;

/// Default options-file name looked up in the current working directory
/// when `--options` is not supplied.
pub const DEFAULT_OPTIONS_FILE: &str = "snug.options";

/// Errors that can arise while resolving or loading a `snug.options`
/// file. Missing files are *not* errors — they just mean no defaults
/// were supplied.
#[derive(Debug, Error)]
pub enum OptionsFileError {
    /// I/O error reading the file (other than "not found").
    #[error("reading options file {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    /// A line in the file failed shell-style tokenisation. The 1-based
    /// `line` lets the user locate the bad input.
    #[error("invalid syntax in {path} at line {line}: {message}")]
    Syntax {
        path: PathBuf,
        line: usize,
        message: String,
    },
}

/// Resolve the options-file path from the raw command-line arguments.
///
/// - If `--options <path>` or `--options=<path>` is present, returns
///   that path (and the caller is expected to fail loudly if it does
///   not exist — explicit user intent).
/// - Otherwise, returns `cwd/snug.options` *only if it exists*. Returns
///   `None` if the default file is absent (no error).
pub fn resolve(raw_args: &[String], cwd: &Path) -> Option<PathBuf> {
    if let Some(p) = find_options_flag(raw_args) {
        return Some(p);
    }
    let default = cwd.join(DEFAULT_OPTIONS_FILE);
    if default.is_file() {
        Some(default)
    } else {
        None
    }
}

/// Walk the raw argv looking for `--options <path>` or `--options=<path>`.
///
/// Returns the resolved path, or `None` if the flag wasn't supplied.
/// Does not validate that the file exists; that's the caller's job.
pub fn find_options_flag(raw_args: &[String]) -> Option<PathBuf> {
    let mut iter = raw_args.iter().enumerate();
    while let Some((i, arg)) = iter.next() {
        if arg == "--options" {
            return raw_args.get(i + 1).map(PathBuf::from);
        }
        if let Some(value) = arg.strip_prefix("--options=") {
            return Some(PathBuf::from(value));
        }
    }
    None
}

/// Load a `snug.options` file and return its contents as a flat list of
/// argv-style tokens.
///
/// Comment lines (starting with `#` after trimming) and blank lines are
/// skipped. Each remaining line is tokenised with
/// [`shell_words::split`], which honours double-quoted strings,
/// single-quoted strings, and backslash escapes.
pub fn load(path: &Path) -> Result<Vec<String>, OptionsFileError> {
    let content = std::fs::read_to_string(path).map_err(|source| OptionsFileError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut tokens = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        match shell_words::split(trimmed) {
            Ok(line_tokens) => tokens.extend(line_tokens),
            Err(e) => {
                return Err(OptionsFileError::Syntax {
                    path: path.to_path_buf(),
                    line: idx + 1,
                    message: e.to_string(),
                });
            }
        }
    }
    Ok(tokens)
}

/// Build the merged argv that clap should parse.
///
/// The merged layout is: `[program_name, ...file_tokens, ...cli_args_minus_options_flag]`.
///
/// - `file_tokens` are prepended so any *later* CLI occurrence of the
///   same flag wins (clap's last-wins semantics).
/// - The `--options <path>` flag and its value are stripped from the
///   CLI portion so clap doesn't see them twice (the file has already
///   been loaded and merged).
pub fn merge(raw_args: &[String], file_tokens: Vec<String>) -> Vec<String> {
    let cli_flag_names = collect_long_flag_names(&raw_args[1..]);
    let file_tokens_filtered = strip_overridden_flags(&file_tokens, &cli_flag_names);

    let mut out = Vec::with_capacity(raw_args.len() + file_tokens_filtered.len());
    if let Some(prog) = raw_args.first() {
        out.push(prog.clone());
    } else {
        out.push("snug".to_string());
    }
    out.extend(file_tokens_filtered);

    let mut skip_next = false;
    for arg in &raw_args[1..] {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--options" {
            skip_next = true;
            continue;
        }
        if arg.starts_with("--options=") {
            continue;
        }
        out.push(arg.clone());
    }
    out
}

/// Collect long-flag names (`--name`, `--name=value`) from a slice of
/// argv tokens. Excludes `--options` and its value, stops at `--`.
fn collect_long_flag_names(args: &[String]) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--" {
            break;
        }
        if arg == "--options" {
            skip_next = true;
            continue;
        }
        if arg.starts_with("--options=") {
            continue;
        }
        if let Some(name) = long_flag_name(arg) {
            names.insert(name);
        }
    }
    names
}

/// Flag names whose `Cli` field is `Vec<T>` (ArgAction::Append).
///
/// Repeated occurrences of these flags must NOT be deduped at merge
/// time — both file and CLI contributions are collected and appended.
const REPEATABLE_FLAGS: &[&str] = &["jvm-arg"];

/// Strip any long flag (and its separate value token) from `tokens`
/// whose name is in `cli_flags` *and* is not in [`REPEATABLE_FLAGS`].
/// Embedded `--name=value` is stripped as a single token;
/// space-separated `--name value` consumes both.
fn strip_overridden_flags(
    tokens: &[String],
    cli_flags: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut out = Vec::with_capacity(tokens.len());
    let mut skip_next = false;
    for token in tokens {
        if skip_next {
            skip_next = false;
            continue;
        }
        if token == "--" {
            out.push(token.clone());
            continue;
        }
        if let Some(name) = long_flag_name(token) {
            if cli_flags.contains(&name) && !REPEATABLE_FLAGS.contains(&name.as_str()) {
                if !token.contains('=') {
                    skip_next = true;
                }
                continue;
            }
        }
        out.push(token.clone());
    }
    out
}

/// Extract the long-flag name from a token, or `None` if the token is
/// not a long flag. Handles `--name` and `--name=value`; rejects `--`
/// and `--something-with-leading-dash`.
fn long_flag_name(token: &str) -> Option<String> {
    let rest = token.strip_prefix("--")?;
    if rest.is_empty() || rest.starts_with('-') {
        return None;
    }
    let name = rest.split('=').next()?;
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn find_options_flag_space_form() {
        let a = args(&["snug", "app.jar", "--options", "custom.opts"]);
        assert_eq!(find_options_flag(&a), Some(PathBuf::from("custom.opts")));
    }

    #[test]
    fn find_options_flag_equals_form() {
        let a = args(&["snug", "--options=foo.opts", "app.jar"]);
        assert_eq!(find_options_flag(&a), Some(PathBuf::from("foo.opts")));
    }

    #[test]
    fn find_options_flag_absent() {
        let a = args(&["snug", "app.jar", "--name", "Foo"]);
        assert_eq!(find_options_flag(&a), None);
    }

    #[test]
    fn load_skips_comments_and_blank_lines() {
        let dir = tempdir();
        let f = dir.join("snug.options");
        std::fs::write(
            &f,
            "# top comment\n\
             \n\
             --name \"My App\"\n\
             # indented comment\n  \n\
             --company Acme\n",
        )
        .unwrap();
        let tokens = load(&f).unwrap();
        assert_eq!(
            tokens,
            vec!["--name".to_string(), "My App".to_string(), "--company".to_string(), "Acme".to_string()]
        );
    }

    #[test]
    fn load_reports_syntax_errors_with_line_number() {
        let dir = tempdir();
        let f = dir.join("snug.options");
        // Unbalanced quote on line 2.
        std::fs::write(&f, "# comment\n--name \"oops\n").unwrap();
        let err = load(&f).unwrap_err();
        match err {
            OptionsFileError::Syntax { line, .. } => assert_eq!(line, 2),
            other => panic!("expected Syntax, got {other:?}"),
        }
    }

    #[test]
    fn merge_prepends_file_tokens_and_strips_options_flag() {
        let raw = args(&["snug", "app.jar", "--options", "x.opts"]);
        let merged = merge(&raw, vec!["--name".into(), "File".into()]);
        // File tokens come first; CLI supplies its own values. No
        // dedup needed here because CLI doesn't repeat --name.
        assert_eq!(
            merged,
            vec![
                "snug".to_string(),
                "--name".to_string(),
                "File".to_string(),
                "app.jar".to_string(),
            ]
        );
    }

    #[test]
    fn merge_strips_file_flag_when_cli_overrides() {
        let raw = args(&["snug", "app.jar", "--name", "CLI"]);
        let merged = merge(&raw, vec!["--name".into(), "File".into(), "--company".into(), "Co".into()]);
        // The file's --name is dropped because CLI also specifies it;
        // --company from the file is kept.
        assert_eq!(
            merged,
            vec![
                "snug".to_string(),
                "--company".to_string(),
                "Co".to_string(),
                "app.jar".to_string(),
                "--name".to_string(),
                "CLI".to_string(),
            ]
        );
    }

    #[test]
    fn merge_strips_file_flag_equals_form_when_cli_overrides() {
        let raw = args(&["snug", "app.jar", "--name=CLI"]);
        let merged = merge(&raw, vec!["--name=File".into(), "--company=Co".into()]);
        assert_eq!(
            merged,
            vec![
                "snug".to_string(),
                "--company=Co".to_string(),
                "app.jar".to_string(),
                "--name=CLI".to_string(),
            ]
        );
    }

    #[test]
    fn merge_keeps_repeatable_flags_from_both() {
        // --jvm-arg is repeatable (Vec<String>). It should appear twice
        // in the merged argv because clap's ArgAction::Append collects
        // all occurrences.
        let raw = args(&["snug", "app.jar", "--jvm-arg=-Xmx2g"]);
        let merged = merge(&raw, vec!["--jvm-arg=-Xms256m".into()]);
        assert!(merged.contains(&"--jvm-arg=-Xms256m".to_string()));
        assert!(merged.contains(&"--jvm-arg=-Xmx2g".to_string()));
    }

    #[test]
    fn merge_strips_options_equals_form() {
        let raw = args(&["snug", "--options=x.opts", "app.jar"]);
        let merged = merge(&raw, vec![]);
        assert_eq!(merged, vec!["snug".to_string(), "app.jar".to_string()]);
    }

    #[test]
    fn resolve_prefers_explicit_options_flag() {
        let dir = tempdir();
        let explicit = dir.join("custom.opts");
        std::fs::write(&explicit, "--name X\n").unwrap();
        // Default file also exists, but the explicit flag wins.
        std::fs::write(dir.join(DEFAULT_OPTIONS_FILE), "--name Y\n").unwrap();
        let raw = args(&["snug", "--options", explicit.to_str().unwrap()]);
        let resolved = resolve(&raw, &dir).unwrap();
        assert_eq!(resolved, explicit);
    }

    #[test]
    fn resolve_falls_back_to_cwd_default() {
        let dir = tempdir();
        std::fs::write(dir.join(DEFAULT_OPTIONS_FILE), "--name Y\n").unwrap();
        let raw = args(&["snug", "app.jar"]);
        let resolved = resolve(&raw, &dir).unwrap();
        assert_eq!(resolved, dir.join(DEFAULT_OPTIONS_FILE));
    }

    #[test]
    fn resolve_returns_none_when_no_file_present() {
        let dir = tempdir();
        let raw = args(&["snug", "app.jar"]);
        assert_eq!(resolve(&raw, &dir), None);
    }

    fn tempdir() -> std::path::PathBuf {
        let base = std::env::temp_dir();
        let unique = format!(
            "snug-options-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = base.join(unique);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
