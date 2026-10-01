//! How the dropper invokes `snug.exe`, and how it reads the result.
//!
//! Everything here except [`run`] is pure and cross-platform, which is
//! what lets the whole argument vector and the whole error-summary
//! heuristic be unit-tested without a Windows box or a real build.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// `--name` value. Deliberately the literal word "Example": this is a
/// starter utility, so the artefact it produces announces itself as a
/// sample rather than pretending to be a real product.
pub const APP_NAME: &str = "Example Application Name";

/// `--company` value. Same reasoning as [`APP_NAME`].
pub const COMPANY: &str = "Example Company Pty Ltd";

/// File name stem of the produced EXE.
///
/// Matches [`APP_NAME`]: the point is that someone who finds
/// `Example Application Name.exe` on a stranger's desktop immediately
/// knows it came from here.
pub const OUTPUT_STEM: &str = "Example Application Name";

/// Where the child's stdout + stderr are mirrored, so an error dialog can
/// point at a real log instead of guessing.
pub const LOG_FILE_NAME: &str = "snug-build.log";

/// A fully-resolved child invocation: what to run, with what, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// Absolute path to `snug.exe`.
    pub program: PathBuf,
    /// Argument vector, `argv[1..]`. Never passes through a shell, so
    /// paths containing spaces or `&` need no quoting and can't break.
    pub args: Vec<OsString>,
    /// Working directory for the child.
    pub cwd: PathBuf,
    /// Where the EXE will land.
    pub output: PathBuf,
}

/// Build the `snug.exe` invocation for one dropped input.
///
/// The working directory is the input's **parent** in both cases, and
/// that single choice buys two things:
///
/// 1. A project-local `snug.options` sitting beside the JAR (or the
///    folder) is picked up for free, so icons, splashes and
///    `--main-class` work through a plain drag-and-drop.
/// 2. The EXE lands next to what the user dropped — beside the JAR, or
///    beside the *folder* rather than inside it. Writing into a dropped
///    `build/libs` would put the artefact somewhere the next `gradle
///    clean` erases.
///
/// Note the precedence consequence: `snug.options` beside `snug.exe`
/// outranks the CWD copy, so the packaging layout must not ship one
/// there or it would silently beat every project file.
pub fn invocation(snug_exe: &Path, input: &Path) -> Invocation {
    let cwd = match input.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        // A bare relative filename has no parent. Fall back to the
        // process directory rather than an empty path, which `Command`
        // treats as "inherit" and would leave the output location
        // ambiguous.
        _ => PathBuf::from("."),
    };
    let output = cwd.join(format!("{OUTPUT_STEM}.exe"));

    let args = vec![
        OsString::from("--name"),
        OsString::from(APP_NAME),
        OsString::from("--company"),
        OsString::from(COMPANY),
        OsString::from("-o"),
        output.clone().into_os_string(),
        input.as_os_str().to_owned(),
    ];

    Invocation {
        program: snug_exe.to_path_buf(),
        args,
        cwd,
        output,
    }
}

/// Prefix `snug` uses when reporting a successful build
/// (`eprintln!("snug: built {path}")` in `snug-cli/src/main.rs`).
const BUILT_PREFIX: &str = "snug: built ";

/// Pull the output path out of `snug`'s own success line.
///
/// Read from the child's output rather than re-derived from
/// [`Invocation::output`], so the dialog names the file `snug` believes
/// it wrote. If the line is somehow absent the caller still has
/// [`Invocation::output`] to fall back on.
pub fn parse_built_line(output: &str) -> Option<PathBuf> {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix(BUILT_PREFIX))
        .map(PathBuf::from)
}

/// Longest one-line summary shown in a dialog before it defers to the log.
const SUMMARY_MAX: usize = 200;

