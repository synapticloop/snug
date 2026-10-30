//! How paths appear in the build log.
//!
//! A log that repeats the same absolute prefix on every line is hard to
//! read and harder to paste into a bug report, so snug states the
//! working directory **once** — on the log's first line — and writes
//! every path after it relative to that directory.
//!
//! The relativising happens on the *text* of a line rather than on
//! `Path` values, because most of what a build prints is assembled from
//! several sources: a diagnostic's message, a location, a hint. Working
//! on the finished line means a path reaches the log relative whether it
//! was formatted by snug or by a library snug called.

use std::path::{Path, PathBuf};

/// The directory the log's paths are relative to.
///
/// The base is the working directory snug was started in — the same
/// directory the default build log lands in, and the one every other
/// path snug resolves is relative to.
#[derive(Debug, Clone)]
pub struct PathBase {
    /// The base directory itself.
    base: PathBuf,
    /// The ways the base can be written inside a line of text.
    needles: Vec<Needle>,
}

/// One way the base directory can appear in a line of text.
#[derive(Debug, Clone)]
struct Needle {
    /// The text that identifies the base.
    text: String,
    /// What has to follow the text for it to name a path.
    after: After,
}

/// What must follow a needle for it to be a path rather than a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum After {
    /// A separator, which belongs to the path that follows it and is
    /// consumed along with the needle.
    Separator,
    /// A path character: the needle *is* the whole prefix, the way the
    /// filesystem root is.
    NotSeparator,
}

impl PathBase {
    /// The base directory for a run started in `cwd`.
    pub fn new(cwd: &Path) -> Self {
        let text = cwd.display().to_string();
        let mut needles = Vec::new();

        if cwd.parent().is_none() {
            // The filesystem root. On Windows that is `C:\`, whose
            // meaningful prefix is the drive `C:`; on Unix it is `/`,
            // which is the whole of it.
            if cfg!(windows) {
                needles.push(Needle {
                    text: trim_separators(&text),
                    after: After::Separator,
                });
            } else {
                needles.push(Needle {
                    text: "/".to_string(),
                    after: After::NotSeparator,
                });
            }
        } else {
            let trimmed = trim_separators(&text);
            needles.push(Needle {
                text: trimmed.clone(),
                after: After::Separator,
            });
            // A path can be written with either separator, and a path the
            // user typed keeps the slashes it was given.
            let slashed = trimmed.replace('\\', "/");
            if slashed != trimmed {
                needles.push(Needle {
                    text: slashed,
                    after: After::Separator,
                });
            }
        }

        Self {
            base: cwd.to_path_buf(),
            needles,
        }
    }

    /// The base directory.
    pub fn path(&self) -> &Path {
        &self.base
    }

    /// The line that states the base directory.
    ///
    /// This is the log's first line: everything after it is relative to
    /// the directory it names, so this is the one place the absolute
    /// path appears.
    pub fn headline(&self) -> String {
        format!("snug: working directory {}", self.base.display())
    }

    /// Rewrite every path in `line` that lies under the base so that it
    /// is relative to it.
    ///
    /// A path outside the base is left alone. There is no relative form
    /// of it that is not a guess about where the reader is standing,
    /// and an absolute path is the honest answer.
    pub fn shorten(&self, line: &str) -> String {
        if self.needles.is_empty() {
            return line.to_string();
        }

        let mut out = String::with_capacity(line.len());
        let mut cursor = 0usize;

        while cursor < line.len() {
            let Some((start, len, after)) = self.find(line, cursor) else {
                break;
            };
            let end = start + len;
            let next = line[end..].chars().next();

            let consumed = match after {
                After::Separator => match next {
                    Some(ch) if is_separator(ch) => Some(end + 1),
                    _ => None,
                },
                After::NotSeparator => match next {
                    Some(ch) if !is_separator(ch) => Some(end),
                    _ => None,
                },
            };

            match consumed {
                Some(next_cursor) => {
                    out.push_str(&line[cursor..start]);
                    cursor = next_cursor;
                }
                None => {
                    // The needle matched but does not name a path here: it
                    // is the base directory itself, or a longer name that
                    // merely starts with it. Keep the text and look again
                    // further along.
                    out.push_str(&line[cursor..end]);
                    cursor = end;
                }
            }
        }

        out.push_str(&line[cursor..]);
        out
    }

    /// The earliest place in `line` (at or after `from`) where one of the
    /// needles names a path.
    fn find(&self, line: &str, from: usize) -> Option<(usize, usize, After)> {
        let mut best: Option<(usize, usize, After)> = None;

        for needle in &self.needles {
            let mut index = from;
            while index + needle.text.len() <= line.len() {
                let matched = line
                    .get(index..index + needle.text.len())
                    .is_some_and(|candidate| text_eq(candidate, &needle.text))
                    && boundary_ok(line, index);
                if matched {
                    let better = match best {
                        None => true,
                        Some((start, _, _)) => index < start,
                    };
                    if better {
                        best = Some((index, needle.text.len(), needle.after));
                    }
                    break;
                }
                // Advance one character. `index` always sits on a
                // character boundary, so this cannot split a UTF-8
                // sequence.
                match line[index..].chars().next() {
                    Some(ch) => index += ch.len_utf8(),
                    None => break,
                }
            }
        }

        best
    }
}

