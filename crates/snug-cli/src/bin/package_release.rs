//! Package the staged release tree into the ZIP a user downloads.
//!
//! `scripts\windows\build.cmd` stages the shipping artefacts into
//! `release\windows-x86_64\`: `snug.exe`, the `Snug Quick Build.exe` shim,
//! the `snug_preview.exe` dialog preview and the demo JAR. That folder is
//! the *build output*; what a user downloads is one archive.
//!
//! ## The archive's shape
//!
//! ```text
//! snug-<version>-windows-x86_64.zip
//! └── snug/
//!     ├── snug.exe
//!     ├── snug_preview.exe
//!     ├── Snug Quick Build.exe
//!     ├── LICENSE
//!     ├── LICENSE-MIT
//!     ├── LICENSE-APACHE
//!     └── NOTICE
//! ```
//!
//! The inner folder is always `snug`, and the platform lives in the file
//! name. That is the same convention the release tree already uses — the
//! binary is always called `snug` and the platform is never baked into it
//! — so the documentation never has to name two different folders either.
//!
//! ## Why this is a Rust binary and not a shell one-liner
//!
//! Both of PowerShell's ways of making a zip write **backslash** entry
//! names. `Compress-Archive -Path <dir>` and
//! `[System.IO.Compression.ZipFile]::CreateFromDirectory(<dir>, <zip>, ..., $true)`
//! both produce `snug\snug.exe`, and the ZIP appnote requires forward
//! slashes in a member name. Windows unzippers tolerate it — `\` is a path
//! separator there anyway — but a POSIX extractor treats `snug\snug.exe` as
//! a single filename *containing* a backslash, so the archive unpacks into
//! one flat file rather than a `snug/` folder. The obvious .NET call
//! without `includeBaseDirectory` is worse still: it drops the root folder
//! entirely, which is the exact layout this archive exists to produce.
//!
//! The `zip` crate is already a dependency of this crate, so writing the
//! archive here costs no new dependency, and it lets every entry name be
//! explicit rather than inferred from a directory layout.
//!
//! ## What is deliberately not in the archive
//!
//! The demo JAR is copied *beside* the archive rather than into it, so a
//! release page can offer the application and the sample separately. It is
//! also the one payload that does not belong to someone who is here to
//! *build* an app.
//!
//! ## Verification
//!
//! The archive is reopened and read back after it is written, and every
//! entry is compared byte for byte against the staged file it came from. A
//! zip that quietly lost a file still looks exactly like a zip, and the
//! failure only surfaces when a user drags a JAR onto a missing shim.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipArchive, ZipWriter};

/// snug's own version, from the workspace `Cargo.toml`.
///
/// This is the version of *snug*, not of an app snug built: that one comes
/// from the `--version` in `snug.options`, which stamps the demo's own
/// version resource and has nothing to do with the release package.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The folder inside the archive.
///
/// Always `snug`, on every platform, for the reason given in the module
/// docs: the binary keeps its own name and the platform is never baked
/// into it.
const ROOT: &str = "snug";

/// The folder the archive and its sidecar are written into, inside the
/// staged tree.
///
/// Named for what it is *for*: these are the files that get attached to a
/// GitHub release. It sits inside the staged tree rather than beside it
/// so that one `release\windows-x86_64\` folder holds everything a release
/// consists of, and so the whole tree can still be uploaded in one go.
const OUT_DIR: &str = "github";

/// The artefacts that go *inside* the archive, in the order they are
/// written.
///
/// `snug.exe` and `Snug Quick Build.exe` travel together on purpose: the
/// shim resolves `snug.exe` relative to its own path, so an archive with
/// one and not the other is broken by construction. `snug_preview.exe` is
/// a dev tool and ships anyway — it is how you read snug's dialogs and
/// error copy without building and launching an app.
///
/// The licence texts travel with them, and that is not a nicety. MIT
/// requires its notice in "all copies or substantial portions of the
/// Software", and Apache-2.0 §4(a) requires a copy of the licence to
/// accompany redistribution. A release ZIP without these is a licence
/// violation rather than a documentation gap, so they are as load-bearing
/// as the binaries — which is why `run` refuses to build an archive that
/// is missing one.
const SHIPPED: &[&str] = &[
    "snug.exe",
    "snug_preview.exe",
    "Snug Quick Build.exe",
    "LICENSE",
    "LICENSE-MIT",
    "LICENSE-APACHE",
    "NOTICE",
];

