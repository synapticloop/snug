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
    *STARTED.lock().unwrap() = Some(std::time::Instant::now());
    Ok(log_path.to_path_buf())
}

/// Where the log file is currently being written to, if [`init`] has
/// been called. Used for diagnostics — e.g. including the path in the
/// error dialog so the user can attach it to a bug report.
pub fn path() -> Option<PathBuf> {
    INITIALISED_PATH.lock().unwrap().clone()
}

/// Set at [`init`] so every line can carry a monotonic offset from it.
///
/// Unix seconds in the prefix is fine for *ordering* but useless for the
/// question a hang actually raises: "how long between these two lines?".
/// With whole-second resolution a 900 ms stall and a 0 ms one look
/// identical, and the JNI boundary is precisely where a stall lives. The
/// elapsed field is what turns the log from a list of events into a
/// timeline.
static STARTED: Mutex<Option<std::time::Instant>> = Mutex::new(None);

/// Milliseconds since [`init`], or `?` if it has not been called.
///
/// Returns the bare timestamp-plus-offset with **no** surrounding
/// brackets, because every caller wraps the result in its own.
fn elapsed_ms() -> String {
    match *STARTED.lock().unwrap() {
        Some(t) => format!("{}+{}ms", unix_secs(), t.elapsed().as_millis()),
        None => format!("{}+?", unix_secs()),
    }
}

/// Is `SNUG_DEBUG` asking for the verbose trace?
///
/// Read once per call rather than cached, so a test can flip it. The set
/// is deliberately forgiving (`SNUG_DEBUG=1`, `true`, `on`, or any
/// non-empty value) and only `0` / `false` / empty mean off — a debug
/// switch that needs exact spelling is a debug switch nobody sets.
pub fn debug_enabled() -> bool {
    match std::env::var_os("SNUG_DEBUG") {
        Some(v) => {
            let s = v.to_string_lossy().trim().to_ascii_lowercase();
            !matches!(s.as_str(), "" | "0" | "false" | "no" | "off")
        }
        None => false,
    }
}

/// Append a line only when `SNUG_DEBUG` is set, tagged `DEBUG` and
/// carrying the elapsed offset.
///
/// This is the channel for the narrow questions `log` cannot answer:
/// which side of a boundary are we on, and how long ago was it. In
/// particular it brackets `call_static_method`, because "the app started
/// and then nothing" is indistinguishable from "the app blocked" until
/// there is a marker on each side of the call.
pub fn debug(msg: &str) {
    if !debug_enabled() {
        return;
    }
    let line = format!("[{}] DEBUG {msg}\n", elapsed_ms());
    let _ = std::io::stderr().write_all(line.as_bytes());
    if let Ok(mut guard) = FILE.lock() {
        if let Some(file) = guard.as_mut() {
            let _ = file.write_all(line.as_bytes());
            let _ = file.flush();
        }
    }
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

    /// The debug switch has to be forgiving, because a switch that needs
    /// exact spelling is a switch nobody flips while chasing a hang. These
    /// are the spellings that must read as *on*.
    #[test]
    fn debug_switch_accepts_the_obvious_spellings() {
        // Scoped so the process-global env cannot leak into another test
        // running on a different thread of the same binary.
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for v in ["1", "true", "TRUE", "on", "yes", "debug", "2"] {
            // SAFETY: the guard above proves no other thread in this
            // binary is touching the environment.
            unsafe { std::env::set_var("SNUG_DEBUG", v) };
            assert!(debug_enabled(), "{v:?} should enable debug logging");
        }
        for v in ["", "0", "false", "off", "no", "FALSE"] {
            unsafe { std::env::set_var("SNUG_DEBUG", v) };
            assert!(!debug_enabled(), "{v:?} should NOT enable debug logging");
        }
        unsafe { std::env::remove_var("SNUG_DEBUG") };
        assert!(!debug_enabled(), "unset should mean off, not default-on");
    }

    /// Serialises the tests in this module.
    ///
    /// `FILE` is a process-global: `init()` rebinds it to whichever
    /// file that caller asked for, so two tests running concurrently
    /// fight over the one writer. Left unserialised, the
    /// truncation test's `log()` calls land in whatever file the
    /// neighbouring test last initialised, and it reads back empty.
    ///
    /// Poisoning is tolerated — one failing test shouldn't cascade
    /// into every other test in the module.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn serialised() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn init_truncates_existing_log() {
        let _guard = serialised();
        // Init, write, init again — the second init should wipe the
        // first session's content.
        // Counter is load-bearing: pid + nanos is not unique when tests run
        // in parallel on a coarse clock (macOS).
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "snug-log-test-{}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
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
        let _guard = serialised();
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "snug-log-nested-{}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
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
        let _guard = serialised();
        // We can't truly simulate "no init called" — `init()` in a
        // sibling test would have rebound the global, and with
        // `TEST_LOCK` held the sibling can't run concurrently
        // either. So this just asserts `log()` is panic-free
        // regardless of init state.
        log("orphaned log line should not crash");
    }
}