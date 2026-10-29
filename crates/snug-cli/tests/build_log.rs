//! End-to-end tests for the build log, the effective-options report and
//! the diagnostics that stop a build.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn snug_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_snug"))
}

/// A directory of its own for each test.
///
/// The counter is load-bearing: these tests share a pid, and a coarse
/// clock can hand two parallel tests the same directory — which would
/// let one test's `snug.options` show up inside another's and change
/// the expected value.
fn tempdir() -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let unique = format!(
        "snug-log-e2e-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let dir = std::env::temp_dir().join(unique);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// A JAR with a `Main-Class` and one class file.
///
/// The class is Java 17 (major 61), so it satisfies every `--min-java`
/// these tests use. The one test that needs a *newer* class asks for it
/// explicitly.
fn write_jar(path: &Path, main_class: Option<&str>) {
    write_jar_with_version(path, main_class, 61);
}

/// A JAR whose class was compiled for the given class-file major
/// version. Major 61 is Java 17; major 69 is Java 25.
fn write_jar_with_version(path: &Path, main_class: Option<&str>, major: u16) {
    write_jar_classes(
        path,
        main_class,
        major,
        &[TestClass {
            entry: "com/example/Main.class",
            name: "com/example/Main",
            super_class: "java/lang/Object",
            declares_main: true,
        }],
    );
}

/// One class file to put in a test JAR.
struct TestClass {
    /// The path inside the JAR, e.g. `com/example/Main.class`.
    entry: &'static str,
    /// The class's own name, in internal form (`com/example/Main`).
    name: &'static str,
    /// The superclass's name, in internal form (`java/lang/Object`).
    super_class: &'static str,
    /// Whether the class declares `public static void main(String[])`.
    declares_main: bool,
}

/// Write a JAR with a manifest and the given class files.
fn write_jar_classes(path: &Path, main_class: Option<&str>, major: u16, classes: &[TestClass]) {
    let entries: Vec<(&str, Vec<u8>)> = classes
        .iter()
        .map(|class| {
            (
                class.entry,
                class_file(major, class.name, class.super_class, class.declares_main),
            )
        })
        .collect();
    write_jar_entries(path, main_class, &entries);
}

/// A JAR with a JAR inside it.
fn write_jar_with_nested_jar(path: &Path, main_class: Option<&str>) {
    let entries: Vec<(&str, Vec<u8>)> = vec![
        (
            "com/example/Main.class",
            class_file(61, "com/example/Main", "java/lang/Object", true),
        ),
        // The bytes do not have to be a real archive: only the name has to
        // end in `.jar` for snug to notice it.
        ("lib/inner.jar", vec![0x50, 0x4B, 0x03, 0x04]),
    ];
    write_jar_entries(path, main_class, &entries);
}

/// Write a JAR from a manifest and a list of `(entry name, contents)`.
fn write_jar_entries(path: &Path, main_class: Option<&str>, entries: &[(&str, Vec<u8>)]) {
    use std::io::Write;
    use zip::CompressionMethod;
    use zip::write::SimpleFileOptions;

    let file = fs::File::create(path).expect("create jar");
    let mut zip = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    zip.start_file("META-INF/MANIFEST.MF", opts)
        .expect("start_file");
    let manifest = match main_class {
        Some(class) => format!("Manifest-Version: 1.0\nMain-Class: {class}\n"),
        None => "Manifest-Version: 1.0\n".to_string(),
    };
    zip.write_all(manifest.as_bytes()).expect("write manifest");
    for (name, bytes) in entries {
        zip.start_file(*name, opts).expect("start_file");
        zip.write_all(bytes).expect("write entry");
    }
    zip.finish().expect("finish zip");
}

/// A JAR whose manifest is stored uncompressed and then corrupted.
///
/// The archive still opens — the central directory is intact — but reading
/// the manifest fails its checksum. That is what `SNUG-MANIFEST-001` is for.
fn write_jar_with_corrupt_manifest(path: &Path) {
    use std::io::Write;
    use zip::CompressionMethod;
    use zip::write::SimpleFileOptions;

    let file = fs::File::create(path).expect("create jar");
    let mut zip = zip::ZipWriter::new(file);
    // Stored, not deflated: the manifest is then in the file verbatim, and
    // one byte of it can be flipped.
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    zip.start_file("META-INF/MANIFEST.MF", opts)
        .expect("start_file");
    zip.write_all(b"Manifest-Version: 1.0\nMain-Class: com.example.Main\n")
        .expect("write manifest");
    zip.start_file("com/example/Main.class", opts)
        .expect("start_file");
    zip.write_all(&class_file(
        61,
        "com/example/Main",
        "java/lang/Object",
        true,
    ))
    .expect("write class");
    zip.finish().expect("finish zip");

    let mut bytes = fs::read(path).expect("read jar");
    let needle = b"Manifest-Version: 1.0";
    let at = bytes
        .windows(needle.len())
        .position(|window| window == needle)
        .expect("find the manifest in the jar");
    bytes[at] = b'X';
    fs::write(path, &bytes).expect("write jar");
}

/// A minimal but *valid* class file.
///
/// The build parses class files now — the entry-point check reads the
/// superclass and the method table — so a fixture that was only a magic
/// number and a version would make every test report an unreadable entry
/// point.
///
/// Names are in internal form (`com/example/Main`), which is what a class
/// file stores.
fn class_file(major: u16, name: &str, super_class: &str, declares_main: bool) -> Vec<u8> {
    let methods: &[common::MethodSpec] = if declares_main {
        &[(common::PUBLIC_STATIC, "main", common::MAIN_DESCRIPTOR)]
    } else {
        &[]
    };
    common::build_class_with(name, super_class, major, methods)
}

/// The effective-options table, as (label, value) pairs.
fn effective_options(stderr: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut in_table = false;
    for line in stderr.lines() {
        if line.starts_with("Effective build options:") {
            in_table = true;
            continue;
        }
        if !in_table {
            continue;
        }
        let Some(rest) = line.strip_prefix("  ") else {
            break;
        };
        let Some((label, rest)) = rest.split_once(':') else {
            break;
        };
        let value = rest.trim_start();
        let value = match value.rfind(" [") {
            Some(index) => value[..index].trim_end(),
            None => value.trim_end(),
        };
        out.push((label.to_string(), value.to_string()));
    }
    out
}

fn field(stderr: &str, key: &str) -> Option<String> {
    effective_options(stderr)
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
}

fn origin(stderr: &str, key: &str) -> Option<String> {
    stderr
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("  ")?;
            let (label, rest) = rest.split_once(':')?;
            let open = rest.rfind(" [")?;
            let origin = rest[open + 2..].trim_end_matches(']');
            Some((label.to_string(), origin.to_string()))
        })
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
}

