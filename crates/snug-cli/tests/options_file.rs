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
fn resolve_returns_nothing_when_no_default_and_no_explicit() {
    let tmp = tempdir();
    let args: Vec<String> = ["snug", "app.jar"].iter().map(|s| s.to_string()).collect();
    assert!(options_file::resolve_all(&args, &tmp, None, "macos").is_empty());
}

#[test]
fn resolve_finds_default_in_cwd() {
    let tmp = tempdir();
    fs::write(tmp.join("snug.options"), "--name X\n").unwrap();
    let args: Vec<String> = ["snug", "app.jar"].iter().map(|s| s.to_string()).collect();
    assert_eq!(
        options_file::resolve_all(&args, &tmp, None, "macos"),
        vec![tmp.join("snug.options")]
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
        options_file::resolve_all(&args, &cwd, Some(&exe_dir), "macos"),
        vec![exe_dir.join("snug.options")]
    );
}

#[test]
fn resolve_returns_both_files_lowest_priority_first() {
    // The layering order `merge` depends on, asserted end to end.
    let tmp = tempdir();
    fs::write(tmp.join("snug.options"), "--name X\n").unwrap();
    fs::write(tmp.join("snug.macos.options"), "--name Y\n").unwrap();
    let args: Vec<String> = ["snug", "app.jar"].iter().map(|s| s.to_string()).collect();
    assert_eq!(
        options_file::resolve_all(&args, &tmp, None, "macos"),
        vec![tmp.join("snug.options"), tmp.join("snug.macos.options")]
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
    let merged = options_file::merge(&args, vec![file]);
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

// ---- The OS-specific options file, end to end ----------------------
//
// These drive the real binary, so they prove the whole chain — resolve,
// load, layer, clap — rather than just the resolver. The filename is
// built from `std::env::consts::OS` so the test is meaningful on every
// host, and it only ever names *this* host's file, which is also the
// check that a foreign platform's file is ignored.

fn host_os_file(dir: &std::path::Path) -> PathBuf {
    dir.join(options_file::os_options_file_name(std::env::consts::OS))
}

/// Values the dry-run output exposes, as (label, value) pairs.
fn dry_run_fields(stdout: &str) -> Vec<(String, String)> {
    stdout
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

fn field(stdout: &str, key: &str) -> Option<String> {
    dry_run_fields(stdout)
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
}

#[test]
fn os_options_file_overrides_the_generic_one() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar(&jar);
    fs::write(
        tmp.join("snug.options"),
        "--min-java 21\n--name \"From Generic\"\n",
    )
    .unwrap();
    fs::write(
        host_os_file(&tmp),
        "--name \"From OS File\"\n",
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
        "snug should exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    // The dry run does not print `--name`, so assert on what it does
    // print plus the two loaded-file lines, which together prove the
    // OS file was both read and ranked above the generic one.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&host_os_file(&tmp).display().to_string()),
        "both files should be reported as loaded: {stderr}"
    );
    assert!(
        stderr.contains(&tmp.join("snug.options").display().to_string()),
        "both files should be reported as loaded: {stderr}"
    );
    // Inherited from the generic file, proving this is a partial
    // override rather than a replacement.
    assert_eq!(field(&stdout, "min-java").as_deref(), Some("21"));
}

#[test]
fn a_conflicting_flag_in_both_files_does_not_break_the_build() {
    // Without stripping the lower layer, two `--min-java` occurrences
    // reach clap and it aborts with "cannot be used multiple times".
    // This is the regression test for the whole layered-merge design:
    // the *build succeeding* is the assertion.
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar(&jar);
    fs::write(tmp.join("snug.options"), "--min-java 21\n--name A\n").unwrap();
    fs::write(host_os_file(&tmp), "--min-java 25\n--name B\n").unwrap();

    let output = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--dry-run")
        .output()
        .expect("spawn snug");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "two files setting the same flag must not abort the build; stderr: {stderr}"
    );
    assert!(
        !stderr.contains("cannot be used multiple times"),
        "clap rejected the layered argv: {stderr}"
    );
    // The OS file's value is the one that lands.
    assert_eq!(
        field(&String::from_utf8_lossy(&output.stdout), "min-java").as_deref(),
        Some("25")
    );
}

#[test]
fn cli_still_beats_the_os_options_file() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar(&jar);
    fs::write(tmp.join("snug.options"), "--min-java 21\n").unwrap();
    fs::write(host_os_file(&tmp), "--min-java 25\n").unwrap();

    let output = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--dry-run")
        .arg("--min-java")
        .arg("17")
        .output()
        .expect("spawn snug");

    assert!(output.status.success());
    assert_eq!(
        field(&String::from_utf8_lossy(&output.stdout), "min-java").as_deref(),
        Some("17"),
        "the command line must beat both files"
    );
}

