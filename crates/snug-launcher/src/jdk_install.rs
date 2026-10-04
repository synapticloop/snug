//! "No JDK found" → download-and-install flow.
//!
//! 1. **`find_cached_jdk`** scans `%LOCALAPPDATA%\snug\jdk\` for a
//!    previously-downloaded JDK whose `java -version` reports a major
//!    ≥ `min_java_major`. Hit → return silently, no prompt.
//! 2. **`fetch_metadata`** hits Adoptium's v3 API for the latest
//!    Temurin GA matching the requested major.
//! 3. **`prompt_window::show`** (in `prompt_window.rs`) renders the
//!    "Java Runtime Required" dialog as a custom-painted modal on the
//!    same paint path as `error_window` / `retry_window` /
//!    `metadata_failed_window`. Three buttons (Download / Open in
//!    browser / Cancel). The "do not show again" checkbox has been
//!    **removed** per product decision — the cache layer means the
//!    user only sees the prompt when no usable JDK is on disk, so
//!    re-asking is fine.
//! 4. **`progress_window::show`** renders a custom-painted modal
//!    matching the user-facing mockup, drives the bar from the
//!    worker thread, and auto-dismisses on completion or failure.
//! 5. The worker thread streams the zip straight to disk (no in-memory
//!    SHA), then hashes the **file on disk** via [`hash_file_sha256`],
//!    then extracts.
//!
//! # UX paths
//!
//! | State on entry              | Result                              |
//! |-----------------------------|-------------------------------------|
//! | Cached JDK satisfies min    | return cached home, no GUI          |
//! | No cache, user picks Open   | open URL in browser, no install     |
//! | No cache, user picks Cancel | bail, no install                    |
//! | No cache, user picks Download | progress dialog â†’ worker thread â†’ result |

#![cfg(any(windows, target_os = "macos"))]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::sync::atomic::AtomicIsize;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

/// `Win32` process-creation flag that prevents Windows from allocating a
/// new console for the child. Without this, spawning a console-subsystem
/// binary (like `java.exe`) from our GUI-subsystem launcher would flash a
/// command prompt window briefly before the child exits.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

use crate::dialogs;
use crate::log;

use serde::Deserialize;
use sha2::{Digest, Sha256};

#[cfg(windows)]
use windows_sys::Win32::Foundation::HWND;
#[cfg(windows)]
use windows_sys::Win32::UI::Shell::ShellExecuteW;
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    HICON, IDCANCEL, IDOK, IDYES, SW_SHOWNORMAL,
};

/// Mirrors of the Win32 control IDs, for platforms that have no
/// `windows-sys` dependency at all.
///
/// Only the comparisons matter: the flow compares what `ui::*` returned
/// against these, and on a non-Windows target `ui::*` is the only thing
/// that produced a return value. Declared here rather than imported
/// because `windows-sys` is not a dependency off Windows.
#[cfg(not(windows))]
mod id {
    /// `IDYES`
    pub const IDYES: i32 = 6;
    /// `IDOK`
    #[allow(dead_code)]
    pub const IDOK: i32 = 1;
    /// `IDCANCEL`
    #[allow(dead_code)]
    pub const IDCANCEL: i32 = 2;
}
#[cfg(not(windows))]
use id::IDYES;

// ===========================================================================
//  Errors
// ===========================================================================

#[derive(Debug)]
pub enum JdkError {
    MetadataFetch(String),
    NoMetadataForVersion(u16),
    BadMetadataShape(String),
    BadField { name: &'static str, value: String },
    Download(String),
    Sha256Mismatch { declared: String, computed: String },
    Extract(String),
    NoJavaExe(PathBuf),
    Io(std::io::Error),
    Dialog(String),
}

impl std::fmt::Display for JdkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Localized via the `jdk.err.*` keys in the
        // `snug-localisations.<tag>.txt` bundle. The English baseline
        // ships the same wording as the original literals, so the
        // user-visible text is unchanged when no other locale is
        // bundled.
        let s = match self {
            JdkError::MetadataFetch(_) => crate::localize::lookup("jdk.err.metadata_fetch"),
            JdkError::NoMetadataForVersion(_) => {
                crate::localize::lookup("jdk.err.no_metadata_for_version")
            }
            JdkError::BadMetadataShape(_) => {
                crate::localize::lookup("jdk.err.bad_metadata_shape")
            }
            JdkError::BadField { .. } => crate::localize::lookup("jdk.err.bad_field"),
            JdkError::Download(_) => crate::localize::lookup("jdk.err.download"),
            JdkError::Sha256Mismatch { .. } => crate::localize::lookup("jdk.err.sha256_mismatch"),
            JdkError::Extract(_) => crate::localize::lookup("jdk.err.extract"),
            JdkError::NoJavaExe(_) => crate::localize::lookup("jdk.err.no_java_exe"),
            JdkError::Io(_) => crate::localize::lookup("jdk.err.io"),
            JdkError::Dialog(_) => crate::localize::lookup("jdk.err.dialog"),
        };
        let rendered: String = match self {
            JdkError::MetadataFetch(m) => {
                crate::localize::fill_placeholders(&s, &[("0", &m.to_string())])
            }
            JdkError::NoMetadataForVersion(v) => {
                crate::localize::fill_placeholders(&s, &[("0", &v.to_string())])
            }
            JdkError::BadMetadataShape(m) => {
                crate::localize::fill_placeholders(&s, &[("0", &m.to_string())])
            }
            JdkError::BadField { name, value } => {
                crate::localize::fill_placeholders(&s, &[("name", name), ("value", value)])
            }
            JdkError::Download(m) => {
                crate::localize::fill_placeholders(&s, &[("0", &m.to_string())])
            }
            JdkError::Sha256Mismatch { declared, computed } => {
                crate::localize::fill_placeholders(
                    &s,
                    &[("declared", declared), ("computed", computed)],
                )
            }
            JdkError::Extract(m) => {
                crate::localize::fill_placeholders(&s, &[("0", &m.to_string())])
            }
            JdkError::NoJavaExe(p) => {
                crate::localize::fill_placeholders(&s, &[("0", &p.display().to_string())])
            }
            JdkError::Io(e) => {
                crate::localize::fill_placeholders(&s, &[("0", &e.to_string())])
            }
            JdkError::Dialog(m) => {
                crate::localize::fill_placeholders(&s, &[("0", &m.to_string())])
            }
        };
        f.write_str(&rendered)
    }
}

impl std::error::Error for JdkError {}

impl From<std::io::Error> for JdkError {
    fn from(e: std::io::Error) -> Self {
        JdkError::Io(e)
    }
}

impl From<zip::result::ZipError> for JdkError {
    fn from(e: zip::result::ZipError) -> Self {
        JdkError::Extract(e.to_string())
    }
}

// ===========================================================================
//  Adoptium metadata
// ===========================================================================

#[derive(Debug, Deserialize)]
struct AdoptiumAssetList(Vec<AdoptiumAsset>);

#[derive(Debug, Deserialize)]
struct AdoptiumAsset {
    binaries: Vec<AdoptiumBinary>,
    version_data: AdoptiumVersion,
}

#[derive(Debug, Deserialize)]
struct AdoptiumBinary {
    package: AdoptiumPackage,
}

#[derive(Debug, Deserialize)]
struct AdoptiumPackage {
    link: String,
    /// SHA-256 of the zip. Not optional: every Adoptium release
    /// entry includes a checksum.
    checksum: String,
    size: u64,
}

#[derive(Debug, Deserialize)]
struct AdoptiumVersion {
    semver: Option<String>,
}

#[derive(Debug, Clone)]
pub struct JdkMetadata {
    pub package_link: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub version: String,
    pub major: u16,
}

/// Build an HTTP agent with timeouts that suit a multi-hundred-megabyte
/// download.
///
/// `ureq` defaults to `timeout_read: None` — **no read timeout at all** —
/// so a connection that establishes and then goes quiet blocks forever with
/// no error, no retry, and nothing in the log. That is indistinguishable
/// from a hang, and it is the worst possible failure for something the user
/// cannot see into.
///
/// A per-read timeout is the right shape rather than a total-download
/// budget: each successful read resets it, so a slow-but-progressing
/// 185 MB fetch is never killed, while a dead socket is caught in 30
/// seconds and handed to the retry dialog.
fn http_agent() -> Result<std::sync::Arc<ureq::Agent>, JdkError> {
    use std::time::Duration;
    use ureq::native_tls::TlsConnector;

    Ok(std::sync::Arc::new(
        ureq::AgentBuilder::new()
            .tls_connector(std::sync::Arc::new(
                TlsConnector::new().map_err(|e| JdkError::Download(e.to_string()))?,
            ))
            // Stated rather than relied upon: this happens to match
            // ureq's default today, and a silent change there should not
            // silently change our behaviour.
            .timeout_connect(Duration::from_secs(30))
            // Per-read, so a stall is caught but progress is not punished.
            .timeout_read(Duration::from_secs(30))
            .timeout_write(Duration::from_secs(30))
            .build(),
    ))
}

