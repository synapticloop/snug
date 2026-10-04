//! End-to-end tests for the `--init-*` modes: `snug --init-options`
//! and `snug --init-localizations`.
//!
//! These drive the real binary rather than the library functions,
//! because the interesting part of `--init-localizations` is *where*
//! it writes: the directory snug was invoked from, not the directory
//! the binary lives in. A unit test can't observe that without
//! mutating process-global CWD state, so each test sets the child's
//! working directory to a fresh temp dir and inspects the result.
//!
//! Nothing here needs a JAR, a JDK, or a network.

use std::path::{Path, PathBuf};
use std::process::Command;

fn snug_bin() -> PathBuf {
    // Tests run from the workspace root via `cargo test`.
    PathBuf::from(env!("CARGO_BIN_EXE_snug"))
}

fn tmpdir(label: &str) -> PathBuf {
    let unique = format!(
        "snug-init-e2e-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let dir = std::env::temp_dir().join(unique);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run `snug` with the given args from `cwd`.
fn run_in(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new(snug_bin())
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn snug")
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn init_localizations_creates_directory_in_cwd() {
    let dir = tmpdir("cwd");
    let out = run_in(&dir, &["--init-localizations"]);
    assert!(
        out.status.success(),
        "exit 0 expected, got {:?}\n{}",
        out.status,
        stderr_of(&out)
    );

    // The whole point of the default: relative to the invocation
    // directory. `snug/snug.exe --init-localizations` run from a
    // project root must land in that project root, not next to the
    // executable.
    let bundle = dir.join("localisations").join("snug-localisations.en.txt");
    assert!(
        bundle.is_file(),
        "expected {} to exist, dir contents: {:?}",
        bundle.display(),
        std::fs::read_dir(dir.join("localisations"))
            .map(|d| d
                .filter_map(|e| e.ok())
                .map(|e| e.file_name())
                .collect::<Vec<_>>())
            .unwrap_or_default()
    );
    let text = std::fs::read_to_string(&bundle).unwrap();
    // Full key inventory, not a stub.
    for key in [
        "err.jvm_not_found =",
        "err.no_main_class =",
        "launcher.bare_stub.hint =",
        "launcher.error.content =",
    ] {
        assert!(text.contains(key), "baseline should contain `{key}`");
    }
}

#[test]
fn init_localizations_scaffolds_requested_tags() {
    let dir = tmpdir("tags");
    let out = run_in(
        &dir,
        &[
            "--init-localizations",
            "--init-localizations-tag",
            "de",
            "--init-localizations-tag",
            "pt-BR",
        ],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));

    let loc = dir.join("localisations");
    let mut names: Vec<String> = std::fs::read_dir(&loc)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "snug-localisations.de.txt",
            "snug-localisations.en.txt",
            "snug-localisations.pt-BR.txt",
        ],
        "every file must match the canonical pattern so `--localization <dir>` accepts it"
    );

    let de = std::fs::read_to_string(loc.join("snug-localisations.de.txt")).unwrap();
    assert!(de.contains("snug-localisations.de.txt"));
    // Scaffolding, not a finished translation: still the English
    // values, but flagged for the translator.
    assert!(de.contains("err.zip = ZIP error: {0}"));
    assert!(
        de.to_lowercase()
            .contains("still the built-in english baseline")
    );
}

