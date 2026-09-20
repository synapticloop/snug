//! Read the `Main-Class` attribute from a JAR's manifest on disk.
//!
//! At runtime the launcher works against the extracted JAR in the
//! per-user cache, so we don't have access to the embedded bytes via
//! `include_bytes!()` — we read directly from the cached file.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::LauncherError;

/// Locate `META-INF/MANIFEST.MF` inside the JAR at `jar_path` and
/// extract its `Main-Class` attribute.
pub fn read_main_class(jar_path: &Path) -> Result<Option<String>, LauncherError> {
    let file = File::open(jar_path)?;
    let mut archive = match zip::ZipArchive::new(file) {
        Ok(a) => a,
        Err(e) => {
            // Treat an unparseable JAR as "no Main-Class" rather than
            // fatal — the explicit `--main-class` override may still
            // save us.
            let _ = e;
            return Ok(None);
        }
    };

    let mut manifest = match archive.by_name("META-INF/MANIFEST.MF") {
        Ok(m) => m,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    let mut buf = String::new();
    manifest.read_to_string(&mut buf)?;
    Ok(parse_main_class(&buf))
}

fn parse_main_class(manifest: &str) -> Option<String> {
    for line in manifest.lines() {
        if let Some((key, value)) = line.split_once(':') {
            if key.trim() == "Main-Class" {
                let v = value.trim();
                if !v.is_empty() {
                    return Some(v.to_owned());
                }
            }
        }
    }
    None
}

/// Read `Main-Class`, failing if it cannot be located.
pub fn read_main_class_required(jar_path: &Path) -> Result<String, LauncherError> {
    match read_main_class(jar_path)? {
        Some(cls) => Ok(cls),
        None => Err(LauncherError::NoMainClass),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn write_jar(path: &Path, main_class: Option<&str>) {
        let file = File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = SimpleFileOptions::default();
        if let Some(mc) = main_class {
            zip.start_file("META-INF/MANIFEST.MF", opts).unwrap();
            let mf = format!("Manifest-Version: 1.0\nMain-Class: {mc}\n");
            zip.write_all(mf.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn reads_main_class_from_jar() {
        let dir = std::env::temp_dir().join(format!(
            "snug-runtime-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let jar = dir.join("demo.jar");
        write_jar(&jar, Some("com.example.Main"));

        assert_eq!(
            read_main_class(&jar).unwrap().as_deref(),
            Some("com.example.Main")
        );
    }

    #[test]
    fn returns_none_when_no_manifest() {
        let dir = std::env::temp_dir().join(format!(
            "snug-runtime-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let jar = dir.join("norc.jar");
        write_jar(&jar, None);
        assert_eq!(read_main_class(&jar).unwrap(), None);
    }
}