pub fn fetch_metadata(min_java_major: u16) -> Result<JdkMetadata, JdkError> {
    let agent = http_agent()?;

    // The `/v3/assets/latest/{maj}/hotspots` endpoint returns an empty
    // array for current majors (verified against Adoptium 2026-09). The
    // `/v3/assets/feature_releases/{maj}/ga` endpoint returns the full
    // GA release list with `binaries[].package.{link,checksum,size}`
    // and `version_data.semver` â€” exactly the fields `AdoptiumBinary`
    // deserialises. We take the first element, which Adoptium returns
    // sorted newest-first by `timestamp`.
    let (os, arch) = adoptium_target();
    let url = format!(
        "https://api.adoptium.net/v3/assets/feature_releases/{maj}/ga\
         ?architecture={arch}&image_type=jdk&os={os}&vendor=eclipse",
        maj = min_java_major,
    );

    let resp = agent
        .get(&url)
        .call()
        .map_err(|e| JdkError::MetadataFetch(e.to_string()))?;
    if resp.status() != 200 {
        return Err(JdkError::MetadataFetch(format!(
            "HTTP {} for {}",
            resp.status(),
            url
        )));
    }
    let body = resp
        .into_string()
        .map_err(|e| JdkError::MetadataFetch(e.to_string()))?;
    let list: AdoptiumAssetList = serde_json::from_str(&body)
        .map_err(|e| JdkError::BadMetadataShape(e.to_string()))?;

    let asset = list
        .0
        .into_iter()
        .next()
        .ok_or(JdkError::NoMetadataForVersion(min_java_major))?;

    // The query filters narrow each release's `binaries[]` to at most
    // one entry. If the API ever returns more, prefer the `package`
    // (zip) form over the `installer` (msi) form â€” both live on the
    // same binary but the zip is what we extract.
    let binary = asset
        .binaries
        .into_iter()
        .next()
        .ok_or_else(|| JdkError::BadField {
            name: "binaries",
            value: "empty".into(),
        })?;

    let package_link = binary.package.link;
    let sha256 = binary.package.checksum;
    let size = binary.package.size;
    let version = asset
        .version_data
        .semver
        .unwrap_or_else(|| format!("{min_java_major}"));

    Ok(JdkMetadata {
        package_link,
        sha256,
        size_bytes: size,
        version,
        major: min_java_major,
    })
}


// ===========================================================================
//  Progress dialog: callback + thread-local shared state
// ===========================================================================

/// Shared state between the worker thread (writes) and the
/// `progress_window` reads via the per-field getters / setters below;
/// `worker_thread` writes. `pub` so the preview bin in
/// `examples/progress_preview.rs` can build its own.
pub struct ProgressShared {
    /// 0..=100 download percent. Set by the worker during phases
    /// 0/1/2 â€” the progress window reads this and repaints the bar
    /// in `WM_PAINT`.
    pub(crate) pct: AtomicU32,
    /// 0 = running, 1 = success, 2 = error, 3 = cancelled.
    pub(crate) done: AtomicI32,
    /// Set on failure.
    error: std::sync::Mutex<Option<String>>,
    /// 0 = downloading, 1 = verifying SHA-256, 2 = extracting.
    /// Drives the live status text the progress window renders in
    /// `WM_TIMER`.
    pub(crate) phase: AtomicI32,
    /// Bytes written to the temp zip so far (phase 0). The progress
    /// window uses this to render "X MB / Y MB" in real time.
    pub(crate) bytes: AtomicU64,
    /// Total bytes from the Adoptium metadata. Captured at init so
    /// the progress window can render percentages even after the
    /// worker thread has moved on to verify/extract.
    pub(crate) total_bytes: AtomicU64,
    /// Set to `true` by the progress window when the user clicks
    /// the "Install" button. The worker spins on this at the top of
    /// `worker_thread` so the download doesn't actually start until
    /// the user has explicitly opted in â€” the progress window
    /// appears in a "ready to install" paused state.
    pub(crate) started: AtomicBool,
    /// Cancellation latch. Flipped to `true` by the progress window
    /// when the user clicks Cancel mid-download (and by
    /// `run_one_install_attempt` when the dialog returns anything
    /// other than IDOK). `download_to_disk` consults it once per
    /// read so the in-flight network read aborts within ~256 KB;
    /// the worker also checks it between phases (verify, extract) so
    /// post-download phases exit promptly. Lives on `shared` so the
    /// dialog, which only sees `shared`, can flip it without a
    /// second Arc.
    pub(crate) cancel: AtomicBool,
    /// Optional `HBITMAP` handle (cast to `i32`) for a custom mascot
    /// to draw in the 170Ã—170 slot. `0` means "fall back to the EXE's
    /// main icon resource" (the production path). The
    /// `progress_preview` bin embeds `assets/snug-icon.png` via
    /// `include_bytes!`, decodes it with the `image` crate, and
    /// stores the resulting top-down DIB section here. The dialog
    /// paints this bitmap via `StretchDIBits` ahead of the icon
    /// fallback in `WM_PAINT`.
    pub(crate) mascot: AtomicI32,
    /// Resolved JAVA_HOME after a successful extract. The worker
    /// writes this so the caller doesn't have to walk the
    /// extracted tree again.
    home: std::sync::Mutex<Option<PathBuf>>,
}

unsafe impl Send for ProgressShared {}
unsafe impl Sync for ProgressShared {}

impl ProgressShared {
    /// Construct a fresh `ProgressShared` initialised for a download of
    /// `total_bytes` bytes. Mirrors the literal the production worker
    /// thread uses so `progress_preview` (and any future test harness)
    /// can build one without poking at private fields.
    pub fn new(total_bytes: u64) -> Self {
        Self {
            pct: AtomicU32::new(0),
            done: AtomicI32::new(0),
            error: Mutex::new(None),
            phase: AtomicI32::new(0),
            bytes: AtomicU64::new(0),
            total_bytes: AtomicU64::new(total_bytes),
            home: Mutex::new(None),
            started: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
            mascot: AtomicI32::new(0),
        }
    }

    // -- Getter / setter pairs for the worker-accessible fields. ----
    //
    // The fields themselves stay `pub(crate)`; these accessors are the
    // only way binaries inside the `snug-launcher` package (currently
    // just `progress_preview`) can read or drive the dialog state.
    //
    // All use `Ordering::SeqCst` to match the production worker
    // thread and dialog callback. The dialog polls at 5 Hz so the cost
    // of a function-call wrapper over an atomic load is irrelevant.

    /// Current 0..=100 percent shown on the bar.
    pub fn pct(&self) -> u32 {
        self.pct.load(Ordering::SeqCst)
    }
    /// Update the bar percentage.
    pub fn set_pct(&self, pct: u32) {
        self.pct.store(pct, Ordering::SeqCst);
    }

    /// Bytes downloaded so far (phase 0). Goes stale once the worker
    /// moves on to verify / extract.
    pub fn bytes_done(&self) -> u64 {
        self.bytes.load(Ordering::SeqCst)
    }
    /// Update the byte counter (used by the worker and by the preview
    /// worker thread).
    pub fn set_bytes_done(&self, bytes: u64) {
        self.bytes.store(bytes, Ordering::SeqCst);
    }

    /// Total bytes the download is expected to land at. Captured at
    /// construction from the Adoptium metadata (or, in the preview,
    /// from the CLI args).
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes.load(Ordering::SeqCst)
    }

    /// Current phase: 0 = downloading, 1 = verifying SHA-256,
    /// 2 = extracting.
    pub fn phase(&self) -> i32 {
        self.phase.load(Ordering::SeqCst)
    }
    /// Update the current phase.
    pub fn set_phase(&self, phase: i32) {
        self.phase.store(phase, Ordering::SeqCst);
    }

    /// `true` once the user has clicked Install (i.e. the worker
    /// thread has been released from its `started` spin loop).
    pub fn is_started(&self) -> bool {
        self.started.load(Ordering::SeqCst)
    }
    /// Set the started flag.
    pub fn set_started(&self, started: bool) {
        self.started.store(started, Ordering::SeqCst);
    }

    /// Terminal status: 0 = running, 1 = success, 2 = error,
    /// 3 = cancelled. The dialog observes non-zero and exits.
    pub fn status(&self) -> i32 {
        self.done.load(Ordering::SeqCst)
    }
    /// Set the terminal status.
    pub fn set_status(&self, status: i32) {
        self.done.store(status, Ordering::SeqCst);
    }

    /// Optional `HBITMAP` handle (cast to `i32`) for a custom mascot
    /// image. `0` means "use the EXE icon fallback". See
    /// [`ProgressShared::mascot`] for the field doc.
    pub fn mascot_hbitmap(&self) -> i32 {
        self.mascot.load(Ordering::SeqCst)
    }
    /// Set the mascot `HBITMAP`. Pass `0` to clear (use icon
    /// fallback). The bitmap must remain valid for as long as the
    /// dialog is open.
    pub fn set_mascot_hbitmap(&self, hbitmap: i32) {
        self.mascot.store(hbitmap, Ordering::SeqCst);
    }
}

/// Process-wide override for the dialogs' **title-bar / Alt-Tab /
/// taskbar** icon, as an `HICON` cast to `isize`. `0` — the default —
/// means "read the EXE's own `MAINICON` resource", which is what every
/// shipped launcher does.
///
/// This exists for `snug_preview --icon <FILE>`. Swapping the fallback
/// icon any other way means relinking the EXE, because the resource
/// lives *in* the binary, and `bin/launcher-stub.exe` is committed and
/// cross-compiled from macOS/Linux — far too slow a loop for "try this
/// icon across all eight dialogs". The production launcher never calls
/// the setter, so this is inert there and costs one relaxed atomic read
/// on the fallback path.
///
/// The handle is deliberately never destroyed: dialogs are top-level
/// windows on detached threads that can outlive whatever installed the
/// override, so the effective owner is the process. Handles from
/// `LoadImageW(..., LR_LOADFROMFILE)` are additionally system-managed.
#[cfg(windows)]
static WINDOW_ICON_OVERRIDE: AtomicIsize = AtomicIsize::new(0);

/// Process-wide override for the in-dialog **mascot** image (the large
/// picture at the top-left), same `isize`-cast-`HICON` convention and
/// same "never the production path" reasoning as
/// [`WINDOW_ICON_OVERRIDE`].
///
/// Kept separate from the title-bar one because they want different
/// pixel sizes: the mascot box is ~154 px, so the caller loads at
/// [`MASCOT_LOAD_CX`] while the title bar wants `SM_CXICON`. Loading
/// once at each size and handing the right handle to the right slot
/// also avoids upscaling a 32 px icon into a 154 px box.
#[cfg(windows)]
static MASCOT_ICON_OVERRIDE: AtomicIsize = AtomicIsize::new(0);

/// Install the dialogs' title-bar / Alt-Tab / taskbar icon override.
/// Pass `0` to go back to the EXE resource.
#[cfg(windows)]
pub fn set_window_icon_override(hicon: isize) {
    WINDOW_ICON_OVERRIDE.store(hicon, Ordering::SeqCst);
}

