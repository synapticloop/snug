//! Tests for the no-args / `--snug-version` behaviour.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use clap::Parser;

use snug_cli::cli::Cli;

fn snug_bin() -> std::path::PathBuf {
    // Tests run from the workspace root via `cargo test`.
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_snug"))
}

fn tempdir() -> PathBuf {
    let base = std::env::temp_dir();
    let unique = format!(
        "snug-no-args-{}-{}",
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

#[test]
fn no_args_parses_with_none_jar() {
    let cli = Cli::try_parse_from(["snug"]).expect("try_parse_from succeeds");
    assert!(cli.jar.is_none(), "jar should be None when no positional given");
}

#[test]
fn snug_version_field_uses_action_version() {
    // clap's ArgAction::Version signals "display version and exit" via a
    // special error kind rather than returning Ok. main() handles this by
    // calling error.exit() which prints and exits 0.
    let err = Cli::try_parse_from(["snug", "fake.jar", "--snug-version"])
        .expect_err("--snug-version should signal DisplayVersion");
    assert_eq!(err.kind(), clap::error::ErrorKind::DisplayVersion);
    assert!(err.to_string().contains("snug "), "error renders the version");
}

#[test]
fn no_args_reports_the_missing_input() {
    // Run in an empty directory so the assertion cannot be perturbed by
    // whatever `snug.options` happens to sit in the workspace root —
    // that file is loaded from the cwd, and a real one is a separate
    // case (see `options_file.rs`).
    let tmp = tempdir();
    let output = Command::new(snug_bin())
        .current_dir(&tmp)
        .output()
        .expect("spawn snug");

    assert!(
        !output.status.success(),
        "a missing input is an error, not a success"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no input JAR"),
        "stderr should say the input is missing, got: {stderr}"
    );
    assert!(
        stderr.contains("hint:"),
        "stderr should say how to supply one, got: {stderr}"
    );
    // The whole point of the change: no help dump. Anyone who wants
    // the usage has `--help`, and a screenful of flags here reads as
    // "my options file was ignored" rather than "one value is absent".
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("Wrap a Java fat JAR"),
        "a missing input must not print the help text, got: {stdout}"
    );
}

#[test]
fn help_flag_prints_the_full_help() {
    // `--help` is now the only way to get the usage, so it has to carry
    // the whole flag inventory. It renders the *long* help (the ASCII
    // banner, per-flag docs, examples); the one-line about lives in the
    // short `-h`, which is why the assertions below differ from the
    // help dump this path used to print.
    let output = Command::new(snug_bin())
        .arg("--help")
        .output()
        .expect("spawn snug");
    assert!(output.status.success(), "exit 0 expected, got {:?}", output.status);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(".-----.-----"),
        "stdout should be the long help, got: {stdout}"
    );
    // All major flags from the brief should appear in the help.
    for needle in [
        "--output",
        "--name",
        "--company",
        "--version",
        "--min-java",
        "--main-class",
        "--icon",
        "--manifest",
        "--splash",
        "--jvm-arg",
        "--snug-version",
        "--find-main",
    ] {
        assert!(
            stdout.contains(needle),
            "help should mention {needle}, got: {stdout}"
        );
    }

    // The short help still carries the about text, which the removed
    // no-args path used to be the only way to see.
    let short = Command::new(snug_bin())
        .arg("-h")
        .output()
        .expect("spawn snug");
    assert!(short.status.success(), "exit 0 expected, got {:?}", short.status);
    let short_stdout = String::from_utf8_lossy(&short.stdout);
    assert!(
        short_stdout.contains("Wrap a Java fat JAR"),
        "short help should contain the about text, got: {short_stdout}"
    );
}

#[test]
fn snug_version_flag_prints_version_and_exits() {
    let output = Command::new(snug_bin())
        .arg("--snug-version")
        .output()
        .expect("spawn snug");
    assert!(output.status.success(), "exit 0 expected, got {:?}", output.status);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("snug "),
        "stdout should contain 'snug <version>', got: {stdout}"
    );
    // Should NOT print the full help text — just the version.
    assert!(
        !stdout.contains("Wrap a Java fat JAR"),
        "--snug-version should not print the full help, got: {stdout}"
    );
}
