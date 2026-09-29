//! Read the `Main-Class` attribute from a JAR's `META-INF/MANIFEST.MF`.
//!
//! The implementation is intentionally minimal: we don't need full manifest
//! parsing, only the `Main-Class` attribute of the **first** manifest entry
//! we encounter (which is the canonical one for executable JARs).

use std::io::{Read, Seek};
use std::path::Path;

use anyhow::{Context, Result};

/// Locate `META-INF/MANIFEST.MF` inside the JAR and extract its
/// `Main-Class` attribute.
///
/// Returns `Ok(None)` if the manifest has no `Main-Class` line, or if the
/// JAR has no `META-INF/MANIFEST.MF` entry at all.
pub fn read_main_class(jar_path: &Path) -> Result<Option<String>> {
    let file = std::fs::File::open(jar_path)
        .with_context(|| format!("opening JAR {}", jar_path.display()))?;
    read_main_class_from(file).with_context(|| format!("reading JAR {}", jar_path.display()))
}

/// Same as [`read_main_class`], but for JAR bytes already in memory.
///
/// The builder holds every JAR's bytes, so re-opening the file (or
/// worse, writing it to a temp file just to have a path) is pure waste.
/// Use this on the build path; use [`read_main_class`] for one-off
/// checks against a file on disk.
pub fn read_main_class_from_bytes(bytes: &[u8]) -> Result<Option<String>> {
    read_main_class_from(std::io::Cursor::new(bytes))
        .context("reading Main-Class from an in-memory JAR")
}

/// Shared implementation. `zip::ZipArchive` needs `Read + Seek`, which
/// both `File` and `Cursor<&[u8]>` provide.
fn read_main_class_from<R: Read + Seek>(reader: R) -> Result<Option<String>> {
    let mut archive = zip::ZipArchive::new(reader).context("reading JAR as zip")?;

    let mut manifest = match archive.by_name("META-INF/MANIFEST.MF") {
        Ok(m) => m,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(e).context("locating META-INF/MANIFEST.MF in the JAR"),
    };

    let mut buf = String::new();
    manifest
        .read_to_string(&mut buf)
        .context("reading manifest contents")?;

    Ok(parse_main_class(&buf))
}

/// One JAR's `Main-Class` declaration, if it makes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MainClassDeclaration {
    /// Display label for the JAR this came from (file name, or the
    /// full path when the label would otherwise be ambiguous).
    pub jar: String,

    /// The declared `Main-Class` value.
    pub main_class: String,
}

/// Survey every JAR for a `Main-Class` declaration.
///
/// Exists because "which manifest wins?" is not answerable from a
/// single JAR. For a multi-JAR input, any number of them may declare a
/// `Main-Class` — a fat JAR plus a bundled runnable tool, a directory
/// holding a dependency that happens to be executable itself — and the
/// builder's rule (first JAR, alphabetically) is a filename-ordering
/// accident, not a decision the user made.
///
/// Individual failures are skipped rather than propagated: a JAR that
/// isn't a readable zip can't contribute a `Main-Class` anyway, and the
/// build path will report the real problem separately.
pub fn survey_main_classes<F>(labels_and_bytes: F) -> Vec<MainClassDeclaration>
where
    F: IntoIterator<Item = (String, Vec<u8>)>,
{
    let mut found = Vec::new();
    for (jar, bytes) in labels_and_bytes {
        if let Ok(Some(main_class)) = read_main_class_from_bytes(&bytes) {
            found.push(MainClassDeclaration { jar, main_class });
        }
    }
    found
}

/// Parse a `Main-Class:` line out of a manifest body.
///
/// The manifest format is line-based, with continuation lines indented by
/// a single space; we only need single-line `Main-Class:` and ignore
/// continuations (none are legal for `Main-Class` anyway).
pub fn parse_main_class(manifest: &str) -> Option<String> {
    for line in manifest.lines() {
        // Manifest headers are ASCII; non-UTF-8 is impossible by spec.
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim() == "Main-Class" {
            let v = value.trim();
            if v.is_empty() {
                return None;
            }
            return Some(v.to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_main_class() {
        let mf = "Manifest-Version: 1.0\nMain-Class: com.example.Main\n";
        assert_eq!(parse_main_class(mf).as_deref(), Some("com.example.Main"));
    }

    #[test]
    fn ignores_other_attributes() {
        let mf = "Manifest-Version: 1.0\nBuilt-By: julian\nMain-Class: a.b.C\n";
        assert_eq!(parse_main_class(mf).as_deref(), Some("a.b.C"));
    }

    #[test]
    fn returns_none_when_absent() {
        let mf = "Manifest-Version: 1.0\nBuilt-By: julian\n";
        assert_eq!(parse_main_class(mf), None);
    }

    #[test]
    fn handles_blank_value() {
        let mf = "Main-Class: \n";
        assert_eq!(parse_main_class(mf), None);
    }

    #[test]
    fn handles_no_trailing_newline() {
        let mf = "Manifest-Version: 1.0\r\nMain-Class: x.Y";
        assert_eq!(parse_main_class(mf).as_deref(), Some("x.Y"));
    }
}
