//! Integration test that takes the real precompiled `launcher-stub.exe`,
//! embeds a snug payload as an `RT_RCDATA` resource entry (slice 4 /
//! v2 storage), and verifies the locator finds the payload despite
//! `SNUGEMBD` substrings already living inside the stub binary.

use std::fs;
use std::path::PathBuf;

use editpe::Image;
use snug_format::{
    embedded_file, encode, AppMetadata, LauncherBehavior, LauncherConfig, SnugEmbedded, SnugPayload,
};
use snug_launcher::find_in_file;
use snug_launcher::payload_locator::PAYLOAD_RESOURCE_NAME;

const STUB_PATH: &str = "../../bin/launcher-stub.exe";

#[test]
fn finds_payload_as_rcdata_resource_on_real_stub() {
    let stub_path = canonicalise(STUB_PATH);
    if !stub_path.exists() {
        eprintln!(
            "skipping: {} not found (build with `cargo build --release -p snug-launcher`)",
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
                update_check_url: None,
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
        jars: vec![embedded_file(b"fake jar body".to_vec())],
        icon: None,
    };
    let embedded = SnugEmbedded::new(payload);
    let encoded = encode(&embedded).unwrap();

    // Parse the stub, embed the payload as RT_RCDATA "SNUGEMBD", write.
    let stub_bytes = fs::read(&stub_path).expect("read stub");
    let mut image = Image::parse(&stub_bytes).expect("parse stub as PE");
    let mut resources = image
        .resource_directory()
        .cloned()
        .unwrap_or_default();
    resources
        .set_rcdata(PAYLOAD_RESOURCE_NAME, encoded.clone())
        .expect("set_rcdata");
    image
        .set_resource_directory(resources)
        .expect("set_resource_directory");

    let combined_path = tempdir().join("stubbed-app.exe");
    fs::write(&combined_path, image.data()).expect("write combined");

    // The locator must read the payload back out of the resource
    // directory. The stub itself contains false-positive `SNUGEMBD`
    // substrings in its data section — these are no longer relevant in
    // v2 (the locator doesn't scan for them) but the test still
    // exercises the full file round-trip.
    let found = find_in_file(&combined_path).expect("scan");
    let found = found.expect("payload must be located");
    assert_eq!(found.payload.config.app.name, "End-to-end");
    assert_eq!(found.payload.jars[0].bytes, b"fake jar body");
    assert_eq!(
        found.payload.config.jvm_args,
        vec!["-Xmx512m".to_string()]
    );
}

#[test]
fn bare_stub_returns_none() {
    let stub_path = canonicalise(STUB_PATH);
    if !stub_path.exists() {
        eprintln!(
            "skipping: {} not found",
            stub_path.display()
        );
        return;
    }

    // The bare stub has no resource directory entry, so the locator
    // should return None — this is the "no payload appended yet" path
    // that triggers the friendly stub error at runtime.
    let found = find_in_file(&stub_path).expect("scan");
    assert!(found.is_none(), "bare stub should report no payload");
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
