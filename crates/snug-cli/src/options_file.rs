//! Support for a `snug.options` configuration file.
//!
//! The CLI accepts an optional `--options <path>` flag pointing at a
//! file containing one option per line. If no flag is given, `snug`
//! looks for `snug.options` next to the `snug` executable first, then
//! in the current working directory.
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

/// Default options-file name looked up next to the `snug` executable
/// and then in the current working directory when `--options` is not
/// supplied.
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
/// Precedence, highest first:
///
/// 1. An explicit `--options <path>` / `--options=<path>` flag. The
///    caller is expected to fail loudly if the path does not exist —
///    explicit user intent.
/// 2. `<exe_dir>/snug.options`, if it exists. This lets a portable
///    `snug.exe` shipped alongside a `snug.options` carry its defaults
///    with it, regardless of where it is invoked from.
/// 3. `<cwd>/snug.options`, if it exists.
///
/// `exe_dir` is `None` when the executable's own location is unknown.
/// Returns `None` when no candidate file is present (no error).
pub fn resolve(raw_args: &[String], cwd: &Path, exe_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = find_options_flag(raw_args) {
        return Some(p);
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = exe_dir {
        candidates.push(dir.join(DEFAULT_OPTIONS_FILE));
    }
    candidates.push(cwd.join(DEFAULT_OPTIONS_FILE));
    candidates.into_iter().find(|p| p.is_file())
}

/// The directory containing the running `snug` executable, or `None` if
/// the platform refuses to report it.
pub fn current_exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
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
    let spec = FlagSpec::from_cli();
    let cli_flag_names = spec.collect_cli_flags(&raw_args[1..]);
    let file_tokens_filtered = spec.strip_overridden(&file_tokens, &cli_flag_names);

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

/// What clap knows about the flag surface, resolved once per merge.
///
/// This exists because the previous implementation compared flag *strings*.
/// A `snug.options` carrying `--output` was therefore not overridden by
/// `-o` on the command line, and clap rejected the merged argv with
///
/// ```text
/// error: the argument '--output <EXE>' cannot be used multiple times
/// ```
///
/// which is the worst possible shape for a documented rule ("CLI flags
/// always override file values"): the file, the short form, and the error
/// message all have to be read together to see that nothing is actually
/// wrong. Deriving the alias table from the [`Cli`] definition means a
/// new `#[arg(short = 'x')]` can never be half-wired, and it lets us learn
/// arity — which is what makes `--jvm-arg -Xmx2g` parse as one flag plus
/// its value rather than two flags.
struct FlagSpec {
    /// Every accepted spelling (long, short, hidden and visible aliases)
    /// mapped to the arg's long name — the canonical form used everywhere
    /// below.
    aliases: std::collections::HashMap<String, String>,
    /// Canonical names of args that consume a following value token.
    takes_value: std::collections::HashSet<String>,
    /// Canonical names of `ArgAction::Append` args, i.e. `Vec<T>` fields.
    /// Their values accumulate across file and CLI instead of overriding.
    repeatable: std::collections::HashSet<String>,
}

impl FlagSpec {
    /// Build the table from the `Cli` definition itself.
    fn from_cli() -> Self {
        use clap::CommandFactory;

        let mut spec = Self {
            aliases: std::collections::HashMap::new(),
            takes_value: std::collections::HashSet::new(),
            repeatable: std::collections::HashSet::new(),
        };

        for arg in crate::cli::Cli::command().get_arguments() {
            // A positional has no long name; fall back to its id so the
            // sets below still get a key. It can never collide with a
            // flag because positionals do not start with `-`.
            let canonical = arg
                .get_long()
                .unwrap_or_else(|| arg.get_id().as_str())
                .to_string();

            if let Some(long) = arg.get_long() {
                spec.aliases.insert(long.to_string(), canonical.clone());
            }
            if let Some(aliases) = arg.get_all_aliases() {
                for alias in aliases {
                    spec.aliases.insert(alias.to_string(), canonical.clone());
                }
            }
            if let Some(short) = arg.get_short() {
                spec.aliases.insert(short.to_string(), canonical.clone());
            }
            if let Some(shorts) = arg.get_all_short_aliases() {
                for short in shorts {
                    spec.aliases.insert(short.to_string(), canonical.clone());
                }
            }

            // `ArgAction` is the reliable signal. `Arg::get_num_args`
            // reads a field clap only populates while *building* a
            // command, so on a freshly derived `Command` it reports
            // `None` for everything and every arg looks valueless — which
            // silently turns off the value-skipping below and lets a
            // flag's value survive as a stray positional.
            let action = arg.get_action();
            if action.takes_values() {
                spec.takes_value.insert(canonical.clone());
            }
            if matches!(action, clap::ArgAction::Append) {
                spec.repeatable.insert(canonical);
            }
        }

        spec
    }

    /// The canonical flag a token names, plus whether that token already
    /// carries its value. `None` for anything that is not a known flag —
    /// positionals, `--`, a bare `-`, and unknown flags, which must not be
    /// allowed to override anything.
    fn token_flag(&self, token: &str) -> Option<(String, bool)> {
        if let Some(name) = long_flag_name(token) {
            return Some((self.aliases.get(&name)?.clone(), token.contains('=')));
        }

        let rest = token.strip_prefix('-')?;
        if rest.is_empty() || rest.starts_with('-') {
            return None;
        }

        // Short form. clap accepts `-o`, `-oVALUE` and `-o=VALUE`, and
        // bundles several boolean shorts as `-abc`. Walk the characters:
        // the first one that takes a value ends the scan, because
        // everything after it is that value rather than another flag.
        let mut last: Option<String> = None;
        for (idx, ch) in rest.char_indices() {
            let canonical = match self.aliases.get(&ch.to_string()) {
                Some(c) => c.clone(),
                // An unrecognised short means this is not a flag we know.
                // Stop rather than guess which prefix was meant.
                None => return None,
            };
            if self.takes_value.contains(&canonical) {
                return Some((canonical, idx + ch.len_utf8() < rest.len()));
            }
            last = Some(canonical);
        }
        last.map(|c| (c, false))
    }

    /// Canonical names of the flags the user actually typed.
    fn collect_cli_flags(&self, args: &[String]) -> std::collections::HashSet<String> {
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
            if let Some((canonical, attached)) = self.token_flag(arg) {
                names.insert(canonical.clone());
                // `--name value` consumes the next token. Skipping it is
                // what stops a value that begins with `-` from being read
                // as a flag of its own — `--jvm-arg -Xmx2g` declares
                // `--jvm-arg`, not `--jvm-arg` *and* nothing else.
                if !attached && self.takes_value.contains(&canonical) {
                    skip_next = true;
                }
            }
        }

        names
    }

    /// Drop file tokens for any flag the CLI also set.
    ///
    /// Repeatable flags survive: `--jvm-arg` and `--localization` are
    /// `Vec<T>` fields, so both sides' occurrences are meant to accumulate.
    /// Stripping either would silently discard the file's half of the JVM
    /// options.
    fn strip_overridden(
        &self,
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
            if let Some((canonical, attached)) = self.token_flag(token) {
                if cli_flags.contains(&canonical) && !self.repeatable.contains(&canonical) {
                    if !attached && self.takes_value.contains(&canonical) {
                        skip_next = true;
                    }
                    continue;
                }
            }
            out.push(token.clone());
        }

        out
    }
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
        let resolved = resolve(&raw, &dir, None).unwrap();
        assert_eq!(resolved, explicit);
    }

    #[test]
    fn resolve_falls_back_to_cwd_default() {
        let dir = tempdir();
        std::fs::write(dir.join(DEFAULT_OPTIONS_FILE), "--name Y\n").unwrap();
        let raw = args(&["snug", "app.jar"]);
        let resolved = resolve(&raw, &dir, None).unwrap();
        assert_eq!(resolved, dir.join(DEFAULT_OPTIONS_FILE));
    }

    #[test]
    fn resolve_returns_none_when_no_file_present() {
        let dir = tempdir();
        let raw = args(&["snug", "app.jar"]);
        assert_eq!(resolve(&raw, &dir, None), None);
    }

    #[test]
    fn resolve_prefers_exe_dir_over_cwd() {
        let exe_dir = tempdir();
        let cwd = tempdir();
        std::fs::write(exe_dir.join(DEFAULT_OPTIONS_FILE), "--name ExeDir\n").unwrap();
        std::fs::write(cwd.join(DEFAULT_OPTIONS_FILE), "--name Cwd\n").unwrap();
        let raw = args(&["snug", "app.jar"]);
        let resolved = resolve(&raw, &cwd, Some(&exe_dir)).unwrap();
        assert_eq!(resolved, exe_dir.join(DEFAULT_OPTIONS_FILE));
    }

    #[test]
    fn resolve_uses_cwd_when_exe_dir_has_no_default() {
        let exe_dir = tempdir();
        let cwd = tempdir();
        std::fs::write(cwd.join(DEFAULT_OPTIONS_FILE), "--name Cwd\n").unwrap();
        let raw = args(&["snug", "app.jar"]);
        let resolved = resolve(&raw, &cwd, Some(&exe_dir)).unwrap();
        assert_eq!(resolved, cwd.join(DEFAULT_OPTIONS_FILE));
    }

    #[test]
    fn resolve_explicit_flag_beats_exe_dir_default() {
        let exe_dir = tempdir();
        let cwd = tempdir();
        std::fs::write(exe_dir.join(DEFAULT_OPTIONS_FILE), "--name ExeDir\n").unwrap();
        let explicit = cwd.join("custom.opts");
        std::fs::write(&explicit, "--name Custom\n").unwrap();
        let raw = args(&["snug", "--options", explicit.to_str().unwrap()]);
        let resolved = resolve(&raw, &cwd, Some(&exe_dir)).unwrap();
        assert_eq!(resolved, explicit);
    }

    #[test]
    fn resolve_ignores_directory_named_like_the_default() {
        let exe_dir = tempdir();
        let cwd = tempdir();
        // A *directory* called snug.options next to the exe must not be
        // treated as a config file; the CWD one wins instead.
        std::fs::create_dir_all(exe_dir.join(DEFAULT_OPTIONS_FILE)).unwrap();
        std::fs::write(cwd.join(DEFAULT_OPTIONS_FILE), "--name Cwd\n").unwrap();
        let raw = args(&["snug", "app.jar"]);
        let resolved = resolve(&raw, &cwd, Some(&exe_dir)).unwrap();
        assert_eq!(resolved, cwd.join(DEFAULT_OPTIONS_FILE));
    }

    #[test]
    fn resolve_exe_dir_equal_to_cwd_yields_one_hit() {
        let dir = tempdir();
        std::fs::write(dir.join(DEFAULT_OPTIONS_FILE), "--name Same\n").unwrap();
        let raw = args(&["snug", "app.jar"]);
        assert_eq!(resolve(&raw, &dir, Some(&dir)), Some(dir.join(DEFAULT_OPTIONS_FILE)));
    }

    // ---- Short/long flag identity -------------------------------------
    //
    // The regression these guard: overriding was matched on the literal
    // flag *string*, so `-o` on the command line did not strip
    // `--output` from `snug.options` and clap aborted the whole build
    // with "the argument '--output <EXE>' cannot be used multiple
    // times". Both directions matter, because either side of the merge
    // can be written in either form.

    #[test]
    fn flag_spec_derives_short_and_long_aliases_from_the_cli_definition() {
        let spec = FlagSpec::from_cli();
        // The pair that actually caused the bug.
        assert_eq!(spec.aliases.get("o").map(String::as_str), Some("output"));
        assert_eq!(spec.aliases.get("output").map(String::as_str), Some("output"));
        // Repeatable flags come from ArgAction::Append, not a hand-written
        // list, so a new `Vec<T>` field is classified correctly on arrival.
        assert!(spec.repeatable.contains("jvm-arg"));
        assert!(spec.repeatable.contains("localization"));
        // `--output` takes a value, so `--output App.exe` must not be
        // mistaken for the positional JAR.
        assert!(spec.takes_value.contains("output"));
    }

    #[test]
    fn cli_short_form_overrides_file_long_form() {
        // The exact reported failure: `snug -o App.exe app.jar` against a
        // `snug.options` that already sets `--output`.
        let raw = args(&["snug", "app.jar", "-o", "CLI.exe"]);
        let merged = merge(
            &raw,
            vec!["--output".into(), "File.exe".into(), "--company".into(), "Co".into()],
        );
        assert_eq!(
            merged,
            vec![
                "snug".to_string(),
                "--company".to_string(),
                "Co".to_string(),
                "app.jar".to_string(),
                "-o".to_string(),
                "CLI.exe".to_string(),
            ]
        );
    }

    #[test]
    fn cli_long_form_overrides_file_short_form() {
        // The mirror image: the file may use the short form too.
        let raw = args(&["snug", "app.jar", "--output", "CLI.exe"]);
        let merged = merge(
            &raw,
            vec!["-o".into(), "File.exe".into(), "--company".into(), "Co".into()],
        );
        assert_eq!(
            merged,
            vec![
                "snug".to_string(),
                "--company".to_string(),
                "Co".to_string(),
                "app.jar".to_string(),
                "--output".to_string(),
                "CLI.exe".to_string(),
            ]
        );
    }

    #[test]
    fn short_form_with_attached_value_is_one_token() {
        // clap accepts `-oApp.exe`. It is a single token carrying a value,
        // so it must not leave a tail behind, and it must still override
        // the file's two-token form.
        let raw = args(&["snug", "app.jar", "-oCLI.exe"]);
        let merged = merge(&raw, vec!["--output".into(), "File.exe".into()]);
        assert_eq!(
            merged,
            vec![
                "snug".to_string(),
                "app.jar".to_string(),
                "-oCLI.exe".to_string(),
            ]
        );
    }

    #[test]
    fn stripped_file_flag_consumes_its_value_token() {
        // Guards the arity lookup. Without it, stripping `--name` from the
        // file left a bare `File` behind, which clap then read as a second
        // positional alongside app.jar.
        let raw = args(&["snug", "app.jar", "--name", "CLI"]);
        let merged = merge(
            &raw,
            vec![
                "--name".into(),
                "File".into(),
                "--min-java".into(),
                "25".into(),
            ],
        );
        assert_eq!(
            merged,
            vec![
                "snug".to_string(),
                "--min-java".to_string(),
                "25".to_string(),
                "app.jar".to_string(),
                "--name".to_string(),
                "CLI".to_string(),
            ]
        );
    }

    #[test]
    fn a_value_beginning_with_a_dash_is_not_mistaken_for_a_flag() {
        // `--jvm-arg` allows hyphen values, and snug's own generated
        // `snug.options` ships `--jvm-arg --enable-native-access=…`. The
        // value must be consumed as a value, and — because `jvm-arg` is
        // repeatable — both sides' occurrences survive.
        let raw = args(&["snug", "app.jar", "--jvm-arg", "-Xmx2g"]);
        let merged = merge(
            &raw,
            vec![
                "--jvm-arg".into(),
                "-Xms256m".into(),
                "--name".into(),
                "File".into(),
            ],
        );
        assert_eq!(
            merged,
            vec![
                "snug".to_string(),
                "--jvm-arg".to_string(),
                "-Xms256m".to_string(),
                "--name".to_string(),
                "File".to_string(),
                "app.jar".to_string(),
                "--jvm-arg".to_string(),
                "-Xmx2g".to_string(),
            ]
        );
    }

    fn tempdir() -> std::path::PathBuf {
        let base = std::env::temp_dir();
        // The counter is load-bearing, not belt-and-braces. The obvious
        // key — pid + `SystemTime::now().as_nanos()` — is NOT unique
        // enough: tests share a pid and macOS clock resolution is coarse
        // enough that two tests running in parallel can read the same
        // nanosecond. They then share a directory, one test's
        // `snug.options` shows up inside another test's deliberately
        // empty exe dir, and the failure looks like a `resolve()` bug
        // rather than a collision. Observed as an intermittent failure
        // of `resolve_uses_cwd_when_exe_dir_has_no_default`.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let unique = format!(
            "snug-options-{}-{}-{}",
            std::process::id(),
            n,
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
