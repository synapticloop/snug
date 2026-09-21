//! Per-launch file logger.
//!
//! The launcher writes a structured trace of the cache + JDK-install
//! + JNI-load steps to a single file alongside the cached JAR, so a
//! user can post-mortem a launch without re-running it. The file is
//! truncated on every `init`, so each launch produces a self-contained
//! record.
//!
//! Default location:
//! ```text
//! %LOCALAPPDATA%\snug\<company>\<app>\<jar-sha256>\snug.log
//! ```
//!
//! `log()` also mirrors each line to stderr so debug builds (console
//! subsystem) see the trace live. Release builds (GUI subsystem)
//! have stderr detached, so the file is the only visible record.
//!
//! Thread-safe: a single `Mutex<Option<File>>` guards the writer.
//! Lock contention is a non-issue — the launcher is single-threaded
//! during startup, and the worker thread emits at most one log line
//! per ~few hundred ms.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);
static INITIALISED_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Open `log_path` for write, truncating it (so each launch gets a
/// fresh file). Creates the parent directory if missing. Returns the
/// resolved path on success.
pub fn init(log_path: &Path) -> std::io::Result<PathBuf> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(log_path)?;
    *FILE.lock().unwrap() = Some(file);
    *INITIALISED_PATH.lock().unwrap() = Some(log_path.to_path_buf());
    Ok(log_path.to_path_buf())
}

/// Where the log file is currently being written to, if [`init`] has
/// been called. Used for diagnostics — e.g. including the path in the
/// error dialog so the user can attach it to a bug report.
pub fn path() -> Option<PathBuf> {
    INITIALISED_PATH.lock().unwrap().clone()
}

/// Append a timestamped line to the log file (and mirror to stderr).
/// If `init` has not been called, the call still echoes to stderr so
/// nothing is lost.
pub fn log(msg: &str) {
    let ts = unix_secs();
    let line = format!("[{ts}] {msg}\n");

    // Mirror to stderr. On the GUI subsystem the console is detached
    // and this is a no-op; on debug builds it shows in the terminal.
    let _ = std::io::stderr().write_all(line.as_bytes());

    // File is the canonical record.
    if let Ok(mut guard) = FILE.lock() {
        if let Some(file) = guard.as_mut() {
            let _ = file.write_all(line.as_bytes());
            let _ = file.flush();
        }
    }
}

/// Same as [`log`] but takes an already-formatted line and skips the
/// trailing newline (caller must add it).
pub fn log_raw(line: &[u8]) {
    let _ = std::io::stderr().write_all(line);
    if let Ok(mut guard) = FILE.lock() {
        if let Some(file) = guard.as_mut() {
            let _ = file.write_all(line);
            let _ = file.flush();
        }
    }
}

fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_truncates_existing_log() {
        // Init, write, init again — the second init should wipe the
        // first session's content.
        let dir = std::env::temp_dir().join(format!(
            "snug-log-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("snug.log");

        init(&path).unwrap();
        log("first launch line 1");
        log("first launch line 2");

        init(&path).unwrap();
        log("second launch line");

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains("first launch"), "got: {body}");
        assert!(body.contains("second launch line"), "got: {body}");
    }

    #[test]
    fn init_creates_parent_dirs() {
        let dir = std::env::temp_dir().join(format!(
            "snug-log-nested-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("a").join("b").join("snug.log");
        init(&path).expect("init must create parent dirs");
        log("nested");
        assert!(path.exists());
    }

    #[test]
    fn log_without_init_does_not_panic() {
        // Tests can't easily simulate "no init called" because the
        // module-level `static FILE` is shared across tests; we just
        // assert that `log()` doesn't panic regardless of init state.
        log("orphaned log line should not crash");
    }
}