/// The current title-bar icon override, `None` when unset.
#[cfg(windows)]
pub fn window_icon_override() -> Option<HICON> {
    match WINDOW_ICON_OVERRIDE.load(Ordering::SeqCst) {
        0 => None,
        other => Some(other as HICON),
    }
}

/// Install the in-dialog mascot image override. Pass `0` to go back to
/// the EXE icon.
#[cfg(windows)]
pub fn set_mascot_icon_override(hicon: isize) {
    MASCOT_ICON_OVERRIDE.store(hicon, Ordering::SeqCst);
}

/// The current mascot override, `None` when unset.
#[cfg(windows)]
pub fn mascot_icon_override() -> Option<HICON> {
    match MASCOT_ICON_OVERRIDE.load(Ordering::SeqCst) {
        0 => None,
        other => Some(other as HICON),
    }
}

/// Load the EXE's main icon for the window class / per-window
/// `WM_SETICON` slots, unless a process-wide override is installed by
/// `snug_preview --icon` (see [`WINDOW_ICON_OVERRIDE`]).
///
/// Picks the system small-icon size (typically16 / 32 px depending on
/// DPI) so the title bar / taskbar end up with their natural pixel
/// target. Delegates to [`find_best_icon_hicon`] which walks the
/// resource tree rather than guessing at a fixed resource id.
#[cfg(windows)]
pub(crate) fn load_exe_main_icon_hicon() -> Option<HICON> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXICON, SM_CYICON};

    if let Some(hicon) = window_icon_override() {
        return Some(hicon);
    }

    let cx = unsafe { GetSystemMetrics(SM_CXICON) };
    let cy = unsafe { GetSystemMetrics(SM_CYICON) };
    find_best_icon_hicon(cx, cy)
}

/// Walk the running EXE's resource directory, find the icon group
/// `editpe` stamped (`RT_GROUP_ICON` entry named `"MAINICON"` â€” also
/// tries id=1 as a fallback for EXEs built by other tools), and load
/// the entry whose bitmap dimensions are closest to `(cx, cy)`.
/// Returns `None` if the EXE has no icon group at all.
///
/// This is the workaround for `editpe` v0.2 not stamping at integer
/// id 1: it picks the icon group's sub-table by **name**, then reads
/// the actual RT_ICON ids from the ICONDIR and `LoadImageW`s against
/// one of those. The returned handle is `LR_SHARED` â€” the system owns
/// it, no `DestroyIcon` needed.
#[cfg(windows)]
pub(crate) fn find_best_icon_hicon(cx: i32, cy: i32) -> Option<HICON> {
    use windows_sys::Win32::System::LibraryLoader::{
        FindResourceW, GetModuleHandleW, LoadResource, LockResource, SizeofResource,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{CreateIconFromResourceEx, LR_SHARED};

    const RT_GROUP_ICON: u16 = 14;
    const RT_ICON: u16 = 3;

    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        if hinst.is_null() {
            return None;
        }

        // Try the "MAINICON" name first (which is what `editpe`
        // writes), then fall back to id=1 (Windows convention, used
        // by `rcedit` and similar tools).
        let name_w: Vec<u16> = "MAINICON".encode_utf16().chain(std::iter::once(0)).collect();
        let hres_named = FindResourceW(hinst, name_w.as_ptr(), RT_GROUP_ICON as *const u16);
        let hres = if hres_named.is_null() {
            FindResourceW(hinst, 1usize as *const u16, RT_GROUP_ICON as *const u16)
        } else {
            hres_named
        };
        if hres.is_null() {
            return None;
        }

        let hmem = LoadResource(hinst, hres);
        if hmem.is_null() {
            return None;
        }
        let pdata = LockResource(hmem);
        if pdata.is_null() {
            return None;
        }

        // ICONDIR layout per `editpe`'s `IconDirectory` / `IconDirectoryEntry`:
        //   header: reserved:u16, type_:u16, count:u16  (6 bytes)
        //   count Ã— ICONDIRENTRY (14 bytes each, repr(C, packed(2))):
        //     width:u8, height:u8, color_count:u8, reserved:u8
        //     planes:u16, bit_count:u16, bytes:u32, id:u16
        // Note: `editpe` replaces the standard Windows 4-byte `image_offset`
        // with a 2-byte RT_ICON id; the entry is therefore 14 bytes, not
        // the 16-byte ICONDIRENTRY Windows uses for .ico files on disk.
        let pbytes = pdata as *const u8;
        let count = (pbytes.add(4) as *const u16).read_unaligned() as usize;
        if count == 0 {
            return None;
        }

        let mut best_id: u16 = 0;
        let mut best_diff: u32 = u32::MAX;
        for i in 0..count {
            let entry = pbytes.add(6 + i * 14); // 14-byte stride for `editpe`'s layout
            let w_raw = entry.read() as u32;
            let h_raw = entry.add(1).read() as u32;
            let w = if w_raw == 0 { 256 } else { w_raw };
            let h = if h_raw == 0 { 256 } else { h_raw };
            let id = (entry.add(12) as *const u16).read_unaligned();
            let diff = ((w as i32 - cx).abs() + (h as i32 - cy).abs()) as u32;
            if diff < best_diff {
                best_diff = diff;
                best_id = id;
            }
        }
        if best_id == 0 {
            return None;
        }

        // RT_ICON contains either PNG-compressed pixels or a DIB, not
        // a complete ICO file. Let Windows decode either representation.
        let hres_icon = FindResourceW(
            hinst,
            best_id as usize as *const u16,
            RT_ICON as *const u16,
        );
        if hres_icon.is_null() {
            return None;
        }
        let bytes = SizeofResource(hinst, hres_icon);
        if bytes == 0 {
            return None;
        }
        let hmem_icon = LoadResource(hinst, hres_icon);
        if hmem_icon.is_null() {
            return None;
        }
        let pbits = LockResource(hmem_icon);
        if pbits.is_null() {
            return None;
        }

        // `CreateIconFromResourceEx` flags: `fIcon=1` (true) for icons,
        // `dwVer=0x00030000` for Win3.0+ format. `LR_SHARED` returns a
        // shared handle the system manages â€” no `DestroyIcon` needed.
        let hicon = CreateIconFromResourceEx(
            pbits as *const u8,
            bytes,
            1,
            0x00030000,
            cx,
            cy,
            LR_SHARED,
        );
        if hicon.is_null() {
            None
        } else {
            Some(hicon)
        }
    }
}



// ===========================================================================
//  Stream-to-disk, hash-on-disk, extract
// ===========================================================================

/// Download `url` to `dest` as a streaming copy. **No** SHA-256 is
/// computed here â€” the file is hashed after the download in
/// [`hash_file_sha256`], so multi-gigabyte zips don't pressure RAM.
fn download_to_disk(
    url: &str,
    dest: &Path,
    cancel: &AtomicBool,
    total_bytes: u64,
    mut on_progress: impl FnMut(u64),
) -> Result<u64, JdkError> {
    let agent = http_agent()?;

    let resp = agent
        .get(url)
        .call()
        .map_err(|e| JdkError::Download(e.to_string()))?;
    if resp.status() != 200 {
        return Err(JdkError::Download(format!("HTTP {} for {url}", resp.status())));
    }

    let mut f = std::fs::File::create(dest)?;
    let mut stream = resp.into_reader();
    let mut buf = vec![0u8; 256 * 1024];
    let mut total_written: u64 = 0;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(JdkError::Download("cancelled by user".into()));
        }
        let n = match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => return Err(JdkError::Download(format!("read body: {e}"))),
        };
        f.write_all(&buf[..n])?;
        total_written += n as u64;
        if total_bytes > 0 {
            on_progress(total_written);
        }
    }
    f.flush()?;
    Ok(total_written)
}

/// Hash a file by streaming through SHA-256. Memory usage is one
/// chunk buffer (~64 KB), regardless of file size.
fn hash_file_sha256(path: &Path) -> Result<String, JdkError> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Unpack a downloaded JDK archive into `dest_dir`.
///
/// Split per platform because Adoptium ships a different format for each:
/// Windows gets a `.zip`, macOS a `.tar.gz`. There is no runtime sniffing
/// of the file name — a hardcoded zip reader handed a tarball does not
/// fail on the tarball, it fails much later during extraction with an
/// unhelpful "not a zip file" about a file that was never supposed to be
/// one. Each platform asserts the name it expects and says so plainly.
#[cfg(windows)]
fn extract_jdk_archive(archive: &Path, dest_dir: &Path) -> Result<PathBuf, JdkError> {
    expect_extension(archive, ".zip")?;
    extract_jdk_zip(archive, dest_dir)
}

#[cfg(target_os = "macos")]
fn extract_jdk_archive(archive: &Path, dest_dir: &Path) -> Result<PathBuf, JdkError> {
    expect_extension(archive, ".tar.gz")?;
    let file = std::fs::File::open(archive)?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(gz);
    std::fs::create_dir_all(dest_dir)?;
    // The `tar` crate refuses absolute paths and `..` components itself,
    // which is the property we want: this archive came off the network.
    tar.unpack(dest_dir)
        .map_err(|e| JdkError::Extract(format!("unpack {}: {e}", archive.display())))?;
    Ok(dest_dir.to_path_buf())
}

/// Fail early, and legibly, if the download is not the format this
/// platform asked for.
#[cfg(any(windows, target_os = "macos"))]
fn expect_extension(archive: &Path, expected: &str) -> Result<(), JdkError> {
    let mut name = archive
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    // The download lands in a *temp* file, so the real name on disk is
    // `<major>.tar.gz.tmp`, not `<major>.tar.gz`. Strip that before
    // looking at the extension — checking the raw name rejects every
    // download the flow actually performs.
    if let Some(stripped) = name.strip_suffix(".tmp") {
        name = stripped.to_string();
    }
    if name.ends_with(expected) {
        Ok(())
    } else {
        Err(JdkError::Extract(format!(
            "expected a {expected} archive on this platform, got {}",
            archive.display()
        )))
    }
}

