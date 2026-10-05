//! Support for `snug.options` configuration files.
//!
//! The CLI accepts an optional `--options <path>` flag pointing at a
//! file containing one option per line. If no flag is given, `snug`
//! looks for **two** default files — a generic one and an
//! operating-system-specific one:
//!
//! ```text
//! snug.options              # every platform
//! snug.<os>.options         # this host only: snug.macos.options,
//!                           # snug.windows.options, snug.linux.options
//! ```
//!
//! Precedence, highest first:
//!
//! 1. Command-line flags.
//! 2. `snug.<os>.options`.
//! 3. `snug.options`.
//!
//! So an OS file is a *partial override* of the generic one: it carries
//! only the values that differ on that platform, and everything else
//! still falls through to `snug.options`. That is the whole point — the
//! motivating case is a `--output` whose extension has to differ per
//! platform (`.exe` vs `.app`), where duplicating the whole file to
//! change one line is how the two copies drift apart.
//!
//! An explicit `--options <path>` means **that file only**. The
//! operating-system tier is not consulted in addition, so pointing at a
//! file is also the escape hatch for a build that must not pick up
//! ambient per-machine configuration.
//!
//! ## Lookup order within a tier
//!
//! For each of the two names, the snug executable's own directory is
//! searched first, then the current working directory — so a portable
//! `snug.exe` shipped alongside its `snug.options` carries its defaults
//! wherever it is invoked from. This is a *per-tier* search: an exe-dir
//! `snug.options` and a CWD `snug.<os>.options` both load, and the OS
//! file still wins on conflicting flags.
//!
//! A missing file is not an error, and not a warning. `snug.<os>.options`
//! is an opt-in, so looking for one and finding nothing must be silent
//! or every single build on a machine without one would print noise.
//!
//! ## File format
//!
//! Files are parsed line by line, with each line split the way a command
//! line would be read: whitespace separates tokens, and `'...'` or
//! `"..."` groups one containing whitespace. Lines starting with `#`
//! (after trimming) are comments; blank lines are skipped.
//!
//! Quoting is the *only* grouping mechanism. There are no backslash
//! escapes, because `\` is a path separator on Windows and treating it
//! as an escape silently mangles every absolute path — see [`tokenise`]
//! for the full reasoning. For the same reason `#` is a comment marker
//! only at the start of a line; elsewhere it is an ordinary character,
//! since `#` is legal in a filename.
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
//!
//! and its `snug.macos.options` companion, overriding only what differs:
//!
//! ```text
//! # The output is the one genuinely platform-specific value here:
//! # a `.app` bundle rather than a Windows PE.
//! --output build/MyApp.app
//! ```

use std::path::{Path, PathBuf};

use thiserror::Error;

/// Generic options-file name, consulted on every platform.
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

/// The OS-specific options-file name for an OS token: `snug.macos.options`,
/// `snug.windows.options`, `snug.linux.options`.
///
/// The `<os>` token is Rust's own [`std::env::consts::OS`] spelling, not
/// a Windows marketing name — `macos`, not `mac`, and `windows`, not
/// `win`. It is the same vocabulary the rest of the project already uses
/// (`#[cfg(target_os = "macos")]`, `release/macos-arm64/`,
/// `scripts/build-macos.sh`), so the filename and the crate's platform
/// gating are always the same string. The token is a *parameter* rather
/// than read from the environment here so the precedence rules are
/// testable for every platform from one host.
pub fn os_options_file_name(os: &str) -> String {
    format!("snug.{os}.options")
}

/// The OS-specific options-file name for the host snug is running on.
pub fn host_os_options_file_name() -> String {
    os_options_file_name(std::env::consts::OS)
}

/// Resolve the options files to load, **lowest priority first**.
///
/// The returned order is the layering order: `snug.options` before
/// `snug.<os>.options`, so a caller can merge by walking front to back
/// and letting each layer win over the ones before it. Empty means no
/// defaults were supplied anywhere, which is not an error.
///
/// Precedence across sources, highest first:
///
/// 1. Command-line flags (handled by the caller, which owns argv).
/// 2. `snug.<os>.options`.
/// 3. `snug.options`.
///
/// An explicit `--options <path>` returns exactly that one file and
/// **nothing else**. The OS tier is deliberately not layered on top of
/// it: an explicit path is how a caller says "use these options", and
/// silently mixing in an ambient `snug.macos.options` would make a
/// build's configuration depend on which machine ran it. The path is
/// not validated as existing — the caller is expected to fail loudly,
/// which is existing behaviour and the integration test for it.
///
/// Within each of the two default names, the snug executable's own
/// directory is searched before the current working directory. That
/// search is per-tier: an exe-dir `snug.options` and a CWD
/// `snug.<os>.options` both load, and the OS file still wins on
/// conflicting flags. Taking the first hit per name also means a
/// directory that happens to share the file's name is skipped
/// (`is_file`, not `exists`) and that `exe_dir == cwd` cannot yield the
/// same file twice.
pub fn resolve_all(
    raw_args: &[String],
    cwd: &Path,
    exe_dir: Option<&Path>,
    os: &str,
) -> Vec<PathBuf> {
    if let Some(p) = find_options_flag(raw_args) {
        return vec![p];
    }

    let os_file_name = os_options_file_name(os);
    [DEFAULT_OPTIONS_FILE, os_file_name.as_str()]
        .iter()
        .filter_map(|name| {
            search_dirs(exe_dir, cwd)
                .map(|dir| dir.join(name))
                .find(|p| p.is_file())
        })
        .collect()
}

