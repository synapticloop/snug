//! `Build with Snug` — the beginner front-end to snug.
//!
//! A standalone Windows shim. Drop a JAR (or a folder of JARs) on it and
//! it builds the EXE and says so; double-click it and it opens a command
//! window; drop the wrong thing and it explains, then opens the same
//! window. The decision lives in [`snug_dropper::decide`], the child
//! process in [`snug_dropper::build`], the dialogs in
//! [`snug_dropper::ui`].
//!
//! The binary is built as `snug-dropper.exe` and renamed to
//! `Build with Snug.exe` at packaging time, so that the file people
//! drag onto carries a name that says what it does. It must sit in the
//! **same folder as `snug.exe`** — that folder is where this looks for
//! the real builder, where it writes `snug-build.log`, and where the
//! double-click opens its terminal.
//!
//! **Windows-only.** Every item below is `#[cfg(windows)]` because the
//! crate is useless anywhere else: it ships as a `.exe`, it is renamed to
//! a `.exe`, and it is dragged onto by beginners who will never run it on
//! macOS. It used to be gated with a single file-level `#![cfg(windows)]`,
//! which is tidier but leaves `cargo build --workspace` failing on a
//! macOS dev box with `E0601: main function not found` — a bin whose whole
//! body was configured out has no entry point. The per-item form keeps
//! the crate building everywhere so `--workspace` builds *and* `--workspace`
//! tests run on macOS / Linux, which is where the library half of this
//! crate (the drop decision and build plumbing) is developed. The
//! non-Windows `main` at the bottom is the price of that, and it says
//! exactly one thing.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

// `ExitCode` is ungated: the non-Windows stub `main` needs it too.
#[cfg(windows)]
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[cfg(windows)]
use snug_dropper::build::{self, Invocation, Outcome, LOG_FILE_NAME};
#[cfg(windows)]
use snug_dropper::decide::{decide, Mode, Reject};
#[cfg(windows)]
use snug_dropper::{terminal, ui};

/// `snug`'s own executable name, looked for beside this one.
#[cfg(windows)]
const SNUG_EXE: &str = "snug.exe";

// ---- Copy ---------------------------------------------------------------
//
// Collected here rather than inline so the wording is editable in one
// place, and so every dialog body is visibly CRLF-terminated — a bare
// `\n` in a `MessageBoxW` body renders inconsistently.

#[cfg(windows)]
const MARQUEE_HEADING: &str = "Building your application...";
#[cfg(windows)]
const MARQUEE_SUB: &str = "This can take a minute for a large application.";

#[cfg(windows)]
const TOO_MANY_BODY: &str = "Only one jar is allowed.\r\n\
    \r\n\
    A command window has been opened so you can build it yourself.";

#[cfg(windows)]
const BAD_TYPE_BODY: &str =
    "Only .jar files and folders can be dropped on Build with Snug.\r\n\
     \r\n\
     A command window has been opened so you can build it yourself.";

#[cfg(windows)]
const MISSING_SNUG_BODY: &str = "snug.exe could not be found.\r\n\
    \r\n\
    Keep Build with Snug.exe in the same folder as snug.exe.";

#[cfg(windows)]
fn main() -> ExitCode {
    // Before any window exists, so the dialogs are laid out once and
    // drawn crisply rather than bitmap-stretched.
    ui::enable_dpi_awareness();

    // Windows passes dropped paths through as arguments, so this is the
    // whole drag-and-drop API.
    let items: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();

    // This exe's own folder is "home": where `snug.exe` is looked for,
    // where the log is written, and where the terminal opens.
    let home = exe_dir();

    match decide(&items) {
        Mode::OpenTerminal => {
            open_terminal(&home);
            ExitCode::SUCCESS
        }
        Mode::Reject(Reject::TooManyItems) => {
            ui::info(TOO_MANY_BODY);
            open_terminal(&home);
            ExitCode::SUCCESS
        }
        Mode::Reject(Reject::UnsupportedType) => {
            ui::info(BAD_TYPE_BODY);
            open_terminal(&home);
            ExitCode::SUCCESS
        }
        Mode::Build { input } => build(&home, input),
    }
}

/// The directory this executable lives in.
#[cfg(windows)]
fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Open the command window. A failure here is not worth a second dialog
/// on top of whatever the user already saw — they are being handed the
/// terminal as a convenience, and if the OS won't give them one there is
/// nothing useful to say about it.
#[cfg(windows)]
fn open_terminal(dir: &Path) {
    let _ = terminal::open(dir);
}

#[cfg(windows)]
fn build(home: &Path, input: PathBuf) -> ExitCode {
    let snug_exe = home.join(SNUG_EXE);
    if !snug_exe.is_file() {
        ui::error(MISSING_SNUG_BODY);
        return ExitCode::FAILURE;
    }

    let invocation = build::invocation(&snug_exe, &input);
    let log = home.join(LOG_FILE_NAME);

    if invocation.output.exists() && !confirm_overwrite(&invocation) {
        return ExitCode::SUCCESS;
    }

    // `log` stays owned here: the failure dialog names it after the
    // worker has finished with it, so the closure gets its own copy.
    let worker_log = log.clone();
    let outcome = ui::with_marquee(MARQUEE_HEADING, MARQUEE_SUB, move || {
        build::run(&invocation, &worker_log)
    });

    match outcome {
        Outcome::Built { output } => {
            ui::info(&format!(
                "Your application was created successfully.\r\n\r\n{}",
                output.display()
            ));
            ExitCode::SUCCESS
        }
        Outcome::Failed { reason } => {
            // No terminal fallback here by design: this dialog and the
            // log are the only recourse, so both have to carry the
            // whole story.
            ui::error(&format!(
                "The build failed.\r\n\r\n{reason}\r\n\r\nDetails: {}",
                log.display()
            ));
            ExitCode::FAILURE
        }
    }
}

/// Ask before replacing an EXE from an earlier build.
///
/// Every build writes the same fixed name, so a rebuild silently
/// replaces the previous one — and for a beginner who just edited and
/// rebuilt, "nothing happened" is indistinguishable from a successful
/// rebuild. One extra click on the rare second build beats that.
#[cfg(windows)]
fn confirm_overwrite(invocation: &Invocation) -> bool {
    let file = invocation
        .output
        .file_name()
        .map_or_else(|| invocation.output.display().to_string(), |n| n.to_string_lossy().into());
    // Title states the fact, body asks the question — the shape Windows
    // itself uses for a replace prompt, so it reads as one.
    ui::confirm(&format!("{file} exists"), "Overwrite?")
}

/// Non-Windows entry point. This binary is a Windows-only artefact (see
/// the module docs), so there is nothing here to do beyond saying so.
/// It exists so `cargo build --workspace` and `cargo test --workspace`
/// succeed on a macOS / Linux dev box instead of failing with
/// `E0601: main function not found`.
#[cfg(not(windows))]
fn main() -> ExitCode {
    eprintln!(
        "snug-dropper / \"Build with Snug\" is a Windows-only tool. \
         It is shipped beside snug.exe for Windows users; on macOS, use \
         `snug` directly."
    );
    ExitCode::FAILURE
}
