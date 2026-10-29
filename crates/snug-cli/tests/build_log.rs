//! End-to-end tests for the build log, the effective-options report and
//! the diagnostics that stop a build.

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
    zip.start_file("com/example/Main.class", opts)
        .expect("start_file");
    // A class-file header: the magic, then the major version.
    let mut class = vec![0u8; 16];
    class[0..4].copy_from_slice(&[0xCA, 0xFE, 0xBA, 0xBE]);
    class[6..8].copy_from_slice(&major.to_be_bytes());
    zip.write_all(&class).expect("write class");
    zip.finish().expect("finish zip");
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