#[test]
fn a_build_log_is_written_in_the_working_directory() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar(&jar, Some("com.example.Main"));

    let output = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--dry-run")
        .output()
        .expect("spawn snug");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = tmp.join("snug.build.log");
    assert!(
        log.exists(),
        "the build log should be written to the working directory"
    );
    let contents = fs::read_to_string(&log).expect("read log");
    assert!(
        contents.contains("Effective build options:"),
        "the log should record the effective options: {contents}"
    );
    assert!(
        contents.contains("Build succeeded."),
        "the log should record the outcome: {contents}"
    );
}

#[test]
fn build_log_flag_redirects_the_log() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar(&jar, Some("com.example.Main"));
    let target = tmp.join("elsewhere.log");

    let output = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--dry-run")
        .arg("--build-log")
        .arg(&target)
        .output()
        .expect("spawn snug");

    assert!(output.status.success());
    assert!(
        target.exists(),
        "--build-log should send the log to the named file"
    );
    assert!(
        !tmp.join("snug.build.log").exists(),
        "the default destination should not be used when --build-log is given"
    );
}

#[test]
fn a_build_log_naming_a_directory_lands_inside_it() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar(&jar, Some("com.example.Main"));
    let logs = tmp.join("logs");
    fs::create_dir_all(&logs).expect("create logs dir");

    let output = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--dry-run")
        .arg("--build-log")
        .arg(&logs)
        .output()
        .expect("spawn snug");

    assert!(output.status.success());
    assert!(
        logs.join("snug.build.log").exists(),
        "a directory destination should get snug.build.log inside it"
    );
}

