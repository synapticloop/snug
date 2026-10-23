//! Single-instance enforcement: "an instance is already running, so
//! bring it forward" instead of "start a second one".
//!
//! Two halves, deliberately separated because they answer different
//! questions and can fail independently:
//!
//! 1. [`claim`] — *should this process be the one that starts?* A
//!    cross-process advisory lock on one file in the app's cache
//!    directory. Exactly one live instance holds it; everyone else is
//!    told to stand down.
//! 2. [`activate_existing`] — *can we point the user at the instance
//!    that is already running?* Platform window management. This is a
//!    nicety, not a correctness requirement: if it cannot find a
//!    window, the second launch still does not start a JVM.
//!
//! Why a lock file rather than a named mutex or `NSRunningApplication`:
//! those were both considered and both are worse *for detection*.
//! `NSRunningApplication` only sees processes LaunchServices knows
//! about, which is not guaranteed for a snug `.app` `execve`'d from a
//! terminal. A named mutex cannot be released if the holder dies
//! without unwinding, and has no portable equivalent at all. An
//! advisory lock on a file is portable, needs no extra dependency, and
//! — the property that actually matters — is released by the operating
//! system when the process exits *however* it exits, so a crash or a
//! `TerminateProcess` cannot strand an application that will never
//! launch again.
//!
//! **Instances are identified per application**, by
//! [`cache::cache_root`](crate::cache::cache_root) — that is, by
//! `--company` and `--name` — not by executable path. Two builds with
//! different metadata are different applications and run side by side;
//! the same build launched twice is one application.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use crate::log;

/// Filename of the lock, inside the app's cache root.
///
/// Not a hash directory, which matters: [`cache::sweep`] only ever
/// considers 64-character hex directory names and skips everything else,
/// so this file is invisible to the retention policy. That is by
/// design — the sweep exists to reclaim extracted JARs, and deleting a
/// live lock would hand the next launch a false "nobody is running".
pub const LOCK_FILE_NAME: &str = "instance.lock";

/// The outcome of trying to become *the* instance.
#[derive(Debug)]
pub enum Claim {
    /// This process won. Hold the guard for as long as the app runs —
    /// dropping it releases the lock and lets the next launch through.
    Primary(InstanceGuard),

    /// Another instance is already running. The caller should activate
    /// it and exit without starting a JVM.
    AlreadyRunning,

    /// `--allow-multiple-instances` was passed at build time. No file
    /// is touched at all.
    Disabled,

    /// The lock could not be created or taken — an unwritable cache
    /// directory, most likely.
    ///
    /// This deliberately does **not** fail the launch. Single-instance
    /// is a usability feature, not a safety one; a read-only home
    /// directory must not stop an application from starting. Failing
    /// open here means the worst case is the old behaviour (two
    /// instances), never a refusal to run.
    Unavailable(String),
}

/// Proof that this process holds the instance lock.
///
/// There is no `Drop` impl on purpose: closing the file *is* the
/// release. Both platforms back the lock with an open handle, so
/// dropping the [`File`] unlocks it, and the OS does the same thing
/// for us if the process dies without unwinding. Deleting the lock
/// file would be wrong — a second process may have already opened it,
/// and on Windows a delete would fail outright while the first handle
/// is open.
#[derive(Debug)]
pub struct InstanceGuard {
    path: PathBuf,
    _file: File,
}

impl InstanceGuard {
    /// Where the lock lives, for the log line.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Full path of the instance lock for a given cache root.
pub fn lock_path(cache_root: &Path) -> PathBuf {
    cache_root.join(LOCK_FILE_NAME)
}

/// Try to become the single running instance of this application.
///
/// `allow_multiple` is
/// [`LauncherBehavior::allow_multiple_instances`](snug_format::LauncherBehavior::allow_multiple_instances);
/// when it is set this returns [`Claim::Disabled`] without touching the
/// filesystem, so opting out costs nothing at runtime.
///
/// A returned [`Claim::Primary`] guard **must be kept alive** for the
/// lifetime of the running application. Dropping it early would let a
/// second launch start a second JVM while this one is still on screen.
///
/// Every outcome is reported twice, through [`log::log`] and through
/// `eprintln!`. The duplication is deliberate and the reason is
/// ordering: the gate runs *before* the launcher opens its log file, so
/// at this point [`crate::log`] has nothing to write to and every
/// `log::log` call here is a silent no-op. `eprintln!` is the only
/// channel still live, and it is what a debug/console build — or any
/// harness capturing stderr — actually sees. A launch suppressed here
/// therefore leaves no new log file at all, which is worth knowing when
/// someone asks why double-clicking did nothing.
pub fn claim(cache_root: &Path, allow_multiple: bool) -> Claim {
    if allow_multiple {
        return Claim::Disabled;
    }

    let path = lock_path(cache_root);

    if let Err(e) = std::fs::create_dir_all(cache_root) {
        let why = format!("could not create {}: {e}", cache_root.display());
        report(&format!(
            "single instance: {why}; allowing multiple instances"
        ));
        return Claim::Unavailable(why);
    }

    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        // Spelled out rather than left to the default: the lock file
        // outlives whoever holds it (a crash leaves it behind, by
        // design), and truncating here would clear it out from under a
        // process that is still holding the lock. Nothing ever writes
        // bytes to it — it exists only to be locked.
        .truncate(false)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) => {
            let why = format!("could not open {}: {e}", path.display());
            report(&format!(
                "single instance: {why}; allowing multiple instances"
            ));
            return Claim::Unavailable(why);
        }
    };

    match file.try_lock() {
        Ok(()) => {
            report(&format!("single instance: lock held at {}", path.display()));
            Claim::Primary(InstanceGuard { path, _file: file })
        }
        Err(e) => {
            report(&format!(
                "single instance: {} is already locked ({e}); standing down",
                path.display()
            ));
            Claim::AlreadyRunning
        }
    }
}