/// Reduce `snug`'s stderr to a single line a beginner can act on.
///
/// `snug-cli` reports failures as `eprintln!("snug: {err:?}")`, so a
/// typical failure looks like:
///
/// ```text
/// snug: building snug payload
///
/// Caused by:
///     0: reading Main-Class from JAR manifest
///     1: reading JAR as zip
///     2: invalid Zip archive: Could not find EOCD
/// ```
///
/// Every line here is an `anyhow` *context* frame except the last, and
/// anyhow prints the outermost context first and the originating error
/// last. That ordering decides what to show: the head of the chain
/// ("reading Main-Class from JAR manifest") points at the manifest when
/// the real problem is that the file is not a JAR at all — actively
/// misleading for someone who has never seen a stack of context frames.
/// The tail is the thing that actually went wrong, so that's what the
/// dialog leads with; the full chain stays in the log.
pub fn summarise(output: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();

    let chosen = lines
        .iter()
        .position(|l| *l == "Caused by:")
        .and_then(|i| lines.get(i + 1..).and_then(|chain| chain.last()))
        .map(|l| strip_cause(l))
        .or_else(|| {
            lines
                .iter()
                .find(|l| l.starts_with("snug: "))
                .map(|l| l.strip_prefix("snug: ").unwrap_or(l))
        })
        .or_else(|| lines.last().copied())
        .unwrap_or("snug did not report a reason.");

    truncate(chosen, SUMMARY_MAX)
}

/// Strip anyhow's cause-line decoration: a 4-space indent, and the `N: `
/// numbering it adds only when a chain has more than one cause.
fn strip_cause(line: &str) -> &str {
    let trimmed = line.trim_start_matches(char::is_whitespace);
    // `take_while` over ASCII digits always lands on a char boundary, so
    // the byte slice below cannot split a UTF-8 sequence.
    let digits = trimmed.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits > 0 && trimmed[digits..].starts_with(": ") {
        trimmed[digits + 2..].trim_start()
    } else {
        trimmed
    }
}

/// Clip to `max` characters, marking the cut with an ellipsis.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}...")
}

/// How a build ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// `snug` exited zero.
    Built {
        /// The EXE that was produced.
        output: PathBuf,
    },
    /// `snug` could not be started, or exited non-zero.
    Failed {
        /// One-line reason for the dialog.
        reason: String,
    },
}