#[test]
fn the_log_records_where_each_value_came_from() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar(&jar, Some("com.example.Main"));
    fs::write(
        tmp.join("snug.options"),
        "--name \"From File\"\n--min-java 21\n",
    )
    .expect("write options");

    let output = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--dry-run")
        .output()
        .expect("spawn snug");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        field(&stderr, "name").as_deref(),
        Some("From File"),
        "the file's value should be the one used: {stderr}"
    );
    let origin = origin(&stderr, "name").expect("the origin should be reported");
    assert!(
        origin.ends_with("snug.options:1"),
        "the origin should name the file and the line, got {origin}: {stderr}"
    );
}

#[test]
fn a_credential_shaped_jvm_argument_is_masked_in_the_log() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar(&jar, Some("com.example.Main"));

    let output = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--dry-run")
        .arg("--jvm-arg=-Dapi.token=supersecretvalue")
        .arg("--jvm-arg=-Xmx2g")
        .output()
        .expect("spawn snug");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("supersecretvalue"),
        "the credential must not be printed: {stderr}"
    );
    assert!(
        stderr.contains("-Dapi.token=***"),
        "the property should be masked, keeping the key: {stderr}"
    );
    assert!(
        stderr.contains("-Xmx2g"),
        "a harmless argument should be shown in full: {stderr}"
    );

    let log = fs::read_to_string(tmp.join("snug.build.log")).expect("read log");
    assert!(
        !log.contains("supersecretvalue"),
        "the credential must not reach the log either: {log}"
    );
}

#[test]
fn an_error_stops_the_build_and_is_reported() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    // No Main-Class, and no --main-class: there is nothing to call.
    write_jar(&jar, None);

    let output = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--dry-run")
        .output()
        .expect("spawn snug");

    assert!(
        !output.status.success(),
        "a build with no entry point must not succeed"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("SNUG-MAIN-001"),
        "the diagnostic should carry its stable code: {stderr}"
    );
    assert!(
        stderr.contains("Build failed"),
        "the summary should say the build failed: {stderr}"
    );
    assert!(
        !stderr.contains("Build succeeded."),
        "the summary must not claim success: {stderr}"
    );
}

#[test]
fn a_class_needing_a_newer_java_stops_the_build() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    // Major 69 is Java 25, and the build asks for Java 17.
    write_jar_with_version(&jar, Some("com.example.Main"), 69);

    let output = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--min-java")
        .arg("17")
        .arg("--dry-run")
        .output()
        .expect("spawn snug");

    assert!(
        !output.status.success(),
        "a class needing a newer Java must not build"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("SNUG-JAVA-001"),
        "the diagnostic should carry its stable code: {stderr}"
    );
}

#[test]
fn a_diagnostic_mode_writes_no_build_log() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar(&jar, Some("com.example.Main"));

    let output = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--find-main")
        .output()
        .expect("spawn snug");

    assert!(output.status.success());
    assert!(
        !tmp.join("snug.build.log").exists(),
        "a diagnostic is not a build, so it writes no build log"
    );
    // And its own output still goes to stdout, which is the documented
    // way to capture it.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("com.example.Main"),
        "--find-main should still print to stdout: {stdout}"
    );
}

#[test]
fn a_build_log_that_is_also_an_input_is_refused() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar(&jar, Some("com.example.Main"));

    // The log is truncated when it opens, so a log that is also an input
    // would destroy the input before any guard could run.
    let output = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--dry-run")
        .arg("--build-log")
        .arg(&jar)
        .output()
        .expect("spawn snug");

    assert!(
        !output.status.success(),
        "a log that is also an input must be refused"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("SNUG-LOG-002"),
        "the diagnostic should carry its stable code: {stderr}"
    );
    // The JAR must still be intact.
    let manifest = fs::read(&jar).expect("read jar");
    assert!(
        !manifest.is_empty(),
        "the input must not have been truncated"
    );
}