/// Copied *beside* the archive rather than into it.
///
/// The name carries the platform because the demo JAR does: the JavaFX
/// demo's native libraries are per-platform (`glass.dll` vs
/// `libglass.dylib`), which is why `snug.windows.options` and
/// `snug.macos.options` name different inputs for the same demo.
const SIDECAR: &str = "snug-javafx-demo-windows.jar";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return ExitCode::SUCCESS;
    }

    match Options::parse(&args).and_then(|opts| run(&opts)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("package_release: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    println!("usage: package_release <source-dir> [--tag <tag>]");
    println!();
    println!("  Packages a staged release tree into <source-dir>\\{OUT_DIR}\\.");
    println!();
    println!("  <source-dir>   the staged tree, e.g. release\\windows-x86_64");
    println!("  --tag <tag>    the platform tag used in the archive's name.");
    println!("                 Defaults to the source directory's own name.");
}

/// What the command line asked for.
struct Options {
    /// The staged tree to package.
    source: PathBuf,
    /// The platform tag that goes into the archive's file name.
    tag: String,
}

impl Options {
    fn parse(args: &[String]) -> Result<Options> {
        let mut source: Option<PathBuf> = None;
        let mut tag: Option<String> = None;

        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--tag" => {
                    i += 1;
                    let value = args.get(i).ok_or_else(|| anyhow!("--tag needs a value"))?;
                    tag = Some(value.clone());
                }
                other if other.starts_with('-') => {
                    bail!("unknown option `{other}`");
                }
                other => {
                    if source.is_some() {
                        bail!(
                            "unexpected argument `{other}` — the source directory is already given"
                        );
                    }
                    source = Some(PathBuf::from(other));
                }
            }
            i += 1;
        }

        let source = source.ok_or_else(|| {
            anyhow!("no source directory given — run `package_release --help` for the usage")
        })?;

        // The tag defaults to the staged directory's own name, because that
        // name *is* the platform: `release\windows-x86_64\` is staged by
        // build.cmd, and `release\macos-arm64\` by build-macos.sh. Deriving
        // it means the archive's name cannot drift from the tree's.
        let tag = match tag {
            Some(tag) => tag,
            None => source
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    anyhow!(
                        "cannot derive the platform tag from `{}` — pass --tag <tag>",
                        source.display()
                    )
                })?
                .to_string(),
        };

        Ok(Options { source, tag })
    }
}

fn run(opts: &Options) -> Result<()> {
    let source = &opts.source;
    if !source.is_dir() {
        bail!("`{}` is not a directory", source.display());
    }

    // Everything is checked *before* anything is written. A partial
    // archive is worse than no archive: it looks complete, downloads
    // complete, and fails at the user's end.
    for name in SHIPPED {
        let path = source.join(name);
        if !path.is_file() {
            bail!(
                "`{}` is missing from the staged tree — the archive would ship an incomplete release",
                path.display()
            );
        }
    }
    let sidecar_source = source.join(SIDECAR);
    if !sidecar_source.is_file() {
        bail!(
            "`{}` is missing from the staged tree",
            sidecar_source.display()
        );
    }

    let out_dir = source.join(OUT_DIR);
    fs::create_dir_all(&out_dir).with_context(|| format!("creating `{}`", out_dir.display()))?;

    let zip_path = out_dir.join(format!("snug-{VERSION}-{}.zip", opts.tag));

    write_archive(&zip_path, source)?;
    let sidecar_path = out_dir.join(SIDECAR);
    copy_sidecar(&sidecar_source, &sidecar_path)?;

    // Read the archive back. The write path is not evidence.
    verify_archive(&zip_path, source)?;
    verify_sidecar(&sidecar_source, &sidecar_path)?;

    println!("package_release: {}", zip_path.display());
    println!("  entries:");
    for name in SHIPPED {
        println!("    {ROOT}/{name}");
    }
    println!("  sidecar: {}", sidecar_path.display());
    println!(
        "  archive: {} bytes, sha256 {}",
        fs::metadata(&zip_path)
            .context("reading the archive's size")?
            .len(),
        sha256_file(&zip_path)?
    );

    Ok(())
}

