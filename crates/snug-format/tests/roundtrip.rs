//! Integration roundtrip test for snug-format.

use snug_format::{
    decode, embedded_file, encode, AppMetadata, EmbeddedFile, JvmDiscovery,
    LauncherBehavior, LauncherConfig, SnugEmbedded, SnugPayload, SplashConfig,
    SplashImage, FORMAT_VERSION, MAGIC,
};

fn sample_config() -> LauncherConfig {
    LauncherConfig {
        app: AppMetadata {
            name: "Demo App".into(),
            company: "SynapticLoop".into(),
            version: "0.3.1".into(),
            description: Some("A friendly demo".into()),
            copyright: Some("(c) 2026 Julian".into()),
        },
        main_class: Some("com.example.Main".into()),
        min_java: 25,
        jvm_args: vec![
            "-Xms256m".into(),
            "-Xmx2g".into(),
            "-Dfile.encoding=UTF-8".into(),
        ],
        splash: Some(SplashConfig {
            duration_ms: 1_500,
            // Pretend a 2×2 fully-opaque red splash. Sized down
            // from a real build-time conversion for test brevity.
            image: SplashImage {
                width: 2,
                height: 2,
                bytes: vec![
                    0x00, 0x00, 0xFF, 0xFF, // B=0 G=0 R=255 A=255
                    0x00, 0x00, 0xFF, 0xFF,
                    0x00, 0x00, 0xFF, 0xFF,
                    0x00, 0x00, 0xFF, 0xFF,
                ],
            },
        }),
        behavior: LauncherBehavior {
            forward_args: true,
            cache_dir: None,
            jvm_discovery: JvmDiscovery {
                explicit: None,
                try_java_home: true,
                try_jdk_home: true,
                try_path: true,
                try_registry: true,
                try_common: true,
            },
        },
    }
}

fn sample_payload() -> SnugPayload {
    SnugPayload {
        config: sample_config(),
        jars: vec![embedded_file(b"PK\x03\x04fake-fat-jar".to_vec())],
        icon: Some(EmbeddedFile {
            sha256: [0u8; 32],
            bytes: b"\x00\x00\x01\x00fake-ico".to_vec(),
        }),
    }
}

#[test]
fn encoded_blob_starts_with_magic_and_version() {
    let payload = sample_payload();
    let embedded = SnugEmbedded::new(payload);
    let bytes = encode(&embedded).unwrap();
    assert_eq!(&bytes[..8], MAGIC);
    let version = u16::from_le_bytes(bytes[8..10].try_into().unwrap());
    assert_eq!(version, FORMAT_VERSION);
}

#[test]
fn embedded_round_trips_all_fields() {
    let payload = sample_payload();
    let embedded = SnugEmbedded::new(payload.clone());
    let bytes = encode(&embedded).unwrap();
    let decoded = decode(&bytes).unwrap();
    assert_eq!(decoded.payload, payload);
}

#[test]
fn sha256_digest_matches_bytes() {
    let f = embedded_file(b"hello world".to_vec());
    // sha256("hello world") = b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9
    let expected = [
        0xb9, 0x4d, 0x27, 0xb9, 0x93, 0x4d, 0x3e, 0x08, 0xa5, 0x2e, 0x52, 0xd7, 0xda, 0x7d, 0xab,
        0xfa, 0xc4, 0x84, 0xef, 0xe3, 0x7a, 0x53, 0x80, 0xee, 0x90, 0x88, 0xf7, 0xac, 0xe2, 0xef,
        0xcd, 0xe9,
    ];
    assert_eq!(f.sha256, expected);
}
