//! End-to-end smoke test: build a real Windows `.exe` by calling the
//! CLI builder directly (skipping the binary entry point), then verify
//! the produced file is a valid Windows GUI PE and that the embedded
//! payload is locatable.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use clap::Parser;
use snug_format::{embedded_file, AppMetadata, LauncherBehavior, LauncherConfig};
use snug_launcher::find_in_file;
use zip::write::SimpleFileOptions;

use snug_cli::build::{build_exe, output_path};
use snug_cli::cli::Cli;

#[test]
fn builds_real_windows_exe_and_locator_finds_payload() {
    let tmp = tempdir();
    let jar = tmp.join("Demo.jar");
    write_fake_jar(&jar, Some("com.example.Main"));
    let exe_path = tmp.join("Demo.exe");

    // Construct a minimal Cli via clap's builder so we don't depend on
    // any binary being on PATH.
    let cli = construct_cli(&jar, &exe_path);

    // Build the payload via the same code path the binary uses.
    let payload = build_payload_for_test(&cli);
    let exe = build_exe(&cli, &payload).expect("build_exe");
    assert_eq!(exe, exe_path);
    assert!(exe_path.exists(), "exe must be written");

    // The produced file must be a valid Windows GUI PE.
    let bytes = fs::read(&exe_path).expect("read exe");
    assert_eq!(&bytes[..2], b"MZ", "must start with MZ");
    let pe_offset = u32::from_le_bytes([bytes[0x3C], bytes[0x3C + 1], bytes[0x3C + 2], bytes[0x3C + 3]])
        as usize;
    assert_eq!(&bytes[pe_offset..pe_offset + 4], b"PE\0\0");

    // The locator must find the appended payload despite false-positive
    // SNUGEMBD substrings inside the embedded stub.
    let embedded = find_in_file(&exe_path)
        .expect("scan")
        .expect("payload must be located");
    assert_eq!(embedded.payload.config.app.name, "Demo");
    assert_eq!(embedded.payload.config.app.company, "SynapticLoop");
    assert_eq!(embedded.payload.config.main_class.as_deref(), Some("com.example.Main"));
    assert_eq!(embedded.payload.config.jvm_args, vec!["-Xmx512m".to_string()]);
}

// --- helpers --------------------------------------------------------------

fn build_payload_for_test(cli: &Cli) -> snug_format::SnugPayload {
    let jar_path = cli.jar.as_ref().expect("jar in test cli");
    let jar_bytes = fs::read(jar_path).expect("read jar");
    let app = AppMetadata {
        name: cli.name.clone().unwrap_or_else(|| "Demo".into()),
        company: cli.company.clone().unwrap_or_else(|| "SynapticLoop".into()),
        version: cli.version.clone().unwrap_or_else(|| "0.1.0".into()),
        description: cli.description.clone(),
        copyright: cli.copyright.clone(),
    };
    snug_format::SnugPayload {
        config: LauncherConfig {
            app,
            main_class: cli.main_class.clone(),
            min_java: cli.min_java,
            jvm_args: cli.jvm_args.clone(),
            splash: None,
            behavior: LauncherBehavior::default(),
        },
        jars: vec![embedded_file(jar_bytes)],
        icon: None,
    }
}

fn construct_cli(jar: &std::path::Path, exe: &std::path::Path) -> Cli {
    let args = vec![
        "snug".to_string(),
        jar.display().to_string(),
        "--output".into(),
        exe.display().to_string(),
        "--name".into(),
        "Demo".into(),
        "--company".into(),
        "SynapticLoop".into(),
        "--version".into(),
        "0.1.0".into(),
        "--min-java".into(),
        "25".into(),
        "--main-class".into(),
        "com.example.Main".into(),
        "--jvm-arg=-Xmx512m".into(),
    ];
    Cli::parse_from(args)
}

#[test]
fn output_path_defaults_to_jar_stem_exe() {
    let tmp = tempdir();
    let jar = tmp.join("MyApp.jar");
    write_fake_jar(&jar, None);
    let cli = Cli::parse_from(["snug", &jar.display().to_string()]);
    let p = output_path(&cli);
    assert_eq!(p, tmp.join("MyApp.exe"));
}

fn write_fake_jar(path: &std::path::Path, main_class: Option<&str>) {
    let file = fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default();
    if let Some(mc) = main_class {
        zip.start_file("META-INF/MANIFEST.MF", opts).unwrap();
        let mf = format!("Manifest-Version: 1.0\nMain-Class: {mc}\n");
        zip.write_all(mf.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

fn tempdir() -> PathBuf {
    let base = std::env::temp_dir();
    let unique = format!(
        "snug-e2e-{}-{}",
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