/// Write the archive.
///
/// The entry names are built here rather than derived from a directory
/// walk, which is the whole point: they are forward-slashed by
/// construction, and the root folder is a literal string rather than
/// whatever the staging directory happened to be called.
fn write_archive(zip_path: &Path, source: &Path) -> Result<()> {
    let file =
        File::create(zip_path).with_context(|| format!("creating `{}`", zip_path.display()))?;
    let mut zip = ZipWriter::new(file);

    for name in SHIPPED {
        let path = source.join(name);
        let bytes = fs::read(&path).with_context(|| format!("reading `{}`", path.display()))?;
        let modified = fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .with_context(|| format!("reading the mtime of `{}`", path.display()))?;

        // 0o644 rather than the default, which is the same thing: these are
        // data files, not scripts, and the low DOS attribute byte stays
        // zero, so nothing extracts as read-only on Windows.
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .unix_permissions(0o644)
            .last_modified_time(dos_datetime(modified));

        zip.start_file(format!("{ROOT}/{name}"), options)
            .with_context(|| format!("adding `{ROOT}/{name}` to the archive"))?;
        zip.write_all(&bytes)
            .with_context(|| format!("writing `{ROOT}/{name}` into the archive"))?;
    }

    zip.finish()
        .with_context(|| format!("finishing `{}`", zip_path.display()))?;
    Ok(())
}

/// Copy the demo JAR beside the archive.
fn copy_sidecar(from: &Path, to: &Path) -> Result<()> {
    fs::copy(from, to)
        .with_context(|| format!("copying `{}` to `{}`", from.display(), to.display()))?;
    Ok(())
}

/// Reopen the archive and prove it holds what it should.
///
/// Three things are checked, in increasing order of how quietly they
/// would otherwise fail:
///
///  1. the entry set is exactly the expected one — no missing file, and no
///     stray one that a later build left behind;
///  2. no entry name contains a backslash, because that is the defect
///     every PowerShell zip route introduces and the one that still
///     unpacks correctly on Windows, so it would pass a Windows-only
///     smoke test;
///  3. every entry's bytes match the staged file it came from.
fn verify_archive(zip_path: &Path, source: &Path) -> Result<()> {
    let file =
        File::open(zip_path).with_context(|| format!("reopening `{}`", zip_path.display()))?;
    let mut archive = ZipArchive::new(file).context("reading the archive back")?;

    let mut found: Vec<String> = archive.file_names().map(|name| name.to_string()).collect();
    found.sort();

    let mut expected: Vec<String> = SHIPPED
        .iter()
        .map(|name| format!("{ROOT}/{name}"))
        .collect();
    expected.sort();

    if found != expected {
        bail!(
            "the archive holds {found:?} but should hold {expected:?} — \
             it was written from a different set of files than it claims"
        );
    }

    for name in &found {
        if name.contains('\\') {
            bail!(
                "entry `{name}` uses a backslash — a POSIX extractor would read that as one \
                 filename containing a backslash, not a path"
            );
        }
    }

    for name in SHIPPED {
        let entry_name = format!("{ROOT}/{name}");
        let mut entry = archive
            .by_name(&entry_name)
            .with_context(|| format!("reading `{entry_name}` back out of the archive"))?;
        let mut archived = Vec::new();
        entry
            .read_to_end(&mut archived)
            .with_context(|| format!("reading `{entry_name}` back out of the archive"))?;

        let staged = fs::read(source.join(name))
            .with_context(|| format!("re-reading `{}` to compare", source.join(name).display()))?;
        if archived != staged {
            bail!(
                "`{entry_name}` in the archive differs from the staged file — \
                 the archive does not hold what it claims to hold"
            );
        }
    }

    Ok(())
}

