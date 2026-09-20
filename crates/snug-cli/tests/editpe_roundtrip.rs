//! Roundtrip integration test for `editpe`-based resource stamping.
//!
//! Builds a real Windows `.exe` by concatenating the precompiled launcher
//! stub with an encoded snug payload, stamps icon + version strings +
//! application manifest on it via [`editpe`], and then re-parses the
//! resulting file and asserts the resources are present and correct.
//!
//! Crucially this test is **not** `#[cfg(windows)]` — `editpe` is a
//! pure-Rust cross-platform PE parser, which means the Mac/Linux dev loop
//! can now roundtrip a real stamped EXE without Wine.

use std::fs;
use std::path::{Path, PathBuf};

use editpe::Image;
use image::{Rgba, RgbaImage};

use snug_cli::build::build_exe;
use snug_cli::cli::Cli;
use snug_cli::resources::ResourcePlan;
use snug_format::{embedded_file, AppMetadata, LauncherBehavior, LauncherConfig};

#[test]
fn stamp_icon_version_and_manifest_then_roundtrip() {
    let tmp = tempdir();
    let jar = tmp.join("Demo.jar");
    write_fake_jar(&jar);
    let exe_path = tmp.join("Demo.exe");

    // Materialise a 32×32 solid teal PNG icon and a small Win manifest.
    let icon_path = tmp.join("teal.png");
    write_solid_png(&icon_path, 32, 32, [32, 160, 160, 255]);
    let manifest_path = tmp.join("app.manifest.xml");
    write_manifest(&manifest_path);

    // Build the EXE through the production code path (stub + payload).
    let cli = construct_cli(&jar, &exe_path, "Demo", &icon_path, &manifest_path);
    let payload = build_payload_for_test(&cli);
    let exe = build_exe(&cli, &payload).expect("build_exe");
    assert_eq!(exe, exe_path);

    // Stamp additional resources on top via editpe.
    let plan = ResourcePlan {
        icon: Some(icon_path.clone()),
        manifest: Some(manifest_path.clone()),
    };
    let embedded = snug_format::SnugEmbedded::new(payload.clone());
    plan.run(&exe, &embedded).expect("ResourcePlan::run");

    // Re-parse and assert every stamped resource is present and correct.
    let image = Image::parse_file(&exe).expect("parse_file after stamp");
    let resources = image.resource_directory().expect("resources present");

    // --- icon ------------------------------------------------------------
    // `get_main_icon` returns the highest-resolution icon as raw bytes.
    // editpe's `images` feature stores the source image verbatim — for
    // our PNG source, that's the PNG magic (`89 50 4E 47`).
    let icon_bytes = resources
        .get_main_icon()
        .expect("read main icon")
        .expect("icon should be present after stamp");
    assert!(icon_bytes.len() >= 4, "icon bytes should be non-trivial");
    assert_eq!(
        &icon_bytes[..4],
        &[0x89, 0x50, 0x4E, 0x47],
        "icon should start with the PNG magic (we passed PNG and editpe stores the source format verbatim)"
    );

    // --- version info ----------------------------------------------------
    let version_info = resources
        .get_version_info()
        .expect("read version info")
        .expect("version info should be present after stamp");
    // First (and only) string table — fixed 040904B0 key.
    assert_eq!(version_info.strings.len(), 1);
    let table = &version_info.strings[0];
    assert_eq!(table.key, "040904B0");
    assert_eq!(
        table.strings.get("ProductName").map(String::as_str),
        Some("Demo")
    );
    assert_eq!(
        table.strings.get("CompanyName").map(String::as_str),
        Some("SynapticLoop")
    );
    assert_eq!(
        table.strings.get("FileDescription").map(String::as_str),
        Some("Demo description")
    );
    assert_eq!(
        table.strings.get("LegalCopyright").map(String::as_str),
        Some("© 2026 SynapticLoop")
    );

    // Fixed-file info: signature must be valid, file type must be VFT_APP.
    assert_eq!(version_info.info.signature, 0xFEEF_04BD);
    assert_eq!(version_info.info.file_type, 0x1);

    // --- manifest --------------------------------------------------------
    // `get_manifest` is not directly exposed; read back the raw RT_MANIFEST
    // entry and confirm our text made it in verbatim.
    let manifest_xml = resources
        .get_manifest()
        .expect("manifest resource present")
        .expect("manifest should be valid");
    assert!(
        manifest_xml.contains("requestedExecutionLevel"),
        "manifest should roundtrip verbatim: {manifest_xml}"
    );
    assert!(
        manifest_xml.contains("snug-test-app"),
        "manifest identity should be preserved: {manifest_xml}"
    );

    // --- subsystem --------------------------------------------------------
    // The launcher's GUI subsystem should be preserved (or set) after stamp.
    assert_eq!(image.subsystem(), editpe::constants::IMAGE_SUBSYSTEM_WINDOWS_GUI as u16);
}