#[test]
fn log_cannot_truncate_an_existing_executable() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    let exe = tmp.join("demo.exe");
    write_jar(&jar, Some("com.example.Main"));
    fs::write(&exe, b"existing executable").unwrap();

    let result = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--output")
        .arg(&exe)
        .arg("--build-log")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(fs::read(&exe).unwrap(), b"existing executable");
}

#[test]
fn log_cannot_truncate_a_jar_inside_an_input_directory() {
    let tmp = tempdir();
    let jars = tmp.join("jars");
    fs::create_dir_all(&jars).unwrap();
    let jar = jars.join("app.jar");
    write_jar(&jar, Some("com.example.Main"));
    let original = fs::read(&jar).unwrap();

    let result = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg("--input")
        .arg(&jars)
        .arg("--build-log")
        .arg(&jar)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(fs::read(&jar).unwrap(), original);
}

#[test]
fn duplicate_main_class_uses_the_first_jar_version() {
    let tmp = tempdir();
    let jars = tmp.join("jars");
    fs::create_dir_all(&jars).unwrap();
    write_jar_with_version(&jars.join("a.jar"), Some("com.example.Main"), 61);
    write_jar_with_version(&jars.join("b.jar"), Some("com.example.Main"), 69);

    let result = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg("--input")
        .arg(&jars)
        .arg("--min-java")
        .arg("17")
        .arg("--dry-run")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    assert!(stderr.contains("SNUG-MAIN-005"), "{stderr}");
    assert!(!stderr.contains("error SNUG-JAVA-001"), "{stderr}");
}

#[test]
fn dry_run_rejects_invalid_manifest_without_writing_executable() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    let manifest = tmp.join("broken.manifest");
    let exe = tmp.join("demo.exe");
    write_jar(&jar, Some("com.example.Main"));
    fs::write(&manifest, "<assembly><unclosed>").unwrap();

    let result = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--manifest")
        .arg(&manifest)
        .arg("--dry-run")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(!result.status.success(), "{stderr}");
    assert!(stderr.contains("SNUG-RESOURCE-001"), "{stderr}");
    assert!(!exe.exists());
}

#[test]
fn dry_run_rejects_invalid_icon_without_writing_executable() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    let icon = tmp.join("broken.png");
    let exe = tmp.join("demo.exe");
    write_jar(&jar, Some("com.example.Main"));
    fs::write(&icon, b"not a PNG").unwrap();

    let result = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--icon")
        .arg(&icon)
        .arg("--dry-run")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(!result.status.success(), "{stderr}");
    assert!(stderr.contains("SNUG-RESOURCE-001"), "{stderr}");
    assert!(!exe.exists());
}

#[test]
fn a_missing_input_is_an_input_problem_not_a_snug_bug() {
    let tmp = tempdir();

    let result = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(tmp.join("no-such.jar"))
        .arg("--dry-run")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);

    assert!(!result.status.success(), "{stderr}");
    assert!(
        stderr.contains("SNUG-INPUT-001"),
        "a path that does not exist is an input problem: {stderr}"
    );
    assert!(
        !stderr.contains("SNUG-INTERNAL-001"),
        "a missing input is not a snug bug, and must not be reported as one: {stderr}"
    );
    assert!(
        stderr.contains("no-such.jar"),
        "the message should name the path snug tried: {stderr}"
    );

    let log = fs::read_to_string(tmp.join("snug.build.log")).expect("read log");
    assert!(
        log.contains("SNUG-INPUT-001"),
        "the failure should reach the log: {log}"
    );
}

#[test]
fn an_empty_input_directory_is_an_input_problem() {
    let tmp = tempdir();
    let libs = tmp.join("libs");
    fs::create_dir_all(&libs).unwrap();

    let result = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg("--input")
        .arg(&libs)
        .arg("--dry-run")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);

    assert!(!result.status.success(), "{stderr}");
    assert!(
        stderr.contains("SNUG-INPUT-001"),
        "a directory with no JARs is an input problem: {stderr}"
    );
    assert!(
        stderr.contains("no *.jar files found"),
        "the message should say what was wrong: {stderr}"
    );

    let log = fs::read_to_string(tmp.join("snug.build.log")).expect("read log");
    assert!(
        log.contains("SNUG-INPUT-001"),
        "the failure should reach the log: {log}"
    );
}