/// Prove the copied sidecar is the file it claims to be.
fn verify_sidecar(from: &Path, to: &Path) -> Result<()> {
    let original = fs::read(from).with_context(|| format!("reading `{}`", from.display()))?;
    let copied = fs::read(to).with_context(|| format!("reading `{}`", to.display()))?;
    if original != copied {
        bail!(
            "the copied sidecar `{}` differs from `{}`",
            to.display(),
            from.display()
        );
    }
    Ok(())
}

/// The SHA-256 of a file, lowercase hex.
///
/// Printed so the archive can be referred to unambiguously — two release
/// builds of the same version produce different archives, so the version
/// in the name is not enough to identify one.
fn sha256_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("reading `{}`", path.display()))?;
    Ok(Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Convert a Unix timestamp into the MS-DOS date and time a ZIP stores.
///
/// The ZIP format keeps a local-time timestamp in DOS form: a 16-bit date
/// (day, month, year since 1980) and a 16-bit time (hour, minute, second
/// in two-second units). The `zip` crate can convert from a `time`
/// `OffsetDateTime`, but that feature is off in this workspace and the
/// alternative — enabling it — would add a dependency to a crate that has
/// none to gain from one.
///
/// So the calendar maths is done here: days-since-the-epoch to a civil
/// date, using Howard Hinnant's `civil_from_days`, which is exact for the
/// whole proleptic Gregorian calendar and is the same algorithm the
/// standard library's own date handling is built on.
///
/// A timestamp outside the range a DOS date can hold — before 1980, or
/// after 2107 — falls back to the DOS epoch rather than failing the
/// build. It is a file modification time, not a fact worth aborting over.
fn dos_datetime(time: SystemTime) -> DateTime {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0);

    let days = seconds.div_euclid(86_400);
    let time_of_day = seconds.rem_euclid(86_400);

    let (year, month, day) = civil_from_days(days);
    let hour = (time_of_day / 3_600) as u8;
    let minute = ((time_of_day % 3_600) / 60) as u8;
    let second = (time_of_day % 60) as u8;

    DateTime::from_date_and_time(year, month, day, hour, minute, second)
        .unwrap_or_else(|_| DateTime::default())
}