/// Whether two pieces of path text are the same path.
///
/// Windows paths are case-insensitive, so a path typed by a user can
/// differ in case from the one snug resolved itself. Everywhere else
/// they are not, and pretending otherwise would rewrite a path that is
/// genuinely a different file.
fn text_eq(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// Whether the text before `index` can end a word rather than a path.
///
/// This is what keeps a needle from matching in the middle of a longer
/// name: `C:\work\project` must not be found inside
/// `C:\work\projectile`, and a URL's `https://host/share` must not be
/// mistaken for a path.
fn boundary_ok(line: &str, index: usize) -> bool {
    if index == 0 {
        return true;
    }
    match line[..index].chars().next_back() {
        Some(ch) => is_boundary(ch),
        None => true,
    }
}

/// Whether a character can precede a path in a line of text.
fn is_boundary(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '"' | '\'' | '(' | '[' | '{' | '=' | ':' | '>' | '|' | ',' | ';'
        )
}

/// Whether a character separates two path segments.
fn is_separator(ch: char) -> bool {
    ch == '/' || ch == '\\'
}

/// A path without the trailing separator it may have been written with.
fn trim_separators(text: &str) -> String {
    let mut out = text.to_string();
    while out.len() > 1 && out.ends_with(['/', '\\']) {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path built from segments, so a test does not depend on which
    /// separator the host uses.
    fn p(segments: &[&str]) -> PathBuf {
        segments.iter().collect()
    }

    #[test]
    fn a_path_under_the_base_is_written_relative_to_it() {
        let base = PathBase::new(&p(&["work", "project"]));
        let line = format!(
            "snug: loaded options from {}",
            p(&["work", "project", "snug.options"]).display()
        );
        assert_eq!(
            base.shorten(&line),
            "snug: loaded options from snug.options"
        );
    }

    #[test]
    fn a_nested_path_keeps_the_part_below_the_base() {
        let base = PathBase::new(&p(&["work", "project"]));
        let line = format!(
            "reading {}",
            p(&["work", "project", "build", "libs", "app.jar"]).display()
        );
        assert_eq!(
            base.shorten(&line),
            format!("reading {}", p(&["build", "libs", "app.jar"]).display())
        );
    }

    #[test]
    fn a_path_outside_the_base_is_left_alone() {
        let base = PathBase::new(&p(&["work", "project"]));
        let line = format!("reading {}", p(&["elsewhere", "app.jar"]).display());
        assert_eq!(base.shorten(&line), line);
    }

    #[test]
    fn the_base_itself_is_not_rewritten() {
        let base = PathBase::new(&p(&["work", "project"]));
        let line = format!("the directory is {}", p(&["work", "project"]).display());
        assert_eq!(base.shorten(&line), line);
    }

    #[test]
    fn a_longer_name_that_starts_with_the_base_is_not_rewritten() {
        let base = PathBase::new(&p(&["work", "project"]));
        let line = format!(
            "reading {}",
            p(&["work", "projectile", "app.jar"]).display()
        );
        assert_eq!(base.shorten(&line), line);
    }

    #[test]
    fn a_relative_path_is_untouched() {
        let base = PathBase::new(&p(&["work", "project"]));
        let line = format!("reading {}", p(&["build", "libs", "app.jar"]).display());
        assert_eq!(base.shorten(&line), line);
    }

    #[test]
    fn a_url_is_not_mistaken_for_a_path() {
        let base = PathBase::new(&p(&["work", "project"]));
        let line = "see https://example.com/work/project/app.jar";
        assert_eq!(base.shorten(line), line);
    }

    #[test]
    fn the_headline_names_the_base_directory() {
        let dir = p(&["work", "project"]);
        let base = PathBase::new(&dir);
        assert_eq!(
            base.headline(),
            format!("snug: working directory {}", dir.display())
        );
    }

    #[test]
    fn the_base_is_the_directory_it_was_built_from() {
        let dir = p(&["work", "project"]);
        let base = PathBase::new(&dir);
        assert_eq!(base.path(), dir);
    }

    #[cfg(windows)]
    #[test]
    fn a_drive_root_is_the_base() {
        let base = PathBase::new(Path::new("C:\\"));
        assert_eq!(
            base.shorten("reading C:\\Users\\Admin\\app.jar"),
            "reading Users\\Admin\\app.jar"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_path_written_with_different_case_is_still_relative() {
        let base = PathBase::new(Path::new("C:\\Work\\Project"));
        assert_eq!(
            base.shorten("reading c:\\work\\project\\app.jar"),
            "reading app.jar"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_path_written_with_forward_slashes_is_still_relative() {
        let base = PathBase::new(Path::new("C:\\Work\\Project"));
        assert_eq!(
            base.shorten("reading C:/Work/Project/app.jar"),
            "reading app.jar"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_filesystem_root_is_the_base() {
        let base = PathBase::new(Path::new("/"));
        assert_eq!(base.shorten("reading /etc/app.jar"), "reading etc/app.jar");
    }
}