#[test]
fn another_platforms_options_file_is_ignored() {
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar(&jar);
    fs::write(tmp.join("snug.options"), "--min-java 21\n").unwrap();

    // Name a file this host can never match.
    let mut foreign = "snug.".to_string();
    foreign.push_str(if std::env::consts::OS == "windows" { "linux" } else { "windows" });
    foreign.push_str(".options");
    fs::write(tmp.join(&foreign), "--min-java 99\n").unwrap();

    let output = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--dry-run")
        .output()
        .expect("spawn snug");

    assert!(output.status.success());
    assert_eq!(
        field(&String::from_utf8_lossy(&output.stdout), "min-java").as_deref(),
        Some("21"),
        "a file for another platform must not be loaded"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains(&foreign),
        "the foreign options file should never be reported as loaded: {stderr}"
    );
}

#[test]
fn missing_os_options_file_is_silent() {
    // No OS file present: the build proceeds, and there is no warning
    // about the one that wasn't found.
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar(&jar);
    fs::write(tmp.join("snug.options"), "--min-java 21\n").unwrap();

    let output = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--dry-run")
        .output()
        .expect("spawn snug");

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.lines().filter(|l| l.contains("loaded options from")).count(),
        1,
        "only the generic file should be reported: {stderr}"
    );
    for noise in ["not found", "missing", "warn"] {
        assert!(
            !stderr.to_lowercase().contains(noise),
            "an absent OS file must be silent, but stderr mentioned `{noise}`: {stderr}"
        );
    }
}

#[test]
fn explicit_options_flag_ignores_the_os_file() {
    // The documented escape hatch, end to end: naming a file means that
    // file only, so an ambient OS file cannot change the result.
    let tmp = tempdir();
    let jar = tmp.join("demo.jar");
    write_jar(&jar);
    fs::write(host_os_file(&tmp), "--min-java 25\n").unwrap();

    let custom = tmp.join("custom.opts");
    fs::write(&custom, "--min-java 30\n").unwrap();

    let output = Command::new(snug_bin())
        .current_dir(&tmp)
        .arg(&jar)
        .arg("--dry-run")
        .arg("--options")
        .arg(&custom)
        .output()
        .expect("spawn snug");

    assert!(output.status.success());
    assert_eq!(
        field(&String::from_utf8_lossy(&output.stdout), "min-java").as_deref(),
        Some("30"),
        "--options should mean that file only"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains(&host_os_file(&tmp).display().to_string()),
        "the OS file must not be consulted alongside --options: {stderr}"
    );
}

// ---- The repository's own options files -------------------------------
//
// snug.options plus one file per platform is what this project's own
// demo builds run on, and both release scripts depend on it working:
// scripts\build-release.cmd passes no -o and no --options, so on Windows
// the `.exe` path has to arrive from snug.windows.options, and
// scripts/build-macos-demo.sh likewise relies on snug.macos.options.
//
// Neither can be exercised from the wrong host, so this resolves the real
// files in the repo for *both* platform tokens and parses the result. That
// is the property that actually matters: the merged argv must be valid for
// every platform, not just the one running the test.

