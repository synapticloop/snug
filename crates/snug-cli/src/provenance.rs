//! Where each effective option value came from.
//!
//! The options loader flattens every source — the command line and each
//! options file — into a single argv before clap ever sees it. That is
//! what makes the precedence rules work, and it is deliberately kept that
//! way: parsing each source separately would stop clap from seeing a
//! conflict that spans two of them (`--emit-payload` on the command line
//! and `--dry-run` in `snug.options` is one argv today, and one error).
//!
//! The cost of the flat argv is that the origin of a value is gone by the
//! time clap has parsed it: `--name` from `snug.options:4` and `--name`
//! from the command line are indistinguishable, and a value that lost the
//! precedence fight has been stripped out of the argv entirely.
//!
//! So the merge keeps a **side-channel** alongside the argv it produces:
//!
//! - one [`Origin`] per argv token, so any token clap points at can be
//!   traced back to a file and a line;
//! - the canonical flag each token starts, so a *value* can be attributed
//!   to the flag that introduced it;
//! - the values a higher-priority source overrode, which never reach the
//!   argv but still belong in the log.
//!
//! Clap's own `value_source` then answers the one question the side-channel
//! cannot: was this value supplied at all, or did it fall back to a
//! default? `Option<T>` fields that were never supplied are `[not set]`;
//! fields with a `default_value` that were not supplied are `[default]`;
//! and a `--min-java 25` that was typed is `[command line]`, which is
//! exactly the distinction a flat argv cannot make on its own.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use clap::ArgMatches;
use clap::parser::ValueSource;

/// One token from an options file, with the line it came from.
///
/// The line number is the whole point: an options file is a list of
/// one-flag-per-line settings, so `snug.options:12` is the only useful
/// way to point a user at the line they need to edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// The token text, exactly as it will appear in argv.
    pub text: String,
    /// 1-based line number in the file this token came from.
    pub line: usize,
}

impl Token {
    /// A token on the given line.
    pub fn new(text: impl Into<String>, line: usize) -> Self {
        Self {
            text: text.into(),
            line,
        }
    }
}

/// One options-file layer: the file it came from, and its tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionsLayer {
    /// The file the tokens were read from.
    pub path: PathBuf,
    /// The tokens, in file order.
    pub tokens: Vec<Token>,
}

impl OptionsLayer {
    /// A layer with the given path and tokens.
    pub fn new(path: impl Into<PathBuf>, tokens: Vec<Token>) -> Self {
        Self {
            path: path.into(),
            tokens,
        }
    }
}

/// A layer built from bare token text.
///
/// The line numbers are assigned by position, which is the best a
/// caller that has no file behind it can do.
impl From<Vec<String>> for OptionsLayer {
    fn from(tokens: Vec<String>) -> Self {
        Self {
            path: PathBuf::from("snug.options"),
            tokens: tokens
                .into_iter()
                .enumerate()
                .map(|(index, text)| Token::new(text, index + 1))
                .collect(),
        }
    }
}

/// Where an effective value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Typed on the command line.
    CommandLine,
    /// Read from an options file, at this line.
    OptionsFile { path: PathBuf, line: usize },
    /// A built-in default.
    Default,
    /// A built-in default produced by a named rule, such as the company
    /// falling back to the application name.
    DefaultRule(String),
    /// Derived from another value rather than supplied directly.
    Inferred { from: String },
    /// Read from a JAR's manifest.
    Manifest { jar: String },
    /// An optional setting that was not supplied at all.
    NotSet,
}