#[cfg(windows)]
fn extract_jdk_zip(zip: &Path, dest_dir: &Path) -> Result<PathBuf, JdkError> {
    let file = std::fs::File::open(zip)?;
    let mut zip_reader = zip::ZipArchive::new(file)?;
    std::fs::create_dir_all(dest_dir)?;
    for i in 0..zip_reader.len() {
        let mut entry = zip_reader.by_index(i)?;
        let raw = match entry.enclosed_name() {
            Some(p) => p.to_path_buf(),
            None => continue,
        };
        let outpath = dest_dir.join(raw);
        if entry.is_dir() {
            std::fs::create_dir_all(&outpath)?;
            continue;
        }
        if let Some(parent) = outpath.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&outpath)?;
        std::io::copy(&mut entry, &mut out)?;
    }
    Ok(dest_dir.to_path_buf())
}

/// `extract_jdk_zip` writes everything verbatim from the zip into
/// `dest_dir`. Adoptium's zips carry a single leading
/// `jdk-<version>/` directory, so the extracted layout is:
///
/// ```text
/// dest_dir/
///   jdk-25.0.4.1+1/
///     bin/java.exe
///     conf/
///     jmods/
///     lib/
///     release
///     ...
/// ```
///
/// This helper walks a small depth cap and returns the first
/// directory that actually contains `bin/java.exe` â€” that's the
/// JAVA_HOME we need to hand to JNI. Returns `None` if no
/// directory inside `root` (up to `MAX_JAVA_HOME_DEPTH` levels)
/// contains the JDK layout, which means the zip is not a Temurin
/// JDK or the extraction went sideways.
const MAX_JAVA_HOME_DEPTH: usize = 3;

fn find_java_home(root: &Path) -> Option<PathBuf> {
    fn recurse(dir: &Path, depth: usize) -> Option<PathBuf> {
        if depth > MAX_JAVA_HOME_DEPTH {
            return None;
        }
        let java = java_binary(dir);
        if java.is_file() {
            return Some(dir.to_path_buf());
        }
        let entries = std::fs::read_dir(dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(home) = recurse(&path, depth + 1) {
                    return Some(home);
                }
            }
        }
        None
    }
    recurse(root, 0)
}

/// Parses the first version token out of `java -version` stdout/stderr.
fn parse_java_version(text: &str) -> Option<u16> {
    let needle = "version \"";
    let i = text.find(needle)?;
    let rest = &text[i + needle.len()..];
    let end = rest.find('"')?;
    let token = &rest[..end];
    let major = token.split('.').next()?;
    major.parse::<u16>().ok()
}

// ===========================================================================
//  Caching: reuse a previously-downloaded JDK if one is good enough
// ===========================================================================

/// Scan `install_root` for any directory containing `bin\java.exe`
/// whose `-version` reports a major version â‰¥ `min_java_major`.
///
/// Each top-level entry under `install_root` is a previously-
/// extracted Temurin install; the actual JAVA_HOME may be the entry
/// itself (flat layout) or one nested directory inside it (Adoptium
/// default â€” `jdk-25.0.4.1+1/bin/java.exe`). `find_java_home` walks
/// both shapes.
fn find_cached_jdk(min_java_major: u16, install_root: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(install_root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(home) = find_java_home(&path) else {
            continue;
        };
        let java = java_binary(&home);
        let Ok(out) = java_version_probe(&java) else {
            continue;
        };
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if let Some(major) = parse_java_version(&combined) {
            if major >= min_java_major {
                return Some(home);
            }
        }
    }
    None
}

// ===========================================================================
//  Worker thread: download â†’ hash â†’ extract
// ===========================================================================

// Monotonic progress-bar boundaries. The bar must only ever move
// forward across phase transitions â€” earlier versions let it jump
// 99 â†’ 95 â†’ 99 â†’ 100 which read as "the bar reset". See
// `tests::bar_progress_is_monotonic_across_phases`.
const PHASE_0_PCT_MAX: u32 = 95; // download ends here
const PHASE_1_PCT_START: u32 = 96; // verify starts here
const PHASE_1_PCT_END: u32 = 98; // verify ends here
const PHASE_2_PCT_START: u32 = 99; // extract starts here

fn worker_thread(
    install_dir: PathBuf,
    tmp_zip: PathBuf,
    url: String,
    expected_sha: String,
    total_bytes: u64,
    shared: Arc<ProgressShared>,
) {
    let set_error = |msg: String| {
        if let Ok(mut g) = shared.error.lock() {
            *g = Some(msg);
        }
    };

    // There is deliberately no wait for a click here any more.
    //
    // This used to spin on `shared.started` until the progress window's
    // "Install" button was pressed, which made that click the *only* thing
    // that could start a download — with no timeout, so a platform where
    // the window cannot appear hung forever. The on-disk evidence is
    // unambiguous: the install directory gets created, but the
    // `*.tar.gz.tmp` that `download_to_disk` would create never does,
    // because the worker never got past this loop. Minutes of silence and
    // zero bytes, with every network timeout in the flow never reached
    // because no connection was ever opened.
    //
    // The ask is `ui::consent`, which runs on the calling thread before
    // this thread exists. If the user said yes we are here; if they said
    // no this function was never called. Nothing left to wait for.

    // The bar is split monotonically across the three phases so the user
    // never sees it move backwards:
//   - phase 0 (download)   : 0 â†’ 95%
//   - phase 1 (verify SHA) : 95 â†’ 98%
//   - phase 2 (extract)    : 98 â†’ 100%
// The earlier 0â†’99â†’95â†’99â†’100 sequence made the bar look like it
// reset when the user clicked Download â€” confusing.

    // Phase 0: download. `on_progress` updates the bytes/pct shared
    // state so the dialog callback can render live text + bar.
    shared.phase.store(0, Ordering::SeqCst);
    log::log(&format!(
        "phase 0 (download): url={url}, expected_size={total_bytes} bytes"
    ));
    let on_progress = |written: u64| {
        shared.bytes.store(written, Ordering::SeqCst);
        if total_bytes > 0 {
            // Scale phase 0 to 0..=PHASE_0_PCT_MAX. Phase 1 picks up at
            // PHASE_1_PCT_START and climbs to PHASE_1_PCT_END; phase 2
            // takes PHASE_2_PCT_START â†’ 100.
            let pct = ((written as f64 / total_bytes as f64 * PHASE_0_PCT_MAX as f64) as u32)
                .min(PHASE_0_PCT_MAX);
            shared.pct.store(pct, Ordering::SeqCst);
        }
    };

    match download_to_disk(&url, &tmp_zip, &shared.cancel, total_bytes, on_progress) {
        Ok(written) => {
            log::log(&format!(
                "phase 0 complete: {} bytes written to {}",
                written,
                tmp_zip.display()
            ));
        }
        Err(e) => {
            log::log(&format!("phase 0 failed: download: {e}"));
            set_error(format!("download: {e}"));
            shared.done.store(2, Ordering::SeqCst);
            return;
        }
    }
    if shared.cancel.load(Ordering::SeqCst) {
        log::log("phase 0 cancelled by user");
        shared.done.store(3, Ordering::SeqCst);
        return;
    }
    shared.bytes.store(total_bytes, Ordering::SeqCst);
    // Land exactly on PHASE_0_PCT_MAX so phase 1 picks up with no jump.
    shared.pct.store(PHASE_0_PCT_MAX, Ordering::SeqCst);

    // Phase 1: SHA-256. Hash is a single sequential pass over the
    // file, so the bar climbs PHASE_1_PCT_START â†’ PHASE_1_PCT_END
    // within this phase and the callback renders "Verifying SHA-256â€¦
    // (Z%)" while it does.
    shared.phase.store(1, Ordering::SeqCst);
    log::log("phase 1 (verify SHA-256) starting");
    shared.pct.store(PHASE_1_PCT_START, Ordering::SeqCst);

    let computed = match hash_file_sha256(&tmp_zip) {
        Ok(h) => {
            log::log(&format!("phase 1: computed SHA-256 = {h}"));
            h
        }
        Err(e) => {
            log::log(&format!("phase 1 failed: hash: {e}"));
            set_error(format!("hash: {e}"));
            shared.done.store(2, Ordering::SeqCst);
            return;
        }
    };
    if !computed.eq_ignore_ascii_case(&expected_sha) {
        log::log(&format!(
            "phase 1: SHA-256 mismatch (expected {expected_sha}, computed {computed})"
        ));
        set_error(format!(
            "SHA-256 mismatch â€” declared {}, computed {}",
            expected_sha, computed
        ));
        shared.done.store(2, Ordering::SeqCst);
        return;
    }
    log::log("phase 1: SHA-256 verified");
    shared.pct.store(PHASE_1_PCT_END, Ordering::SeqCst);

    // Phase 2: extract.
    shared.phase.store(2, Ordering::SeqCst);
    shared.pct.store(PHASE_2_PCT_START, Ordering::SeqCst);
    log::log(&format!(
        "phase 2 (extract): zip={} -> dest={}",
        tmp_zip.display(),
        install_dir.display()
    ));
    if let Err(e) = extract_jdk_archive(&tmp_zip, &install_dir) {
        log::log(&format!("phase 2 failed: extract: {e}"));
        set_error(format!("extract: {e}"));
        shared.done.store(2, Ordering::SeqCst);
        return;
    }
    // Adoptium's zip carries a leading `jdk-<version>/` directory,
    // so `install_dir/bin/java.exe` doesn't exist â€” the JDK home is
    // nested one level deeper. `find_java_home` walks the extracted
    // tree to the actual JAVA_HOME.
    let Some(home) = find_java_home(&install_dir) else {
        log::log(&format!(
            "phase 2 failed: extracted to {} but no bin\\java.exe found inside",
            install_dir.display()
        ));
        set_error(format!(
            "extracted to {} but no bin\\java.exe found inside (depth {})",
            install_dir.display(),
            MAX_JAVA_HOME_DEPTH
        ));
        shared.done.store(2, Ordering::SeqCst);
        return;
    };
    if let Ok(mut g) = shared.home.lock() {
        *g = Some(home.clone());
    }
    log::log(&format!("phase 2: extracted JDK home = {}", home.display()));
    shared.pct.store(100, Ordering::SeqCst);
    shared.done.store(1, Ordering::SeqCst);
}