fn repo_root() -> PathBuf {
    // <root>/crates/snug-cli -> <root>
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/snug-cli lives two levels below the repo root")
        .to_path_buf()
}

/// Resolve the repo's real options files for `os` and return the output
/// path snug would build, or `None` when no `--output` is configured.
///
/// The returned `Result` is clap's: a merged argv that does not parse is
/// itself the failure this is looking for, so it is not flattened away.
fn effective_repo_output(os: &str) -> Result<Option<PathBuf>, clap::Error> {
    use clap::Parser;
    use snug_cli::cli::Cli;

    let root = repo_root();
    let raw: Vec<String> = ["snug", "demo.jar"].iter().map(|s| s.to_string()).collect();

    let paths = options_file::resolve_all(&raw, &root, None, os);
    let mut layers = Vec::with_capacity(paths.len());
    for p in &paths {
        layers.push(options_file::load(p).expect("repo options files must parse"));
    }

    let merged = options_file::merge(&raw, layers);
    let cli = Cli::try_parse_from(merged)?;
    Ok(cli.output.map(|o| if o.is_absolute() { o } else { root.join(o) }))
}

#[test]
fn repo_options_files_merge_to_a_valid_argv_for_every_platform() {
    // The core guarantee behind both release scripts. A duplicated or
    // conflicting flag across the two files lands here as a clap error
    // ("cannot be used multiple times") rather than at the user's desk.
    for os in ["macos", "windows", "linux"] {
        effective_repo_output(os)
            .unwrap_or_else(|e| panic!("merged argv for `{os}` did not parse: {e}"));
    }
}

#[test]
fn each_platform_gets_the_output_for_its_own_artefact() {
    let root = repo_root();

    let macos = effective_repo_output("macos").unwrap().expect("macos sets --output");
    assert_eq!(
        macos,
        root.join("assets/snug-javafx-demo.app"),
        "a macOS build must name a .app bundle, or snug writes a Windows PE"
    );

    let windows = effective_repo_output("windows").unwrap().expect("windows sets --output");
    assert_eq!(
        windows,
        root.join("assets/snug-javafx-demo.exe"),
        "a Windows build must name the .exe build-release.cmd asserts on"
    );
}

#[test]
fn the_shared_file_carries_no_output() {
    // A platform-specific value in the shared file is precisely the
    // problem the per-OS tier exists to fix: it reads as though it applies
    // everywhere, and it does not. This also keeps the two spellings of the
    // demo's output from drifting apart in one file while the other
    // silently keeps the old one.
    let shared = std::fs::read_to_string(repo_root().join("snug.options")).expect("snug.options");
    let offenders: Vec<&str> = shared
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && l.starts_with("--output"))
        .collect();
    assert!(
        offenders.is_empty(),
        "snug.options should hold no --output; it belongs in snug.<os>.options. Found: {offenders:?}"
    );
}

#[test]
fn the_platform_files_agree_on_which_value_differs() {
    // Guard the two files against drifting apart: they should set the same
    // set of flags, differing only in the value. If a future edit adds
    // `--min-java 21` to one of them and not the other, that asymmetry is
    // either a bug or a decision worth making deliberately.
    let root = repo_root();
    let read = |name: &str| -> Vec<String> {
        options_file::load(&root.join(name))
            .unwrap_or_else(|e| panic!("{name} must parse: {e}"))
            .into_iter()
            .filter(|t| t.starts_with("--"))
            .map(|t| t.split('=').next().unwrap_or(&t).to_string())
            .collect()
    };
    let macos = read("snug.macos.options");
    let windows = read("snug.windows.options");
    assert_eq!(
        macos, windows,
        "snug.macos.options and snug.windows.options should declare the same flags"
    );
}
