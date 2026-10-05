//! Locate a snug embedded payload on disk.
//!
//! This is the seam between snug's two output shapes. The payload itself
//! is defined by [`snug_format`] and is identical either way; what differs
//! is *where it lives*:
//!
//! - **Windows.** The payload is stamped into the launcher's own PE
//!   resource directory as an `RT_RCDATA` entry named
//!   [`PAYLOAD_RESOURCE_NAME`], so the launcher reads it out of its
//!   running image. `editpe` is a pure Rust PE parser with no Win32
//!   dependency, so this lookup works on macOS and Linux too — which is
//!   what lets `snug-cli` *build* a Windows EXE from a Mac.
//! - **macOS.** A `.app` is a *directory*, so there is nothing to stamp
//!   into: a Mach-O has no resource directory. The launcher is a plain
//!   byte-copy and the payload sits beside it as
//!   `Contents/Resources/<App>.<PAYLOAD_SUFFIX>`, read by
//!   `snug-launcher`'s `platform::macos::locate_payload`.
//!
//! It is its own crate, depending only on `snug-format` and `editpe`,
//! for one reason: **`snug-cli` needs the lookup and none of the runtime.**
//! It used to depend on the whole `snug-launcher` library to read three
//! things from this module, which dragged the 1600-line JDK download
//! flow, `jni`, `windows-sys` and `ureq` into a CLI that never launches
//! anything. Keeping this module separate lets the CLI depend on the wire
//! format and a PE parser, and nothing else.
//!
//! The two filename constants live here together for a reason: the
//! producer (`snug-cli` stamping the resource or writing the sibling
//! file) and the consumer (the launcher reading it) must agree exactly.
//! Spelling the string out in both places is how a mismatch gets written,
//! and it surfaces only as a bare "payload not found".

use std::path::Path;

use editpe::Image;
use snug_format::{decode, FormatError, SnugEmbedded};

/// The resource entry name that holds the encoded snug payload.
pub const PAYLOAD_RESOURCE_NAME: &str = "SNUGEMBD";

/// File-name suffix of a macOS bundle's payload file: `<App>.snugpayload`.
///
/// Re-exported from `snug-launcher`'s platform layer so both ends agree —
/// see the module docs.
pub const PAYLOAD_SUFFIX: &str = "snugpayload";

/// Re-export the embedded-payload magic from `snug-format` for callers
/// who want it without depending on `snug-format` directly.
pub use snug_format::MAGIC;

/// Scan `path` for a snug payload stored as an `RT_RCDATA` resource
/// entry. Returns `Ok(None)` if no valid payload is found.
///
/// O(1): one PE parse plus one resource-tree walk, regardless of payload
/// size.
pub fn find_in_file(path: &Path) -> Result<Option<SnugEmbedded>, FormatError> {
    let image = Image::parse_file(path).map_err(|e| {
        FormatError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("parse self as PE image: {e}"),
        ))
    })?;

    let dir = match image.resource_directory() {
        Some(d) => d,
        None => return Ok(None),
    };

    let bytes = match dir.get_rcdata(PAYLOAD_RESOURCE_NAME) {
        Ok(Some(b)) => b,
        Ok(None) => return Ok(None),
        Err(e) => {
            return Err(FormatError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("read {PAYLOAD_RESOURCE_NAME} resource: {e}"),
            )))
        }
    };

    decode(&bytes).map(Some)
}

#[cfg(test)]
mod tests {
    use std::fs;
    

    use snug_format::{embedded_file, AppMetadata, LauncherBehavior, LauncherConfig, SnugEmbedded};

    use super::*;

    fn write_payload_to(path: &Path) {
        let payload = snug_format::SnugPayload {
            config: LauncherConfig {
                app: AppMetadata {
                    name: "Stub Test".into(),
                    company: "Test Co".into(),
                    update_check_url: None,
                    version: "0.1.0".into(),
                    description: None,
                    copyright: None,
                },
                main_class: Some("com.example.Main".into()),
                min_java: 25,
                jvm_args: vec![],
                splash: None,
                behavior: LauncherBehavior::default(),
            },
            jars: vec![embedded_file(b"jar-bytes".to_vec())],
            icon: None,
            localizations: Vec::new(),
        };
        let embedded = SnugEmbedded::new(payload);
        let encoded = snug_format::encode(&embedded).unwrap();

        // Read the stub, add the payload as an RT_RCDATA resource via
        // editpe, write it back. This mirrors what `snug-cli` does at
        // build time.
        let stub_bytes = fs::read("../../bin/launcher-stub-windows-x86_64.exe")
            .or_else(|_| fs::read("../bin/launcher-stub-windows-x86_64.exe"))
            .expect("read committed stub");
        let mut image = Image::parse(&stub_bytes).expect("parse stub");
        image
            .set_resource_directory({
                let mut dir = image
                    .resource_directory()
                    .cloned()
                    .unwrap_or_default();
                dir.set_rcdata(PAYLOAD_RESOURCE_NAME, encoded).unwrap();
                dir
            })
            .expect("set resource dir");
        fs::write(path, image.data()).expect("write stubbed EXE");
    }

    #[test]
    fn finds_payload_in_rcdata_resource() {
        let dir = tempdir();
        let path = dir.join("stubbed.exe");
        write_payload_to(&path);

        let found = find_in_file(&path).unwrap();
        let embedded = found.expect("payload should be located");
        assert_eq!(embedded.payload.config.app.name, "Stub Test");
        assert_eq!(embedded.payload.jars[0].bytes, b"jar-bytes");
    }

    #[test]
    fn returns_none_when_no_resource_directory() {
        let dir = tempdir();
        let path = dir.join("empty.exe");
        // 4096 bytes of zeros — not a valid PE, but Image::parse_file
        // will reject it; for this test we want to exercise the "valid
        // PE but no resource directory" path. Use the bare stub.
        let stub = fs::read("../../bin/launcher-stub-windows-x86_64.exe")
            .or_else(|_| fs::read("../bin/launcher-stub-windows-x86_64.exe"))
            .expect("read committed stub");
        fs::write(&path, &stub).expect("write bare stub");
        assert!(find_in_file(&path).unwrap().is_none());
    }

    #[test]
    fn returns_none_when_rcdata_entry_missing() {
        let dir = tempdir();
        let path = dir.join("no-entry.exe");
        let stub = fs::read("../../bin/launcher-stub-windows-x86_64.exe")
            .or_else(|_| fs::read("../bin/launcher-stub-windows-x86_64.exe"))
            .expect("read committed stub");
        fs::write(&path, &stub).expect("write stub");
        // The bare stub has no RCDATA entry.
        assert!(find_in_file(&path).unwrap().is_none());
    }

    fn tempdir() -> std::path::PathBuf {
        let base = std::env::temp_dir();
        // Counter is load-bearing: pid + nanos is not unique under
        // parallel tests on a coarse clock (see the same fix in
        // snug-cli's options_file tests).
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let unique = format!(
            "snug-locator-test-{}-{}-{}",
            std::process::id(),
            n,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = base.join(unique);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
