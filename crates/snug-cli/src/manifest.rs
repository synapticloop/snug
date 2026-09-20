//! Read the `Main-Class` attribute from a JAR's `META-INF/MANIFEST.MF`.
//!
//! The implementation is intentionally minimal: we don't need full manifest
//! parsing, only the `Main-Class` attribute of the **first** manifest entry
//! we encounter (which is the canonical one for executable JARs).

use std::io::Read;
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
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("reading JAR {} as zip", jar_path.display()))?;

    let mut manifest = match archive.by_name("META-INF/MANIFEST.MF") {
        Ok(m) => m,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => {
            return Err(e).with_context(|| {
                format!("locating META-INF/MANIFEST.MF in {}", jar_path.display())
            })
        }
    };

    let mut buf = String::new();
    manifest
        .read_to_string(&mut buf)
        .context("reading manifest contents")?;

    Ok(parse_main_class(&buf))
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
