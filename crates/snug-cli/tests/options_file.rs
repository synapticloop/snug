//! End-to-end smoke tests for the `snug.options` file feature.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use snug_cli::options_file;

fn snug_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_snug"))
}

fn tempdir() -> PathBuf {
    let base = std::env::temp_dir();
    let unique = format!(
        "snug-options-e2e-{}-{}",
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

fn write_jar(path: &PathBuf) {
    // Build the JAR with the Rust `zip` crate so the test is portable
    // across hosts (previously this helper spawned the system `zip`
    // binary, which only exists on macOS / Linux).
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::CompressionMethod;

    let file = fs::File::create(path).expect("create jar");
    let mut zip = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    zip.start_file("META-INF/MANIFEST.MF", opts)
        .expect("start_file");
    zip.write_all(b"Manifest-Version: 1.0\nMain-Class: com.example.Main\n")
        .expect("write manifest");
    zip.finish().expect("finish zip");
}

#[test]
fn default_snug_options_in_cwd_is_loaded() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar(&jar);
    let opts = tmp.join("snug.options");
    fs::write(
        &opts,
        "--name \"File Default\"\n--company \"File Co\"\n--min-java 21\n",
    )
    .unwrap();

    let output = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--dry-run")
        .output()
        .expect("spawn snug");

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stderr.contains("loaded options from"),
        "stderr should mention loading the file: {stderr}"
    );
    assert!(
        stdout.contains("min-java:    21"),
        "stdout should reflect the file's --min-java: {stdout}"
    );
}

#[test]
fn cli_options_override_file_options() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar(&jar);
    let opts = tmp.join("snug.options");
    fs::write(
        &opts,
        "--name \"File Default\"\n--company \"File Co\"\n--min-java 21\n",
    )
    .unwrap();

    let output = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--dry-run")
        .arg("--min-java")
        .arg("17")
        .output()
        .expect("spawn snug");

    assert!(
        output.status.success(),
        "snug should exit 0 (code={:?}); stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("min-java:    17"),
        "CLI --min-java 17 should override file's 21: {stdout}"
    );
    assert!(
        !stdout.contains("min-java:    21"),
        "file's --min-java 21 should not appear when CLI overrides: {stdout}"
    );
}

#[test]
fn explicit_options_flag_overrides_cwd_default() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar(&jar);

    // CWD default with one set of values.
    fs::write(
        tmp.join("snug.options"),
        "--min-java 21\n--name \"From CWD\"\n",
    )
    .unwrap();

    // Explicit file with different values.
    let custom = tmp.join("custom.opts");
    fs::write(&custom, "--min-java 30\n--name \"From Custom\"\n").unwrap();

    let output = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--dry-run")
        .arg("--options")
        .arg(&custom)
        .output()
        .expect("spawn snug");

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("loaded options from"),
        "stderr should mention loading: {stderr}"
    );
    assert!(stderr.contains("custom.opts"));
    assert!(!stderr.contains("snug.options\n") || stderr.contains("custom.opts"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("min-java:    30"),
        "explicit --options should win over CWD default: {stdout}"
    );
}

#[test]
fn missing_explicit_options_file_errors() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar(&jar);

    let output = Command::new(snug_bin())
        .arg(&jar)
        .arg("--options")
        .arg("/this/path/does/not/exist.opts")
        .output()
        .expect("spawn snug");

    assert!(!output.status.success(), "should exit non-zero");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The exact OS error wording differs across platforms ("No such file or
    // directory" on Unix, "The system cannot find the path specified." on
    // Windows). The path we asked for and the snug-specific prefix are
    // stable, so assert against those.
    assert!(
        stderr.contains("loading options file /this/path/does/not/exist.opts"),
        "stderr should explain the missing file: {stderr}"
    );
}

#[test]
fn options_file_load_skips_comments() {
    let tmp = tempdir();
    let opts = tmp.join("x.opts");
    fs::write(
        &opts,
        "# this is a comment\n\
         --name \"OK\"\n\
         # another comment\n\
         --company \"Co\"\n",
    )
    .unwrap();
    let tokens = options_file::load(&opts).unwrap();
    assert_eq!(
        tokens,
        vec![
            "--name".to_string(),
            "OK".to_string(),
            "--company".to_string(),
            "Co".to_string()
        ]
    );
}

#[test]
fn resolve_returns_none_when_no_default_and_no_explicit() {
    let tmp = tempdir();
    let args: Vec<String> = ["snug", "app.jar"].iter().map(|s| s.to_string()).collect();
    assert_eq!(options_file::resolve(&args, &tmp, None), None);
}