/// Say a decision on both channels. See [`claim`] for why the second
/// one is not redundant.
fn report(message: &str) {
    log::log(message);
    eprintln!("snug-launcher: {message}");
}

/// Try to raise the window of an instance that is already running.
///
/// Returns whether a window was found and activated. `false` is a
/// normal outcome, not an error — see the module docs for why
/// activation and detection are allowed to disagree.
///
/// Windows enumerates top-level windows and keeps those owned by a
/// process running the *same executable*, then raises the first one
/// that has a title. Matching on the executable rather than the window
/// class is deliberate: snug does not create the application's main
/// window (JavaFX does), so there is no snug-owned class to key on, and
/// the application may legitimately own several windows.
///
/// macOS has no equivalent yet; see the `macOS` entry in the Backlog
/// section of `AGENTS.md`. Detection still works there — a second
/// launch still declines to start a second JVM — it just cannot bring
/// the first one forward.
#[cfg(windows)]
pub fn activate_existing(self_path: &Path) -> bool {
    let ours = std::process::id();
    let mut candidates = Candidates {
        image: normalize(self_path),
        ours,
        titled: None,
        untitled: None,
    };

    unsafe {
        EnumWindows(
            Some(collect_window),
            &mut candidates as *mut Candidates as LPARAM,
        );
    }

    // A titled window is the application's real main window; an
    // untitled one is usually an incidental helper. Prefer the former,
    // but take whatever exists rather than nothing.
    let Some(hwnd) = candidates.titled.or(candidates.untitled) else {
        log::log("single instance: no window found for the running instance");
        return false;
    };

    // A minimized window has to be restored first, or `SetForegroundWindow`
    // brings forward a taskbar entry rather than the app itself.
    if unsafe { IsIconic(hwnd) } != 0 {
        unsafe { ShowWindow(hwnd, SW_RESTORE) };
    }

    // `SetForegroundWindow` is refused unless the caller owns the
    // foreground. Attaching our input queue to the target window's is
    // the long-standing way to acquire that standing; without it the
    // common case — user clicks the icon in the taskbar or Explorer,
    // launching us with no foreground rights — silently does nothing.
    let target_thread = unsafe { GetWindowThreadProcessId(hwnd, std::ptr::null_mut()) };
    let our_thread = unsafe { GetCurrentThreadId() };
    let attached = target_thread != 0
        && target_thread != our_thread
        && unsafe { AttachThreadInput(our_thread, target_thread, TRUE) } != 0;
    let ok = unsafe { SetForegroundWindow(hwnd) } != 0;
    if attached {
        unsafe { AttachThreadInput(our_thread, target_thread, 0) };
    }

    log::log(&format!(
        "single instance: activated window of the running instance (ok={ok})"
    ));
    ok
}

/// Placeholder for platforms with no window-activation story yet.
///
/// macOS is the one that matters: detection works there, so a second
/// launch correctly declines to start a second JVM, but there is
/// nothing to raise. Logging rather than silently returning keeps the
/// gap visible in the launch log instead of looking like a feature
/// that works.
#[cfg(not(windows))]
pub fn activate_existing(_self_path: &Path) -> bool {
    log::log(
        "single instance: another instance is running, but bringing it to the \
         front is not implemented on this platform; it is still running",
    );
    false
}

/// Reduce a path to a form two processes will agree on.
///
/// Windows path comparison is case-insensitive, and the same file comes
/// back spelled differently depending on which API reported it:
/// `current_exe` and `QueryFullProcessImageNameW` can disagree on case
/// and on a `\\?\` verbatim prefix. Normalising both sides is cheaper
/// and far less brittle than trying to predict which prefix arrives.
#[cfg(windows)]
fn normalize(path: &Path) -> String {
    normalize_str(&path.to_string_lossy())
}

/// The same normalisation for a path that arrived as a UTF-16 buffer
/// from a Win32 call rather than as a [`Path`].
#[cfg(windows)]
fn normalize_str(raw: &str) -> String {
    let trimmed = raw.strip_prefix("\\\\?\\").unwrap_or(raw);
    trimmed
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

#[cfg(windows)]
use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, TRUE};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, SW_RESTORE, SetForegroundWindow,
    ShowWindow, WS_EX_TOOLWINDOW,
};
#[cfg(windows)]
use windows_sys::{Win32::Foundation::CloseHandle, core::PWSTR};