// ===========================================================================
//  Top-level
// ===========================================================================

/// Run the "no JDK found" recovery. Returns `Ok(Some(<jdk_home>))` on
/// success (cache hit or fresh install), `Ok(None)` if the user
/// cancelled or chose to open the URL in a browser.
///
/// Failure modes the user must see:
///
/// - `fetch_metadata` (the Adoptium v3 API call) can fail with no
///   network, captive portal, corporate firewall, TLS/DNS issues, or
///   Adoptium downtime. On the GUI subsystem build, `eprintln!` is
///   invisible â€” silently returning the error leaves the user with
///   only the final `show_launcher_error` dialog and no chance to
///   recover. We pop a dedicated "could not reach Adoptium" dialog
///   here instead, with
///   an "Open the download page in my browser" button so the user
///   can still install Temurin manually.
/// Should this progress sample be logged?
///
/// Split out because it is the whole bug. The first version reported only
/// a *phase* change, and `phase` is 0 for the entire download - so a
/// 114 MB fetch produced exactly one line and then silence, which reads
/// as a hang. Reporting on byte progress as well is what makes the wait
/// legible.
///
/// A phase change always reports, because it is the boundary where the
/// meaning of "bytes" changes (download -> verify -> extract).
///
/// Only `appkit.rs` calls this, so on a Windows build it has no caller and
/// rustc reports it as dead code. The Windows poller still makes the same
/// decision inline, which is why this was extracted rather than moved. The
/// allowance is scoped to Windows deliberately: on any other target the
/// function really is used, and a future unused call should be a warning.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn should_report(phase: i32, last_phase: i32, pct: u32, last_pct: u32) -> bool {
    phase != last_phase || pct >= last_pct + 5
}

pub fn maybe_install(
    parent: ui::ParentWindow,
    min_java_major: u16,
    install_root: &Path,
) -> Result<Option<PathBuf>, JdkError> {
    std::fs::create_dir_all(install_root)?;

    // 1. Cache hit â€” silent reuse.
    if let Some(home) = find_cached_jdk(min_java_major, install_root) {
        log::log(&format!(
            "JDK cache hit: {} (meets min_java={})",
            home.display(),
            min_java_major
        ));
        return Ok(Some(home));
    }
    log::log(&format!(
        "JDK cache miss under {}; need min_java={}",
        install_root.display(),
        min_java_major
    ));

    // 2. Fetch metadata. On failure, pop an "Adoptium unreachable"
    //    dialog so the user still has a path forward (manual browser
    //    install), rather than a silent drop to the final error box.
    let metadata = match fetch_metadata(min_java_major) {
        Ok(m) => {
            log::log(&format!(
                "Adoptium metadata: version={}, size={} bytes",
                m.version,
                m.size_bytes
            ));
            log::log(&format!("Adoptium package link: {}", m.package_link));
            log::log(&format!("Adoptium SHA-256: {}", m.sha256));
            m
        }
        Err(e) => {
            log::log(&format!("Adoptium metadata fetch failed: {e}"));
            if ui::metadata_failed(parent, min_java_major, &e.to_string())
                == IDYES
            {
                let url = format!(
                    "https://adoptium.net/temurin/releases/?version={min_java_major}"
                );
                let _ = open_in_browser(&url);
                log::log(&format!("opened browser at {url}"));
            }
            return Ok(None);
        }
    };

    // The "JDK wasn't found" install prompt has been removed per the
    // current product decision â€” go straight to the download so the
    // user sees the progress window immediately. The progress window's
    // abort button starts labelled "Install" (download is part of the
    // install flow) and switches to "Cancel" once phase 1 (verify) /
    // phase 2 (extract) begins â€” see `progress_window::show`.

    // 3. Download with progress + SHA-on-disk + extract, with retries.
    // Install under `<install_root>/<major>/` so re-downloading after a
    // Temurin point release replaces the previous install instead of
    // leaving stale versions like `25.0.4+101.0.LTS` next to
    // `25.0.5+8.LTS`. Adoptium's zip still carries a nested
    // `jdk-X.Y.Z+1/` inside, so `find_java_home` walks one level deeper
    // to find `bin/java.exe` and reports that as JAVA_HOME.
    //
    // On a non-fatal failure we surface a Retry / Cancel modal
    // and loop up to `MAX_DOWNLOAD_ATTEMPTS` times. Explicit user
    // cancellation (`done == 3` from `worker_thread`) exits the loop
    // immediately without asking.
    const MAX_DOWNLOAD_ATTEMPTS: u32 = 3;
    let install_dir = install_root.join(min_java_major.to_string());
    // Named after the actual archive format: `extract_jdk_archive`
    // dispatches on this extension, and a `.zip.tmp` holding a tarball
    // would be handed to the zip reader.
    let archive_suffix = if cfg!(target_os = "macos") { ".tar.gz" } else { ".zip" };
    let tmp_zip = install_root.join(format!("{min_java_major}{archive_suffix}.tmp"));

    for attempt in 1..=MAX_DOWNLOAD_ATTEMPTS {
        log::log(&format!(
            "JDK download attempt {attempt}/{MAX_DOWNLOAD_ATTEMPTS}"
        ));
        match run_one_install_attempt(parent, &metadata, &install_dir, &tmp_zip) {
            AttemptOutcome::Success(home) => {
                // No success dialog; the download + verify + extract
                // was the long part, and the dialog stayed up while
                // the bar filled, so the user already knows it
                // succeeded.
                return Ok(Some(home));
            }
            AttemptOutcome::Cancelled => {
                log::log("user cancelled mid-download; not retrying");
                return Ok(None);
            }
            AttemptOutcome::Failed(err) => {
                if attempt >= MAX_DOWNLOAD_ATTEMPTS {
                    log::log(&format!(
                        "all {MAX_DOWNLOAD_ATTEMPTS} attempts exhausted; showing terminal failure dialog"
                    ));
                    let dlg = dialogs::dialogs();
                    let content = dialogs::fill(
                        dlg.jdk_install.failure.content.as_str(),
                        &[
                            ("version", metadata.version.as_str()),
                            ("error", err.as_str()),
                        ],
                    );
                    ui::failure(
                        parent,
                        &dlg.jdk_install.failure.title,
                        &dlg.jdk_install.failure.title,
                        &content,
                    );
                    return Ok(None);
                }
                if !ui::retry(
                    parent,
                    attempt,
                    MAX_DOWNLOAD_ATTEMPTS,
                    &metadata.version,
                    &err,
                ) {
                    log::log(&format!("user declined retry at attempt {attempt}"));
                    return Ok(None);
                }
                log::log("user chose retry; looping");
            }
        }
    }
    // Unreachable: the loop either returns or asks the user whether to
    // retry. If we somehow fall through, behave like a cancel.
    log::log("retry loop fell through unexpectedly; returning None");
    Ok(None)
}

/// Result of a single download + verify + extract attempt.
enum AttemptOutcome {
    /// Worker finished all three phases; value is the resolved JAVA_HOME.
    Success(PathBuf),
    /// Worker set `done == 3` â€” the user closed the progress dialog
    /// mid-stream. Distinct from "errored and chose to give up";
    /// never triggers a Retry prompt.
    Cancelled,
    /// Worker set `done == 2` (or didn't finish). Value is the
    /// human-readable error message from the worker, used in the
    /// Retry / terminal-failure dialog content.
    Failed(String),
}

/// Run one download â†’ SHA-256 â†’ extract cycle. Cleans up any partial
/// state from a previous attempt first (the temp zip and the
/// extracted-tree directory under `<install_root>/<major>/`).
///
/// The progress dialog stays up for the full duration and auto-dismisses
/// on success (`done == 1`) or error (`done == 2` after the
/// `ERROR_HOLD_DURATION` red-bar hold). User-cancel (`done == 3`)
/// short-circuits.
fn run_one_install_attempt(
    parent: ui::ParentWindow,
    metadata: &JdkMetadata,
    install_dir: &Path,
    tmp_zip: &Path,
) -> AttemptOutcome {
    // Start clean: previous attempts may have left a partial zip and a
    // partial extract on disk.
    let _ = std::fs::remove_file(tmp_zip);
    let _ = std::fs::remove_dir_all(install_dir);
    let _ = std::fs::create_dir_all(install_dir);

    // The ask happens here, with nothing running yet, so that the only
    // outstanding thing is a human decision — never a worker thread
    // waiting on a window that may never appear. See `ui::consent`.
    let size_mb = (metadata.size_bytes as f64 / 1_048_576.0).round() as u32;
    if !ui::consent(
        parent,
        &metadata.version,
        size_mb,
        &metadata.package_link,
        &metadata.sha256,
    ) {
        log::log("user declined the install prompt; not downloading");
        return AttemptOutcome::Cancelled;
    }

    let shared = Arc::new(ProgressShared {
        pct: AtomicU32::new(0),
        done: AtomicI32::new(0),
        error: std::sync::Mutex::new(None),
        phase: AtomicI32::new(0),
        bytes: AtomicU64::new(0),
        total_bytes: AtomicU64::new(metadata.size_bytes),
        home: std::sync::Mutex::new(None),
        started: AtomicBool::new(false),
        cancel: AtomicBool::new(false),
        mascot: AtomicI32::new(0),
    });

    // Consent is settled by now — `ui::consent` ran before this thread
    // existed — so the flag is set from the start rather than waited on.
    // It is still a field because the preview harnesses read and set it,
    // but nothing in the real flow may block on it again.
    shared.started.store(true, Ordering::SeqCst);

    let worker = thread::spawn({
        let shared = shared.clone();
        let install_dir = install_dir.to_path_buf();
        let tmp_zip = tmp_zip.to_path_buf();
        let url = metadata.package_link.clone();
        let sha = metadata.sha256.clone();
        let total = metadata.size_bytes;
        move || worker_thread(install_dir, tmp_zip, url, sha, total, shared)
    });

    let dlg = dialogs::dialogs();
    let progress_main = dialogs::fill(
        dlg.jdk_install.progress.main.as_str(),
        &[
            ("version", metadata.version.as_str()),
            (
                "size_mb",
                &format!("{:.0}", metadata.size_bytes as f64 / 1_048_576.0),
            ),
        ],
    );
    ui::progress(
        parent,
        &dlg.jdk_install.progress.title,
        &progress_main,
        shared.clone(),
    );

    // Dismissal handling lives in `ui::progress`: on Windows that sets
    // `shared.cancel` when the user closes the window before the worker
    // finishes. Without it the worker would report `done == 1` for a
    // download the user explicitly cancelled.

    let _ = worker.join();

    let done = shared.done.load(Ordering::SeqCst);
    if done == 1 {
        if let Ok(g) = shared.home.lock() {
            if let Some(home) = g.clone() {
                let _ = std::fs::remove_file(tmp_zip);
                return AttemptOutcome::Success(home);
            }
        }
    }
    if done == 3 {
        let _ = std::fs::remove_dir_all(install_dir);
        let _ = std::fs::remove_file(tmp_zip);
        return AttemptOutcome::Cancelled;
    }
    // Worker errored (or, in the unlikely case `done == 0` after
    // `worker.join()`, the dialog closed before the worker finished).
    let err = shared
        .error
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(|| format!("Download did not finish (status={done})."));
    let _ = std::fs::remove_dir_all(install_dir);
    let _ = std::fs::remove_file(tmp_zip);
    AttemptOutcome::Failed(err)
}