#[test]
fn resolve_finds_default_in_cwd() {
    let tmp = tempdir();
    fs::write(tmp.join("snug.options"), "--name X\n").unwrap();
    let args: Vec<String> = ["snug", "app.jar"].iter().map(|s| s.to_string()).collect();
    assert_eq!(
        options_file::resolve(&args, &tmp, None),
        Some(tmp.join("snug.options"))
    );
}

#[test]
fn resolve_prefers_exe_dir_over_cwd() {
    let exe_dir = tempdir();
    let cwd = tempdir();
    fs::write(exe_dir.join("snug.options"), "--name ExeDir\n").unwrap();
    fs::write(cwd.join("snug.options"), "--name Cwd\n").unwrap();
    let args: Vec<String> = ["snug", "app.jar"].iter().map(|s| s.to_string()).collect();
    assert_eq!(
        options_file::resolve(&args, &cwd, Some(&exe_dir)),
        Some(exe_dir.join("snug.options"))
    );
}

#[test]
fn options_file_next_to_executable_is_loaded_from_any_cwd() {
    // Simulates the portable-distro layout: `snug.exe` and
    // `snug.options` shipped together, invoked from an unrelated
    // working directory.
    let tool_dir = tempdir();
    let cwd = tempdir();
    let local_snug = tool_dir.join(snug_bin().file_name().unwrap());
    fs::copy(snug_bin(), &local_snug).expect("copy snug binary next to its options file");
    fs::write(
        tool_dir.join("snug.options"),
        "--min-java 21\n--name \"From Exe Dir\"\n",
    )
    .unwrap();

    // A CWD default that must lose to the exe-dir one.
    fs::write(cwd.join("snug.options"), "--min-java 17\n--name \"From CWD\"\n").unwrap();

    let jar = cwd.join("demo.jar");
    write_jar(&jar);

    let output = Command::new(&local_snug)
        .current_dir(&cwd)
        .arg(&jar)
        .arg("--dry-run")
        .output()
        .expect("spawn local snug");

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stderr.contains("loaded options from"),
        "stderr should mention loading the file: {stderr}"
    );
    assert!(
        stderr.contains(&tool_dir.join("snug.options").display().to_string()),
        "the exe-dir options file should win over the CWD one: {stderr}"
    );
    assert!(
        stdout.contains("min-java:    21"),
        "exe-dir --min-java 21 should win over CWD's 17: {stdout}"
    );
}

#[test]
fn cwd_options_file_still_loaded_when_exe_dir_has_none() {
    // `CARGO_BIN_EXE_snug` lives in `target/<profile>/`, which normally
    // has no `snug.options` — so the CWD default must still win.
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar(&jar);
    fs::write(
        tmp.join("snug.options"),
        "--min-java 21\n--name \"From CWD\"\n",
    )
    .unwrap();

    let output = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--dry-run")
        .output()
        .expect("spawn snug");

    assert!(
        output.status.success(),
        "snug should exit 0 (code={:?}); stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("min-java:    21"),
        "CWD options file should still be used: {stdout}"
    );
}

#[test]
fn merge_dedups_overridden_flags_but_keeps_repeatable() {
    let args = vec![
        "snug".to_string(),
        "app.jar".to_string(),
        "--name".to_string(),
        "CLI".to_string(),
        "--jvm-arg=-Xmx2g".to_string(),
    ];
    let file = vec![
        "--name=File".to_string(),
        "--company=Co".to_string(),
        "--jvm-arg=-Xms256m".to_string(),
    ];
    let merged = options_file::merge(&args, file);
    // --name appears once (CLI version only).
    let name_count = merged.iter().filter(|s| s.starts_with("--name")).count();
    assert_eq!(name_count, 1, "--name should be deduped: {merged:?}");
    // The CLI used space-separated --name CLI; the file used --name=File.
    // The CLI version wins, and the file's --name=File is dropped.
    assert!(
        merged.windows(2).any(|w| w == ["--name".to_string(), "CLI".to_string()]),
        "CLI's --name CLI should be present: {merged:?}"
    );
    assert!(!merged.contains(&"--name=File".to_string()));
    // --company appears once (file only, CLI didn't override).
    assert!(merged.contains(&"--company=Co".to_string()));
    // --jvm-arg appears twice (repeatable flag — both kept).
    let jvm_count = merged.iter().filter(|s| s.starts_with("--jvm-arg")).count();
    assert_eq!(jvm_count, 2, "--jvm-arg should be kept from both: {merged:?}");
}
