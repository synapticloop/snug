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

    // Read the manifest as bytes and decode lossily. `read_to_string` fails
    // the whole build on a single non-UTF-8 byte -- a Latin-1 `Name:`
    // attribute is enough -- refusing a JAR that `java -jar` runs happily,
    // with the error pointing at the JAR rather than at the one attribute.
    // `from_utf8_lossy` substitutes U+FFFD for the offending byte and
    // leaves everything else intact, which is all we need: the parser
    // below only ever looks for an ASCII `Main-Class:` header, and a
    // mangled attribute elsewhere cannot affect it.
    let mut bytes = Vec::new();
    manifest
        .read_to_end(&mut bytes)
        .context("reading manifest contents")?;
    let buf = String::from_utf8_lossy(&bytes);

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
/// Manifest values wrap at 72 bytes, with continuation lines starting with a
/// single space — and a long package name can push `Main-Class` past that.
/// A conforming writer then emits
///
/// ```text
/// Main-Class: com.example.some.rather.long.package.name.and.Cla
///  ss
/// ```
///
/// which this used to read as `com.example...Cla`, silently producing a
/// launcher that cannot start. Continuation lines are joined here.
///
/// Contrary to what the previous comment claimed, continuations *are* legal
/// for `Main-Class` — the 72-byte limit applies to every line in the file.
pub fn parse_main_class(manifest: &str) -> Option<String> {
    let mut lines = manifest.lines();
    while let Some(line) = lines.next() {
        // Manifest headers are ASCII; non-UTF-8 is impossible by spec.
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim() != "Main-Class" {
            continue;
        }
        let mut v = value.trim_start().to_owned();
        // Join continuation lines: each begins with a single space and
        // carries the rest of the value.
        for next in lines.by_ref() {
            let Some(continuation) = next.strip_prefix(' ') else {
                break;
            };
            v.push_str(continuation);
        }
        let v = v.trim();
        if v.is_empty() {
            return None;
        }
        return Some(v.to_owned());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_a_wrapped_main_class_continuation() {
        // Manifest values wrap at 72 bytes. A long package name pushes
        // `Main-Class` over the limit and a conforming writer splits it.
        // This used to read only the first physical line, producing a
        // silently wrong class name and a launcher that cannot start.
        let mf = "Manifest-Version: 1.0\nMain-Class: com.example.a.rather.long.package.\n com.example.Fully.Qualified.Main\n";
        assert_eq!(
            parse_main_class(mf).as_deref(),
            Some("com.example.a.rather.long.package.com.example.Fully.Qualified.Main"),
            "the continuation carries the rest of the value"
        );
    }

    #[test]
    fn a_three_line_continuation_is_joined() {
        let mf = "Main-Class: com.example.\n Very.\n Long.Main\n";
        assert_eq!(parse_main_class(mf).as_deref(), Some("com.example.Very.Long.Main"));
    }

    #[test]
    fn a_continuation_does_not_swallow_the_next_header() {
        // The join must stop at the first line that is not a continuation,
        // or every later header would be appended to the class name.
        let mf = "Main-Class: com.example.Main\n continued\nBuilt-By: someone\n";
        assert_eq!(
            parse_main_class(mf).as_deref(),
            Some("com.example.Maincontinued"),
            "the join stops at the first non-continuation line"
        );
    }

    #[test]
    fn an_ordinary_single_line_is_unaffected() {
        let mf = "Manifest-Version: 1.0\nBuilt-By: julian\nMain-Class: a.b.C\n";
        assert_eq!(parse_main_class(mf).as_deref(), Some("a.b.C"));
    }

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
    fn a_non_utf8_attribute_does_not_lose_the_main_class() {
        // Drives the real entry point, not `String::from_utf8_lossy` in
        // isolation: the bug was that `read_to_string` returns `Err` on
        // invalid UTF-8 and the caller turned that into a hard failure --
        // so `snug` refused a JAR that `java -jar` runs, with the error
        // pointing at the archive rather than at the one attribute that
        // was mis-encoded.
        let mut mf: Vec<u8> = b"Manifest-Version: 1.0\n".to_vec();
        mf.extend_from_slice(b"Built-By: Andr\xe9\n"); // Latin-1 'e-acute'
        mf.extend_from_slice(b"Main-Class: com.example.Main\n");

        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("META-INF/MANIFEST.MF", zip::write::SimpleFileOptions::default())
            .expect("start manifest");
        std::io::Write::write_all(&mut zip, &mf).expect("write manifest");
        let bytes = zip.finish().expect("finish zip").into_inner();

        assert_eq!(
            read_main_class_from_bytes(&bytes).expect("a bad byte must not fail the read"),
            Some("com.example.Main".to_string()),
            "the Main-Class must survive an undecodable neighbour"
        );
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