/// Days since 1970-01-01 to a proleptic Gregorian (year, month, day).
///
/// Hinnant's algorithm: shift the epoch to 0000-03-01 so that the leap
/// day lands at the *end* of the year, which is what makes the month
/// length arithmetic come out in one step.
fn civil_from_days(days: i64) -> (u16, u8, u8) {
    // 719_468 days from 0000-03-01 to 1970-01-01.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097; // [0, 146_096]
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100); // [0, 365]
    let mp = (5 * day_of_year + 2) / 153; // [0, 11], March = 0
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;

    let year = if month <= 2 { year + 1 } else { year };
    (year as u16, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique scratch directory per test, so they can run in parallel.
    fn scratch(name: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "snug-package-test-{}-{}-{}",
            name,
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// Build a staged tree that looks like the one build.cmd stages.
    fn staged_tree(dir: &Path) {
        fs::create_dir_all(dir).expect("staged tree");
        fs::write(dir.join("snug.exe"), b"snug cli").expect("snug.exe");
        fs::write(dir.join("snug_preview.exe"), b"preview").expect("preview");
        fs::write(dir.join("Snug Quick Build.exe"), b"dropper").expect("dropper");
        fs::write(dir.join(SIDECAR), b"demo jar").expect("sidecar");
        for name in ["LICENSE", "LICENSE-MIT", "LICENSE-APACHE", "NOTICE"] {
            fs::write(dir.join(name), name.as_bytes()).expect(name);
        }
    }

    #[test]
    fn the_archive_has_the_expected_shape() {
        let dir = scratch("shape");
        staged_tree(&dir);

        let opts = Options {
            source: dir.clone(),
            tag: "windows-x86_64".into(),
        };
        run(&opts).expect("packaging should succeed");

        let zip_path = dir
            .join(OUT_DIR)
            .join(format!("snug-{VERSION}-windows-x86_64.zip"));
        assert!(zip_path.is_file(), "the archive should exist");

        let file = File::open(&zip_path).expect("open");
        let archive = ZipArchive::new(file).expect("read");
        let mut names: Vec<String> = archive.file_names().map(|n| n.to_string()).collect();
        names.sort();

        assert_eq!(
            names,
            vec![
                "snug/LICENSE".to_string(),
                "snug/LICENSE-APACHE".to_string(),
                "snug/LICENSE-MIT".to_string(),
                "snug/NOTICE".to_string(),
                "snug/Snug Quick Build.exe".to_string(),
                "snug/snug.exe".to_string(),
                "snug/snug_preview.exe".to_string(),
            ],
            "the archive should hold exactly the three shipping EXEs and the four \
             licence texts under snug/"
        );
    }

    /// The demo JAR travels beside the archive, not inside it. It is the
    /// one payload that does not belong to someone here to build an app,
    /// and a release page can offer the two separately.
    #[test]
    fn the_demo_jar_is_copied_beside_the_archive_and_not_into_it() {
        let dir = scratch("sidecar");
        staged_tree(&dir);

        let opts = Options {
            source: dir.clone(),
            tag: "windows-x86_64".into(),
        };
        run(&opts).expect("packaging should succeed");

        let copied = dir.join(OUT_DIR).join(SIDECAR);
        assert!(copied.is_file(), "the demo JAR should be copied");
        assert_eq!(
            fs::read(&copied).expect("read copied"),
            fs::read(dir.join(SIDECAR)).expect("read staged"),
            "the copied JAR should be byte-identical to the staged one"
        );

        let zip_path = dir
            .join(OUT_DIR)
            .join(format!("snug-{VERSION}-windows-x86_64.zip"));
        let file = File::open(&zip_path).expect("open");
        let archive = ZipArchive::new(file).expect("read");
        assert!(
            !archive.file_names().any(|n| n.ends_with(".jar")),
            "the demo JAR should not be inside the archive"
        );
    }

    /// A missing shipping artefact must stop the build, not produce an
    /// archive that looks complete.
    #[test]
    fn a_missing_shipping_file_fails_the_build() {
        let dir = scratch("missing");
        staged_tree(&dir);
        fs::remove_file(dir.join("Snug Quick Build.exe")).expect("remove");

        let opts = Options {
            source: dir.clone(),
            tag: "windows-x86_64".into(),
        };
        let err = run(&opts).expect_err("a missing file should fail");
        assert!(
            err.to_string().contains("Snug Quick Build.exe"),
            "the error should name the missing file, got: {err}"
        );
    }

    /// The archive's bytes must be the staged file's bytes. A zip that
    /// quietly holds something else still looks like a zip.
    #[test]
    fn the_archived_bytes_match_the_staged_files() {
        let dir = scratch("bytes");
        staged_tree(&dir);

        let opts = Options {
            source: dir.clone(),
            tag: "windows-x86_64".into(),
        };
        run(&opts).expect("packaging should succeed");

        let zip_path = dir
            .join(OUT_DIR)
            .join(format!("snug-{VERSION}-windows-x86_64.zip"));
        let file = File::open(&zip_path).expect("open");
        let mut archive = ZipArchive::new(file).expect("read");

        for name in SHIPPED {
            let entry_name = format!("snug/{name}");
            let mut entry = archive.by_name(&entry_name).expect("entry");
            let mut archived = Vec::new();
            entry.read_to_end(&mut archived).expect("read entry");
            assert_eq!(
                archived,
                fs::read(dir.join(name)).expect("read staged"),
                "{entry_name} should hold the staged file's bytes"
            );
        }
    }

    /// The whole point of writing the archive in Rust: entry names are
    /// forward-slashed by construction. Every PowerShell zip route writes
    /// backslashes, and that still unpacks correctly on Windows, so it
    /// would survive a Windows-only smoke test.
    #[test]
    fn entry_names_use_forward_slashes() {
        let dir = scratch("slashes");
        staged_tree(&dir);

        let opts = Options {
            source: dir.clone(),
            tag: "windows-x86_64".into(),
        };
        run(&opts).expect("packaging should succeed");

        let zip_path = dir
            .join(OUT_DIR)
            .join(format!("snug-{VERSION}-windows-x86_64.zip"));
        let file = File::open(&zip_path).expect("open");
        let archive = ZipArchive::new(file).expect("read");

        for name in archive.file_names() {
            assert!(
                !name.contains('\\'),
                "entry `{name}` uses a backslash, which a POSIX extractor would read as one filename"
            );
        }
    }

    #[test]
    fn the_platform_tag_defaults_to_the_staged_directorys_name() {
        let args = vec!["release\\windows-x86_64".to_string()];
        let opts = Options::parse(&args).expect("parse");
        assert_eq!(opts.tag, "windows-x86_64");
        assert_eq!(opts.source, PathBuf::from("release\\windows-x86_64"));
    }

    #[test]
    fn an_explicit_tag_overrides_the_derived_one() {
        let args = vec![
            "release\\windows-x86_64".to_string(),
            "--tag".to_string(),
            "windows".to_string(),
        ];
        let opts = Options::parse(&args).expect("parse");
        assert_eq!(opts.tag, "windows");
    }

    #[test]
    fn a_missing_source_directory_is_a_usage_error() {
        assert!(Options::parse(&[]).is_err());
        assert!(Options::parse(&["a".to_string(), "b".to_string()]).is_err());
        assert!(Options::parse(&["--nope".to_string()]).is_err());
        assert!(Options::parse(&["--tag".to_string()]).is_err());
    }

    /// The calendar maths is the one piece of this that is easy to get
    /// subtly wrong, so it is pinned against dates whose answers are not
    /// in doubt.
    #[test]
    fn civil_dates_are_right() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(1), (1970, 1, 2));
        // 1970 is not a leap year, so day 59 is 1 March, not 29 February.
        assert_eq!(civil_from_days(59), (1970, 3, 1));
        // Ten years and two leap days on: the epoch of the DOS date format.
        assert_eq!(civil_from_days(3_652), (1980, 1, 1));
        // A year boundary.
        assert_eq!(civil_from_days(365), (1971, 1, 1));
        // A leap day in a leap century — 2000 is divisible by 400.
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        // A leap day in an ordinary one.
        assert_eq!(civil_from_days(18_321), (2020, 2, 29));
        // Before the epoch, and in a century that is *not* a leap year:
        // 1900 is not divisible by 400, so it had no 29 February. The
        // naive "every fourth year" rule gets this one wrong.
        assert_eq!(civil_from_days(-25_508), (1900, 3, 1));
    }

    /// A DOS timestamp has to survive the round trip through the format:
    /// the year has to be in range, and the seconds land in DOS's
    /// two-second units.
    #[test]
    fn dos_timestamps_are_usable() {
        let epoch = UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let dt = dos_datetime(epoch);
        assert!(dt.is_valid(), "a 2023 timestamp must be representable");
        assert_eq!(dt.year(), 2023);
        assert_eq!(dt.month(), 11);
        assert_eq!(dt.day(), 14);
    }

    /// A file modified before the DOS epoch cannot be represented, and
    /// must not fail the build over it.
    #[test]
    fn an_unrepresentable_timestamp_falls_back_to_the_dos_epoch() {
        let before = UNIX_EPOCH - std::time::Duration::from_secs(86_400 * 400);
        let dt = dos_datetime(before);
        assert!(dt.is_valid());
        assert_eq!(dt.year(), 1980);
    }
}
