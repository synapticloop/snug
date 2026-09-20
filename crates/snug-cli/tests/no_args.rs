//! Tests for the no-args / `--snug-version` behaviour.

use std::process::Command;

use clap::Parser;

use snug_cli::cli::Cli;

fn snug_bin() -> std::path::PathBuf {
    // Tests run from the workspace root via `cargo test`.
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_snug"))
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
fn snug_no_args_prints_help_with_version() {
    let output = Command::new(snug_bin())
        .output()
        .expect("spawn snug");
    assert!(output.status.success(), "exit 0 expected, got {:?}", output.status);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Wrap a Java fat JAR"),
        "stdout should contain the about text, got: {stdout}"
    );
    assert!(
        stdout.contains("snug "),
        "stdout should contain the 'snug <version>' line, got: {stdout}"
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
        "--splash",
        "--jvm-arg",
        "--rcedit",
        "--snug-version",
    ] {
        assert!(
            stdout.contains(needle),
            "help should mention {needle}, got: {stdout}"
        );
    }
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
