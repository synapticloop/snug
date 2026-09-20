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
    let tmp = tempdir();
    // Build a real manifest inside a META-INF/ directory.
    let mut mf_dir = tmp.clone();
    mf_dir.push("META-INF");
    fs::create_dir_all(&mf_dir).unwrap();
    fs::write(
        mf_dir.join("MANIFEST.MF"),
        "Manifest-Version: 1.0\nMain-Class: com.example.Main\n",
    )
    .unwrap();

    let status = Command::new("zip")
        .arg("-q")
        .arg(path)
        .arg(mf_dir.join("MANIFEST.MF"))
        .status()
        .expect("spawn zip — install via `brew install zip` or apt");
    assert!(status.success(), "zip failed: {status:?}");
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
        .arg("--no-rcedit")
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
        .arg("--no-rcedit")
        .arg("--dry-run")
        .arg("--min-java")
        .arg("17")
        .output()
        .expect("spawn snug");

    assert!(output.status.success());
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
        .arg("--no-rcedit")
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
    assert!(
        stderr.contains("No such file or directory"),
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
    assert_eq!(options_file::resolve(&args, &tmp), None);
}

#[test]
fn resolve_finds_default_in_cwd() {
    let tmp = tempdir();
    fs::write(tmp.join("snug.options"), "--name X\n").unwrap();
    let args: Vec<String> = ["snug", "app.jar"].iter().map(|s| s.to_string()).collect();
    assert_eq!(options_file::resolve(&args, &tmp), Some(tmp.join("snug.options")));
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