impl Origin {
    /// The bracketed label shown next to an effective option value.
    ///
    /// An options file in the working directory is shown by name
    /// (`[snug.options:12]`); one named from elsewhere is shown by full
    /// path, because two files called `snug.options` in different
    /// directories are not the same file and the bare name would not say
    /// which one was read.
    pub fn label(&self, cwd: &Path) -> String {
        match self {
            Origin::CommandLine => "[command line]".to_string(),
            Origin::OptionsFile { path, line } => {
                format!("[{}:{}]", display_path(path, cwd), line)
            }
            Origin::Default => "[default]".to_string(),
            Origin::DefaultRule(rule) => format!("[default: {rule}]"),
            Origin::Inferred { from } => format!("[inferred from {from}]"),
            Origin::Manifest { jar } => format!("[JAR manifest: {jar}]"),
            Origin::NotSet => "[not set]".to_string(),
        }
    }
}

/// How a path is shown in an origin label.
fn display_path(path: &Path, cwd: &Path) -> String {
    let in_cwd = match path.parent() {
        Some(parent) => parent == cwd,
        None => true,
    };
    if in_cwd {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string())
    } else {
        path.display().to_string()
    }
}

/// A value that lost to a higher-priority source.
///
/// These never reach the merged argv, so without this record the log
/// could only say *what* won — never *what it beat*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overridden {
    /// The canonical flag name, such as `name`.
    pub flag: String,
    /// The value that was overridden, when one could be read.
    pub value: Option<String>,
    /// Where the overridden value came from.
    pub origin: Origin,
}

/// The merged argv plus the provenance side-channel that explains it.
#[derive(Debug, Clone, Default)]
pub struct Merged {
    /// The argv clap parses. Index 0 is the program name.
    pub argv: Vec<String>,
    /// One origin per argv token, parallel to `argv`.
    pub origins: Vec<Origin>,
    /// The canonical flag each argv token starts, if it starts one.
    ///
    /// This is what lets a *value* be attributed to the flag that
    /// introduced it: clap reports the index of a value, and the value
    /// token carries the same line as its flag in every well-formed
    /// options file, but the flag token is the authoritative answer.
    pub flag_at: Vec<Option<String>>,
    /// The argv indexes of the tokens that are positional arguments.
    ///
    /// A positional is not a flag, so it has no entry in `flag_at` — and
    /// an options file *can* carry one, which is why the input JAR's
    /// origin needs its own lookup.
    pub positional_at: Vec<usize>,
    /// Values a higher-priority source overrode.
    pub overridden: Vec<Overridden>,
    /// clap argument id to canonical long name.
    ///
    /// clap's ids are field names (`update_url`) while the canonical name
    /// is the flag (`update-url`), and the side-channel is keyed by the
    /// latter.
    pub id_to_canonical: HashMap<String, String>,
    /// The clap ids of the positional arguments.
    pub positional_ids: Vec<String>,
}

impl Merged {
    /// The origin of a single-valued argument's effective value.    ///
    /// `ValueSource::DefaultValue` means clap supplied the value itself,
    /// so the origin is the built-in default. `ValueSource::CommandLine`
    /// means it was supplied — but "supplied" does not say *where*, which
    /// is what the side-channel is for.
    pub fn origin_of(&self, matches: &ArgMatches, id: &str) -> Origin {
        match matches.value_source(id) {
            Some(ValueSource::DefaultValue) => Origin::Default,
            Some(ValueSource::CommandLine) => {
                let canonical = self.canonical_for(id);
                if let Some(index) = self.flag_indices(&canonical).first() {
                    return self.origins[*index].clone();
                }
                // A positional argument is not a flag token, so it has to
                // be looked up separately. An options file can carry a
                // bare JAR, and that JAR's origin is worth knowing.
                if self.positional_ids.iter().any(|known| known == id)
                    && let Some(index) = self.positional_at.first()
                {
                    return self.origins[*index].clone();
                }
                Origin::CommandLine
            }
            // `EnvVariable` and `File` are clap sources snug does not use.
            // Treating them as defaults would be a lie, and there is no
            // better answer available.
            Some(_) => Origin::Default,
            None => Origin::NotSet,
        }
    }