/// The directories searched for a default options file, highest location
/// priority first: the snug executable's own directory, then the current
/// working directory.
fn search_dirs<'a>(exe_dir: Option<&'a Path>, cwd: &'a Path) -> impl Iterator<Item = &'a Path> {
    exe_dir.into_iter().chain(std::iter::once(cwd))
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
/// skipped. Each remaining line is split by [`tokenise`], which honours
/// double-quoted and single-quoted strings.
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
        match tokenise(trimmed) {
            Ok(line_tokens) => tokens.extend(line_tokens),
            Err(message) => {
                return Err(OptionsFileError::Syntax {
                    path: path.to_path_buf(),
                    line: idx + 1,
                    message: message.to_string(),
                });
            }
        }
    }
    Ok(tokens)
}

/// Split one options-file line into tokens.
///
/// This is deliberately **not** a POSIX-shell tokeniser, even though the
/// file reads like one. It implements what `snug.options` actually
/// documents, which is the narrower contract:
///
///   - whitespace separates tokens;
///   - `'...'` and `"..."` group, so a value containing whitespace can
///     be written `--name "Snug JavaFX Demo"`;
///   - `\` is an ordinary character, never an escape;
///   - `#` is handled by [`load`] as a whole-line marker, so here it is
///     just another character.
///
/// The third and fourth points are the whole reason this exists. A shell
/// tokeniser reads `C:\Users\me\app.jar` as `C:Usersmeapp.jar`, because
/// in POSIX a backslash escapes the character after it and both
/// characters vanish. That silently corrupts *every absolute Windows
/// path*, which is the single most common thing an options file names.
/// The same tokeniser also treats `#` as a comment wherever a token
/// starts, so `--input C:\release#2\app.jar` truncates at the hash —
/// and `#` is a legal character in a Windows filename.
///
/// Neither of those behaviours was ever promised. `snug.options` says
/// "`#`-prefixed lines are comments" and "you __MUST__ quote any value
/// with whitespace" — full-line comments and quoting, nothing about
/// escapes. Following the documented contract fixes the Windows paths
/// and stops two undocumented POSIX-isms leaking into a config file.
///
/// The cost is that there is no escape mechanism at all, so a value
/// containing a literal `'` or `"` cannot be written. That limitation
/// is unchanged from the previous behaviour, and both characters are
/// illegal in Windows filenames anyway.
///
/// Errors only on an unterminated quote. Silently swallowing the rest of
/// the line would turn a typo into a confusing "unknown flag" from clap,
/// several tokens later; naming the line is the useful error.
fn tokenise(line: &str) -> Result<Vec<String>, &'static str> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    // `started` rather than `!current.is_empty()`, because `""` is a real
    // token and must not be swallowed by the whitespace branch.
    let mut started = false;
    let mut chars = line.chars();

    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if started {
                    tokens.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            '\'' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => current.push(c),
                        None => return Err("unterminated '...' - the quote is never closed"),
                    }
                }
            }
            '"' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some(c) => current.push(c),
                        None => return Err("unterminated \"...\" - the quote is never closed"),
                    }
                }
            }
            // Everything else, `\` included, is literal. There is no
            // escape branch on purpose: see the note above.
            c => {
                started = true;
                current.push(c);
            }
        }
    }

    if started {
        tokens.push(current);
    }
    Ok(tokens)
}

/// The flag a positional JAR supersedes. `--input <JAR|DIR>` and the
/// positional `[JAR|DIR]` are two spellings of the same slot, so the
/// merge has to be able to reason about them as one thing.
const INPUT_FLAG: &str = "input";

