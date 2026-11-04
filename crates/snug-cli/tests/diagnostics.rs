//! Every SNUG diagnostic, exercised end to end.
//!
//! A diagnostic code is a promise: `SNUG-INPUT-002` means "a directory
//! entry could not be read", and a user who sees it should be able to
//! look it up in `docs/diagnostics.md` and find out what to do about
//! it. That promise has two ways to break, and this file guards both:
//!
//! - **A code no build can raise.** The documentation would describe a
//!   message that never appears, and nobody would notice until a user
//!   pasted it into a bug report. `every_code_can_be_raised` walks
//!   [`codes::ALL`] and fails if any of them cannot be produced.
//! - **A code the documentation forgot.** `the_documentation_lists_every_code`
//!   compares the documented codes against the declared ones, so the page
//!   and the builder cannot drift apart.
//!
//! The scenarios below are also the catalogue. Each one drives a real
//! `snug` build — or, for the two diagnostics that cannot be produced
//! from the command line, the real library call the binary makes — and
//! `the_catalogue_lists_every_diagnostic` writes the messages they
//! actually render to `target/tmp/diagnostics-catalogue.md`.
//!
//! Run it with `cargo test -p snug-cli --test diagnostics -- --nocapture`
//! to read every diagnostic message in one place.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use snug_cli::build_log::BuildLog;
use snug_cli::report::{Reporter, Severity, codes};

fn snug_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_snug"))
}