#[test]
fn stamp_runs_without_icon_or_manifest_too() {
    let tmp = tempdir();
    let jar = tmp.join("Bare.jar");
    write_fake_jar(&jar);
    let exe_path = tmp.join("Bare.exe");

    let cli = construct_cli(&jar, &exe_path, "Bare", &PathBuf::new(), &PathBuf::new());
    // Sanity: --icon and --manifest weren't set, so the plan has neither.
    assert!(cli.icon.is_none());
    assert!(cli.manifest.is_none());

    let payload = build_payload_for_test(&cli);
    let exe = build_exe(&cli, &payload).expect("build_exe");

    let plan = ResourcePlan::default();
    assert!(plan.should_run(), "version-info stamping is always on");
    let embedded = snug_format::SnugEmbedded::new(payload.clone());
    plan.run(&exe, &embedded).expect("ResourcePlan::run without icon/manifest");

    let image = Image::parse_file(&exe).expect("parse_file");
    let resources = image.resource_directory().expect("resources present");
    let version_info = resources
        .get_version_info()
        .expect("read version info")
        .expect("version info always stamped");
    assert_eq!(
        version_info.strings[0]
            .strings
            .get("ProductName")
            .map(String::as_str),
        Some("Bare")
    );
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
        jar: embedded_file(jar_bytes),
        icon: None,
    }
}

fn construct_cli(
    jar: &Path,
    exe: &Path,
    name: &str,
    icon: &Path,
    manifest: &Path,
) -> Cli {
    use clap::Parser;
    let mut args = vec![
        "snug".to_string(),
        jar.display().to_string(),
        "--output".into(),
        exe.display().to_string(),
        "--name".into(),
        name.to_string(),
        "--company".into(),
        "SynapticLoop".into(),
        "--version".into(),
        "1.2.3.4".into(),
        "--description".into(),
        "Demo description".into(),
        "--copyright".into(),
        "© 2026 SynapticLoop".into(),
        "--min-java".into(),
        "25".into(),
        "--main-class".into(),
        "com.example.Main".into(),
    ];
    if !icon.as_os_str().is_empty() {
        args.push("--icon".into());
        args.push(icon.display().to_string());
    }
    if !manifest.as_os_str().is_empty() {
        args.push("--manifest".into());
        args.push(manifest.display().to_string());
    }
    Cli::parse_from(args)
}

fn write_solid_png(path: &Path, w: u32, h: u32, rgba: [u8; 4]) {
    let mut img = RgbaImage::new(w, h);
    for px in img.pixels_mut() {
        *px = Rgba(rgba);
    }
    img.save(path).expect("save PNG");
}

fn write_manifest(path: &Path) {
    let xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity version="1.2.3.4" name="snug-test-app" processorArchitecture="amd64" type="win32" />
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v2">
    <security>
      <requestedPrivileges xmlns="urn:schemas-microsoft-com:asm.v3">
        <requestedExecutionLevel level="asInvoker" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}" />
    </application>
  </compatibility>
</assembly>
"#;
    fs::write(path, xml).expect("write manifest");
}

fn write_fake_jar(path: &Path) {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    let file = fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default();
    zip.start_file("META-INF/MANIFEST.MF", opts).unwrap();
    zip.write_all(b"Manifest-Version: 1.0\nMain-Class: com.example.Main\n")
        .unwrap();
    zip.finish().unwrap();
}

fn tempdir() -> PathBuf {
    let base = std::env::temp_dir();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = base.join(format!("snug-test-{pid}-{nanos}"));
    fs::create_dir_all(&dir).expect("create tempdir");
    dir
}