    /// The origin of every occurrence of a repeatable argument, in argv
    /// order.
    ///
    /// A repeatable flag accumulates across sources, so each value needs
    /// its own origin — `--jvm-arg` from `snug.options` and `--jvm-arg`
    /// from the command line are different settings.
    pub fn origins_of_all(&self, matches: &ArgMatches, id: &str) -> Vec<Origin> {
        if matches.value_source(id).is_none() {
            return Vec::new();
        }
        self.flag_indices(&self.canonical_for(id))
            .into_iter()
            .map(|index| self.origins[index].clone())
            .collect()
    }

    /// The clap argument id for a canonical long name.
    ///
    /// The inverse of [`Merged::canonical_for`]. The overridden-value
    /// records name the flag the way the user wrote it (`update-url`),
    /// while clap knows the argument by its field name (`update_url`), so
    /// finding the winner of an overridden flag means going back the other
    /// way.
    pub fn id_for(&self, canonical: &str) -> Option<&str> {
        self.id_to_canonical
            .iter()
            .find(|(_, name)| name.as_str() == canonical)
            .map(|(id, _)| id.as_str())
    }

    /// The canonical long name for a clap argument id.
    fn canonical_for(&self, id: &str) -> String {
        self.id_to_canonical
            .get(id)
            .cloned()
            .unwrap_or_else(|| id.to_string())
    }

    /// The argv indexes of every token that starts the given flag.
    fn flag_indices(&self, canonical: &str) -> Vec<usize> {
        self.flag_at
            .iter()
            .enumerate()
            .filter(|(_, flag)| flag.as_deref() == Some(canonical))
            .map(|(index, _)| index)
            .collect()
    }
}

/// A merged result behaves like the argv it carries.
///
/// The provenance side-channel is additional information about the
/// tokens, not a different thing from them, so a caller that only wants
/// the argv can treat the merged result as the token list.
impl std::ops::Deref for Merged {
    type Target = [String];

    fn deref(&self) -> &Self::Target {
        &self.argv
    }
}