#[test]
fn an_options_file_that_does_not_parse_is_reported_and_logged() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar(&jar, Some("com.example.Main"));
    // The closing quote is missing.
    fs::write(tmp.join("snug.options"), "--name \"unterminated\n").unwrap();

    let result = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--dry-run")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);

    assert!(!result.status.success(), "{stderr}");
    assert!(
        stderr.contains("SNUG-CFG-001"),
        "a syntax error in an options file is a parse error: {stderr}"
    );
    assert!(
        stderr.contains("Log:"),
        "the log's location should be reported: {stderr}"
    );

    let log = fs::read_to_string(tmp.join("snug.build.log")).expect("read log");
    assert!(
        log.contains("SNUG-CFG-001"),
        "the failure should reach the log: {log}"
    );
}

#[test]
fn the_log_records_the_value_a_higher_priority_source_overrode() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar(&jar, Some("com.example.Main"));
    fs::write(
        tmp.join("snug.options"),
        "--name \"From File\"\n--company \"File Company\"\n",
    )
    .unwrap();

    let result = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--dry-run")
        .arg("--name")
        .arg("From Command Line")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    assert_eq!(
        field(&stderr, "name").as_deref(),
        Some("From Command Line"),
        "the command line should win: {stderr}"
    );

    let log = fs::read_to_string(tmp.join("snug.build.log")).expect("read log");
    let loser = log
        .lines()
        .find(|line| line.contains("From File") && line.contains("overridden by"))
        .unwrap_or_else(|| panic!("the log should record the overridden value: {log}"));
    assert!(
        loser.contains("snug.options:1"),
        "the overridden value should keep its origin: {loser}"
    );
    assert!(
        loser.contains("[command line]"),
        "the log should name the origin that beat it: {loser}"
    );

    // The company was not overridden, so it appears once and only once.
    let company_rows = log
        .lines()
        .filter(|line| line.trim_start().starts_with("company:"))
        .count();
    assert_eq!(
        company_rows, 1,
        "a value that was not overridden should appear once: {log}"
    );
}

#[test]
fn a_localisation_missing_keys_has_its_own_code() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar(&jar, Some("com.example.Main"));
    // One key out of the whole baseline: the rest are missing. The tag is
    // not "en", because a user-supplied English bundle *replaces* the
    // baseline rather than being measured against it.
    let bundle = tmp.join("snug-localisations.fr.txt");
    fs::write(&bundle, "err.zip = Erreur ZIP : {0}\n").unwrap();

    let result = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--localisation")
        .arg(&bundle)
        .arg("--dry-run")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);

    assert!(result.status.success(), "{stderr}");
    assert!(
        stderr.contains("SNUG-LOCALISATION-001"),
        "a translation gap has its own code: {stderr}"
    );
    assert!(
        !stderr.contains("SNUG-RESOURCE-001"),
        "a translation gap is not a broken icon: {stderr}"
    );
}

#[test]
fn a_build_with_no_input_reports_where_the_log_is() {
    let tmp = tempdir();

    let result = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg("--dry-run")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);

    assert!(!result.status.success(), "{stderr}");
    assert!(
        stderr.contains("SNUG-CFG-002"),
        "no input at all is a configuration problem: {stderr}"
    );
    assert!(
        stderr.contains("Log:"),
        "the log's location should be reported: {stderr}"
    );

    let log = fs::read_to_string(tmp.join("snug.build.log")).expect("read log");
    assert!(
        log.contains("Build failed during input."),
        "the log should record the outcome: {log}"
    );
}

#[test]
fn an_output_collision_reports_where_the_log_is() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar(&jar, Some("com.example.Main"));
    let original = fs::read(&jar).unwrap();

    let result = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("-o")
        .arg(&jar)
        .arg("--dry-run")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);

    assert!(!result.status.success(), "{stderr}");
    assert!(
        stderr.contains("SNUG-OUTPUT-001"),
        "an output that is also an input is a collision: {stderr}"
    );
    assert!(
        stderr.contains("Log:"),
        "the log's location should be reported: {stderr}"
    );

    let log = fs::read_to_string(tmp.join("snug.build.log")).expect("read log");
    assert!(
        log.contains("Build failed during output."),
        "the log should record the outcome: {log}"
    );
    assert_eq!(
        fs::read(&jar).unwrap(),
        original,
        "the input must not have been truncated"
    );
}