/// A directory of its own for each scenario.
///
/// The counter is load-bearing: these scenarios share a pid, and a
/// coarse clock can hand two parallel tests the same directory.
fn tempdir() -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let unique = format!(
        "snug-diagnostics-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let dir = std::env::temp_dir().join(unique);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

// ---------------------------------------------------------------------------
// Observations
// ---------------------------------------------------------------------------

/// One diagnostic, as it was raised.
#[derive(Debug, Clone)]
struct Observed {
    severity: Severity,
    code: String,
    message: String,
    hint: Option<String>,
}

/// What a scenario saw.
#[derive(Debug)]
struct Observation {
    /// Whether the build succeeded.
    success: bool,
    /// Every diagnostic that was raised, in order.
    diagnostics: Vec<Observed>,
    /// The whole stderr, for the failure message.
    stderr: String,
}

impl Observation {
    /// The diagnostic carrying `code`, if there was one.
    fn find(&self, code: &str) -> Option<&Observed> {
        self.diagnostics.iter().find(|d| d.code == code)
    }
}

/// Parse the coded diagnostics out of a build's stderr.
///
/// The shape is fixed by `report::diagnostic_line`:
///
/// ```text
/// [ ERROR ] SNUG-INPUT-001: the explanation
///           hint: what to do about it
/// ```
///
/// but a message is not always one line. An `anyhow` chain renders as
/// the context, a blank line, `Caused by:`, and the numbered causes —
/// and one message (`SNUG-OUTPUT-001`) explains itself over three lines
/// ending in a `hint:` of its own. So the block is read until the next
/// thing that is *not* part of it, and the reporter's hint — which is
/// indented, unlike a `hint:` inside a message — is pulled out of it.
fn parse_diagnostics(stderr: &str) -> Vec<Observed> {
    let all: Vec<&str> = stderr.lines().collect();
    let mut out: Vec<Observed> = Vec::new();
    let mut current: Option<Observed> = None;
    let mut lines: Vec<String> = Vec::new();

    for (index, line) in all.iter().enumerate() {
        if let Some((severity, rest)) = split_severity(line) {
            flush(&mut current, &mut lines, &mut out);
            let (code, message) = rest.split_once(": ").unwrap_or((rest, ""));
            current = Some(Observed {
                severity,
                code: code.trim().to_string(),
                message: String::new(),
                hint: None,
            });
            lines.push(message.trim_end().to_string());
            continue;
        }
        if current.is_none() {
            continue;
        }
        if line_role(&all, index) == Role::End {
            flush(&mut current, &mut lines, &mut out);
            continue;
        }
        lines.push((*line).to_string());
    }
    flush(&mut current, &mut lines, &mut out);
    out
}

/// Finish the diagnostic being read.
fn flush(current: &mut Option<Observed>, lines: &mut Vec<String>, out: &mut Vec<Observed>) {
    let Some(mut diagnostic) = current.take() else {
        return;
    };
    // The reporter's hint is indented under its diagnostic. A `hint:` at
    // column 0 is part of the message — one message carries its own.
    if let Some(last) = lines.last()
        && last.starts_with(' ')
        && last.trim_start().starts_with("hint: ")
    {
        diagnostic.hint = Some(last.trim_start()["hint: ".len()..].trim().to_string());
        lines.pop();
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    diagnostic.message = lines.join("\n");
    out.push(diagnostic);
}

/// What a line inside a diagnostic block is.
#[derive(PartialEq)]
enum Role {
    /// Part of the message.
    Continue,
    /// The end of the block: something else entirely.
    End,
}

/// Decide whether a line belongs to the diagnostic above it.
fn line_role(lines: &[&str], index: usize) -> Role {
    let line = lines[index];
    // The head of an `anyhow` chain.
    if line.trim_start() == "Caused by:" {
        return Role::Continue;
    }
    // A *run* of summary rows is the report's own tail — the effective
    // options, the dry-run summary, the closing counts. A lone row that
    // looks like one is not: clap's own `  tip:` line sits inside a
    // message, and it is followed by a blank line rather than by
    // another row.
    if starts_summary_run(lines, index) {
        return Role::End;
    }
    if line.starts_with(' ') {
        // A deeper indent is a chain line, which belongs to the message.
        return Role::Continue;
    }
    if line.trim().is_empty() {
        return Role::Continue;
    }
    let trimmed = line.trim_start();
    if trimmed.starts_with("snug: ")
        || trimmed.starts_with("Effective build options:")
        || trimmed.starts_with("Build succeeded.")
        || trimmed.starts_with("Build failed during ")
    {
        return Role::End;
    }
    Role::Continue
}

/// Whether the summary rows start at `index`.
fn starts_summary_run(lines: &[&str], index: usize) -> bool {
    if !is_summary_row(lines[index]) {
        return false;
    }
    let Some(next) = lines.get(index + 1) else {
        // A single row at the very end of the output.
        return true;
    };
    if is_summary_row(next) {
        return true;
    }
    let trimmed = next.trim_start();
    trimmed.starts_with("Build succeeded.") || trimmed.starts_with("Build failed during ")
}

/// Whether a line is one of snug's own `  label: value` summary rows.
fn is_summary_row(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("  ") else {
        return false;
    };
    if rest.starts_with(' ') {
        return false;
    }
    let Some((label, _)) = rest.split_once(':') else {
        return false;
    };
    !label.is_empty()
        && label.starts_with(|c: char| c.is_ascii_alphabetic())
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-')
}

/// Split a severity prefix off a line.
fn split_severity(line: &str) -> Option<(Severity, &str)> {
    if let Some(rest) = line.strip_prefix("[ ERROR ] ") {
        Some((Severity::Error, rest))
    } else if let Some(rest) = line.strip_prefix("[ WARN ] ") {
        Some((Severity::Warning, rest))
    } else if let Some(rest) = line.strip_prefix("[ INFO ] ") {
        Some((Severity::Info, rest))
    } else {
        None
    }
}

/// Run a build and collect what it said.
fn observe(success: bool, stderr: String) -> Observation {
    let diagnostics = parse_diagnostics(&stderr);
    Observation {
        success,
        diagnostics,
        stderr,
    }
}

/// Collect what a reporter collected.
///
/// The two diagnostics that cannot be produced from the command line —
/// a log write that failed mid-run, and the catch-all for an error snug
/// did not anticipate — are raised through the library, and the library
/// is where they are observed.
fn observe_reporter(reporter: &Reporter) -> Observation {
    Observation {
        success: !reporter.has_errors(),
        diagnostics: reporter
            .diagnostics()
            .iter()
            .map(|d| Observed {
                severity: d.severity,
                code: d.code.to_string(),
                message: d.message.clone(),
                hint: d.hint.clone(),
            })
            .collect(),
        stderr: String::new(),
    }
}

/// Run `snug` in `dir` with `args`.
fn run_snug(dir: &Path, args: &[&str]) -> Observation {
    let output = Command::new(snug_bin())
        .current_dir(dir)
        .args(args)
        .output()
        .expect("spawn snug");
    observe(
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A class file for the given entry point.
fn class_file(major: u16, minor: u16, name: &str, declares_main: bool) -> Vec<u8> {
    let methods: &[common::MethodSpec] = if declares_main {
        &[(common::PUBLIC_STATIC, "main", common::MAIN_DESCRIPTOR)]
    } else {
        &[]
    };
    common::build_class_with_minor(name, "java/lang/Object", major, minor, methods)
}

/// Write a JAR with a manifest and a list of `(entry name, contents)`.
fn write_jar_entries(path: &Path, manifest: &str, entries: &[(&str, Vec<u8>)]) {
    use std::io::Write;
    use zip::CompressionMethod;
    use zip::write::SimpleFileOptions;

    let file = fs::File::create(path).expect("create jar");
    let mut zip = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    zip.start_file("META-INF/MANIFEST.MF", opts)
        .expect("start_file");
    zip.write_all(manifest.as_bytes()).expect("write manifest");
    for (name, bytes) in entries {
        zip.start_file(*name, opts).expect("start_file");
        zip.write_all(bytes).expect("write entry");
    }
    zip.finish().expect("finish zip");
}

/// A JAR declaring `main_class`, holding one class file.
fn write_jar(path: &Path, main_class: Option<&str>) {
    write_jar_with_class(path, main_class, "com/example/Main.class", 61, 0, true);
}

/// A JAR holding one class file, with the version and entry point given.
fn write_jar_with_class(
    path: &Path,
    main_class: Option<&str>,
    entry: &str,
    major: u16,
    minor: u16,
    declares_main: bool,
) {
    let manifest = match main_class {
        Some(class) => format!("Manifest-Version: 1.0\nMain-Class: {class}\n"),
        None => "Manifest-Version: 1.0\n".to_string(),
    };
    let name = entry.trim_end_matches(".class").replace('/', ".");
    write_jar_entries(
        path,
        &manifest,
        &[(entry, class_file(major, minor, &name, declares_main))],
    );
}

/// A JAR whose manifest is stored uncompressed and then corrupted.
///
/// The archive still opens — the central directory is intact — but
/// reading the manifest fails its checksum. That is what
/// `SNUG-MANIFEST-001` is for.
fn write_jar_with_corrupt_manifest(path: &Path) {
    use std::io::Write;
    use zip::CompressionMethod;
    use zip::write::SimpleFileOptions;

    let file = fs::File::create(path).expect("create jar");
    let mut zip = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    zip.start_file("META-INF/MANIFEST.MF", opts)
        .expect("start_file");
    zip.write_all(b"Manifest-Version: 1.0\nMain-Class: com.example.Main\n")
        .expect("write manifest");
    zip.start_file("com/example/Main.class", opts)
        .expect("start_file");
    zip.write_all(&class_file(61, 0, "com.example.Main", true))
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

/// A PNG larger than the launcher's preferred splash size.
fn write_oversized_png(path: &Path, width: u32, height: u32) {
    image::RgbImage::new(width, height)
        .save(path)
        .expect("write splash png");
}

// ---------------------------------------------------------------------------
// The scenarios
// ---------------------------------------------------------------------------

/// One diagnostic, and the build that raises it.
struct Scenario {
    /// The code the scenario must raise.
    code: &'static str,
    /// The severity it must carry.
    severity: Severity,
    /// The scenario's name, for the catalogue.
    name: &'static str,
    /// The build that raises it.
    run: fn(&Path) -> Observation,
}

/// Declare the scenarios, and a test for each.
///
/// The macro exists so the coverage test and the per-code tests read the
/// *same* list: a scenario that exists but is not in the table would
/// satisfy neither, and a code in the table with no scenario would fail
/// the coverage test loudly.
macro_rules! scenarios {
    ($( $name:ident => ($code:expr, $severity:expr, $run:expr) ),* $(,)?) => {
        const SCENARIOS: &[Scenario] = &[
            $(
                Scenario {
                    code: $code,
                    severity: $severity,
                    name: stringify!($name),
                    run: $run,
                },
            )*
        ];

        $(
            #[test]
            fn $name() {
                let dir = tempdir();
                let observation = ($run)(&dir);
                assert_raises(&observation, $code, $severity);
            }
        )*
    };
}

scenarios! {
    cfg_parse_is_reported => (
        codes::CFG_PARSE,
        Severity::Error,
        scenario_cfg_parse
    ),
    cfg_invalid_is_reported => (
        codes::CFG_INVALID,
        Severity::Error,
        scenario_cfg_invalid
    ),
    input_unavailable_is_reported => (
        codes::INPUT_UNAVAILABLE,
        Severity::Error,
        scenario_input_unavailable
    ),
    input_entry_unreadable_is_reported => (
        codes::INPUT_ENTRY_UNREADABLE,
        Severity::Warning,
        scenario_input_entry_unreadable
    ),
    jar_invalid_is_reported => (
        codes::JAR_INVALID,
        Severity::Error,
        scenario_jar_invalid
    ),
    manifest_unusable_is_reported => (
        codes::MANIFEST_UNUSABLE,
        Severity::Error,
        scenario_manifest_unusable
    ),
    main_none_is_reported => (
        codes::MAIN_NONE,
        Severity::Error,
        scenario_main_none
    ),
    main_absent_is_reported => (
        codes::MAIN_ABSENT,
        Severity::Error,
        scenario_main_absent
    ),
    main_unverified_is_reported => (
        codes::MAIN_UNVERIFIED,
        Severity::Warning,
        scenario_main_unverified
    ),
    main_conflict_is_reported => (
        codes::MAIN_CONFLICT,
        Severity::Warning,
        scenario_main_conflict
    ),
    main_duplicate_is_reported => (
        codes::MAIN_DUPLICATE,
        Severity::Warning,
        scenario_main_duplicate
    ),
    dep_classpath_is_reported => (
        codes::DEP_CLASSPATH,
        Severity::Warning,
        scenario_dep_classpath
    ),
    dep_nested_is_reported => (
        codes::DEP_NESTED,
        Severity::Info,
        scenario_dep_nested
    ),
    java_too_new_is_reported => (
        codes::JAVA_TOO_NEW,
        Severity::Error,
        scenario_java_too_new
    ),
    java_preview_is_reported => (
        codes::JAVA_PREVIEW,
        Severity::Warning,
        scenario_java_preview
    ),
    resource_invalid_is_reported => (
        codes::RESOURCE_INVALID,
        Severity::Error,
        scenario_resource_invalid
    ),
    resource_oversized_is_reported => (
        codes::RESOURCE_OVERSIZED,
        Severity::Warning,
        scenario_resource_oversized
    ),
    localisation_invalid_is_reported => (
        codes::LOCALISATION_INVALID,
        Severity::Warning,
        scenario_localisation_invalid
    ),
    output_collision_is_reported => (
        codes::OUTPUT_COLLISION,
        Severity::Error,
        scenario_output_collision
    ),
    output_write_is_reported => (
        codes::OUTPUT_WRITE,
        Severity::Error,
        scenario_output_write
    ),
    log_write_is_reported => (
        codes::LOG_WRITE,
        Severity::Warning,
        scenario_log_write
    ),
    log_unavailable_is_reported => (
        codes::LOG_UNAVAILABLE,
        Severity::Error,
        scenario_log_unavailable
    ),
    internal_is_reported => (
        codes::INTERNAL,
        Severity::Error,
        scenario_internal
    ),
}

// --- Configuration ---------------------------------------------------------

/// A flag that does not exist.
fn scenario_cfg_parse(dir: &Path) -> Observation {
    run_snug(dir, &["--not-a-flag"])
}

/// No input at all.
fn scenario_cfg_invalid(dir: &Path) -> Observation {
    run_snug(dir, &[])
}

// --- Input -----------------------------------------------------------------

/// An input path that is not there.
fn scenario_input_unavailable(dir: &Path) -> Observation {
    run_snug(dir, &["missing.jar"])
}

/// A directory holding one good JAR and one that cannot be read.
fn scenario_input_entry_unreadable(dir: &Path) -> Observation {
    let jars = dir.join("jars");
    fs::create_dir_all(&jars).expect("create jars dir");
    write_jar(&jars.join("a-good.jar"), Some("com.example.Main"));
    // An empty JAR holds nothing to embed, so it is left out of the
    // build — and the build says so.
    fs::write(jars.join("b-empty.jar"), b"").expect("write empty jar");

    run_snug(dir, &["--input", "jars", "--dry-run"])
}

// --- Archives --------------------------------------------------------------

/// A file that is handed over as a JAR but is not a ZIP.
fn scenario_jar_invalid(dir: &Path) -> Observation {
    fs::write(dir.join("not-a-jar.jar"), b"this is not a ZIP archive").expect("write jar");
    run_snug(dir, &["not-a-jar.jar", "--dry-run"])
}

/// A JAR whose manifest cannot be read.
fn scenario_manifest_unusable(dir: &Path) -> Observation {
    write_jar_with_corrupt_manifest(&dir.join("corrupt.jar"));
    run_snug(dir, &["corrupt.jar", "--dry-run"])
}

// --- Entry point -----------------------------------------------------------

/// A JAR that declares no entry point.
fn scenario_main_none(dir: &Path) -> Observation {
    write_jar(&dir.join("demo.jar"), None);
    run_snug(dir, &["demo.jar", "--dry-run"])
}

/// A class that is in none of the supplied JARs.
fn scenario_main_absent(dir: &Path) -> Observation {
    write_jar(&dir.join("demo.jar"), Some("com.example.Main"));
    run_snug(
        dir,
        &[
            "demo.jar",
            "--main-class",
            "com.example.Absent",
            "--dry-run",
        ],
    )
}

/// A class that declares no `main` and is not a JavaFX application.
fn scenario_main_unverified(dir: &Path) -> Observation {
    write_jar_with_class(
        &dir.join("demo.jar"),
        Some("com.example.Main"),
        "com/example/Main.class",
        61,
        0,
        false,
    );
    run_snug(
        dir,
        &["demo.jar", "--main-class", "com.example.Main", "--dry-run"],
    )
}

/// Two JARs that declare different entry points.
fn scenario_main_conflict(dir: &Path) -> Observation {
    let jars = dir.join("jars");
    fs::create_dir_all(&jars).expect("create jars dir");
    write_jar_with_class(
        &jars.join("a-first.jar"),
        Some("com.example.First"),
        "com/example/First.class",
        61,
        0,
        true,
    );
    write_jar_with_class(
        &jars.join("b-second.jar"),
        Some("com.example.Second"),
        "com/example/Second.class",
        61,
        0,
        true,
    );
    run_snug(dir, &["--input", "jars", "--dry-run"])
}

/// Two JARs that both carry the same entry class.
fn scenario_main_duplicate(dir: &Path) -> Observation {
    let jars = dir.join("jars");
    fs::create_dir_all(&jars).expect("create jars dir");
    write_jar(&jars.join("a-first.jar"), Some("com.example.Main"));
    write_jar(&jars.join("b-second.jar"), Some("com.example.Main"));
    run_snug(dir, &["--input", "jars", "--dry-run"])
}

// --- Dependencies ----------------------------------------------------------

/// A manifest that names a classpath entry snug does not bundle.
fn scenario_dep_classpath(dir: &Path) -> Observation {
    let manifest = "Manifest-Version: 1.0\nMain-Class: com.example.Main\nClass-Path: other.jar\n";
    write_jar_entries(
        &dir.join("demo.jar"),
        manifest,
        &[(
            "com/example/Main.class",
            class_file(61, 0, "com.example.Main", true),
        )],
    );
    run_snug(dir, &["demo.jar", "--dry-run"])
}

/// A JAR inside a JAR.
fn scenario_dep_nested(dir: &Path) -> Observation {
    let entries: Vec<(&str, Vec<u8>)> = vec![
        (
            "com/example/Main.class",
            class_file(61, 0, "com.example.Main", true),
        ),
        // The bytes do not have to be a real archive: only the name has
        // to end in `.jar` for snug to notice it.
        ("lib/inner.jar", vec![0x50, 0x4B, 0x03, 0x04]),
    ];
    write_jar_entries(
        &dir.join("demo.jar"),
        "Manifest-Version: 1.0\nMain-Class: com.example.Main\n",
        &entries,
    );
    run_snug(dir, &["demo.jar", "--dry-run"])
}

// --- Bytecode --------------------------------------------------------------

/// An entry class that needs a newer Java than `--min-java` allows.
fn scenario_java_too_new(dir: &Path) -> Observation {
    // Major 69 is Java 25; the build asks for Java 17.
    write_jar_with_class(
        &dir.join("demo.jar"),
        Some("com.example.Main"),
        "com/example/Main.class",
        69,
        0,
        true,
    );
    run_snug(dir, &["demo.jar", "--min-java", "17", "--dry-run"])
}

/// A class compiled with preview features enabled.
fn scenario_java_preview(dir: &Path) -> Observation {
    // Minor 0xFFFF is the JVM's preview marker.
    write_jar_with_class(
        &dir.join("demo.jar"),
        Some("com.example.Main"),
        "com/example/Main.class",
        61,
        0xFFFF,
        true,
    );
    run_snug(dir, &["demo.jar", "--dry-run"])
}

// --- Resources -------------------------------------------------------------

/// A Windows manifest that is not well-formed XML.
fn scenario_resource_invalid(dir: &Path) -> Observation {
    write_jar(&dir.join("demo.jar"), Some("com.example.Main"));
    fs::write(
        dir.join("broken.manifest"),
        "<assembly><trustInfo><requestedPrivileges>",
    )
    .expect("write manifest");
    run_snug(
        dir,
        &["demo.jar", "--manifest", "broken.manifest", "--dry-run"],
    )
}

/// A splash image larger than the launcher's preferred size.
fn scenario_resource_oversized(dir: &Path) -> Observation {
    write_jar(&dir.join("demo.jar"), Some("com.example.Main"));
    write_oversized_png(&dir.join("big.png"), 700, 400);
    run_snug(dir, &["demo.jar", "--splash", "big.png", "--dry-run"])
}

// --- Localisation ----------------------------------------------------------

/// A translation bundle that does not cover the English baseline.
fn scenario_localisation_invalid(dir: &Path) -> Observation {
    write_jar(&dir.join("demo.jar"), Some("com.example.Main"));
    fs::write(
        dir.join("snug-localisations.de.txt"),
        "err.zip = ZIP-Fehler: {0}\n",
    )
    .expect("write bundle");
    run_snug(
        dir,
        &[
            "demo.jar",
            "--localisation",
            "snug-localisations.de.txt",
            "--dry-run",
        ],
    )
}

// --- Output and log --------------------------------------------------------

/// An output path that is one of the build's own inputs.
fn scenario_output_collision(dir: &Path) -> Observation {
    write_jar(&dir.join("demo.jar"), Some("com.example.Main"));
    run_snug(dir, &["demo.jar", "-o", "demo.jar", "--dry-run"])
}

/// An output path that cannot be written.
fn scenario_output_write(dir: &Path) -> Observation {
    write_jar(&dir.join("demo.jar"), Some("com.example.Main"));
    // The output path names a directory. The EXE is staged beside it and
    // renamed into place, and the rename cannot replace a directory —
    // so the build fails at the write, with the output code.
    fs::create_dir_all(dir.join("demo.exe")).expect("create dir");
    run_snug(dir, &["demo.jar", "-o", "demo.exe"])
}

/// A log that could not be written completely.
///
/// A full disk cannot be arranged in a test, so the log is told to
/// fail instead — and the *reporting* of that is what is under test.
fn scenario_log_write(_dir: &Path) -> Observation {
    let mut reporter = Reporter::new(BuildLog::failing());
    reporter.report_log_write_failure();
    observe_reporter(&reporter)
}

/// A log destination that cannot be opened.
fn scenario_log_unavailable(dir: &Path) -> Observation {
    write_jar(&dir.join("demo.jar"), Some("com.example.Main"));
    // The parent directory does not exist, and snug does not create
    // directories, so the open fails with the operating system's own
    // "path not found".
    run_snug(dir, &["demo.jar", "--build-log", "missing-dir/build.log"])
}

/// An error snug did not anticipate.
///
/// This is the catch-all for bugs, so there is no input that reaches it
/// on purpose — every input that *can* be reached has its own code. The
/// mapping the binary uses is called directly instead, which is the only
/// honest way to exercise it.
fn scenario_internal(_dir: &Path) -> Observation {
    let mut reporter = Reporter::new(BuildLog::none());
    snug_cli::report::report_payload_error(
        &mut reporter,
        &anyhow::anyhow!("something snug did not anticipate"),
    );
    observe_reporter(&reporter)
}

// ---------------------------------------------------------------------------
// The guards
// ---------------------------------------------------------------------------

/// Assert that a scenario raised `code`, at `severity`.
fn assert_raises(observation: &Observation, code: &str, severity: Severity) {
    let Some(found) = observation.find(code) else {
        panic!(
            "expected {code} to be raised, but the build raised {}.\nstderr:\n{}",
            if observation.diagnostics.is_empty() {
                "nothing".to_string()
            } else {
                observation
                    .diagnostics
                    .iter()
                    .map(|d| d.code.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            },
            observation.stderr,
        );
    };
    assert_eq!(
        found.severity, severity,
        "{code} was raised at the wrong severity: {observation:?}"
    );
    // The severity is not decoration: an error stops the build, and
    // everything else lets it finish. A code that says "error" but
    // leaves a half-built executable behind is a code that lies.
    match severity {
        Severity::Error => assert!(
            !observation.success,
            "{code} is an error, so the build must not have succeeded: {observation:?}"
        ),
        Severity::Warning | Severity::Info => assert!(
            observation.success,
            "{code} is only a {severity:?}, so the build must have carried on: {observation:?}"
        ),
    }
}

#[test]
fn every_code_can_be_raised() {
    let mut observed: BTreeSet<&str> = BTreeSet::new();
    for scenario in SCENARIOS {
        let dir = tempdir();
        let observation = (scenario.run)(&dir);
        assert_raises(&observation, scenario.code, scenario.severity);
        observed.insert(scenario.code);
    }

    let declared: BTreeSet<&str> = codes::ALL.iter().copied().collect();
    let missing: Vec<&str> = declared.difference(&observed).copied().collect();
    let extra: Vec<&str> = observed.difference(&declared).copied().collect();

    assert!(
        missing.is_empty(),
        "these codes are declared but no build can raise them: {missing:?}"
    );
    assert!(
        extra.is_empty(),
        "these codes are raised but not declared in codes::ALL: {extra:?}"
    );
}

#[test]
fn the_documentation_lists_every_code() {
    // Read at run time rather than embedded: `docs/` is outside this
    // package, so cargo does not treat a change to it as a reason to
    // rebuild — an embedded copy would go stale without the test ever
    // noticing.
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("docs")
        .join("diagnostics.md");
    let docs = fs::read_to_string(&path).expect("read docs/diagnostics.md");

    // The codes are the ones in the `#### \`SNUG-…\`` headings.
    let documented: BTreeSet<&str> = docs
        .lines()
        .filter_map(|line| line.strip_prefix("#### `"))
        .filter_map(|rest| rest.split('`').next())
        .filter(|code| code.starts_with("SNUG-"))
        .collect();
    let declared: BTreeSet<&str> = codes::ALL.iter().copied().collect();

    assert_eq!(
        documented, declared,
        "the documented codes and the declared codes have drifted apart"
    );
}

#[test]
fn the_catalogue_lists_every_diagnostic() {
    let mut catalogue = String::new();
    catalogue.push_str("# SNUG diagnostic catalogue\n\n");
    catalogue.push_str(
        "Every diagnostic the builder can raise, with the message it actually\n\
         renders. Generated by `crates/snug-cli/tests/diagnostics.rs` — do not\n\
         edit it by hand; run the test to regenerate it.\n",
    );

    for scenario in SCENARIOS {
        let dir = tempdir();
        let observation = (scenario.run)(&dir);
        assert_raises(&observation, scenario.code, scenario.severity);
        let found = observation
            .find(scenario.code)
            .expect("the scenario raised its code");

        catalogue.push_str("\n## `");
        catalogue.push_str(scenario.code);
        catalogue.push_str("` — ");
        catalogue.push_str(match scenario.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        });
        catalogue.push_str("\n\nRaised by: `");
        catalogue.push_str(scenario.name);
        catalogue.push_str("`\n\n```text\n");
        catalogue.push_str(&format!(
            "{} {}: {}\n",
            found.severity.prefix(),
            found.code,
            found.message
        ));
        if let Some(hint) = &found.hint {
            catalogue.push_str(&format!("{indent}hint: {hint}\n", indent = " ".repeat(9)));
        }
        catalogue.push_str("```\n");
    }

    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("diagnostics-catalogue.md");
    fs::write(&path, &catalogue).expect("write the catalogue");
    println!("{catalogue}");
    println!("catalogue written to {}", path.display());
}