/// Windows search state, threaded through `EnumWindows` as a raw
/// pointer because that is the only way the Win32 callback can carry
/// anything.
#[cfg(windows)]
struct Candidates {
    /// Lowercased, prefix-stripped image path of *our* executable.
    image: String,
    /// Our own PID, so we never pick one of our own windows.
    ours: u32,
    /// Best window found so far, split by whether it has a title.
    titled: Option<HWND>,
    untitled: Option<HWND>,
}

#[cfg(windows)]
unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = unsafe { &mut *(lparam as *mut Candidates) };

    // Invisible windows are not what the user means by "bring it
    // forward", and skipping them also skips the hidden helper windows
    // some toolkits leave around.
    if unsafe { IsWindowVisible(hwnd) } == 0 {
        return TRUE;
    }

    // Tool windows are palettes, find bars and notification icons —
    // raising one of those instead of the main window looks broken.
    let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    if ex_style & WS_EX_TOOLWINDOW as isize != 0 {
        return TRUE;
    }

    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid == 0 || pid == ctx.ours {
        return TRUE;
    }

    if process_image_path(pid).as_deref() != Some(ctx.image.as_str()) {
        return TRUE;
    }

    let has_title = window_has_title(hwnd);
    if has_title {
        ctx.titled = Some(hwnd);
    } else if ctx.untitled.is_none() {
        ctx.untitled = Some(hwnd);
    }

    // Always keep enumerating: a later titled window is a better
    // answer than an earlier untitled one.
    TRUE
}

#[cfg(windows)]
fn window_has_title(hwnd: HWND) -> bool {
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return false;
    }
    let mut buf = vec![0u16; len as usize + 1];
    let written = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr() as PWSTR, buf.len() as i32) };
    written > 0
}

#[cfg(windows)]
fn process_image_path(pid: u32) -> Option<String> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }

    // Long enough for a `\\?\`-prefixed path; Windows caps the
    // un-prefixed form at MAX_PATH but honours longer when asked.
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr() as PWSTR, &mut len) };
    unsafe { CloseHandle(handle) };

    if ok == 0 {
        return None;
    }
    // `from_utf16_lossy` rather than an `OsString` round-trip: the value
    // comes from the OS, so it is already valid UTF-16, and this
    // sidesteps any dependence on which path type a `PATH_MAX`-sized
    // buffer should be built as.
    Some(normalize_str(&String::from_utf16_lossy(
        &buf[..len as usize],
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temp directory per test, so the suite never collides
    /// with a real user cache or with a parallel test.
    fn temp_root(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("snug-si-{label}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn lock_lives_in_the_cache_root() {
        let root = Path::new("/tmp/whatever");
        assert_eq!(lock_path(root), Path::new("/tmp/whatever/instance.lock"));
    }

    #[test]
    fn first_claim_wins_and_creates_the_lock() {
        let root = temp_root("first");
        match claim(&root, false) {
            Claim::Primary(guard) => {
                assert!(guard.path().exists(), "the lock file should exist");
                // The guard must keep the claim alive; dropping it here
                // releases the lock, which is the contract.
            }
            other => panic!("expected Primary, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn disabled_never_touches_the_filesystem() {
        // A path that does not exist yet. `temp_root` creates its own
        // argument, so nest one level deeper to get a genuinely absent
        // directory — if enforcement were merely *skipped* rather than
        // short-circuited before any I/O, `create_dir_all` would leave
        // this behind.
        let root = temp_root("disabled").join("never-created");
        assert!(matches!(claim(&root, true), Claim::Disabled));
        assert!(
            !root.exists(),
            "--allow-multiple-instances must not create {root:?}"
        );
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn an_uncreatable_cache_root_fails_open() {
        // A path whose parent is an existing *file* can never be a
        // directory, so `create_dir_all` must fail and we must not
        // refuse to launch.
        let blocker = temp_root("blocked").join("a-file");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let root = blocker.join("cache");

        match claim(&root, false) {
            Claim::Unavailable(why) => assert!(!why.is_empty()),
            other => panic!("expected Unavailable, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(blocker.parent().unwrap());
    }

    #[test]
    fn a_released_lock_can_be_reclaimed() {
        let root = temp_root("reclaim");
        {
            let first = claim(&root, false);
            assert!(matches!(first, Claim::Primary(_)));
        } // dropped here
        // The lock file survives its holder by design, so this also
        // pins that a stale file is not mistaken for a live claim.
        assert!(lock_path(&root).exists());
        assert!(matches!(claim(&root, false), Claim::Primary(_)));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn activate_existing_is_safe_to_call_with_no_other_instance() {
        // Nothing is running, so this must report failure rather than
        // hang or abort.
        let me = std::env::current_exe().unwrap();
        assert!(!activate_existing(&me));
    }
}