#[test]
fn a_positional_that_beats_an_options_file_input_is_named_as_the_winner() {
    // `--input` and the positional JAR are the same setting, and a command
    // line that carries one of them beats an options file that carries the
    // other. The loser has to name the one that won, not "not set".
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar(&jar, Some("com.example.Main"));
    fs::write(tmp.join("snug.options"), "--input demo.jar\n").unwrap();

    let result = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--dry-run")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");

    let log = fs::read_to_string(tmp.join("snug.build.log")).expect("read log");
    let loser = log
        .lines()
        .find(|line| line.contains("overridden by"))
        .unwrap_or_else(|| {
            panic!("the options file's input should be recorded as overridden: {log}")
        });
    assert!(
        loser.contains("[command line]"),
        "the command line's positional is what beat it: {loser}"
    );
    assert!(
        !loser.contains("[not set]"),
        "the winner is not an unset value: {loser}"
    );
}

#[test]
fn an_entry_class_without_a_main_is_reported() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar_classes(
        &jar,
        Some("com.example.Main"),
        61,
        &[TestClass {
            entry: "com/example/Main.class",
            name: "com/example/Main",
            super_class: "java/lang/Object",
            declares_main: false,
        }],
    );

    let result = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--dry-run")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);

    assert!(result.status.success(), "{stderr}");
    assert!(
        stderr.contains("SNUG-MAIN-003"),
        "a class with no main should be reported: {stderr}"
    );
}

#[test]
fn an_entry_class_that_inherits_a_main_is_not_reported() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar_classes(
        &jar,
        Some("com.example.Child"),
        61,
        &[
            TestClass {
                entry: "com/example/Base.class",
                name: "com/example/Base",
                super_class: "java/lang/Object",
                declares_main: true,
            },
            TestClass {
                entry: "com/example/Child.class",
                name: "com/example/Child",
                super_class: "com/example/Base",
                declares_main: false,
            },
        ],
    );

    let result = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--dry-run")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);

    assert!(result.status.success(), "{stderr}");
    assert!(
        !stderr.contains("SNUG-MAIN-003"),
        "an inherited main launches fine, and must not be reported: {stderr}"
    );
}

#[test]
fn a_java_fx_application_is_not_reported_as_missing_a_main() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar_classes(
        &jar,
        Some("com.example.App"),
        61,
        &[
            TestClass {
                entry: "javafx/application/Application.class",
                name: "javafx/application/Application",
                super_class: "java/lang/Object",
                declares_main: false,
            },
            TestClass {
                entry: "com/example/App.class",
                name: "com/example/App",
                super_class: "javafx/application/Application",
                declares_main: false,
            },
        ],
    );

    let result = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--dry-run")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);

    assert!(result.status.success(), "{stderr}");
    assert!(
        !stderr.contains("SNUG-MAIN-003"),
        "the launcher starts a JavaFX Application through Application.launch: {stderr}"
    );
}

#[test]
fn a_nested_jar_is_reported() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar_with_nested_jar(&jar, Some("com.example.Main"));

    let result = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--dry-run")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);

    assert!(result.status.success(), "{stderr}");
    assert!(
        stderr.contains("SNUG-DEP-002"),
        "a nested JAR should be reported: {stderr}"
    );
    assert!(
        stderr.contains("nested JAR"),
        "the message should say what was found: {stderr}"
    );
}

#[test]
fn an_unreadable_manifest_is_reported() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar_with_corrupt_manifest(&jar);

    let result = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--dry-run")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);

    assert!(!result.status.success(), "{stderr}");
    assert!(
        stderr.contains("SNUG-MANIFEST-001"),
        "a manifest that cannot be read should be reported: {stderr}"
    );
    assert!(
        !stderr.contains("SNUG-INTERNAL-001"),
        "the user's JAR is not a snug bug: {stderr}"
    );
}