/// Run the build, mirroring the child's output to `log`.
///
/// Cross-platform in every respect except `CREATE_NO_WINDOW`, so the
/// tests that exercise a real `snug.exe` are ordinary integration tests.
pub fn run(inv: &Invocation, log: &Path) -> Outcome {
    let mut cmd = std::process::Command::new(&inv.program);
    cmd.args(&inv.args)
        .current_dir(&inv.cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // Without this a console-subsystem `snug.exe` would flash a window
    // behind the marquee — the dropper owns the UI, the child must not
    // add one.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    // `output()` drains both pipes concurrently, so a chatty build can't
    // deadlock against a full pipe buffer.
    let out = match cmd.output() {
        Ok(out) => out,
        Err(err) => {
            return Outcome::Failed {
                reason: format!("snug could not be started: {err}"),
            };
        }
    };

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{stdout}{stderr}");

    // Best-effort. The dialog names this path, so a failure to write it
    // is worth reporting — but not worth failing a build that succeeded.
    let log_written = std::fs::write(log, &combined).is_ok();

    if !out.status.success() {
        let mut reason = summarise(&combined);
        if !log_written {
            reason.push_str(&format!(
                " (the log at {} could not be written)",
                log.display()
            ));
        }
        return Outcome::Failed { reason };
    }

    Outcome::Built {
        output: parse_built_line(&combined).unwrap_or_else(|| inv.output.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invocation_pins_name_company_and_output() {
        let inv = invocation(
            Path::new(r"C:\tools\snug.exe"),
            Path::new(r"C:\work\App.jar"),
        );

        assert_eq!(
            inv.args,
            vec![
                OsString::from("--name"),
                OsString::from("Example Application Name"),
                OsString::from("--company"),
                OsString::from("Example Company Pty Ltd"),
                OsString::from("-o"),
                OsString::from(r"C:\work\Example Application Name.exe"),
                OsString::from(r"C:\work\App.jar"),
            ]
        );
    }

    #[test]
    fn invocation_cwds_and_outputs_beside_a_jar() {
        let inv = invocation(Path::new("snug.exe"), Path::new("/home/u/App.jar"));
        assert_eq!(inv.cwd, PathBuf::from("/home/u"));
        assert_eq!(
            inv.output,
            PathBuf::from("/home/u/Example Application Name.exe")
        );
    }

    #[test]
    fn invocation_lands_beside_a_dropped_folder_not_inside_it() {
        // Dropping `C:\proj\build\libs` must not write into the build
        // directory — `gradle clean` would eat the artefact.
        let inv = invocation(Path::new("snug.exe"), Path::new(r"C:\proj\build\libs"));
        assert_eq!(inv.cwd, PathBuf::from(r"C:\proj\build"));
        assert_eq!(
            inv.output,
            PathBuf::from(r"C:\proj\build\Example Application Name.exe")
        );
    }

    #[test]
    fn invocation_survives_paths_with_spaces_and_ampersands() {
        // No shell is involved, so these are passed through verbatim.
        // This is the property that would break if anyone "simplified"
        // this into a `cmd /c` string.
        let inv = invocation(
            Path::new(r"C:\Program Files\snug.exe"),
            Path::new(r"C:\My Builds\a&b\App.jar"),
        );
        assert_eq!(
            inv.args.last().unwrap(),
            &OsString::from(r"C:\My Builds\a&b\App.jar")
        );
        assert_eq!(
            inv.output,
            PathBuf::from(r"C:\My Builds\a&b\Example Application Name.exe")
        );
    }

    #[test]
    fn invocation_handles_a_bare_relative_filename() {
        let inv = invocation(Path::new("snug.exe"), Path::new("App.jar"));
        assert_eq!(inv.cwd, PathBuf::from("."));
        assert_eq!(inv.output, PathBuf::from("./Example Application Name.exe"));
    }

    #[test]
    fn parses_the_built_line() {
        let out = "snug: loaded options from C:\\p\\snug.options\nsnug: built C:\\p\\App.exe\n";
        assert_eq!(
            parse_built_line(out),
            Some(PathBuf::from(r"C:\p\App.exe"))
        );
    }

    #[test]
    fn built_line_absent_yields_none() {
        assert_eq!(parse_built_line("snug: some other message\n"), None);
    }

    #[test]
    fn summarise_reports_the_root_cause_not_the_outer_context() {
        // Real output from a file that is not a JAR. Leading with
        // "reading Main-Class from JAR manifest" would send a beginner
        // looking at a manifest that does not exist; the tail is the
        // actual failure.
        let stderr = "snug: building snug payload\n\nCaused by:\n    0: reading Main-Class from JAR manifest\n    1: reading JAR as zip\n    2: invalid Zip archive: Could not find EOCD\n";
        assert_eq!(
            summarise(stderr),
            "invalid Zip archive: Could not find EOCD"
        );
    }

    #[test]
    fn summarise_handles_a_single_unchained_cause() {
        let stderr = "snug: building snug payload\n\nCaused by:\n    No such file or directory (os error 2)\n";
        assert_eq!(summarise(stderr), "No such file or directory (os error 2)");
    }

    #[test]
    fn summarise_reports_a_missing_main_class_clearly() {
        // The common real-world mistake: a JAR that builds fine but has
        // no `Main-Class`, which must read as exactly that.
        let stderr = "snug: building snug payload\n\nCaused by:\n    0: reading Main-Class from JAR manifest\n    1: no Main-Class in META-INF/MANIFEST.MF\n";
        assert_eq!(summarise(stderr), "no Main-Class in META-INF/MANIFEST.MF");
    }

    #[test]
    fn summarise_falls_back_to_the_snig_line_without_a_chain() {
        assert_eq!(
            summarise("snug: --min-java must be a number\n"),
            "--min-java must be a number"
        );
    }

    #[test]
    fn summarise_falls_back_to_the_last_line_for_foreign_output() {
        assert_eq!(summarise("something went very wrong\n"), "something went very wrong");
    }

    #[test]
    fn summarise_handles_empty_output() {
        assert_eq!(summarise("   \n\n"), "snug did not report a reason.");
    }

    #[test]
    fn summarise_truncates_a_runaway_line() {
        let long = "x".repeat(SUMMARY_MAX + 50);
        let out = summarise(&long);
        assert_eq!(out.chars().count(), SUMMARY_MAX + 3);
        assert!(out.ends_with("..."));
    }
}
