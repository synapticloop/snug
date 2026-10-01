//! Opening a command window where `snug` lives.
//!
//! The double-click mode, and the hand-off after a refused drop. This is
//! the escape hatch: anything this starter can't do — icons, splashes,
//! `--main-class`, localisations, multi-JAR directory builds — is one
//! command line away in a window that's already sitting in the right
//! directory.
//!
//! The directory is passed as the child's **working directory**, never as
//! an argument. The tempting alternative is `cmd /c start /d "<dir>"`,
//! which routes a path through a command parser — so a folder containing
//! `&`, `^`, or `(` breaks it. Spawning `cmd.exe /k` with a real
//! `lpCurrentDirectory` has no such failure mode, and needs no quoting
//! at all.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// A resolved `cmd.exe /k` launch. Pure, so the shape is testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalPlan {
    pub program: PathBuf,
    /// `/k` — *keep* the window open after the command, so the window is
    /// a usable shell rather than one that flashes and dies.
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
}

/// Plan a command window in `dir`.
pub fn plan(dir: &Path) -> TerminalPlan {
    TerminalPlan {
        program: PathBuf::from("cmd.exe"),
        args: vec![OsString::from("/k")],
        cwd: dir.to_path_buf(),
    }
}

/// Open the command window and return immediately.
///
/// Detached on purpose: we must not wait, and the child must outlive this
/// process so the window stays put after we exit.
pub fn open(dir: &Path) -> std::io::Result<()> {
    let plan = plan(dir);
    let mut cmd = std::process::Command::new(&plan.program);
    cmd.args(&plan.args).current_dir(&plan.cwd);

    // Our process is GUI-subsystem, so a console child would normally
    // still get a fresh console. But when the dropper is launched *from*
    // a terminal (a developer testing it), the child would inherit that
    // console instead and `/k` would block inside the caller's window.
    // Asking for a new console makes the behaviour identical either way.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE;
        cmd.creation_flags(CREATE_NEW_CONSOLE);
    }

    cmd.spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_cmd_k_in_the_given_directory() {
        let plan = plan(Path::new(r"C:\tools\snug"));
        assert_eq!(plan.program, PathBuf::from("cmd.exe"));
        assert_eq!(plan.args, vec![OsString::from("/k")]);
        assert_eq!(plan.cwd, PathBuf::from(r"C:\tools\snug"));
    }

    #[test]
    fn the_directory_is_never_passed_as_an_argument() {
        // Regression guard for the `cmd /c start /d "<dir>"` shape: the
        // folder would be parsed as a command line, so a `&` in the path
        // would split it. The CWD is a process attribute instead.
        let plan = plan(Path::new(r"C:\My Builds\a&b"));
        assert_eq!(plan.args.len(), 1);
        assert!(!plan.args.iter().any(|a| a.to_string_lossy().contains('&')));
    }
}
