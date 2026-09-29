//! End-to-end tests for `--find-main`.
//!
//! These drive the real binary so they cover the whole path: argument
//! parsing, options-file merge, JAR resolution, the class-file scan,
//! and the rendered report. Test JARs are built with a hand-rolled
//! class-file writer so the suite stays hermetic — no JDK, no checked-in
//! binary fixtures, and no dependency on what any particular fat JAR
//! happens to contain.

use std::path::{Path, PathBuf};
use std::process::Command;

use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

fn snug_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_snug"))
}

// ---------------------------------------------------------------------------
// Minimal class-file writer
// ---------------------------------------------------------------------------

/// A method to encode: `(access_flags, name, descriptor)`.
type MethodSpec = (u16, &'static str, &'static str);

const ACC_PUBLIC: u16 = 0x0001;
const ACC_STATIC: u16 = 0x0008;
const PUBLIC_STATIC: u16 = ACC_PUBLIC | ACC_STATIC;
const MAIN_DESCRIPTOR: &str = "([Ljava/lang/String;)V";

/// Encode a syntactically valid class file declaring `methods`.
///
/// Only what `classfile::has_public_static_main` reads is emitted:
/// magic, version, a constant pool, no interfaces, no fields, the
/// method table, and no attributes. Method attribute blobs are
/// omitted entirely, which is fine — the parser skips them by length
/// and a count of zero is a valid encoding of "none".
fn build_class(internal_name: &str, methods: &[MethodSpec]) -> Vec<u8> {
    let mut pool: Vec<Vec<u8>> = Vec::new();

    let utf8 = |pool: &mut Vec<Vec<u8>>, value: &str| -> u16 {
        let index = pool.len() as u16 + 1;
        let mut entry = vec![1u8];
        entry.extend_from_slice(&(value.len() as u16).to_be_bytes());
        entry.extend_from_slice(value.as_bytes());
        pool.push(entry);
        index
    };

    let this_name = utf8(&mut pool, internal_name);
    let this_class = {
        let index = pool.len() as u16 + 1;
        let mut entry = vec![7u8];
        entry.extend_from_slice(&this_name.to_be_bytes());
        pool.push(entry);
        index
    };
    let super_name = utf8(&mut pool, "java/lang/Object");
    let super_class = {
        let index = pool.len() as u16 + 1;
        let mut entry = vec![7u8];
        entry.extend_from_slice(&super_name.to_be_bytes());
        pool.push(entry);
        index
    };

    let mut method_refs = Vec::new();
    for (_, name, descriptor) in methods {
        let name_index = utf8(&mut pool, name);
        let descriptor_index = utf8(&mut pool, descriptor);
        method_refs.push((name_index, descriptor_index));
    }

    let mut out = Vec::new();
    out.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    out.extend_from_slice(&65u16.to_be_bytes()); // major
    out.extend_from_slice(&((pool.len() + 1) as u16).to_be_bytes());
    for entry in &pool {
        out.extend_from_slice(entry);
    }
    out.extend_from_slice(&ACC_PUBLIC.to_be_bytes());
    out.extend_from_slice(&this_class.to_be_bytes());
    out.extend_from_slice(&super_class.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // interfaces_count
    out.extend_from_slice(&0u16.to_be_bytes()); // fields_count
    out.extend_from_slice(&(method_refs.len() as u16).to_be_bytes());
    for (spec, (name_index, descriptor_index)) in methods.iter().zip(&method_refs) {
        out.extend_from_slice(&spec.0.to_be_bytes());
        out.extend_from_slice(&name_index.to_be_bytes());
        out.extend_from_slice(&descriptor_index.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes()); // attributes_count
    }
    out.extend_from_slice(&0u16.to_be_bytes()); // class attributes_count
    out
}

fn class_with_main(internal_name: &str) -> Vec<u8> {
    build_class(internal_name, &[(PUBLIC_STATIC, "main", MAIN_DESCRIPTOR)])
}

/// A JavaFX-style `Application` subclass: a constructor and `start`,
/// and deliberately **no** `main`. This mirrors the shape of
/// `assets/snug-javafx-demo.jar`'s `HelloApplication`, and is the case
/// that makes a naive "Main-Class must have a main method" check wrong.
fn javafx_application_class(internal_name: &str) -> Vec<u8> {
    build_class(
        internal_name,
        &[
            (ACC_PUBLIC, "<init>", "()V"),
            (ACC_PUBLIC, "start", "(Ljavafx/stage/Stage;)V"),
        ],
    )
}

// ---------------------------------------------------------------------------
// JAR + temp-dir helpers
// ---------------------------------------------------------------------------

/// A JAR as `(entry name, bytes)`.
type Entry = (&'static str, Vec<u8>);

fn write_jar(path: &Path, manifest_main_class: Option<&str>, entries: &[Entry]) {
    let file = std::fs::File::create(path).expect("create jar");
    let mut zip = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    if let Some(main_class) = manifest_main_class {
        zip.start_file("META-INF/MANIFEST.MF", opts)
            .expect("start manifest");
        use std::io::Write;
        writeln!(zip, "Manifest-Version: 1.0").expect("write manifest");
        writeln!(zip, "Main-Class: {main_class}").expect("write manifest");
    }

    for (name, bytes) in entries {
        zip.start_file(*name, opts).expect("start entry");
        use std::io::Write;
        zip.write_all(bytes).expect("write entry");
    }

    zip.finish().expect("finish jar");
}

fn tempdir() -> PathBuf {
    let unique = format!(
        "snug-find-main-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );
    let dir = std::env::temp_dir().join(unique);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Run `snug <args...>` and return `(exit_code, stdout)`.
fn run_snug(args: &[&str]) -> (i32, String) {
    let output = Command::new(snug_bin())
        .args(args)
        .output()
        .expect("spawn snug");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

/// Run `snug <args...>` and return `(exit_code, stdout, stderr)`.
fn run_snug_full(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(snug_bin())
        .args(args)
        .output()
        .expect("spawn snug");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn lists_every_main_class_and_marks_the_cli_match() {
    let dir = tempdir();
    let jar = dir.join("multi.jar");
    write_jar(
        &jar,
        Some("com.example.Second"),
        &[
            ("a/b/c/First.class", class_with_main("a/b/c/First")),
            ("a/b/c/Second.class", class_with_main("a/b/c/Second")),
            ("j/g/h/Third.class", class_with_main("j/g/h/Third")),
            (
                "j/g/h/NotAMain.class",
                build_class("j/g/h/NotAMain", &[(ACC_PUBLIC, "run", "()V")]),
            ),
        ],
    );

    let (code, stdout) = run_snug(&[
        jar.to_str().expect("utf-8 path"),
        "--find-main",
        "--main-class",
        "j.g.h.Third",
    ]);

    assert_eq!(code, 0, "a diagnostic must exit 0; stdout was:\n{stdout}");

    for class in ["a.b.c.First", "a.b.c.Second", "j.g.h.Third"] {
        assert!(
            stdout.contains(class),
            "expected {class} in the listing, got:\n{stdout}"
        );
    }

    // A class with a non-main method must not be listed.
    assert!(
        !stdout.contains("NotAMain"),
        "a class without a main method must not be listed, got:\n{stdout}"
    );

    // The match marker lands on the configured class only.
    let marked: Vec<&str> = stdout.lines().filter(|line| line.contains("<--")).collect();
    assert_eq!(
        marked.len(),
        1,
        "exactly one line should be marked:\n{stdout}"
    );
    assert!(
        marked[0].contains("j.g.h.Third"),
        "the wrong line was marked: {:?}",
        marked[0]
    );
    assert!(
        marked[0].contains("--main-class"),
        "the marker should name its source: {:?}",
        marked[0]
    );

    // No leading space before the marker — the left column stays aligned.
    assert!(
        stdout.contains("  a.b.c.First\n"),
        "unmarked entries should align in one column, got:\n{stdout}"
    );

    assert!(
        stdout.contains("3 main class(es) found."),
        "expected the count, got:\n{stdout}"
    );
    assert!(
        stdout.contains("j.g.h.Third (from --main-class) exists and declares a main method."),
        "expected a positive footer, got:\n{stdout}"
    );
}

#[test]
fn mismatched_main_class_warns_but_does_not_fail() {
    let dir = tempdir();
    let jar = dir.join("app.jar");
    write_jar(
        &jar,
        Some("a.b.c.First"),
        &[("a/b/c/First.class", class_with_main("a/b/c/First"))],
    );

    let (code, stdout) = run_snug(&[
        jar.to_str().expect("utf-8 path"),
        "--find-main",
        "--main-class",
        "com.example.DoesNotExist",
    ]);

    // The whole point: a diagnostic that reports a problem must not
    // break anyone's build pipeline.
    assert_eq!(
        code, 0,
        "a mismatch must not fail the run; stdout was:\n{stdout}"
    );
    assert!(
        stdout.contains(
            "com.example.DoesNotExist (from --main-class) is NOT among the classes found."
        ),
        "expected the mismatch footer, got:\n{stdout}"
    );
    // The existing entry point is still shown so the user can fix it.
    assert!(
        stdout.contains("a.b.c.First"),
        "expected candidates to remain listed, got:\n{stdout}"
    );
}

/// The JavaFX case: the manifest names an `Application` subclass, which
/// has no `main` and must not be reported as a problem.
#[test]
fn javafx_manifest_class_is_not_reported_as_a_defect() {
    let dir = tempdir();
    let jar = dir.join("fx.jar");
    write_jar(
        &jar,
        Some("demo.fx.HelloApplication"),
        &[(
            "demo/fx/HelloApplication.class",
            javafx_application_class("demo/fx/HelloApplication"),
        )],
    );

    let (code, stdout) = run_snug(&[jar.to_str().expect("utf-8 path"), "--find-main"]);

    assert_eq!(code, 0, "stdout was:\n{stdout}");
    assert!(
        stdout.contains("No public static void main(String[]) method found."),
        "expected the empty result, got:\n{stdout}"
    );
    assert!(
        stdout.contains("If it is a JavaFX Application subclass that is expected"),
        "the JavaFX caveat should be surfaced, got:\n{stdout}"
    );
    assert!(
        stdout.contains("manifest Main-Class"),
        "the footer should say the target came from the manifest, got:\n{stdout}"
    );
}

#[test]
fn uses_the_manifest_main_class_when_no_flag_is_passed() {
    let dir = tempdir();
    let jar = dir.join("app.jar");
    write_jar(
        &jar,
        Some("a.b.c.First"),
        &[
            ("a/b/c/First.class", class_with_main("a/b/c/First")),
            ("a/b/c/Second.class", class_with_main("a/b/c/Second")),
        ],
    );

    let (code, stdout) = run_snug(&[jar.to_str().expect("utf-8 path"), "--find-main"]);

    assert_eq!(code, 0, "stdout was:\n{stdout}");
    let marked: Vec<&str> = stdout.lines().filter(|l| l.contains("<--")).collect();
    assert_eq!(marked.len(), 1, "one line should be marked:\n{stdout}");
    assert!(
        marked[0].contains("a.b.c.First") && marked[0].contains("manifest Main-Class"),
        "the manifest's own class should be marked: {:?}",
        marked[0]
    );
}

#[test]
fn reports_when_no_main_class_can_be_resolved() {
    let dir = tempdir();
    let jar = dir.join("app.jar");
    // No manifest, no --main-class: nothing to compare against.
    write_jar(
        &jar,
        None,
        &[("a/b/c/First.class", class_with_main("a/b/c/First"))],
    );

    let (code, stdout) = run_snug(&[jar.to_str().expect("utf-8 path"), "--find-main"]);

    assert_eq!(code, 0, "stdout was:\n{stdout}");
    assert!(
        stdout.contains("1 main class(es) found."),
        "expected the listing, got:\n{stdout}"
    );
    assert!(
        stdout.contains("No main class resolved"),
        "expected the no-target footer, got:\n{stdout}"
    );
}

#[test]
fn ignores_module_info_and_non_class_entries() {
    let dir = tempdir();
    let jar = dir.join("app.jar");
    write_jar(
        &jar,
        Some("a.b.c.First"),
        &[
            ("module-info.class", build_class("module-info", &[])),
            ("a/b/c/First.class", class_with_main("a/b/c/First")),
            ("readme.txt", b"not a class".to_vec()),
            ("logo.png", vec![0x89, b'P', b'N', b'G']),
        ],
    );

    let (code, stdout) = run_snug(&[jar.to_str().expect("utf-8 path"), "--find-main"]);

    assert_eq!(code, 0, "stdout was:\n{stdout}");
    assert!(
        stdout.contains("1 main class(es) found."),
        "only the real class should count, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("module-info"),
        "module-info.class should never be reported, got:\n{stdout}"
    );
}

#[test]
fn a_malformed_class_entry_does_not_abort_the_scan() {
    let dir = tempdir();
    let jar = dir.join("app.jar");
    write_jar(
        &jar,
        Some("a.b.c.First"),
        &[
            // Truncated garbage claiming to be a class.
            ("broken/Broken.class", vec![0xCA, 0xFE, 0xBA, 0xBE, 0x00]),
            // An empty entry.
            ("empty/Empty.class", Vec::new()),
            ("a/b/c/First.class", class_with_main("a/b/c/First")),
        ],
    );

    let (code, stdout) = run_snug(&[jar.to_str().expect("utf-8 path"), "--find-main"]);

    assert_eq!(code, 0, "stdout was:\n{stdout}");
    assert!(
        stdout.contains("a.b.c.First") && stdout.contains("1 main class(es) found."),
        "the good class should still be found, got:\n{stdout}"
    );
}

/// `--find-main` is read-only: it must not produce an EXE, and must not
/// require the metadata a real build needs.
#[test]
fn writes_no_exe_and_needs_no_build_metadata() {
    let dir = tempdir();
    let jar = dir.join("app.jar");
    write_jar(
        &jar,
        Some("a.b.c.First"),
        &[("a/b/c/First.class", class_with_main("a/b/c/First"))],
    );

    // Deliberately no --name / --company / --version / --icon.
    let (code, _stdout) = run_snug(&[jar.to_str().expect("utf-8 path"), "--find-main"]);
    assert_eq!(code, 0);

    let produced: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read temp dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("exe"))
        .collect();
    assert!(
        produced.is_empty(),
        "--find-main must not build an EXE, found: {produced:?}"
    );
}

#[test]
fn scans_a_directory_of_jars() {
    let dir = tempdir();
    let libs = dir.join("libs");
    std::fs::create_dir_all(&libs).expect("create libs dir");

    write_jar(
        &libs.join("01-app.jar"),
        Some("a.b.c.First"),
        &[("a/b/c/First.class", class_with_main("a/b/c/First"))],
    );
    write_jar(
        &libs.join("02-tool.jar"),
        None,
        &[("x/y/Zed.class", class_with_main("x/y/Zed"))],
    );
    // Not a .jar, so it should be ignored by the directory scan.
    std::fs::write(libs.join("notes.txt"), b"ignore me").expect("write notes");

    let (code, stdout) = run_snug(&["--input", libs.to_str().expect("utf-8 path"), "--find-main"]);

    assert_eq!(code, 0, "stdout was:\n{stdout}");
    assert!(
        stdout.contains("01-app.jar") && stdout.contains("02-tool.jar"),
        "each JAR should be labelled, got:\n{stdout}"
    );
    assert!(
        stdout.contains("a.b.c.First") && stdout.contains("x.y.Zed"),
        "both JARs' entry points should be listed, got:\n{stdout}"
    );
    assert!(
        stdout.contains("2 main class(es) found."),
        "expected 2 across both JARs, got:\n{stdout}"
    );
}

#[test]
fn requires_an_input_source() {
    let (code, _stdout) = run_snug(&["--find-main"]);
    assert_ne!(code, 0, "--find-main with no JAR should fail");
}

// ---------------------------------------------------------------------------
// Which manifest wins?
//
// Snug reads `Main-Class` from the first JAR by filename. On a
// multi-JAR input that is a tie-break, not a decision, so these tests
// pin the behaviour that the ambiguity is surfaced rather than
// resolved silently.
// ---------------------------------------------------------------------------

/// A directory of two JARs that declare *different* main classes.
fn conflicting_libs(root: &Path) -> PathBuf {
    let libs = root.join("libs");
    std::fs::create_dir_all(&libs).expect("create libs");
    write_jar(
        &libs.join("01-alpha.jar"),
        Some("com.alpha.Main"),
        &[("com/alpha/Main.class", class_with_main("com/alpha/Main"))],
    );
    write_jar(
        &libs.join("02-beta.jar"),
        Some("com.beta.Main"),
        &[("com/beta/Main.class", class_with_main("com/beta/Main"))],
    );
    libs
}

#[test]
fn flags_conflicting_manifests_when_no_main_class_is_given() {
    let dir = tempdir();
    let libs = conflicting_libs(&dir);

    let (code, stdout, _stderr) =
        run_snug_full(&["--input", libs.to_str().expect("utf-8 path"), "--find-main"]);

    assert_eq!(code, 0, "diagnostics never fail; stdout was:\n{stdout}");
    assert!(
        stdout.contains("Warning: 2 JARs declare different Main-Class values:"),
        "the conflict should be flagged, got:\n{stdout}"
    );
    for expected in [
        "01-alpha.jar",
        "02-beta.jar",
        "com.alpha.Main",
        "com.beta.Main",
    ] {
        assert!(
            stdout.contains(expected),
            "expected {expected} in the conflict report, got:\n{stdout}"
        );
    }
    // Exactly one row is in use, and it is the first JAR by filename.
    let in_use: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("<-- in use"))
        .collect();
    assert_eq!(
        in_use.len(),
        1,
        "exactly one manifest should be in use:\n{stdout}"
    );
    assert!(
        in_use[0].contains("01-alpha.jar") && in_use[0].contains("com.alpha.Main"),
        "the first JAR by filename should be the one in use: {:?}",
        in_use[0]
    );
    assert!(
        stdout.contains("chosen by accident of naming"),
        "the report should say the choice was incidental, got:\n{stdout}"
    );
    assert!(
        stdout.contains("Pass --main-class"),
        "the report should say how to fix it, got:\n{stdout}"
    );
}

#[test]
fn conflicting_manifests_are_silent_when_main_class_is_explicit() {
    let dir = tempdir();
    let libs = conflicting_libs(&dir);

    let (code, stdout, _stderr) = run_snug_full(&[
        "--input",
        libs.to_str().expect("utf-8 path"),
        "--find-main",
        "--main-class",
        "com.beta.Main",
    ]);

    assert_eq!(code, 0, "stdout was:\n{stdout}");
    assert!(
        !stdout.contains("Warning:"),
        "an explicit --main-class settles the question; stdout was:\n{stdout}"
    );
    // The explicit choice is still marked, and it's the *user's* class.
    let marked: Vec<&str> = stdout.lines().filter(|l| l.contains("<--")).collect();
    assert_eq!(marked.len(), 1, "one line should be marked:\n{stdout}");
    assert!(marked[0].contains("com.beta.Main"), "got: {:?}", marked[0]);
}

#[test]
fn manifests_that_agree_are_not_a_conflict() {
    let dir = tempdir();
    let libs = dir.join("agree");
    std::fs::create_dir_all(&libs).expect("create libs");
    for name in ["01-a.jar", "02-b.jar"] {
        write_jar(
            &libs.join(name),
            Some("com.shared.Main"),
            &[("com/shared/Main.class", class_with_main("com/shared/Main"))],
        );
    }

    let (code, stdout, _stderr) =
        run_snug_full(&["--input", libs.to_str().expect("utf-8 path"), "--find-main"]);

    assert_eq!(code, 0, "stdout was:\n{stdout}");
    assert!(
        !stdout.contains("Warning:"),
        "identical manifests are not ambiguous; stdout was:\n{stdout}"
    );
    assert!(
        stdout.contains("No ambiguity"),
        "expected the agreement note, got:\n{stdout}"
    );
}

/// The nastier case: the first JAR declares nothing, so snug embeds no
/// main class even though one was sitting right there in the
/// directory. The build itself still succeeds.
#[test]
fn build_flags_a_first_jar_with_no_main_class() {
    let dir = tempdir();
    let libs = dir.join("libs");
    std::fs::create_dir_all(&libs).expect("create libs");
    write_jar(&libs.join("01-lib.jar"), None, &[]);
    write_jar(
        &libs.join("02-app.jar"),
        Some("com.gamma.Main"),
        &[("com/gamma/Main.class", class_with_main("com/gamma/Main"))],
    );

    let (code, _stdout, stderr) = run_snug_full(&[
        "--input",
        libs.to_str().expect("utf-8 path"),
        "--dry-run",
        "--name",
        "Demo",
    ]);

    assert_eq!(
        code, 0,
        "a warning must not fail the build; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("declares no Main-Class"),
        "expected the missing-manifest warning, stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("02-app.jar") && stderr.contains("com.gamma.Main"),
        "the available declaration should be named, stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("--main-class"),
        "the warning should say how to fix it, stderr:\n{stderr}"
    );
}

/// A single-JAR input can't be ambiguous, so it must not warn.
#[test]
fn single_jar_input_never_warns_about_manifests() {
    let dir = tempdir();
    let jar = dir.join("app.jar");
    write_jar(
        &jar,
        Some("com.example.Main"),
        &[(
            "com/example/Main.class",
            class_with_main("com/example/Main"),
        )],
    );

    let (code, _stdout, stderr) = run_snug_full(&[
        jar.to_str().expect("utf-8 path"),
        "--dry-run",
        "--name",
        "Demo",
    ]);

    assert_eq!(code, 0, "stderr:\n{stderr}");
    assert!(
        !stderr.contains("Main-Class values") && !stderr.contains("declares no Main-Class"),
        "a single JAR is unambiguous; stderr:\n{stderr}"
    );
}