/// A merged result compares equal to the argv it carries.
impl PartialEq<Vec<String>> for Merged {
    fn eq(&self, other: &Vec<String>) -> bool {
        self.argv == *other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{ArgAction, CommandFactory, Parser};

    #[derive(Debug, Parser)]
    struct TestCli {
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value_t = 25)]
        min_java: u16,
        #[arg(long, action = ArgAction::Append, allow_hyphen_values = true)]
        jvm_arg: Vec<String>,
    }

    /// Build a merged result the way the real merge does.
    ///
    /// The id-to-canonical map matters: clap's ids are field names
    /// (`jvm_arg`) while the side-channel is keyed by the flag
    /// (`jvm-arg`), and a merged result whose map is empty would look up
    /// the wrong key.
    fn merged_argv(argv: &[&str], origins: Vec<Origin>) -> Merged {
        let cli = TestCli::command();

        let mut id_to_canonical = HashMap::new();
        for arg in cli.get_arguments() {
            let canonical = arg
                .get_long()
                .unwrap_or_else(|| arg.get_id().as_str())
                .to_string();
            id_to_canonical.insert(arg.get_id().as_str().to_string(), canonical);
        }

        let mut m = Merged {
            argv: argv.iter().map(|s| s.to_string()).collect(),
            origins,
            flag_at: vec![None; argv.len()],
            id_to_canonical,
            ..Default::default()
        };

        // Mark the flag tokens, and skip the value that follows a
        // value-taking flag so it is not mistaken for a flag itself.
        let mut skip = false;
        for (index, token) in m.argv.iter().enumerate() {
            if index == 0 {
                continue;
            }
            if skip {
                skip = false;
                continue;
            }
            let Some(name) = token.strip_prefix("--") else {
                continue;
            };
            let name = name.split('=').next().unwrap_or("");
            m.flag_at[index] = Some(name.to_string());
            if !token.contains('=')
                && cli.get_arguments().any(|a| {
                    a.get_long() == Some(name)
                        && a.get_num_args().unwrap_or_default().takes_values()
                })
            {
                skip = true;
            }
        }
        m
    }

    #[test]
    fn labels_read_the_way_the_brief_shows_them() {
        let cwd = Path::new("/work");
        assert_eq!(Origin::CommandLine.label(cwd), "[command line]");
        assert_eq!(Origin::Default.label(cwd), "[default]");
        assert_eq!(Origin::NotSet.label(cwd), "[not set]");
        assert_eq!(
            Origin::DefaultRule("application name".into()).label(cwd),
            "[default: application name]"
        );
        assert_eq!(
            Origin::Inferred {
                from: "input".into()
            }
            .label(cwd),
            "[inferred from input]"
        );
        assert_eq!(
            Origin::Manifest {
                jar: "app.jar".into()
            }
            .label(cwd),
            "[JAR manifest: app.jar]"
        );
    }

    #[test]
    fn an_options_file_in_the_working_directory_is_shown_by_name() {
        let cwd = Path::new("/work");
        let origin = Origin::OptionsFile {
            path: PathBuf::from("/work/snug.options"),
            line: 12,
        };
        assert_eq!(origin.label(cwd), "[snug.options:12]");
    }

    #[test]
    fn an_options_file_elsewhere_is_shown_by_path() {
        let cwd = Path::new("/work");
        let origin = Origin::OptionsFile {
            path: PathBuf::from("/elsewhere/build.options"),
            line: 3,
        };
        assert_eq!(origin.label(cwd), "[/elsewhere/build.options:3]");
    }

    #[test]
    fn a_default_is_distinguished_from_a_supplied_value() {
        let argv = ["snug", "--min-java", "25"];
        let merged = merged_argv(
            &argv,
            vec![
                Origin::CommandLine,
                Origin::CommandLine,
                Origin::CommandLine,
            ],
        );
        let matches = TestCli::command()
            .try_get_matches_from(argv)
            .expect("parse");
        assert_eq!(
            merged.origin_of(&matches, "min_java"),
            Origin::CommandLine,
            "an explicitly typed value is not a default"
        );
    }

    #[test]
    fn an_untyped_default_reports_as_the_default() {
        let argv = ["snug"];
        let merged = merged_argv(&argv, vec![Origin::CommandLine]);
        let matches = TestCli::command()
            .try_get_matches_from(argv)
            .expect("parse");
        assert_eq!(merged.origin_of(&matches, "min_java"), Origin::Default);
    }

    #[test]
    fn an_unset_optional_reports_as_not_set() {
        let argv = ["snug"];
        let merged = merged_argv(&argv, vec![Origin::CommandLine]);
        let matches = TestCli::command()
            .try_get_matches_from(argv)
            .expect("parse");
        assert_eq!(merged.origin_of(&matches, "name"), Origin::NotSet);
    }

    #[test]
    fn every_occurrence_of_a_repeatable_gets_its_own_origin() {
        let argv = ["snug", "--jvm-arg", "-Xmx2g", "--jvm-arg", "-Xms256m"];
        let merged = merged_argv(
            &argv,
            vec![
                Origin::CommandLine,
                Origin::CommandLine,
                Origin::CommandLine,
                Origin::OptionsFile {
                    path: PathBuf::from("/work/snug.options"),
                    line: 9,
                },
                Origin::OptionsFile {
                    path: PathBuf::from("/work/snug.options"),
                    line: 9,
                },
            ],
        );
        let matches = TestCli::command()
            .try_get_matches_from(argv)
            .expect("parse");
        let origins = merged.origins_of_all(&matches, "jvm_arg");
        assert_eq!(origins.len(), 2, "both occurrences are reported");
        assert_eq!(origins[0], Origin::CommandLine);
        assert_eq!(
            origins[1],
            Origin::OptionsFile {
                path: PathBuf::from("/work/snug.options"),
                line: 9
            }
        );
    }
}
