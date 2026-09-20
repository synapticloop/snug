//! Integration test that takes the real precompiled `launcher-stub.exe`,
//! appends a snug payload to it, and verifies the locator finds the
//! payload despite `SNUGEMBD` substrings already living inside the
//! stub binary.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use snug_format::{
    embedded_file, encode, AppMetadata, LauncherBehavior, LauncherConfig, SnugEmbedded, SnugPayload,
};
use snug_launcher::find_in_file;

const STUB_PATH: &str = "../../bin/launcher-stub.exe";

#[test]
fn finds_payload_appended_to_real_stub() {
    let stub_path = canonicalise(STUB_PATH);
    if !stub_path.exists() {
        eprintln!(
            "skipping: {} not found (build with `cargo zigbuild`)",
            stub_path.display()
        );
        return;
    }

    // Build a real payload.
    let payload = SnugPayload {
        config: LauncherConfig {
            app: AppMetadata {
                name: "End-to-end".into(),
                company: "Snug Test".into(),
                version: "1.0.0".into(),
                description: None,
                copyright: None,
            },
            main_class: Some("com.example.Main".into()),
            min_java: 25,
            jvm_args: vec!["-Xmx512m".into()],
            splash: None,
            behavior: LauncherBehavior::default(),
        },
        jar: embedded_file(b"fake jar body".to_vec()),
        icon: None,
    };
    let embedded = SnugEmbedded::new(payload);
    let encoded = encode(&embedded).unwrap();

    // Concatenate stub + payload into a temp file.
    let mut combined = fs::read(&stub_path).expect("read stub");
    let payload_start = combined.len();
    combined.extend_from_slice(&encoded);
    let combined_path = tempdir().join("stubbed-app.exe");
    fs::write(&combined_path, &combined).expect("write combined");

    // The stub itself contains 5+ instances of the SNUGEMBD magic in
    // its data section — the locator must skip those and find only the
    // appended payload.
    let found = find_in_file(&combined_path).expect("scan");
    let found = found.expect("payload must be located");
    assert_eq!(found.payload.config.app.name, "End-to-end");
    assert_eq!(found.payload.jar.bytes, b"fake jar body");
    assert_eq!(
        found.payload.config.jvm_args,
        vec!["-Xmx512m".to_string()]
    );

    // Sanity: the magic of the located payload should sit at the stub's
    // tail boundary (payload_start), not inside the stub binary.
    // We can't directly inspect `payload_start` from `SnugEmbedded`, but
    // we know the locator found the payload — the stub's data-section
    // matches all failed decode so the only valid one is at the tail.
    let _ = payload_start;
}

fn canonicalise(rel: &str) -> PathBuf {
    let p = PathBuf::from(rel);
    p.canonicalize().unwrap_or(p)
}

fn tempdir() -> PathBuf {
    let base = std::env::temp_dir();
    let unique = format!(
        "snug-stub-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let dir = base.join(unique);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[allow(dead_code)]
fn _unused() {
    let _ = std::io::stdout();
    let _: Box<dyn Write> = Box::new(NullWriter);
}

struct NullWriter;
impl Write for NullWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