/// Build the merged argv that clap should parse.
///
/// The merged layout is
/// `[program_name, ...lowest_layer, ...next_layer, ..., ...cli_args]`.
///
/// `file_layers` must be ordered **lowest priority first**, which is
/// what [`resolve_all`] returns: `snug.options` then
/// `snug.<os>.options`. With the layers in that order the output is also
/// in that order, so each non-repeatable flag appears exactly once,
/// carrying the highest-priority source's value, and repeatable flags
/// (`--jvm-arg`, `--localization`) accumulate base-first.
///
/// ## Why the walk is high-to-low
///
/// The layering cannot be done by simply concatenating the files'
/// tokens: two files both setting `--name` would leave two `--name`
/// occurrences in the argv and clap aborts the whole build with
///
/// ```text
/// error: the argument '--name <NAME>' cannot be used multiple times
/// ```
///
/// — the same failure recorded for the file-vs-CLI case in
/// [`FlagSpec`]. So each layer has to be stripped against everything
/// *above* it, which means walking from highest priority down while
/// accumulating the set of flags already supplied by a higher source,
/// seeded with the command line's own flags. A layer's surviving
/// tokens then join that set, so the next (lower) layer loses to it.
///
/// Building the set once per call and reusing it across layers also
/// keeps the cost at one clap `Command` build per invocation rather
/// than one per layer.
///
/// The `--options <path>` flag and its value are stripped from the CLI
/// portion so clap doesn't see them twice (the file it named has
/// already been loaded and merged).
///
/// ## The positional JAR and `--input`
///
/// Those two are declared `conflicts_with` each other, which is a
/// statement about one command line: `snug app.jar --input other.jar`
/// is a genuine mistake and clap still rejects it. Across sources it is
/// not a conflict but a precedence question, and it resolves the same way
/// every other option does — the command line wins. A file's `--input`
/// is therefore dropped when the command line carries a positional JAR,
/// which is what lets an options file carry a default input for `snug`
/// on its own while `snug some-other.jar` still builds that one instead.
pub fn merge(raw_args: &[String], file_layers: Vec<Vec<String>>) -> Vec<String> {
    let spec = FlagSpec::from_cli();
    let cli_args = strip_options_flag(&raw_args[1..]);

    // Flags already spoken for by a higher-priority source, starting
    // with the command line.
    let mut seen = spec.collect_cli_flags(&raw_args[1..]);

    // A positional JAR supersedes an `--input` from a file. They are two
    // spellings of one slot — "mutually exclusive" is a statement about a
    // single command line, not about the command line versus a config
    // file — so "the command line always wins" has to cover this pair too.
    // Seeding `seen` with the flag's name reuses the existing strip path,
    // which drops the file's `--input` *and* its value exactly as it
    // would any other overridden flag. A command line carrying both is
    // left alone: clap reports that conflict, which is right.
    if spec.cli_has_positional(&raw_args[1..]) {
        seen.insert(INPUT_FLAG.to_string());
    }

    let mut layers: Vec<Vec<String>> = vec![Vec::new(); file_layers.len()];

    for (idx, tokens) in file_layers.iter().enumerate().rev() {
        let surviving = spec.strip_overridden(tokens, &seen);
        seen.extend(spec.collect_cli_flags(&surviving));
        // Same precedence one tier down: a JAR in a higher-priority file
        // beats an `--input` in a lower one. (The reverse — a higher
        // `--input` against a lower file's bare JAR — still conflicts,
        // because stripping a positional is not something the flag-driven
        // walk does. No options file in the wild carries one; it would be
        // a strange thing to write, since a file's whole purpose is to
        // be a set of flags.)
        if spec.cli_has_positional(&surviving) {
            seen.insert(INPUT_FLAG.to_string());
        }
        layers[idx] = surviving;
    }

    let total: usize = cli_args.len() + layers.iter().map(Vec::len).sum::<usize>();
    let mut out = Vec::with_capacity(total + 1);
    out.push(
        raw_args
            .first()
            .cloned()
            .unwrap_or_else(|| "snug".to_string()),
    );
    for layer in &layers {
        out.extend(layer.iter().cloned());
    }
    out.extend(cli_args);
    out
}