/// The download flow's only contact with the user.
///
/// This is the seam that lets the flow be platform-neutral. Windows
/// delegates to the hand-painted dialogs that already exist; every other
/// platform uses **the same `dialogs()` strings** through the launcher log
/// and stderr. Nothing here formats a string of its own, so a
/// `--localization <tag>` bundle translates the macOS output exactly as it
/// translates the Windows one — the localisation requirement is satisfied
/// by construction rather than by a parallel set of strings.
///
/// The non-Windows behaviour is deliberately conservative:
///
/// - It never *pretends* the user answered a question. `retry` auto-retry
///   is the one judgement call, and it is bounded by the same
///   `MAX_DOWNLOAD_ATTEMPTS` the Windows loop uses, with a log line saying
///   it happened.
/// - It never opens a browser unprompted, because a `.app` launched from
///   Finder has no terminal to have agreed to anything in.
mod ui {
    use super::*;

    /// A handle to the window the dialogs should be modal to.
    ///
    /// On Windows this *is* `HWND`, so every call site and signature in
    /// this file is unchanged when compiled for Windows — the alias is the
    /// mechanism, not a wrapper.
    #[cfg(windows)]
    pub type ParentWindow = windows_sys::Win32::Foundation::HWND;
    #[cfg(not(windows))]
    pub type ParentWindow = ();

    /// Ask "download this JDK?" **before** any work begins. `true` means
    /// go ahead.
    ///
    /// Asked here, on the calling thread, with no worker running — which is
    /// the whole point. It used to be the progress window's own "Install"
    /// button, with the worker thread spinning on `shared.started` until
    /// somebody clicked it. That made a click in a window the *only* way
    /// to start a download, so any platform where the window cannot appear
    /// hangs forever: minutes of silence and not one byte. On macOS that
    /// is not hypothetical — a `.app` exec'd from a terminal has no GUI
    /// session for a window, and the flow degraded to logging while the
    /// worker kept waiting for a click nobody could make.
    ///
    /// So the ask is now a step in its own right, taken while the answer
    /// is still the only thing in flight. `OpenBrowser` counts as consent
    /// *not* given: it hands the page to the user and stops, which is what
    /// the button says it does.
    #[cfg(windows)]
    pub fn consent(
        parent: ParentWindow,
        version: &str,
        size_mb: u32,
        url: &str,
        sha256: &str,
    ) -> bool {
        let d = dialogs::dialogs();
        // 256x256 is the load size the other Win32 windows use for the
        // mascot; the window scales the bitmap itself.
        const MASCOT_LOAD: i32 = 256;
        // `prompt_window` takes the mascot as an `isize` handle, which is
        // the same convention `modal_window::ModalDialog::mascot_hbitmap`
        // uses, so a null handle is 0 rather than a null pointer.
        let mascot = super::mascot_icon_override()
            .or_else(|| super::find_best_icon_hicon(MASCOT_LOAD, MASCOT_LOAD))
            .map(|h| h as isize)
            .unwrap_or(0);
        crate::prompt_window::show(
            parent,
            mascot,
            &d.jdk_install.prompt,
            version,
            size_mb,
            url,
            sha256,
        ) == crate::prompt_window::PromptChoice::Download
    }

    #[cfg(target_os = "macos")]
    pub fn consent(
        _parent: ParentWindow,
        version: &str,
        size_mb: u32,
        url: &str,
        sha256: &str,
    ) -> bool {
        crate::appkit::consent(version, size_mb, url, sha256)
    }

    /// Adoptium unreachable. Returns `true` if the user asked for the
    /// release page to be opened.
    #[cfg(windows)]
    pub fn metadata_failed(parent: ParentWindow, min_java: u16, detail: &str) -> i32 {
        super::show_metadata_failed_dialog(parent, min_java, detail)
    }
    #[cfg(target_os = "macos")]
    pub fn metadata_failed(_parent: ParentWindow, min_java: u16, detail: &str) -> i32 {
        crate::appkit::metadata_failed(min_java, detail)
    }

    /// Retry / Cancel between failed attempts. `true` means "retry".
    #[cfg(windows)]
    pub fn retry(
        parent: ParentWindow,
        attempt: u32,
        max: u32,
        version: &str,
        err: &str,
    ) -> bool {
        super::show_retry_dialog(parent, attempt, max, version, err)
    }

    #[cfg(target_os = "macos")]
    pub fn retry(
        _parent: ParentWindow,
        attempt: u32,
        max: u32,
        version: &str,
        err: &str,
    ) -> bool {
        crate::appkit::retry(attempt, max, version, err)
    }

    /// Terminal failure, after the attempts are exhausted.
    #[cfg(windows)]
    pub fn failure(parent: ParentWindow, title: &str, main: &str, content: &str) {
        super::show_error_dialog(parent, title, main, content)
    }

    #[cfg(target_os = "macos")]
    pub fn failure(_parent: ParentWindow, title: &str, _main: &str, content: &str) {
        crate::appkit::failure(title, content);
    }

    /// Show progress and block until the download settles.
    ///
    /// Returns `true` if the user let it run to completion. The
    /// cancellation bookkeeping lives *inside* the Windows implementation
    /// so both platforms share the same post-condition: once this returns,
    /// `shared.cancel` is already set if the user bailed out.
    #[cfg(windows)]
    pub fn progress(
        parent: ParentWindow,
        title: &str,
        main: &str,
        shared: Arc<ProgressShared>,
    ) -> bool {
        let clicked =
            unsafe { crate::progress_window::show(parent, title, main, shared.clone()) };
        if clicked != IDOK {
            shared.cancel.store(true, Ordering::SeqCst);
            if shared.done.load(Ordering::SeqCst) == 0 {
                shared.done.store(3, Ordering::SeqCst);
            }
        }
        clicked == IDOK
    }

    #[cfg(target_os = "macos")]
    pub fn progress(
        _parent: ParentWindow,
        title: &str,
        main: &str,
        shared: Arc<ProgressShared>,
    ) -> bool {
        // A real AppKit progress window on the main thread. The log-only
        // poller that used to live here is now the fallback *inside*
        // `appkit::progress`, for the off-main-thread case.
        let _ = title;
        crate::appkit::progress(main, shared)
    }
}

/// Adoptium's identifiers for the machine we are running on: `(os, architecture)`.
///
/// The JDK is installed on the *user's own* machine — it is never shipped
/// to somebody else's — so this is always the host. `architecture` is
/// Adoptium's spelling, which is not the Rust target triple: Apple
/// Silicon is `aarch64`, not `arm64`. Getting either wrong returns a
/// perfectly well-formed metadata document for the wrong platform, so
/// the mistake surfaces much later as an extraction failure rather than
/// as a bad request.
#[cfg(windows)]
fn adoptium_target() -> (&'static str, &'static str) {
    ("windows", "x64")
}

#[cfg(target_os = "macos")]
fn adoptium_target() -> (&'static str, &'static str) {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        _ => "x64",
    };
    ("mac", arch)
}

/// The `java` launcher inside a JDK home.
///
/// Windows appends `.exe`; the macOS build is a bare `bin/java`. Getting
/// this wrong makes every cached-JDK probe fail, which presents as
/// "it re-downloads a JDK it already has" rather than as an error.
#[cfg(windows)]
fn java_binary(home: &Path) -> PathBuf {
    home.join("bin").join("java.exe")
}

#[cfg(target_os = "macos")]
fn java_binary(home: &Path) -> PathBuf {
    home.join("bin").join("java")
}

/// Run `java -version`, capturing both streams.
///
/// Windows adds `CREATE_NO_WINDOW`, which keeps the parent (GUI
/// subsystem) launcher from flashing a console window for this
/// short-lived probe while scanning cached Temurin installs. macOS has
/// no equivalent and does not need one.
#[cfg(windows)]
fn java_version_probe(java: &Path) -> std::io::Result<std::process::Output> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new(java)
        .arg("-version")
        .creation_flags(CREATE_NO_WINDOW)
        .output()
}

#[cfg(not(windows))]
fn java_version_probe(java: &Path) -> std::io::Result<std::process::Output> {
    std::process::Command::new(java).arg("-version").output()
}

