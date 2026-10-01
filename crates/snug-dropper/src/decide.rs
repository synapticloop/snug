//! Which of the three modes a given set of dropped items selects.
//!
//! Windows delivers a drag-and-drop by launching the target EXE with the
//! dropped paths appended to its command line, so `argv[1..]` **is** the
//! drop payload and "double-clicked" is simply the empty case. There is
//! no drag-drop API to register and no drop-target COM plumbing — which
//! is why this whole module is a pure function over a slice of paths.
//!
//! Kept free of both the filesystem and the UI so the decision table can
//! be tested exhaustively without touching disk: [`decide_with`] takes an
//! injectable classifier, and [`decide`] supplies the real one.

use std::path::{Path, PathBuf};

/// What the process should do with the items it was handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// No items — a double-click. Open a command window where we live so
    /// the user has the real `snug` in front of them.
    OpenTerminal,
    /// Exactly one usable input. Build it.
    Build {
        /// The dropped JAR, or the dropped directory of JARs.
        input: PathBuf,
    },
    /// Not something this starter can do. Say why, then hand over to the
    /// terminal — the escape hatch for every advanced case (icons,
    /// splashes, `--main-class`, localisations, multi-JAR builds).
    Reject(Reject),
}

/// Why a drop was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    /// More than one item was dropped.
    TooManyItems,
    /// A single item that is neither a `.jar` nor a directory.
    UnsupportedType,
}

/// What one dropped path actually is.
///
/// Split out from [`Mode`] so the classifier can be swapped in tests. Note
/// that a *directory* is a first-class input here: `snug` accepts a folder
/// of JARs as a multi-JAR classpath, so dropping one is a legitimate
/// build rather than a mistake to be complained about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    /// A file with a `.jar` extension (case-insensitive).
    Jar,
    /// A directory — a folder of JARs.
    Directory,
    /// Anything else.
    Other,
}

impl ItemKind {
    /// Classify a real path. Directory first: a directory that happens to
    /// be *named* `something.jar` is still a directory, and treating it as
    /// a JAR would hand `snug` a path it can't open.
    pub fn of(path: &Path) -> Self {
        if path.is_dir() {
            return ItemKind::Directory;
        }
        match path.extension().and_then(|e| e.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("jar") => ItemKind::Jar,
            _ => ItemKind::Other,
        }
    }
}

/// Decide what to do with the dropped items.
pub fn decide<I, P>(items: I) -> Mode
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    decide_with(items, ItemKind::of)
}

/// [`decide`] with an injectable classifier.
///
/// The count check runs **before** the type check on purpose: dropping
/// three JARs is "only one is allowed", not three separate complaints, and
/// checking order is part of the behaviour worth pinning in tests.
pub fn decide_with<I, P, F>(items: I, mut kind_of: F) -> Mode
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
    F: FnMut(&Path) -> ItemKind,
{
    let mut items = items.into_iter();

    let first = match items.next() {
        None => return Mode::OpenTerminal,
        Some(p) => p,
    };
    if items.next().is_some() {
        return Mode::Reject(Reject::TooManyItems);
    }

    let input = first.as_ref().to_path_buf();
    match kind_of(&input) {
        ItemKind::Jar | ItemKind::Directory => Mode::Build { input },
        ItemKind::Other => Mode::Reject(Reject::UnsupportedType),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the table without touching the filesystem.
    fn decide_kinds(kinds: &[ItemKind]) -> Mode {
        let paths: Vec<PathBuf> = (0..kinds.len())
            .map(|i| PathBuf::from(format!("item{i}")))
            .collect();
        let mut it = kinds.iter().copied();
        decide_with(paths, |_| it.next().expect("classifier asked for too many items"))
    }

    #[test]
    fn no_items_opens_the_terminal() {
        assert_eq!(decide_kinds(&[]), Mode::OpenTerminal);
    }

    #[test]
    fn one_jar_builds() {
        assert_eq!(
            decide_kinds(&[ItemKind::Jar]),
            Mode::Build {
                input: PathBuf::from("item0")
            }
        );
    }

    #[test]
    fn one_folder_builds() {
        // A folder of JARs is a supported snug input (multi-JAR
        // classpath), so it must not be confused with an unsupported type.
        assert_eq!(
            decide_kinds(&[ItemKind::Directory]),
            Mode::Build {
                input: PathBuf::from("item0")
            }
        );
    }

    #[test]
    fn two_jars_are_rejected() {
        assert_eq!(
            decide_kinds(&[ItemKind::Jar, ItemKind::Jar]),
            Mode::Reject(Reject::TooManyItems)
        );
    }

    #[test]
    fn two_folders_are_rejected() {
        assert_eq!(
            decide_kinds(&[ItemKind::Directory, ItemKind::Directory]),
            Mode::Reject(Reject::TooManyItems)
        );
    }

    #[test]
    fn many_items_are_rejected_regardless_of_type() {
        // Counting comes first: three items is a count problem even when
        // two of them are things we could have built individually.
        assert_eq!(
            decide_kinds(&[
                ItemKind::Jar,
                ItemKind::Jar,
                ItemKind::Other
            ]),
            Mode::Reject(Reject::TooManyItems)
        );
    }

    #[test]
    fn one_unsupported_item_is_rejected() {
        assert_eq!(
            decide_kinds(&[ItemKind::Other]),
            Mode::Reject(Reject::UnsupportedType)
        );
    }

    #[test]
    fn a_mixed_pair_reports_the_count_not_the_type() {
        // A JAR plus a stray text file: still "only one", which is the
        // actionable complaint. The type rule only speaks when the count
        // is fine.
        assert_eq!(
            decide_kinds(&[ItemKind::Jar, ItemKind::Other]),
            Mode::Reject(Reject::TooManyItems)
        );
    }

    #[test]
    fn classify_reads_extensions_case_insensitively() {
        assert_eq!(
            ItemKind::of(Path::new("C:/tmp/App.JAR")),
            ItemKind::Jar
        );
        assert_eq!(
            ItemKind::of(Path::new("C:/tmp/App.Jar")),
            ItemKind::Jar
        );
    }

    #[test]
    fn classify_rejects_other_extensions() {
        assert_eq!(ItemKind::of(Path::new("notes.txt")), ItemKind::Other);
        assert_eq!(ItemKind::of(Path::new("app.war")), ItemKind::Other);
        assert_eq!(ItemKind::of(Path::new("noextension")), ItemKind::Other);
    }

    #[test]
    fn classify_treats_a_missing_path_named_jar_as_a_jar() {
        // `is_dir()` is false for a path that doesn't exist, so the
        // extension decides. A bogus path then reaches `snug`, which
        // reports the real error — better than us guessing here.
        assert_eq!(ItemKind::of(Path::new("nope/missing.jar")), ItemKind::Jar);
    }
}
