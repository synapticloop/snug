//! End-to-end coverage for the build path, driving a real `snug.exe`.
//!
//! The dialogs and the marquee are deliberately not exercised here — they
//! are modal Win32 surfaces that would block a test harness forever.
//! Everything that can decide, spawn, or format is covered instead: this
//! file runs the same [`build::invocation`] the dropper builds at runtime,
//! through the same [`build::run`], and asserts on the artefact that
//! actually lands on disk.
//!
//! `snug.exe` is located in `target/<profile>/` rather than linked, so
//! the test skips when it hasn't been built. `cargo test --workspace`
//! (the command AGENTS.md documents) builds it.

#![cfg(windows)]

use std::path::{Path, PathBuf};

use snug_dropper::build::{self, Outcome, OUTPUT_STEM};
use snug_dropper::decide::{decide, Mode};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/snug-dropper is two levels below the repo root")
        .to_path_buf()
}

/// The real `snug.exe`, when this profile has been built.
fn snug_exe() -> Option<PathBuf> {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let path = repo_root().join("target").join(profile).join("snug.exe");
    path.is_file().then_some(path)
}

/// A private scratch directory for one test.
fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "snug-dropper-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creating the scratch directory");
    dir
}

fn demo_jar() -> PathBuf {
    let jar = repo_root().join("assets").join("snug-javafx-demo.jar");
    assert!(jar.is_file(), "the committed demo jar is missing: {}", jar.display());
    jar
}

#[test]
fn builds_a_real_exe_next_to_the_dropped_jar() {
    let Some(snug) = snug_exe() else {
        eprintln!("skipping: snug.exe has not been built (run `cargo test --workspace`)");
        return;
    };

    let dir = scratch("build");
    let input = dir.join("demo.jar");
    std::fs::copy(demo_jar(), &input).expect("copying the demo jar");

    let invocation = build::invocation(&snug, &input);
    let log = dir.join(build::LOG_FILE_NAME);

    match build::run(&invocation, &log) {
        Outcome::Built { output } => {
            // The name and folder come from `snug`'s own success line, so
            // this also proves that line was parsed rather than guessed.
            assert_eq!(output, dir.join(format!("{OUTPUT_STEM}.exe")));
            assert!(output.is_file(), "{} should exist", output.display());
            assert!(
                output.metadata().expect("stat").len() > 0,
                "the built exe should not be empty"
            );
        }
        Outcome::Failed { reason } => {
            let logged = std::fs::read_to_string(&log).unwrap_or_default();
            panic!("build failed: {reason}\n--- snug output ---\n{logged}");
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_snug_exe_fails_with_a_readable_reason() {
    let dir = scratch("no-snug");
    let input = dir.join("demo.jar");
    std::fs::copy(demo_jar(), &input).expect("copying the demo jar");

    let invocation = build::invocation(&dir.join("snug.exe"), &input);
    let log = dir.join(build::LOG_FILE_NAME);

    match build::run(&invocation, &log) {
        Outcome::Failed { reason } => assert!(
            reason.contains("could not be started"),
            "expected a spawn failure, got: {reason}"
        ),
        other => panic!("expected a failure, got {other:?}"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_build_that_snug_rejects_reports_snugs_own_reason() {
    let Some(snug) = snug_exe() else {
        eprintln!("skipping: snug.exe has not been built (run `cargo test --workspace`)");
        return;
    };

    let dir = scratch("bad-jar");
    // A `.jar` extension is all the dropper checks, so a file that isn't
    // a real JAR reaches `snug` — exactly as it would from a real drop.
    let input = dir.join("broken.jar");
    std::fs::write(&input, b"this is not a zip archive").expect("writing the fake jar");

    let invocation = build::invocation(&snug, &input);
    let log = dir.join(build::LOG_FILE_NAME);

    match build::run(&invocation, &log) {
        Outcome::Failed { reason } => {
            assert!(
                !reason.starts_with("snug: "),
                "the `snug: ` prefix should be stripped: {reason}"
            );
            assert!(
                log.is_file(),
                "the log is the only recourse on this path, so it must exist"
            );
        }
        other => panic!("expected a failure, got {other:?}"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_dropped_folder_of_jars_reaches_the_multi_jar_build() {
    let Some(snug) = snug_exe() else {
        eprintln!("skipping: snug.exe has not been built (run `cargo test --workspace`)");
        return;
    };

    // `build/libs` — the shape of a real Gradle or Maven output folder.
    let dir = scratch("folder");
    let libs = dir.join("build").join("libs");
    std::fs::create_dir_all(&libs).expect("creating the libs folder");
    std::fs::copy(demo_jar(), libs.join("app.jar")).expect("copying the demo jar");

    let outcome = decide([&libs]);
    assert_eq!(outcome, Mode::Build { input: libs.clone() });

    let invocation = build::invocation(&snug, &libs);
    // Beside the folder, not inside it: `gradle clean` would erase it.
    assert_eq!(invocation.cwd, dir.join("build"));
    assert_eq!(invocation.output, dir.join("build").join(format!("{OUTPUT_STEM}.exe")));
}

#[test]
fn the_whole_drop_is_decided_then_invoked() {
    // The end-to-end shape of a successful drop, minus the spawn: one
    // JAR in, one fully-resolved `snug.exe` invocation out.
    let jar = PathBuf::from(r"C:\Users\someone\Downloads\MyApp.jar");
    let Mode::Build { input } = decide([&jar]) else {
        panic!("a single jar should decide as Build");
    };

    let invocation = build::invocation(Path::new(r"C:\tools\snug.exe"), &input);

    assert_eq!(invocation.program, PathBuf::from(r"C:\tools\snug.exe"));
    assert_eq!(invocation.cwd, PathBuf::from(r"C:\Users\someone\Downloads"));
    assert_eq!(
        invocation.output,
        PathBuf::from(r"C:\Users\someone\Downloads\Example Application Name.exe")
    );
    // The jar itself is the final positional argument.
    assert_eq!(
        invocation.args.last().unwrap().to_string_lossy(),
        r"C:\Users\someone\Downloads\MyApp.jar"
    );
    assert!(has_flag(&invocation, "--name", "Example Application Name"));
    assert!(has_flag(&invocation, "--company", "Example Company Pty Ltd"));
    assert!(has_flag(
        &invocation,
        "-o",
        r"C:\Users\someone\Downloads\Example Application Name.exe"
    ));
}

/// True when `flag` is present and immediately followed by `value`.
///
/// Windows and Linux both treat each argument vector slot as opaque, so
/// the dropper never has to quote anything — these assertions are what
/// would catch a regression into shell-word assembly.
fn has_flag(invocation: &build::Invocation, flag: &str, value: &str) -> bool {
    invocation
        .args
        .windows(2)
        .any(|pair| pair[0] == std::ffi::OsStr::new(flag) && pair[1] == std::ffi::OsStr::new(value))
}