/// Pop the Retry / Cancel prompt between failed download attempts.
/// Returns `true` if the user picked Retry.
#[cfg(windows)]
pub fn show_retry_dialog(
    parent: HWND,
    attempt: u32,
    max_attempts: u32,
    version: &str,
    error: &str,
) -> bool {
    let d = dialogs::dialogs();
    let attempt_str = attempt.to_string();
    let max_str = max_attempts.to_string();

    // Format content / subheading / content with the per-attempt
    // placeholders. Bind the resulting Strings to locals first so
    // their borrows are valid when `retry_window::show` reads them
    // (avoids the dangling-`dialogs::fill(...).as_str()` trap that
    // bit `show_metadata_failed_dialog`).
    let content = dialogs::fill(
        d.jdk_install.retry.content.as_str(),
        &[
            ("version", version),
            ("attempt", &attempt_str),
            ("max_attempts", &max_str),
            ("error", error),
        ],
    );
    let subheading = dialogs::fill(
        d.jdk_install.retry.subheading.as_str(),
        &[
            ("version", version),
            ("attempt", &attempt_str),
            ("max_attempts", &max_str),
        ],
    );

    let result = unsafe {
        crate::retry_window::show(
            parent,
            crate::retry_window::RetryDialog {
                title: d.jdk_install.retry.title.as_str(),
                heading: d.jdk_install.retry.heading.as_str(),
                subheading: subheading.as_str(),
                error_content: content.as_str(),
                info_heading: None,
                info_subtext: None,
                info_subtext_2: None,
                primary_label: d.jdk_install.retry.button_retry.as_str(),
                secondary_label: d.jdk_install.retry.button_cancel.as_str(),
                mascot_hbitmap: 0,
            },
        )
    };
    result == crate::retry_window::IDYES_I32
}

/// "We could not reach Adoptium" dialog. Pops when
/// `fetch_metadata` fails â€” either no internet, captive portal,
/// corporate firewall, TLS/DNS issue, or Adoptium downtime. Without
/// this, the GUI-subsystem launcher silently drops the failure to
/// an invisible stderr line and the user only sees the final
/// `show_launcher_error` dialog.
///
/// Two buttons: **Open the download page in my browser** (carries
/// the user to a Temurin release-filtered page so they can still
/// install manually) and **Cancel**. Returns the button id so the
/// caller can act on the choice.
#[cfg(windows)]
pub fn show_metadata_failed_dialog(parent: HWND, min_java: u16, error_detail: &str) -> i32 {
    let d = dialogs::dialogs();
    let major = min_java.to_string();

    // The dialog reads heading / subheading / info_heading /
    // info_subtext / button labels from `[jdk_install.metadata_failed]`.
    // The launcher formats `content` itself (with `{major}` and
    // `{error}` filled) before passing it in.
    //
    // `MetadataFailedDialog` borrows `&str`s â€” every formatted
    // string lives in a local `String` below; never inline
    // `dialogs::fill(...).as_str()` (its temporary drops before
    // the dialog reads it â€” see `metadata_failed_window::show`'s
    // `wide(dlg.title)` call).
    let error_content = dialogs::fill(
        d.jdk_install.metadata_failed.content.as_str(),
        &[("major", &major), ("error", error_detail)],
    );
    let subheading = dialogs::fill(
        d.jdk_install.metadata_failed.subheading.as_str(),
        &[("major", &major)],
    );
    let result = unsafe {
        crate::metadata_failed_window::show(
            parent,
            crate::metadata_failed_window::MetadataFailedDialog {
                title: d.jdk_install.metadata_failed.title.as_str(),
                heading: d.jdk_install.metadata_failed.heading.as_str(),
                subheading: subheading.as_str(),
                error_content: error_content.as_str(),
                info_heading: None,
                info_subtext: None,
                info_subtext_2: None,
                primary_label: d.jdk_install.metadata_failed.button_open_browser.as_str(),
                secondary_label: d.jdk_install.metadata_failed.button_cancel.as_str(),
                mascot_hbitmap: 0,
            },
        )
    };
    // Map the new module's return values onto the legacy
    // `IDYES` / `IDCANCEL` contract that the rest of the
    // install flow (and the post-install-error logic) expects.
    if result == crate::metadata_failed_window::IDYES_I32 {
        IDYES
    } else {
        IDCANCEL
    }
}

#[cfg(windows)]
pub fn show_error_dialog(parent: HWND, title: &str, _main: &str, content: &str) {
    // `title` is the title-bar text; the dialog body reads
    // `failure.heading` / `failure.subheading` from
    // the localization bundle (or the caller-supplied overrides).
    // `content`
    // is the multi-line error description the launcher already
    // formatted with placeholders filled.
    let _ = title;
    let _ = content;
    unsafe {
        crate::error_window::show(
            parent,
            crate::error_window::ErrorDialog {
                title,
                heading: "",
                subheading: "",
                error_content: content,
                info_icon: crate::error_window::InfoIcon::Error,
                info_heading: None,
                info_subtext: None,
                info_subtext_2: None,
                button_label: None,
                mascot_hbitmap: 0,
                update_check_url: None,
                update_check_label: None,
            },
        );
    }
}

/// Open `url` in the user's default browser.
///
/// Windows goes through `ShellExecuteW`; macOS through `/usr/bin/open`,
/// which is the same "hand it to the OS" arrangement and keeps the
/// caller platform-agnostic. Only reached when the user has *asked* to
/// see the release page, so the intrusiveness is already agreed.
#[cfg(windows)]
fn open_in_browser(url: &str) -> Result<(), JdkError> {
    let url_w: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            windows_sys::core::w!("open"),
            url_w.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    let code = result as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(JdkError::Dialog(format!("ShellExecuteW failed: code={code}")))
    }
}

#[cfg(target_os = "macos")]
fn open_in_browser(url: &str) -> Result<(), JdkError> {
    let status = std::process::Command::new("/usr/bin/open")
        .arg(url)
        .status()
        .map_err(|e| JdkError::Dialog(format!("running /usr/bin/open: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(JdkError::Dialog(format!(
            "/usr/bin/open exited with {status}"
        )))
    }
}