/// Drop the `--options <path>` flag and its value from the CLI portion
/// of argv. The file it named has already been loaded and merged, so
/// leaving the flag in place would have clap see it a second time.
fn strip_options_flag(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut skip_next = false;
    for arg in args {
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

    /// Whether this token list carries a positional argument — a bare
    /// token that is neither a flag nor some flag's value.
    ///
    /// The walk mirrors [`Self::collect_cli_flags`] exactly, so the two
    /// never disagree about what a token is: same `--` handling, same
    /// `--options` skip, same value-skipping. The one deliberate
    /// difference is the unknown-flag case: a token starting with `-` is
    /// never counted as a positional, even one [`Self::token_flag`]
    /// doesn't recognise. Such a command line is going to fail parsing
    /// regardless, and reporting "no input" for it would be a worse
    /// message than the parse error that is already coming.
    fn cli_has_positional(&self, args: &[String]) -> bool {
        let mut skip_next = false;

        for (idx, arg) in args.iter().enumerate() {
            if skip_next {
                skip_next = false;
                continue;
            }
            if arg == "--" {
                // Everything after the separator is positional, so the
                // one thing that matters is whether anything follows it.
                return args.len() > idx + 1;
            }
            if arg == "--options" {
                skip_next = true;
                continue;
            }
            if arg.starts_with("--options=") {
                continue;
            }
            if let Some((canonical, attached)) = self.token_flag(arg) {
                if !attached && self.takes_value.contains(&canonical) {
                    skip_next = true;
                }
                continue;
            }
            if !arg.starts_with('-') {
                return true;
            }
        }

        false
    }

    /// Drop file tokens for any flag a higher-priority source also set.
    ///
    /// "Higher-priority" is the command line on the first layer, and an
    /// OS-specific options file on the generic one — the same question
    /// either way, so the caller passes whichever set of canonical names
    /// it has accumulated so far. See [`merge`].
    ///
    /// Repeatable flags survive: `--jvm-arg` and `--localization` are
    /// `Vec<T>` fields, so occurrences from every layer are meant to
    /// accumulate. Stripping any of them would silently discard part of
    /// the JVM options.
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

    /// A single options-file layer. Most merge tests below exercise the
    /// file-vs-CLI relationship with just one file, so this keeps them
    /// readable rather than adding a nesting level to every call.
    fn layer(tokens: Vec<String>) -> Vec<Vec<String>> {
        vec![tokens]
    }

    // --- tokenise: the properties the file format actually promises -----
    //
    // These are the regression tests for the reason this function is
    // hand-written instead of being a POSIX shell tokenizer. Each one
    // documents a class of input that used to be silently corrupted.

    #[test]
    fn absolute_windows_path_keeps_its_backslashes() {
        // The bug this whole change exists for. A shell tokenizer reads
        // `C:\Users` as `C:Users` and drops both the backslash and the
        // character it escaped.
        assert_eq!(
            tokenise(r"--input C:\Users\me\app.jar").unwrap(),
            args(&[r"--input", r"C:\Users\me\app.jar"]),
        );
    }

    #[test]
    fn backslash_is_literal_in_every_position() {
        // Leading, trailing, doubled, and inside quotes. None of these
        // are escapes here, and a trailing backslash in particular must
        // not swallow the character after it.
        assert_eq!(
            tokenise(r#"\leading --x a\  b\\"#).unwrap(),
            args(&[r"\leading", "--x", r"a\", r"b\\"]),
        );
        assert_eq!(tokenise(r#"--icon "C:\a b\icon.png""#).unwrap(), args(&[r#"--icon"#, r"C:\a b\icon.png"]));
    }

    #[test]
    fn forward_slash_paths_are_unaffected() {
        assert_eq!(
            tokenise("--input assets/app.jar").unwrap(),
            args(&["--input", "assets/app.jar"]),
        );
    }

    #[test]
    fn quoted_value_keeps_its_whitespace() {
        // The documented reason quoting exists, so it must not regress.
        assert_eq!(
            tokenise(r#"--name "Snug JavaFX Demo""#).unwrap(),
            args(&["--name", "Snug JavaFX Demo"]),
        );
        assert_eq!(
            tokenise("--copyright 'SynapticLoop Pty Ltd'").unwrap(),
            args(&["--copyright", "SynapticLoop Pty Ltd"]),
        );
    }

    #[test]
    fn quotes_concatenate_with_adjacent_text() {
        // `--name="My App"` is how a user would naturally write it, and
        // a leading `--flag=value` has to survive intact.
        assert_eq!(
            tokenise(r#"--name="My App""#).unwrap(),
            args(&[r#"--name=My App"#]),
        );
        assert_eq!(tokenise(r#"--copyright ©"#).unwrap(), args(&["--copyright", "©"]));
    }

    #[test]
    fn hash_is_literal_inside_a_line() {
        // `#` is only a comment marker at the start of a line, because it
        // is a legal character in a Windows filename. A shell tokenizer
        // truncates this at the hash.
        assert_eq!(
            tokenise(r"--input C:\release#2\app.jar").unwrap(),
            args(&[r"--input", r"C:\release#2\app.jar"]),
        );
    }

    #[test]
    fn empty_quotes_produce_an_empty_token() {
        // `--name ""` is a deliberate empty value, not a missing one, so
        // the token has to be emitted rather than dropped.
        assert_eq!(tokenise(r#"--name "" tail"#).unwrap(), args(&["--name", "", "tail"]));
    }

    #[test]
    fn tabs_separate_tokens_too() {
        assert_eq!(tokenise("--a\t--b  --c").unwrap(), args(&["--a", "--b", "--c"]));
    }

    #[test]
    fn unterminated_quote_is_an_error_not_a_silent_truncation() {
        // Better to name the line than to hand clap a half-read value.
        assert!(tokenise(r#"--name "My App"#).is_err());
        assert!(tokenise("--name 'My App").is_err());
    }

    #[test]
    fn an_empty_line_yields_no_tokens() {
        assert!(tokenise("").unwrap().is_empty());
        assert!(tokenise("   \t ").unwrap().is_empty());
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
        let merged = merge(&raw, layer(vec!["--name".into(), "File".into()]));
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
        let merged = merge(
            &raw,
            layer(vec![
                "--name".into(),
                "File".into(),
                "--company".into(),
                "Co".into(),
            ]),
        );
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
        let merged = merge(
            &raw,
            layer(vec!["--name=File".into(), "--company=Co".into()]),
        );
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
        let merged = merge(&raw, layer(vec!["--jvm-arg=-Xms256m".into()]));
        assert!(merged.contains(&"--jvm-arg=-Xms256m".to_string()));
        assert!(merged.contains(&"--jvm-arg=-Xmx2g".to_string()));
    }

    #[test]
    fn merge_strips_options_equals_form() {
        let raw = args(&["snug", "--options=x.opts", "app.jar"]);
        let merged = merge(&raw, layer(vec![]));
        assert_eq!(merged, vec!["snug".to_string(), "app.jar".to_string()]);
    }

    // ---- Two or more layers -------------------------------------------
    //
    // `file_layers` is ordered lowest priority first, which is what
    // `resolve_all` produces: `snug.options` then `snug.<os>.options`.

    #[test]
    fn os_layer_overrides_generic_layer_and_appears_once() {
        // The regression that shaped the high-to-low walk: concatenating
        // two files that both set `--name` leaves two occurrences and
        // clap aborts with "the argument '--name <NAME>' cannot be used
        // multiple times". So the *count* is the assertion here, not just
        // the winning value.
        let raw = args(&["snug", "app.jar"]);
        let merged = merge(
            &raw,
            vec![
                vec!["--name".into(), "Generic".into()],
                vec!["--name".into(), "OS".into()],
            ],
        );
        let name_count = merged.iter().filter(|t| t.starts_with("--name")).count();
        assert_eq!(name_count, 1, "exactly one --name must reach clap: {merged:?}");
        assert!(
            merged.windows(2).any(|w| w == ["--name".to_string(), "OS".to_string()]),
            "the OS layer's value must win: {merged:?}"
        );
        assert!(!merged.contains(&"Generic".to_string()));
    }

    #[test]
    fn cli_overrides_the_os_layer_which_overrides_the_generic_one() {
        // All three tiers on the same flag, one occurrence, topmost wins.
        let raw = args(&["snug", "app.jar", "--name", "CLI"]);
        let merged = merge(
            &raw,
            vec![
                vec!["--name".into(), "Generic".into()],
                vec!["--name".into(), "OS".into()],
            ],
        );
        let name_count = merged.iter().filter(|t| t.starts_with("--name")).count();
        assert_eq!(name_count, 1, "{merged:?}");
        assert!(merged.windows(2).any(|w| w == ["--name".to_string(), "CLI".to_string()]));
        assert!(!merged.iter().any(|t| t == "Generic" || t == "OS"));
    }

    #[test]
    fn os_layer_keeps_flags_the_generic_layer_did_not_set() {
        // The whole point of the feature: a partial override. The
        // OS file carries one differing value and inherits the rest.
        let raw = args(&["snug", "app.jar"]);
        let merged = merge(
            &raw,
            vec![
                vec!["--name".into(), "Generic".into(), "--company".into(), "Co".into()],
                vec!["--output".into(), "MyApp.app".into()],
            ],
        );
        assert!(
            merged.windows(2).any(|w| w == ["--name".to_string(), "Generic".to_string()]),
            "inherited from the generic layer: {merged:?}"
        );
        assert!(
            merged.windows(2).any(|w| w == ["--company".to_string(), "Co".to_string()]),
            "inherited from the generic layer: {merged:?}"
        );
        assert!(
            merged.windows(2).any(|w| w == ["--output".to_string(), "MyApp.app".to_string()]),
            "supplied by the OS layer: {merged:?}"
        );
    }

    #[test]
    fn layers_reach_argv_low_to_high() {
        // Order is the contract `merge`'s doc states. A flag set by both
        // layers must appear with the higher-priority value, in the
        // lower layer's position — which is what makes the base file's
        // values act as defaults rather than appearing *after* the
        // override and winning by clap's last-wins.
        let raw = args(&["snug", "app.jar"]);
        let merged = merge(
            &raw,
            vec![
                vec!["--company".into(), "GenericCo".into()],
                vec!["--company".into(), "OSCo".into()],
            ],
        );
        assert_eq!(
            merged,
            vec![
                "snug".to_string(),
                "--company".to_string(),
                "OSCo".to_string(),
                "app.jar".to_string(),
            ]
        );
    }

    #[test]
    fn repeatable_flags_accumulate_across_every_tier() {
        // `--jvm-arg` is an `ArgAction::Append` field, so all three
        // sources' values are wanted — base first, then the OS layer,
        // then the command line. This is also the only multi-tier case
        // where more than one occurrence is correct.
        let raw = args(&["snug", "app.jar", "--jvm-arg=-Xmx2g"]);
        let merged = merge(
            &raw,
            vec![
                vec!["--jvm-arg".into(), "-Xms256m".into()],
                vec!["--jvm-arg".into(), "-Dapple.awt.enable-2d=false".into()],
            ],
        );
        // Note the mixed forms: the file layers use the two-token
        // spelling and the command line the `--flag=value` one. Both
        // reach clap, which is what `ArgAction::Append` collects.
        assert_eq!(
            merged,
            vec![
                "snug".to_string(),
                "--jvm-arg".to_string(),
                "-Xms256m".to_string(),
                "--jvm-arg".to_string(),
                "-Dapple.awt.enable-2d=false".to_string(),
                "app.jar".to_string(),
                "--jvm-arg=-Xmx2g".to_string(),
            ]
        );
    }

    #[test]
    fn a_value_beginning_with_a_dash_survives_layer_stripping() {
        // A higher layer overriding a lower one must consume the
        // overridden flag's *value* token too, or the stripped value is
        // left behind and clap reads it as a second positional. Here the
        // OS layer overrides `--jvm-arg`'s sibling flag and the arity
        // lookup has to keep the two apart.
        let raw = args(&["snug", "app.jar"]);
        let merged = merge(
            &raw,
            vec![
                vec!["--min-java".into(), "21".into()],
                vec!["--min-java".into(), "25".into()],
            ],
        );
        assert_eq!(
            merged,
            vec![
                "snug".to_string(),
                "--min-java".to_string(),
                "25".to_string(),
                "app.jar".to_string(),
            ]
        );
    }

    #[test]
    fn resolve_prefers_explicit_options_flag() {
        let dir = tempdir();
        let explicit = dir.join("custom.opts");
        std::fs::write(&explicit, "--name X\n").unwrap();
        // Default file also exists, but the explicit flag wins.
        std::fs::write(dir.join(DEFAULT_OPTIONS_FILE), "--name Y\n").unwrap();
        let raw = args(&["snug", "--options", explicit.to_str().unwrap()]);
        assert_eq!(resolve_all(&raw, &dir, None, "macos"), vec![explicit]);
    }

    #[test]
    fn resolve_falls_back_to_cwd_default() {
        let dir = tempdir();
        std::fs::write(dir.join(DEFAULT_OPTIONS_FILE), "--name Y\n").unwrap();
        let raw = args(&["snug", "app.jar"]);
        assert_eq!(
            resolve_all(&raw, &dir, None, "macos"),
            vec![dir.join(DEFAULT_OPTIONS_FILE)]
        );
    }

    #[test]
    fn resolve_returns_nothing_when_no_file_present() {
        let dir = tempdir();
        let raw = args(&["snug", "app.jar"]);
        assert!(resolve_all(&raw, &dir, None, "macos").is_empty());
    }

    #[test]
    fn resolve_prefers_exe_dir_over_cwd() {
        let exe_dir = tempdir();
        let cwd = tempdir();
        std::fs::write(exe_dir.join(DEFAULT_OPTIONS_FILE), "--name ExeDir\n").unwrap();
        std::fs::write(cwd.join(DEFAULT_OPTIONS_FILE), "--name Cwd\n").unwrap();
        let raw = args(&["snug", "app.jar"]);
        assert_eq!(
            resolve_all(&raw, &cwd, Some(&exe_dir), "macos"),
            vec![exe_dir.join(DEFAULT_OPTIONS_FILE)]
        );
    }

    #[test]
    fn resolve_uses_cwd_when_exe_dir_has_no_default() {
        let exe_dir = tempdir();
        let cwd = tempdir();
        std::fs::write(cwd.join(DEFAULT_OPTIONS_FILE), "--name Cwd\n").unwrap();
        let raw = args(&["snug", "app.jar"]);
        assert_eq!(
            resolve_all(&raw, &cwd, Some(&exe_dir), "macos"),
            vec![cwd.join(DEFAULT_OPTIONS_FILE)]
        );
    }

    #[test]
    fn resolve_explicit_flag_beats_every_default() {
        let exe_dir = tempdir();
        let cwd = tempdir();
        std::fs::write(exe_dir.join(DEFAULT_OPTIONS_FILE), "--name ExeDir\n").unwrap();
        std::fs::write(exe_dir.join("snug.macos.options"), "--name ExeDirOS\n").unwrap();
        let explicit = cwd.join("custom.opts");
        std::fs::write(&explicit, "--name Custom\n").unwrap();
        let raw = args(&["snug", "--options", explicit.to_str().unwrap()]);
        assert_eq!(
            resolve_all(&raw, &cwd, Some(&exe_dir), "macos"),
            vec![explicit]
        );
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
        assert_eq!(
            resolve_all(&raw, &cwd, Some(&exe_dir), "macos"),
            vec![cwd.join(DEFAULT_OPTIONS_FILE)]
        );
    }

    #[test]
    fn resolve_exe_dir_equal_to_cwd_yields_one_hit() {
        let dir = tempdir();
        std::fs::write(dir.join(DEFAULT_OPTIONS_FILE), "--name Same\n").unwrap();
        let raw = args(&["snug", "app.jar"]);
        assert_eq!(
            resolve_all(&raw, &dir, Some(&dir), "macos"),
            vec![dir.join(DEFAULT_OPTIONS_FILE)]
        );
    }

    // ---- The OS-specific tier ----------------------------------------
    //
    // The `os` token is a parameter rather than the host's, so every
    // platform's rules are testable from one host. `macos` stands in for
    // whichever token the test is about.

    #[test]
    fn os_options_file_name_uses_rust_os_spelling() {
        // Deliberately not `mac` / `win`: the project already spells
        // these `macos` / `windows` in cfg gates, script names and the
        // release layout, and the filename has to match.
        assert_eq!(os_options_file_name("macos"), "snug.macos.options");
        assert_eq!(os_options_file_name("windows"), "snug.windows.options");
        assert_eq!(os_options_file_name("linux"), "snug.linux.options");
        assert_eq!(host_os_options_file_name(), os_options_file_name(std::env::consts::OS));
    }

    #[test]
    fn missing_os_file_is_silent_and_yields_only_the_generic_one() {
        // The important shape: an absent OS file must not appear as an
        // error, and must not suppress the generic file either.
        let dir = tempdir();
        std::fs::write(dir.join(DEFAULT_OPTIONS_FILE), "--name Generic\n").unwrap();
        let raw = args(&["snug", "app.jar"]);
        assert_eq!(
            resolve_all(&raw, &dir, None, "macos"),
            vec![dir.join(DEFAULT_OPTIONS_FILE)]
        );
    }

    #[test]
    fn another_platforms_os_file_is_ignored() {
        // A checked-in `snug.windows.options` must not leak into a macOS
        // build just because it sits in the same directory.
        let dir = tempdir();
        std::fs::write(dir.join(DEFAULT_OPTIONS_FILE), "--name Generic\n").unwrap();
        std::fs::write(dir.join("snug.windows.options"), "--name Windows\n").unwrap();
        let raw = args(&["snug", "app.jar"]);
        assert_eq!(
            resolve_all(&raw, &dir, None, "macos"),
            vec![dir.join(DEFAULT_OPTIONS_FILE)]
        );
    }

    #[test]
    fn both_files_resolve_lowest_priority_first() {
        // The order is the layering order `merge` depends on, so assert
        // it explicitly rather than as a set.
        let dir = tempdir();
        let generic = dir.join(DEFAULT_OPTIONS_FILE);
        let os_file = dir.join("snug.macos.options");
        std::fs::write(&generic, "--name Generic\n").unwrap();
        std::fs::write(&os_file, "--name OS\n").unwrap();
        let raw = args(&["snug", "app.jar"]);
        assert_eq!(resolve_all(&raw, &dir, None, "macos"), vec![generic, os_file]);
    }

    #[test]
    fn os_file_in_cwd_layers_over_generic_file_in_exe_dir() {
        // Location is a *per-tier* search: the exe dir still wins for
        // the generic name, but the OS tier is found on its own terms.
        let exe_dir = tempdir();
        let cwd = tempdir();
        let generic = exe_dir.join(DEFAULT_OPTIONS_FILE);
        let os_file = cwd.join("snug.macos.options");
        std::fs::write(&generic, "--name ExeDir\n").unwrap();
        std::fs::write(&os_file, "--name CwdOS\n").unwrap();
        let raw = args(&["snug", "app.jar"]);
        assert_eq!(resolve_all(&raw, &cwd, Some(&exe_dir), "macos"), vec![generic, os_file]);
    }

    #[test]
    fn os_file_follows_the_same_exe_dir_over_cwd_rule() {
        let exe_dir = tempdir();
        let cwd = tempdir();
        std::fs::write(exe_dir.join("snug.macos.options"), "--name ExeDirOS\n").unwrap();
        std::fs::write(cwd.join("snug.macos.options"), "--name CwdOS\n").unwrap();
        let raw = args(&["snug", "app.jar"]);
        assert_eq!(
            resolve_all(&raw, &cwd, Some(&exe_dir), "macos"),
            vec![exe_dir.join("snug.macos.options")]
        );
    }

    #[test]
    fn resolve_ignores_directory_named_like_the_os_file() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.join("snug.macos.options")).unwrap();
        let raw = args(&["snug", "app.jar"]);
        assert!(resolve_all(&raw, &dir, None, "macos").is_empty());
    }

    #[test]
    fn explicit_options_flag_ignores_the_os_tier_entirely() {
        // The escape hatch: naming a file means *that file*, so an
        // ambient per-machine OS file cannot change a build's result.
        let dir = tempdir();
        std::fs::write(dir.join("snug.macos.options"), "--name OS\n").unwrap();
        let custom = dir.join("custom.opts");
        std::fs::write(&custom, "--name Custom\n").unwrap();
        let raw = args(&["snug", "--options", custom.to_str().unwrap()]);
        assert_eq!(resolve_all(&raw, &dir, None, "macos"), vec![custom]);
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
            layer(vec!["--output".into(), "File.exe".into(), "--company".into(), "Co".into()]),
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
            layer(vec!["-o".into(), "File.exe".into(), "--company".into(), "Co".into()]),
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
    fn cli_positional_jar_overrides_a_files_input() {
        // The reason `seen` is seeded with `input`. `--input` and the
        // positional are two spellings of one slot, and the command line
        // has to beat the file for it the same way it beats every other
        // flag. Left in, the file's `--input` would collide with the
        // positional and clap would refuse the whole build.
        let raw = args(&["snug", "other.jar", "--output", "App.exe"]);
        let merged = merge(
            &raw,
            layer(vec!["--input".into(), "default.jar".into(), "--company".into(), "Co".into()]),
        );
        assert_eq!(
            merged,
            vec![
                "snug".to_string(),
                "--company".to_string(),
                "Co".to_string(),
                "other.jar".to_string(),
                "--output".to_string(),
                "App.exe".to_string(),
            ]
        );
    }

    #[test]
    fn cli_positional_jar_overrides_a_files_attached_input() {
        // Same rule, `--input=path` spelling: the flag and its value are
        // one token, so there is no value token to leak if the strip is
        // done by name alone.
        let raw = args(&["snug", "other.jar"]);
        let merged = merge(
            &raw,
            layer(vec!["--input=default.jar".into(), "--name".into(), "File".into()]),
        );
        assert_eq!(
            merged,
            vec![
                "snug".to_string(),
                "--name".to_string(),
                "File".to_string(),
                "other.jar".to_string(),
            ]
        );
    }

    #[test]
    fn a_files_input_survives_when_the_command_line_has_no_jar() {
        // The other direction, and the reason the feature is worth having:
        // with no positional on the command line, the file's `--input`
        // stands, so a bare `snug` still has something to build.
        let raw = args(&["snug", "--output", "App.exe"]);
        let merged = merge(&raw, layer(vec!["--input".into(), "default.jar".into()]));
        assert_eq!(
            merged,
            vec![
                "snug".to_string(),
                "--input".to_string(),
                "default.jar".to_string(),
                "--output".to_string(),
                "App.exe".to_string(),
            ]
        );
    }

    #[test]
    fn a_cli_value_is_never_mistaken_for_a_positional() {
        // The walk has to skip a flag's value, or `--input File.jar` on
        // the command line would look like a bare positional and strip
        // the file's own `--input` — replacing one argument with an
        // identical one and, worse, doing it for a line whose real input
        // came from the command line.
        let raw = args(&["snug", "--input", "cli.jar"]);
        let merged = merge(&raw, layer(vec!["--input".into(), "file.jar".into()]));
        assert_eq!(merged.last().map(String::as_str), Some("cli.jar"));
        assert_eq!(
            merged.iter().filter(|t| t.as_str() == "file.jar").count(),
            0,
            "the file's --input should be gone: {merged:?}"
        );
    }

    #[test]
    fn a_positional_after_a_double_dash_still_counts() {
        // `--` ends flag parsing, so a JAR after it is a JAR.
        let raw = args(&["snug", "--", "app.jar"]);
        let merged = merge(&raw, layer(vec!["--input".into(), "default.jar".into()]));
        assert_eq!(
            merged,
            vec!["snug".to_string(), "--".to_string(), "app.jar".to_string()]
        );
    }

    #[test]
    fn a_bare_double_dash_is_not_a_positional() {
        // ...but a trailing `--` names nothing, so the file's `--input`
        // must survive for clap to report the real problem: a missing
        // JAR rather than a spurious conflict. (File layers precede the
        // command line in the merged argv, so the `--` lands last.)
        let raw = args(&["snug", "--"]);
        let merged = merge(&raw, layer(vec!["--input".into(), "default.jar".into()]));
        assert_eq!(
            merged,
            vec![
                "snug".to_string(),
                "--input".to_string(),
                "default.jar".to_string(),
                "--".to_string(),
            ]
        );
    }

    #[test]
    fn an_unknown_flag_does_not_suppress_a_files_input() {
        // An unrecognised flag fails parsing regardless. It must not also
        // be read as a bare positional, or the build would first lose the
        // file's `--input` and *then* fail — two problems where the parse
        // error was the only real one.
        let raw = args(&["snug", "--nonsense"]);
        let merged = merge(&raw, layer(vec!["--input".into(), "default.jar".into()]));
        assert!(
            merged.windows(2).any(|w| w == ["--input", "default.jar"]),
            "the file's --input should survive: {merged:?}"
        );
    }

    #[test]
    fn a_positional_in_a_higher_file_beats_a_lower_files_input() {
        // Precedence applies between tiers too, not only at the command
        // line: the OS-specific file outranks the generic one, so its JAR
        // wins and the generic `--input` goes.
        let raw = args(&["snug", "--output", "App.exe"]);
        let merged = merge(
            &raw,
            vec![
                vec!["--input".into(), "generic.jar".into()],
                vec!["app.jar".into()],
            ],
        );
        assert_eq!(
            merged,
            vec![
                "snug".to_string(),
                "app.jar".to_string(),
                "--output".to_string(),
                "App.exe".to_string(),
            ]
        );
    }

    #[test]
    fn short_form_with_attached_value_is_one_token() {
        // clap accepts `-oApp.exe`. It is a single token carrying a value,
        // so it must not leave a tail behind, and it must still override
        // the file's two-token form.
        let raw = args(&["snug", "app.jar", "-oCLI.exe"]);
        let merged = merge(&raw, layer(vec!["--output".into(), "File.exe".into()]));
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
            layer(vec![
                "--name".into(),
                "File".into(),
                "--min-java".into(),
                "25".into(),
            ]),
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
            layer(vec![
                "--jvm-arg".into(),
                "-Xms256m".into(),
                "--name".into(),
                "File".into(),
            ]),
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