#[test]
fn init_localizations_honours_custom_directory() {
    let dir = tmpdir("custom");
    let out = run_in(&dir, &["--init-localizations", "i18n/translations"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        dir.join("i18n/translations/snug-localisations.en.txt")
            .is_file()
    );
}

#[test]
fn init_localizations_refuses_to_overwrite_without_force() {
    let dir = tmpdir("force");
    assert!(run_in(&dir, &["--init-localizations"]).status.success());

    let out = run_in(&dir, &["--init-localizations"]);
    assert!(!out.status.success(), "second run should fail");
    let err = stderr_of(&out);
    assert!(
        err.contains("refusing to overwrite") && err.contains("--init-localizations-force"),
        "error should name the escape hatch, got: {err}"
    );

    let out = run_in(
        &dir,
        &["--init-localizations", "--init-localizations-force"],
    );
    assert!(
        out.status.success(),
        "force should succeed: {}",
        stderr_of(&out)
    );
}

#[test]
fn init_localizations_rejects_a_traversal_tag() {
    let dir = tmpdir("traversal");
    let out = run_in(
        &dir,
        &[
            "--init-localizations",
            "--init-localizations-tag",
            "../escape",
        ],
    );
    assert!(!out.status.success(), "traversal tag should fail");
    assert!(stderr_of(&out).contains("invalid locale tag"));
    // Nothing created — the rejection happens before any filesystem
    // work, so there's no half-built directory to clean up.
    assert!(!dir.join("localisations").exists());
    assert!(
        !std::env::temp_dir()
            .join("snug-localisations.escape.txt")
            .exists()
    );
}

#[test]
fn init_localizations_stdout_writes_nothing_to_disk() {
    let dir = tmpdir("stdout");
    let out = run_in(
        &dir,
        &["--init-localizations", "--init-localizations-stdout"],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        !dir.join("localisations").exists(),
        "stdout mode must not create the directory"
    );
    let stdout = stdout_of(&out);
    assert!(stdout.contains("err.jvm_not_found ="));
    assert!(stdout.contains("snug-localisations.en.txt"));
}

#[test]
fn both_init_modes_run_in_one_call() {
    let dir = tmpdir("both");
    let out = run_in(&dir, &["--init-options", "--init-localizations"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(dir.join("snug.options").is_file());
    assert!(
        dir.join("localisations")
            .join("snug-localisations.en.txt")
            .is_file()
    );
}

#[test]
fn init_localizations_tag_without_mode_fails_loudly() {
    // A build must never silently swallow a locale the user asked to
    // scaffold.
    let dir = tmpdir("stray-tag");
    let out = run_in(&dir, &["--init-localizations-tag", "de"]);
    assert!(!out.status.success(), "stray tag should fail");
    let err = stderr_of(&out);
    assert!(
        err.contains("--init-localizations-tag has no effect without --init-localizations"),
        "error should explain the missing mode flag, got: {err}"
    );
    assert!(!dir.join("localisations").exists());
}

#[test]
fn init_localizations_rejects_other_build_flags() {
    // The init branch is strict: it must not silently swallow a
    // mis-typed build command.
    let dir = tmpdir("strict");
    let out = run_in(&dir, &["--init-localizations", "--name", "My App"]);
    assert!(!out.status.success(), "mixed modes should fail");
    assert!(stderr_of(&out).contains("--name"));
}

#[test]
fn init_localizations_directory_is_a_usable_localization_input() {
    // End of the intended workflow: scaffold, then point a build at
    // the directory. A README or stray file would fail this.
    let dir = tmpdir("usable");
    let out = run_in(
        &dir,
        &["--init-localizations", "--init-localizations-tag", "de"],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));

    let bundles = snug_cli::localization::collect(&[dir.join("localisations")])
        .expect("scaffolded directory is a valid --localization input");
    snug_cli::localization::ensure_unique_tags(&bundles).expect("tags are unique");
    let tags: Vec<&str> = bundles.iter().map(|b| b.tag.as_str()).collect();
    assert_eq!(tags, vec!["en", "de"]);
}

#[test]
fn init_modes_are_documented_in_help() {
    // `--help` is explicit: a bare `snug` now reports the missing input
    // instead of printing usage.
    let out = Command::new(snug_bin())
        .arg("--help")
        .output()
        .expect("spawn snug");
    let stdout = stdout_of(&out);
    for needle in [
        "--init-options",
        "--init-options-force",
        "--init-options-stdout",
        "--init-localizations",
        "--init-localizations-tag",
        "--init-localizations-force",
        "--init-localizations-stdout",
    ] {
        assert!(
            stdout.contains(needle),
            "help should mention {needle}, got: {stdout}"
        );
    }
}