// ===========================================================================
//  Tests (non-GUI paths)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    // ---- macOS JDK acquisition ----------------------------------------

    #[cfg(target_os = "macos")]
    #[test]
    fn adoptium_target_uses_adoptiums_spelling_not_rusts() {
        // `arm64`/`x86_64` are Rust's names. Adoptium's are `aarch64`
        // and `x64`, and a wrong `architecture` returns a well-formed
        // metadata document for the wrong platform rather than an error.
        let (os, _arch) = adoptium_target();
        assert_eq!(os, "mac");
        let expected = match std::env::consts::ARCH {
            "aarch64" => "aarch64",
            _ => "x64",
        };
        assert_eq!(adoptium_target().1, expected);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn extracts_a_macos_tarball_and_finds_the_java_home() {
        // Builds a tarball in the shape Adoptium actually ships: a single
        // `jdk-<version>.jdk/Contents/Home/` root, with `bin/java` and
        // `lib/server/libjvm.dylib` inside it. This is the whole point of
        // the macOS path — the Windows layout has neither the `.jdk`
        // bundle nor the dylib.
        let dir = tempdir();
        let src = dir.join("src").join("jdk-21.0.1.jdk").join("Contents").join("Home");
        std::fs::create_dir_all(src.join("bin")).unwrap();
        std::fs::create_dir_all(src.join("lib").join("server")).unwrap();
        std::fs::create_dir_all(src.join("conf")).unwrap();
        std::fs::write(src.join("bin").join("java"), b"#!/bin/sh\n").unwrap();
        std::fs::write(src.join("lib").join("server").join("libjvm.dylib"), b"fake").unwrap();
        std::fs::write(src.join("release"), "JAVA_VERSION=\"21.0.1\"\n").unwrap();

        let archive = dir.join("21.tar.gz.tmp");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let mut builder = tar::Builder::new(enc);
            builder.append_dir_all("jdk-21.0.1.jdk", dir.join("src").join("jdk-21.0.1.jdk"))
                .unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }

        let dest = dir.join("install");
        extract_jdk_archive(&archive, &dest).expect("extract");

        let home = find_java_home(&dest).expect("java home should be found");
        // The home is the `Contents/Home` directory, not the `.jdk` root.
        assert!(home.ends_with("Contents/Home"), "got {}", home.display());
        assert!(home.join("bin").join("java").is_file());
        assert!(home.join("lib").join("server").join("libjvm.dylib").is_file());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn rejects_a_zip_on_macos_and_says_so() {
        // A Windows-shaped download handed to the macOS extractor should
        // fail with a message that names the expectation, not with a zip
        // parser error about a file that was never supposed to be a zip.
        let dir = tempdir();
        let archive = dir.join("21.zip.tmp");
        std::fs::write(&archive, b"PK\x03\x04not really").unwrap();
        let err = extract_jdk_archive(&archive, &dir.join("install")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(".tar.gz"), "unhelpful error: {msg}");
    }

    /// Network diagnostic, not a unit test. Run with
    /// `cargo test -p snug-launcher --lib download_probe -- --ignored --nocapture`.
    ///
    /// Reports *where* the Adoptium download spends its time, which the
    /// flow's own log cannot: `download_to_disk` creates the file only
    /// *after* the response arrives, so a hang there is indistinguishable
    /// from a hang before it.
    #[test]
    #[ignore = "touches the network"]
    fn download_probe_reports_where_the_time_goes() {
        use std::time::Instant;

        let t0 = Instant::now();
        let meta = match fetch_metadata(25) {
            Ok(m) => m,
            Err(e) => {
                println!("metadata FAILED after {:?}: {e}", t0.elapsed());
                return;
            }
        };
        println!("metadata ok after {:?}: {}", t0.elapsed(), meta.version);


        // Reproduce the launcher's conditions exactly: the agent from
        // `http_agent()` (with timeouts) and the download on a *spawned
        // worker thread*, which is where `worker_thread` puts it.
        let t1 = Instant::now();
        let url = meta.package_link.clone();
        let total_bytes = meta.size_bytes;
        let handle = std::thread::spawn(move || {
            let agent = match http_agent() {
                Ok(a) => a,
                Err(e) => return Err(format!("agent: {e}")),
            };
            let resp = agent
                .get(&url)
                .call()
                .map_err(|e| format!("call: {e}"))?;
            println!("  [worker] headers after {:?}", t1.elapsed());
            let mut reader = resp.into_reader();
            let mut buf = vec![0u8; 256 * 1024];
            let mut total = 0u64;
            let started = Instant::now();
            while total < total_bytes {
                match std::io::Read::read(&mut reader, &mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        total += n as u64;
                        if total % (16 * 1_048_576) < n as u64 {
                            println!(
                                "  [worker] {:.0}/{:.0} MiB at {:.1} MiB/s ({:?})",
                                total as f64 / 1_048_576.0,
                                total_bytes as f64 / 1_048_576.0,
                                total as f64 / started.elapsed().as_secs_f64().max(0.001)
                                    / 1_048_576.0,
                                started.elapsed(),
                            );
                        }
                    }
                    Err(e) => return Err(format!("read: {e}")),
                }
            }
            println!(
                "  [worker] finished {:.0} MiB in {:?}",
                total as f64 / 1_048_576.0,
                started.elapsed()
            );
            Ok::<(), String>(())
        });
        match handle.join() {
            Ok(Ok(())) => println!("worker completed"),
            Ok(Err(e)) => println!("worker FAILED: {e}"),
            Err(_) => println!("worker PANICKED"),
        }
    }

    #[test]
    fn progress_is_reported_on_bytes_not_just_phase() {
        // The regression, stated as a test. `phase` is 0 for the whole
        // download, so a poller that only watched the phase logged one
        // line and then nothing - and silence reads as a hang.
        assert!(
            should_report(0, 0, 5, 0),
            "5% of a 114 MB download must produce a line"
        );
        assert!(
            should_report(0, 0, 100, 95),
            "a full download must report even at 100%"
        );

        // No movement, no line: a busy-wait that logged every poll would
        // bury the log in thousands of identical entries.
        assert!(!should_report(0, 0, 4, 0), "under a 5% step, stay quiet");
        assert!(!should_report(3, 3, 99, 99), "no movement, stay quiet");

        // A phase change always reports - it is where "bytes" changes
        // meaning (download -> verify -> extract).
        assert!(should_report(1, 0, 0, 0), "phase change must report");
        assert!(should_report(0, 1, 0, 0), "phase change must report");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_retry_seam_asks_rather_than_looping_silently() {
        // A test body does not run on the process main thread, so
        // `MainThreadMarker` is absent and the AppKit backend degrades to
        // logging. That degradation must return *cancel*, not retry: a
        // silent loop through a few hundred megabytes of downloads is
        // exactly the behaviour that looks like a hang, and the point of
        // the dialog is that a human is asked.
        //
        // (The stderr this produces is the degraded dialog, printed
        // rather than shown — the heading and the localised body.)
        assert!(!ui::retry((), 1, 3, "21.0.12", "connection reset"));
    }

    fn tempdir() -> std::path::PathBuf {
        // The counter is load-bearing, not decoration. `pid` + `as_nanos`
        // is not unique under parallel tests on a coarse clock, and this
        // module is no longer Windows-only: it compiles and runs on macOS
        // (the Adoptium flow is cross-platform, only the tar.gz/zip split
        // is per-OS). Two tests that land on the same name share a
        // directory, and the failure then reads as a logic bug rather than
        // a collision. This is the same fix as snug-payload's `tempdir`.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let unique = format!(
            "snug-jdk-install-test-{}-{}-{}",
            std::process::id(),
            n,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn wide_null_terminates() {
        let w: Vec<u16> = "hi".encode_utf16().chain(std::iter::once(0)).collect();
        assert_eq!(w, vec![b'h' as u16, b'i' as u16, 0u16]);
    }

    #[test]
    fn hash_file_sha256_matches_known_value() {
        let dir = tempdir();
        let p = dir.join("a.txt");
        std::fs::File::create(&p).unwrap().write_all(b"hello").unwrap();

        let h = hash_file_sha256(&p).unwrap();
        assert_eq!(
            h,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The zip extractor only exists on Windows, because that is the only
    // platform Adoptium ships a zip for. The macOS equivalent — a
    // `.tar.gz` with a `*.jdk/Contents/Home` root — has its own test
    // below.
    #[cfg(windows)]
    #[test]
    fn extract_zip_preserves_entry_layout() {
        let dir = tempdir();
        let zip_path = dir.join("tiny.zip");
        let extract_into = dir.join("out");

        {
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut zipw = zip::ZipWriter::new(file);
            zipw.start_file::<_, ()>("jdk-25/bin/java.exe", Default::default())
                .unwrap();
            zipw.write_all(b"fake java bytes").unwrap();
            zipw.finish().unwrap();
        }
        extract_jdk_zip(&zip_path, &extract_into).unwrap();
        let java = extract_into.join("jdk-25/bin/java.exe");
        assert!(java.exists(), "extracted zip should have jdk-25/bin/java.exe");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_java_version_extracts_major() {
        let text = "openjdk version \"25.0.1\" 2025-09-16\nOpenJDK Runtime Environment";
        assert_eq!(parse_java_version(text), Some(25));
        let text = "openjdk version \"17.0.10+7\" 2025-01-20\n";
        assert_eq!(parse_java_version(text), Some(17));
        assert_eq!(parse_java_version(""), None);
    }

    /// Smoke-test for the Adoptium `/v3/assets/feature_releases/.../ga`
    /// JSON shape. If the API drifts again, this fails loudly with the
    /// exact field name that's missing â€” no more silent `[]` returning
    /// a confusing "could not locate a Java 25+ JVM" error to the
    /// user.
    #[test]
    fn adoptium_feature_releases_json_shape_matches() {
        // Minimal but representative snippet â€” only the fields we read.
        let body = r#"[
          {
            "binaries": [
              {
                "package": {
                  "link": "https://example.invalid/jdk.zip",
                  "checksum": "00c847d804f4a78e9f04f2683faf14fed898535b177b7fc704486cb0284e9283",
                  "size": 141167264
                }
              }
            ],
            "version_data": {
              "semver": "25.0.4+101.0.LTS"
            }
          }
        ]"#;

        let list: AdoptiumAssetList = serde_json::from_str(body)
            .expect("Adoptium feature_releases JSON shape drifted");
        let asset = &list.0[0];
        let binary = &asset.binaries[0];

        assert_eq!(binary.package.size, 141167264);
        assert_eq!(
            binary.package.checksum,
            "00c847d804f4a78e9f04f2683faf14fed898535b177b7fc704486cb0284e9283"
        );
        assert_eq!(
            asset.version_data.semver.as_deref(),
            Some("25.0.4+101.0.LTS")
        );
    }

    /// The progress bar must never go backwards across phase
    /// boundaries â€” the previous `99 â†’ 95 â†’ 99 â†’ 100` sequence made
    /// the bar look like it reset when the user clicked Download.
    /// Each boundary step here is what `worker_thread` actually
    /// stores into `shared.pct`; if anyone reorders or re-numbers the
    /// phase constants, this test fails loud.
    #[test]
    fn bar_progress_is_monotonic_across_phases() {
        // Within phase 0, the download is a straight climb from 0 to
        // PHASE_0_PCT_MAX.
        assert_eq!(PHASE_0_PCT_MAX, 95);
        assert!(PHASE_0_PCT_MAX > 0);

        // Phase 0 â†’ phase 1: no jump backwards.
        assert!(PHASE_1_PCT_START >= PHASE_0_PCT_MAX);
        // Phase 1 itself climbs.
        assert!(PHASE_1_PCT_END > PHASE_1_PCT_START);
        assert_eq!(PHASE_1_PCT_END, 98);

        // Phase 1 â†’ phase 2: no jump backwards.
        assert!(PHASE_2_PCT_START >= PHASE_1_PCT_END);
        assert_eq!(PHASE_2_PCT_START, 99);

        // Phase 2 ends at 100.
        // (Verified indirectly: we don't have PHASE_2_PCT_END as a
        // const because we use a literal 100. Asserting against the
        // literal here keeps the invariant explicit.)
        assert!(PHASE_2_PCT_START < 100);
    }

    #[test]
    fn find_java_home_handles_adoptium_nested_layout() {
        // Adoptium default: install_root/<version>/jdk-25.0.4.1+1/bin/<java>.
        // The leaf name is platform-specific, so it goes through
        // `java_binary()`; what is being tested here is the recursion and
        // the depth cap, neither of which is.
        let tmp = tempdir();
        let nested = tmp.join("25.0.4+101.0.LTS").join("jdk-25.0.4.1+1");
        std::fs::create_dir_all(nested.join("bin")).unwrap();
        std::fs::write(java_binary(&nested), b"").unwrap();
        let home = find_java_home(&tmp.join("25.0.4+101.0.LTS")).expect("nested home");
        assert!(home.ends_with("jdk-25.0.4.1+1"));
        assert!(java_binary(&home).is_file());
    }

    #[test]
    fn find_java_home_handles_flat_layout() {
        // Future-proofing: an archive without the leading directory
        // should also be picked up. Leaf name via `java_binary()` so the
        // test exercises the recursion on either platform.
        let tmp = tempdir();
        let flat = tmp.join("25.0.4+101.0.LTS");
        std::fs::create_dir_all(flat.join("bin")).unwrap();
        std::fs::write(java_binary(&flat), b"").unwrap();
        let home = find_java_home(&flat).expect("flat home");
        assert!(home.ends_with("25.0.4+101.0.LTS"));
    }

    #[test]
    fn find_java_home_returns_none_when_no_jdk() {
        // A directory tree with no java.exe anywhere inside should
        // not be misidentified as a JDK home.
        let tmp = tempdir();
        std::fs::create_dir_all(tmp.join("stuff").join("bin")).unwrap();
        std::fs::write(tmp.join("stuff").join("bin").join("notajava"), b"").unwrap();
        assert!(find_java_home(&tmp).is_none());
    }
}
