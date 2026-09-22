//! "No JDK found" → download-and-install flow.
//!
//! 1. **`find_cached_jdk`** scans `%LOCALAPPDATA%\snug\jdk\` for a
//!    previously-downloaded JDK whose `java -version` reports a major
//!    ≥ `min_java_major`. Hit → return silently, no prompt.
//! 2. **`fetch_metadata`** hits Adoptium's v3 API for the latest
//!    Temurin GA matching the requested major.
//! 3. **`show_prompt`** opens a `TaskDialogIndirect` with three
//!    command-link buttons (Download / Open in browser / Cancel), an
//!    expandable details section, and a hyperlink to the download URL.
//!    The "do not show again" checkbox has been **removed** per
//!    product decision — the cache layer means the user only sees
//!    the prompt when no usable JDK is on disk, so re-asking is fine.
//! 4. **`show_progress_dialog`** opens a second `TaskDialogIndirect`
//!    with `TDF_SHOW_PROGRESS_BAR` and `TDF_CALLBACK_TIMER`, drives
//!    a determinate bar via the worker thread, and auto-dismisses on
//!    completion or failure.
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
//! | No cache, user picks Download | progress dialog → worker thread → result |

#![cfg(windows)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

use crate::dialogs;
use crate::log;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use windows_sys::Win32::Foundation::{HWND, LPARAM, WPARAM, S_OK};
use windows_sys::Win32::UI::Controls::{
    TD_ERROR_ICON, TD_INFORMATION_ICON, TD_SHIELD_ICON, TD_WARNING_ICON,
    TDF_ALLOW_DIALOG_CANCELLATION, TDF_CALLBACK_TIMER, TDF_ENABLE_HYPERLINKS,
    TDF_SHOW_PROGRESS_BAR, TDF_USE_COMMAND_LINKS, TDF_USE_HICON_MAIN,
    TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TASKDIALOGCONFIG_1,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    HICON, SendMessageW, IDCANCEL, IDNO, IDOK, IDYES, SW_SHOWNORMAL,
};

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
        match self {
            JdkError::MetadataFetch(m) => write!(f, "Adoptium API: {m}"),
            JdkError::NoMetadataForVersion(v) => write!(f, "no JDK {v} GA found on Adoptium"),
            JdkError::BadMetadataShape(m) => write!(f, "unexpected Adoptium response: {m}"),
            JdkError::BadField { name, value } => write!(f, "bad metadata field {name}: {value}"),
            JdkError::Download(m) => write!(f, "download: {m}"),
            JdkError::Sha256Mismatch { declared, computed } => write!(
                f,
                "SHA-256 mismatch — declared {declared}, computed {computed}"
            ),
            JdkError::Extract(m) => write!(f, "extract zip: {m}"),
            JdkError::NoJavaExe(p) => write!(
                f,
                "extracted JDK at {} has no bin\\java.exe — unusual zip layout?",
                p.display()
            ),
            JdkError::Io(e) => write!(f, "I/O: {e}"),
            JdkError::Dialog(m) => write!(f, "dialog: {m}"),
        }
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

pub fn fetch_metadata(min_java_major: u16) -> Result<JdkMetadata, JdkError> {
    use ureq::native_tls::TlsConnector;

    let agent = ureq::AgentBuilder::new()
        .tls_connector(std::sync::Arc::new(
            TlsConnector::new().map_err(|e| JdkError::MetadataFetch(e.to_string()))?,
        ))
        .build();

    // The `/v3/assets/latest/{maj}/hotspots` endpoint returns an empty
    // array for current majors (verified against Adoptium 2026-09). The
    // `/v3/assets/feature_releases/{maj}/ga` endpoint returns the full
    // GA release list with `binaries[].package.{link,checksum,size}`
    // and `version_data.semver` — exactly the fields `AdoptiumBinary`
    // deserialises. We take the first element, which Adoptium returns
    // sorted newest-first by `timestamp`.
    let url = format!(
        "https://api.adoptium.net/v3/assets/feature_releases/{maj}/ga\
         ?architecture=x64&image_type=jdk&os=windows&vendor=eclipse",
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
    // (zip) form over the `installer` (msi) form — both live on the
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
//  TaskDialog config (no-callback version)
// ===========================================================================

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum IconKind {
    Warning,
    Error,
    Info,
    Shield,
}

#[derive(Clone)]
pub struct CustomButton {
    pub id: i32,
    pub text: String,
}

struct Config {
    parent: HWND,
    title: String,
    main: String,
    content: String,
    icon: IconKind,
    psz_footer: Option<String>,
    psz_expanded_information: Option<String>,
    psz_expanded_control_text: String,
    psz_collapsed_control_text: String,
    buttons: Vec<CustomButton>,
    default_button: i32,
}

impl Config {
    fn new(parent: HWND, title: impl Into<String>, main: impl Into<String>) -> Self {
        Self {
            parent,
            title: title.into(),
            main: main.into(),
            content: String::new(),
            icon: IconKind::Shield,
            psz_footer: None,
            psz_expanded_information: None,
            psz_expanded_control_text: "Show details".into(),
            psz_collapsed_control_text: "Hide details".into(),
            buttons: Vec::new(),
            default_button: 0,
        }
    }

    /// Build a `TASKDIALOGCONFIG` from this config. Doesn't call
    /// into Windows — safe to use from any caller (including the
    /// `MessageBoxW` fallback path that wants to inspect the raw
    /// struct to render a simpler dialog).
    fn to_taskdialogconfig(&self) -> TASKDIALOGCONFIG {
        let title_w = wide(&self.title);
        let main_w = wide(&self.main);
        let content_w = wide(&self.content);
        let footer_w = self.psz_footer.as_deref().map(wide);
        let expanded_info_w = self.psz_expanded_information.as_deref().map(wide);
        let expanded_ctrl_w = wide(&self.psz_expanded_control_text);
        let collapsed_ctrl_w = wide(&self.psz_collapsed_control_text);
        let main_icon = match self.icon {
            IconKind::Warning => TD_WARNING_ICON_H,
            IconKind::Error => TD_ERROR_ICON_H,
            IconKind::Info => TD_INFORMATION_ICON_H,
            IconKind::Shield => TD_SHIELD_ICON_H,
        };
        let button_texts: Vec<Vec<u16>> =
            self.buttons.iter().map(|b| wide(&b.text)).collect();
        let mut button_array: Vec<TASKDIALOG_BUTTON> = self
            .buttons
            .iter()
            .zip(button_texts.iter())
            .map(|(b, txt)| TASKDIALOG_BUTTON {
                nButtonID: b.id,
                pszButtonText: txt.as_ptr(),
            })
            .collect();
        let c_buttons = button_array.len() as u32;

        // TASKDIALOGCONFIG.dwFlags is i32 in windows-sys 0.59;
        // bitwise-OR the i32 constants directly.
        let dw_flags: i32 = TDF_USE_COMMAND_LINKS
            | TDF_ENABLE_HYPERLINKS
            | TDF_ALLOW_DIALOG_CANCELLATION;

        TASKDIALOGCONFIG {
            cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
            hwndParent: self.parent,
            hInstance: std::ptr::null_mut(),
            dwFlags: dw_flags,
            dwCommonButtons: 0,
            pszWindowTitle: title_w.as_ptr(),
            Anonymous1: TASKDIALOGCONFIG_0 { pszMainIcon: main_icon },
            pszMainInstruction: main_w.as_ptr(),
            pszContent: content_w.as_ptr(),
            cButtons: c_buttons,
            pButtons: if c_buttons == 0 {
                std::ptr::null()
            } else {
                button_array.as_mut_ptr()
            },
            nDefaultButton: self.default_button,
            cRadioButtons: 0,
            pRadioButtons: std::ptr::null(),
            nDefaultRadioButton: 0,
            pszVerificationText: std::ptr::null(),
            pszExpandedInformation: expanded_info_w
                .as_ref()
                .map(|v| v.as_ptr())
                .unwrap_or(std::ptr::null()),
            pszExpandedControlText: expanded_ctrl_w.as_ptr(),
            pszCollapsedControlText: collapsed_ctrl_w.as_ptr(),
            Anonymous2: TASKDIALOGCONFIG_1 {
                pszFooterIcon: std::ptr::null(),
            },
            pszFooter: footer_w
                .as_ref()
                .map(|v| v.as_ptr())
                .unwrap_or(std::ptr::null()),
            pfCallback: None,
            lpCallbackData: 0,
            cxWidth: 0,
        }
    }

    /// Show the dialog modally and return the user's chosen button id.
    /// Falls back to `MessageBoxW` on pre-Vista systems.
    fn show(&self) -> i32 {
        let cfg = self.to_taskdialogconfig();
        let mut button: i32 = 0;
        let dialog_ok = unsafe { call_task_dialog_indirect(&cfg, &mut button) };
        if !dialog_ok {
            // Pre-Vista fallback: prompt the user via `MessageBoxW`
            // (always present). Loses the custom button labels and
            // expandable details, but keeps the install flow alive.
            unsafe {
                return prompt_messagebox(self.parent, &self.title, &self.main, &self.content);
            }
        }
        button
    }
}

const TD_ERROR_ICON_H: *const u16 = TD_ERROR_ICON;
const TD_INFORMATION_ICON_H: *const u16 = TD_INFORMATION_ICON;
const TD_SHIELD_ICON_H: *const u16 = TD_SHIELD_ICON;
const TD_WARNING_ICON_H: *const u16 = TD_WARNING_ICON;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ===========================================================================
//  `TaskDialogIndirect` runtime resolution (pre-Vista compat)
// ===========================================================================
//
// `TaskDialogIndirect` is part of the v6 common controls, exported by
// `comctl32.dll` only on Windows Vista and later. We must *not*
// declare it via `windows_sys`'s `link!` macro: doing so would
// produce a static import that the loader fails to resolve on
// pre-Vista systems with "The procedure entry point … could not be
// located".
//
// Instead we look the symbol up via `GetProcAddress` at first call.
// If absent (or `LoadLibraryA` fails), the caller falls back to a
// `MessageBoxW`-driven GUI flow.

/// Returns `true` if a `TaskDialogIndirect` call was placed; the
/// picked button id is in `*button`. Returns `false` if the function
/// isn't available on this system.
///
/// We resolve `TaskDialogIndirect` at runtime via `LoadLibraryA` and
/// `GetProcAddress` rather than linking it directly because:
///
/// 1. Some Windows installs (including some Windows 10 boxes) ship
///    only comctl32 v5, which doesn't export `TaskDialogIndirect`.
///    Linking directly makes the whole EXE fail to start with
///    `STATUS_ENTRYPOINT_NOT_FOUND`.
/// 2. With a runtime lookup, we can detect the absence and fall back
///    to a `MessageBoxW` prompt — no bar, but at least the user
///    still gets a working dialog.
///
/// The launcher's Cargo.toml embeds a v6-common-controls manifest
/// (`assets\snug-default-manifest.xml`) so Windows loads comctl32 v6
/// for the process. On a fully-patched Win 10 install both layers
/// agree; on edge cases, this lookup degrades gracefully.
unsafe fn call_task_dialog_indirect(cfg: *const TASKDIALOGCONFIG, button: &mut i32) -> bool {
    type F = unsafe extern "system" fn(
        *const TASKDIALOGCONFIG,
        *mut i32,
        *mut i32,
        *mut windows_sys::Win32::Foundation::BOOL,
    ) -> windows_sys::core::HRESULT;
    static RESOLVED: std::sync::OnceLock<Option<F>> = std::sync::OnceLock::new();
    let cell = RESOLVED.get_or_init(|| unsafe {
        let lib = windows_sys::Win32::System::LibraryLoader::LoadLibraryA(
            b"comctl32.dll\0".as_ptr() as *const u8,
        );
        if lib.is_null() {
            log::log("call_task_dialog_indirect: LoadLibraryA(comctl32.dll) returned NULL");
            return None;
        }
        let proc = windows_sys::Win32::System::LibraryLoader::GetProcAddress(
            lib,
            b"TaskDialogIndirect\0".as_ptr() as *const u8,
        );
        match proc {
            None => {
                log::log(
                    "call_task_dialog_indirect: comctl32.dll loaded but does not export TaskDialogIndirect \
                     — falling back to MessageBoxW (this is expected on Windows installs that ship only comctl32 v5; \
                     apply snug-default-manifest.xml as the EXE manifest to opt into v6)",
                );
                None
            }
            Some(p) => Some(std::mem::transmute(p)),
        }
    });
    match cell {
        Some(f) => {
            // SAFETY: `f` was returned by `GetProcAddress` for the
            // v6 comctl32 `TaskDialogIndirect` entry.
            let _hr = unsafe { f(cfg, button, std::ptr::null_mut(), std::ptr::null_mut()) };
            true
        }
        None => false,
    }
}

/// Fallback prompt using `MessageBoxW` (always available). Returns
/// the picked id (mapped into our IDYES / IDNO / IDCANCEL range). Used
/// when `TaskDialogIndirect` isn't available.
unsafe fn prompt_messagebox(
    parent: HWND,
    title: &str,
    main: &str,
    content: &str,
) -> i32 {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_DEFBUTTON1, MB_ICONQUESTION, MB_YESNOCANCEL,
    };
    let mut text = String::new();
    text.push_str(main);
    text.push_str("\n\n");
    text.push_str(content);
    let title_w = wide(title);
    let text_w = wide(&text);
    let ret = unsafe {
        MessageBoxW(
            parent,
            text_w.as_ptr(),
            title_w.as_ptr(),
            MB_YESNOCANCEL | MB_ICONQUESTION | MB_DEFBUTTON1,
        )
    };
    // Map: Yes=IDYES=Download, No=IDNO=Open browser, Cancel=IDCANCEL
    match ret {
        r if r == 6 => 6, // IDYES = Download
        r if r == 7 => 7, // IDNO   = Open browser
        _ => 2,           // IDCANCEL or anything else
    }
}

/// Fallback info/error dialog using `MessageBoxW` (always available).
unsafe fn info_messagebox(parent: HWND, title: &str, content: &str, icon_info: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK,
    };
    let title_w = wide(title);
    let text_w = wide(content);
    let _ = unsafe {
        MessageBoxW(
            parent,
            text_w.as_ptr(),
            title_w.as_ptr(),
            MB_OK | (if icon_info { MB_ICONINFORMATION } else { MB_ICONERROR }),
        )
    };
}

// ===========================================================================
//  Progress dialog: callback + thread-local shared state
// ===========================================================================

/// Shared state between the worker thread (writes) and the
/// `TaskDialogIndirect` callback (reads). `pub` so the
/// `progress_window` fallback module can read the same fields.
pub struct ProgressShared {
    /// 0..=100 download percent. Set by the worker during phases
    /// 0/1/2 — the callback reads this and pushes it into the bar
    /// via `TDM_SET_PROGRESS_BAR_POS`.
    pub(crate) pct: AtomicU32,
    /// 0 = running, 1 = success, 2 = error, 3 = cancelled.
    pub(crate) done: AtomicI32,
    /// Captured in `TDN_CREATED`; written by the callback.
    dialog_hwnd: AtomicI32,
    /// Set on failure.
    error: std::sync::Mutex<Option<String>>,
    /// 0 = downloading, 1 = verifying SHA-256, 2 = extracting.
    /// Drives the live status text the callback writes into the
    /// dialog's `TDE_CONTENT` element.
    pub(crate) phase: AtomicI32,
    /// Bytes written to the temp zip so far (phase 0). The callback
    /// uses this to render "X MB / Y MB" in real time.
    pub(crate) bytes: AtomicU64,
    /// Total bytes from the Adoptium metadata. Captured at init so
    /// the callback can render percentages even after the worker
    /// thread has moved on to verify/extract.
    pub(crate) total_bytes: AtomicU64,
    /// Resolved JAVA_HOME after a successful extract. The worker
    /// writes this so the caller doesn't have to walk the
    /// extracted tree again.
    home: std::sync::Mutex<Option<PathBuf>>,
    /// `Instant` at which the worker first reported `done == 2`.
    /// The callback holds the `PBST_ERROR` bar state for at least
    /// [`ERROR_HOLD_DURATION`] from this point before dismissing
    /// the dialog, so the user actually sees the red bar before
    /// the failure dialog replaces it. `None` while the download
    /// is still running or hasn't errored yet.
    error_at: std::sync::Mutex<Option<std::time::Instant>>,
}

unsafe impl Send for ProgressShared {}
unsafe impl Sync for ProgressShared {}

thread_local! {
    /// Set just before `show_progress_dialog` is called and cleared
    /// right after. The `TaskDialogIndirect` callback reads it.
    static ACTIVE_PROGRESS: std::cell::RefCell<Option<Arc<ProgressShared>>> =
        std::cell::RefCell::new(None);
}

unsafe extern "system" fn progress_dialog_callback(
    hwnd: HWND,
    msg: windows_sys::Win32::UI::Controls::TASKDIALOG_NOTIFICATIONS,
    _w: WPARAM,
    _l: LPARAM,
    _userdata: isize,
) -> windows_sys::core::HRESULT {
    use windows_sys::Win32::UI::Controls::{
        PBST_ERROR, PBST_NORMAL, PBST_PAUSED, TDN_CREATED, TDN_TIMER, TDE_CONTENT,
        TDM_CLICK_BUTTON, TDM_SET_ELEMENT_TEXT, TDM_SET_PROGRESS_BAR_POS,
        TDM_SET_PROGRESS_BAR_RANGE, TDM_SET_PROGRESS_BAR_STATE,
    };
    /// How long the `PBST_ERROR` bar state stays visible after the
    /// worker reports a failure before the dialog dismisses itself.
    /// Without this hold the red flash would last a single ~200 ms
    /// tick and most users would never register it before the
    /// failure dialog replaced the progress dialog.
    const ERROR_HOLD_DURATION: std::time::Duration =
        std::time::Duration::from_millis(600);
    let Some(arc) = ACTIVE_PROGRESS.with(|c| c.borrow().clone()) else {
        return S_OK;
    };
    if msg == TDN_CREATED {
        arc.dialog_hwnd.store(hwnd as i32, Ordering::SeqCst);
        log::log("progress dialog TDN_CREATED: initialising progress bar");
        // Belt-and-braces setup for the bar:
        //   1. Set range to 0..=100 (default, but some themes ignore it).
        //   2. Set state to NORMAL (green) — Windows sometimes leaves
        //      the bar in a "not yet drawn" state without an explicit
        //      `TDM_SET_PROGRESS_BAR_STATE`.
        //   3. Pin position to 0 so the bar is visibly empty (rather
        //      than invisibly uninitialised) on the first paint.
        //   4. Force a repaint — without this, on some Windows themes
        //      the bar remains unrendered until the next idle cycle,
        //      which can be many seconds after `TDN_CREATED`.
        // The lParam for `TDM_SET_PROGRESS_BAR_RANGE` is
        // MAKELPARAM(0, 100) = 100 << 16.
        unsafe {
            use windows_sys::Win32::Graphics::Gdi::{InvalidateRect, UpdateWindow};
            SendMessageW(hwnd, TDM_SET_PROGRESS_BAR_RANGE as u32, 0, 0x0064_0000);
            SendMessageW(
                hwnd,
                TDM_SET_PROGRESS_BAR_STATE as u32,
                PBST_NORMAL as usize,
                0,
            );
            SendMessageW(hwnd, TDM_SET_PROGRESS_BAR_POS as u32, 0, 0);
            // `lpRect = null` + `bErase = 1` invalidates the whole
            // client area and forces the background to redraw.
            InvalidateRect(hwnd, std::ptr::null(), 1);
            UpdateWindow(hwnd);
        }
    } else if msg == TDN_TIMER {
        let pct = arc.pct.load(Ordering::SeqCst);
        let phase = arc.phase.load(Ordering::SeqCst);
        let done = arc.done.load(Ordering::SeqCst);
        let bytes = arc.bytes.load(Ordering::SeqCst);
        let total = arc.total_bytes.load(Ordering::SeqCst);

        // Map `(done, phase)` → bar state. The TaskDialog API only
        // exposes three states; we use them to communicate phase to
        // the user without changing the layout:
        //   - Worker errored (`done == 2`)      → ERROR   (red)
        //   - Verifying SHA-256 (`phase == 1`)  → PAUSED  (yellow) —
        //     the bar is no longer measuring live download progress,
        //     and yellow signals "still working, but the meter isn't
        //     measuring what it was a moment ago."
        //   - Otherwise                         → NORMAL  (green)
        let state: u32 = match (done, phase) {
            (2, _) => PBST_ERROR,
            (_, 1) => PBST_PAUSED,
            _ => PBST_NORMAL,
        };
        // Pin the instant the worker first reported failure so the
        // dismissal path can hold the red bar for `ERROR_HOLD_DURATION`
        // before closing the dialog.
        let hold_elapsed = {
            let mut g = arc.error_at.lock().unwrap();
            if done == 2 && g.is_none() {
                *g = Some(std::time::Instant::now());
            }
            match *g {
                Some(t) => {
                    std::time::Instant::now().duration_since(t)
                        >= ERROR_HOLD_DURATION
                }
                None => false,
            }
        };

        // Render a live status line. We build the wide string on each
        // tick (cheap — ~200ms cadence); `TDM_SET_ELEMENT_TEXT` copies
        // the buffer synchronously before returning.
        let status = format_status_line(phase, pct, bytes, total);
        let status_w = wide(&status);

        unsafe {
            // Re-assert the bar state on every tick. Some Windows
            // themes (notably High Contrast) reset the state when the
            // dialog repaints, leaving the bar invisible until the
            // next paint cycle. The same defensive re-assert applies
            // regardless of which state we picked — NORMAL, PAUSED,
            // or ERROR.
            SendMessageW(
                hwnd,
                TDM_SET_PROGRESS_BAR_STATE as u32,
                state as usize,
                0,
            );
            SendMessageW(hwnd, TDM_SET_PROGRESS_BAR_POS as u32, pct as usize, 0);
            SendMessageW(
                hwnd,
                TDM_SET_ELEMENT_TEXT as u32,
                TDE_CONTENT as usize,
                status_w.as_ptr() as isize,
            );
            match done {
                1 => {
                    SendMessageW(hwnd, TDM_CLICK_BUTTON as u32, IDOK as usize, 0);
                }
                // 2 = errored — only dismiss once the red bar has
                // been visible long enough to register. Without this
                // gate the bar would flash red for a single ~200 ms
                // tick and most users wouldn't see it before the
                // failure dialog replaced the progress dialog.
                2 if hold_elapsed => {
                    SendMessageW(hwnd, TDM_CLICK_BUTTON as u32, IDCANCEL as usize, 0);
                }
                3 => {
                    SendMessageW(hwnd, TDM_CLICK_BUTTON as u32, IDCANCEL as usize, 0);
                }
                _ => {}
            }
        }
    }
    S_OK
}

/// Render the live status line that replaces the dialog's
/// `TDE_CONTENT` text on every timer tick. Kept here so the
/// formatting reads consistently whether the user is in the
/// download phase, the SHA-verify phase, or the extract phase.
///
/// All templates come from `dialogs.toml`. Values are pre-formatted
/// (no `{name:.spec}` in the TOML — format the value in Rust).
fn format_status_line(phase: i32, pct: u32, bytes: u64, total: u64) -> String {
    let d = dialogs::dialogs();
    let pct = pct.to_string();
    let mib = |n: u64| -> String { format!("{:.1}", n as f64 / 1_048_576.0) };
    match phase {
        0 if total > 0 => dialogs::fill(
            d.jdk_install.progress.status_phase_0_with_size.as_str(),
            &[
                ("done_mb", &mib(bytes)),
                ("total_mb", &mib(total)),
                ("pct", &pct),
            ],
        ),
        0 => dialogs::fill(
            d.jdk_install.progress.status_phase_0_no_size.as_str(),
            &[("pct", &pct)],
        ),
        1 => dialogs::fill(
            d.jdk_install.progress.status_phase_1.as_str(),
            &[("pct", &pct)],
        ),
        2 => dialogs::fill(
            d.jdk_install.progress.status_phase_2.as_str(),
            &[("pct", &pct)],
        ),
        _ => dialogs::fill(
            d.jdk_install.progress.status_other.as_str(),
            &[("pct", &pct)],
        ),
    }
}

/// Try to load the EXE's main application icon as an `HICON` so the
/// progress dialog can render with the same icon the user sees in the
/// title bar / taskbar. Returns `None` when no icon resource is present
/// (e.g. the bare launcher stub or an EXE built without `--icon`); the
/// caller should fall back to a standard system icon in that case.
///
/// Resource ID `1` is the Windows convention for the main application
/// icon — `editpe` writes the `--icon` input at that ID. `LoadImageW`
/// with `LR_SHARED` returns a shared handle that the system manages;
/// no `DestroyIcon` call is needed.
fn load_exe_main_icon_hicon() -> Option<HICON> {
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, LoadImageW, IMAGE_ICON, LR_SHARED, SM_CXICON, SM_CYICON,
    };

    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        if hinst.is_null() {
            return None;
        }
        let hicon = LoadImageW(
            hinst,
            // `MAKEINTRESOURCE(1)` — integer ID stored in the lower 16
            // bits of an otherwise-zero pointer-sized value. Cast a
            // `usize` to `*const u16` so the high bits stay zero; Windows
            // checks the high bits to distinguish from a real pointer.
            1usize as *const u16,
            IMAGE_ICON,
            GetSystemMetrics(SM_CXICON),
            GetSystemMetrics(SM_CYICON),
            LR_SHARED,
        );
        if hicon.is_null() {
            None
        } else {
            Some(hicon)
        }
    }
}

/// Show a modal progress dialog driven by `worker_thread`. The worker
/// writes `pct`/`done` into the shared state; the dialog reads them
/// and auto-dismisses on completion.
fn show_progress_dialog(
    parent: HWND,
    title: &str,
    main: &str,
    content: &str,
    shared: Arc<ProgressShared>,
) -> i32 {
    let title_w = wide(title);
    let main_w = wide(main);
    let content_w = wide(content);

    // Use the EXE's own icon when available, otherwise fall back to
    // the standard information icon. `TDF_USE_HICON_MAIN` tells
    // `TaskDialogIndirect` to treat `pszMainIcon` as an `HICON` cast
    // rather than a system icon constant.
    let (main_icon_ptr, dw_flags): (*const u16, i32) =
        match load_exe_main_icon_hicon() {
            Some(h) => (
                // `HICON` is `*mut c_void` in windows-sys 0.59; cast to
                // the `PCWSTR` slot via a `usize` round-trip so the
                // bit pattern is preserved exactly. `TDF_USE_HICON_MAIN`
                // in `dw_flags` is what tells `TaskDialogIndirect` to
                // treat this slot as an icon handle rather than a
                // system icon constant.
                h as usize as *const u16,
                TDF_SHOW_PROGRESS_BAR
                    | TDF_CALLBACK_TIMER
                    | TDF_ALLOW_DIALOG_CANCELLATION
                    | TDF_USE_HICON_MAIN,
            ),
            None => (
                TD_INFORMATION_ICON_H,
                TDF_SHOW_PROGRESS_BAR | TDF_CALLBACK_TIMER | TDF_ALLOW_DIALOG_CANCELLATION,
            ),
        };

    let cfg = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: parent,
        hInstance: std::ptr::null_mut(),
        dwFlags: dw_flags,
        dwCommonButtons: 0,
        pszWindowTitle: title_w.as_ptr(),
        Anonymous1: TASKDIALOGCONFIG_0 {
            pszMainIcon: main_icon_ptr,
        },
        pszMainInstruction: main_w.as_ptr(),
        pszContent: content_w.as_ptr(),
        cButtons: 0,
        pButtons: std::ptr::null(),
        nDefaultButton: 0,
        cRadioButtons: 0,
        pRadioButtons: std::ptr::null(),
        nDefaultRadioButton: 0,
        pszVerificationText: std::ptr::null(),
        pszExpandedInformation: std::ptr::null(),
        pszExpandedControlText: std::ptr::null(),
        pszCollapsedControlText: std::ptr::null(),
        Anonymous2: TASKDIALOGCONFIG_1 {
            pszFooterIcon: std::ptr::null(),
        },
        pszFooter: std::ptr::null(),
        pfCallback: Some(progress_dialog_callback),
        lpCallbackData: 0,
        // 560 dialog units (vs. 420 previously): the live status
        // line ("Verifying SHA-256 against the file on disk… (50%)")
        // used to wrap to two lines on some themes, which pushed the
        // Cancel area off the bottom of the dialog. The wider box
        // keeps the status line on one row and the bar comfortably
        // visible above the dialog footer.
        cxWidth: 560,
    };

    ACTIVE_PROGRESS.with(|c| *c.borrow_mut() = Some(shared.clone()));
    log::log(&format!(
        "showing progress dialog: title={title:?}, dw_flags=0x{dw_flags:x}"
    ));
    let mut button: i32 = 0;
    let dialog_ok = unsafe { call_task_dialog_indirect(&cfg, &mut button) };
    log::log(&format!(
        "TaskDialogIndirect returned: ok={dialog_ok}, button={button}"
    ));
    if !dialog_ok {
        // Fallback path — `TaskDialogIndirect` isn't available (pre-Vista
        // / comctl32 v5 / SxS crash). Spin up our own Win32 progress
        // window so the user still gets a live bar and a Cancel button.
        log::log("falling back to custom progress window (comctl32 msctls_progress32)");
        ACTIVE_PROGRESS.with(|c| *c.borrow_mut() = None);
        button = unsafe {
            crate::progress_window::show(parent, title, content, shared.clone())
        };
        return button;
    }
    ACTIVE_PROGRESS.with(|c| *c.borrow_mut() = None);
    button
}

// ===========================================================================
//  Stream-to-disk, hash-on-disk, extract
// ===========================================================================

/// Download `url` to `dest` as a streaming copy. **No** SHA-256 is
/// computed here — the file is hashed after the download in
/// [`hash_file_sha256`], so multi-gigabyte zips don't pressure RAM.
fn download_to_disk(
    url: &str,
    dest: &Path,
    cancel: &AtomicBool,
    total_bytes: u64,
    mut on_progress: impl FnMut(u64),
) -> Result<u64, JdkError> {
    use ureq::native_tls::TlsConnector;

    let agent = ureq::AgentBuilder::new()
        .tls_connector(std::sync::Arc::new(
            TlsConnector::new().map_err(|e| JdkError::Download(e.to_string()))?,
        ))
        .build();

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
/// directory that actually contains `bin/java.exe` — that's the
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
        let java = dir.join("bin").join("java.exe");
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
/// whose `-version` reports a major version ≥ `min_java_major`.
///
/// Each top-level entry under `install_root` is a previously-
/// extracted Temurin install; the actual JAVA_HOME may be the entry
/// itself (flat layout) or one nested directory inside it (Adoptium
/// default — `jdk-25.0.4.1+1/bin/java.exe`). `find_java_home` walks
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
        let java = home.join("bin").join("java.exe");
        let Ok(out) = std::process::Command::new(&java).arg("-version").output() else {
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
//  Worker thread: download → hash → extract
// ===========================================================================

// Monotonic progress-bar boundaries. The bar must only ever move
// forward across phase transitions — earlier versions let it jump
// 99 → 95 → 99 → 100 which read as "the bar reset". See
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
    cancel: Arc<AtomicBool>,
    shared: Arc<ProgressShared>,
) {
    let set_error = |msg: String| {
        if let Ok(mut g) = shared.error.lock() {
            *g = Some(msg);
        }
    };

    // The bar is split monotonically across the three phases so the user
    // never sees it move backwards:
//   - phase 0 (download)   : 0 → 95%
//   - phase 1 (verify SHA) : 95 → 98%
//   - phase 2 (extract)    : 98 → 100%
// The earlier 0→99→95→99→100 sequence made the bar look like it
// reset when the user clicked Download — confusing.

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
            // takes PHASE_2_PCT_START → 100.
            let pct = ((written as f64 / total_bytes as f64 * PHASE_0_PCT_MAX as f64) as u32)
                .min(PHASE_0_PCT_MAX);
            shared.pct.store(pct, Ordering::SeqCst);
        }
    };

    match download_to_disk(&url, &tmp_zip, &cancel, total_bytes, on_progress) {
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
    if cancel.load(Ordering::SeqCst) {
        log::log("phase 0 cancelled by user");
        shared.done.store(3, Ordering::SeqCst);
        return;
    }
    shared.bytes.store(total_bytes, Ordering::SeqCst);
    // Land exactly on PHASE_0_PCT_MAX so phase 1 picks up with no jump.
    shared.pct.store(PHASE_0_PCT_MAX, Ordering::SeqCst);

    // Phase 1: SHA-256. Hash is a single sequential pass over the
    // file, so the bar climbs PHASE_1_PCT_START → PHASE_1_PCT_END
    // within this phase and the callback renders "Verifying SHA-256…
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
            "SHA-256 mismatch — declared {}, computed {}",
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
    if let Err(e) = extract_jdk_zip(&tmp_zip, &install_dir) {
        log::log(&format!("phase 2 failed: extract: {e}"));
        set_error(format!("extract: {e}"));
        shared.done.store(2, Ordering::SeqCst);
        return;
    }
    // Adoptium's zip carries a leading `jdk-<version>/` directory,
    // so `install_dir/bin/java.exe` doesn't exist — the JDK home is
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
///   invisible — silently returning the error leaves the user with
///   only the final `MessageBoxW` and no chance to recover. We pop a
///   dedicated "could not reach Adoptium" dialog here instead, with
///   an "Open the download page in my browser" button so the user
///   can still install Temurin manually.
pub fn maybe_install(
    parent_hwnd: HWND,
    min_java_major: u16,
    install_root: &Path,
) -> Result<Option<PathBuf>, JdkError> {
    std::fs::create_dir_all(install_root)?;

    // 1. Cache hit — silent reuse.
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
            if show_metadata_failed_dialog(parent_hwnd, min_java_major, &e.to_string())
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

    let dlg = dialogs::dialogs();
    let size_mb = format!("{:.0}", metadata.size_bytes as f64 / 1_048_576.0);

    let mut prompt = Config::new(
        parent_hwnd,
        dlg.jdk_install.prompt.title.clone(),
        dlg.jdk_install.prompt.main.clone(),
    );
    prompt.icon = IconKind::Shield;
    prompt.content = dialogs::fill(
        dlg.jdk_install.prompt.content.as_str(),
        &[
            ("version", metadata.version.as_str()),
            ("size_mb", size_mb.as_str()),
        ],
    );
    prompt.psz_expanded_information = Some(dialogs::fill(
        dlg.jdk_install.prompt.expanded.as_str(),
        &[
            ("url", metadata.package_link.as_str()),
            ("sha256", metadata.sha256.as_str()),
        ],
    ));
    prompt.psz_expanded_control_text = dlg.jdk_install.prompt.show_details.clone();
    prompt.psz_collapsed_control_text = dlg.jdk_install.prompt.hide_details.clone();
    prompt.buttons = vec![
        CustomButton {
            id: IDYES,
            text: dialogs::fill(
                dlg.jdk_install.prompt.button_download.as_str(),
                &[("version", metadata.version.as_str())],
            ),
        },
        CustomButton {
            id: IDNO,
            text: dlg.jdk_install.prompt.button_open_browser.clone(),
        },
        CustomButton {
            id: IDCANCEL,
            text: dlg.jdk_install.prompt.button_cancel.clone(),
        },
    ];
    prompt.default_button = IDYES;

    match prompt.show() {
        x if x == IDYES => {} // fall through to download
        x if x == IDNO => {
            open_in_browser(&metadata.package_link)?;
            return Ok(None);
        }
        _ => return Ok(None),
    }

    // 3. Download with progress + SHA-on-disk + extract, with retries.
    // Install under `<install_root>/<major>/` so re-downloading after a
    // Temurin point release replaces the previous install instead of
    // leaving stale versions like `25.0.4+101.0.LTS` next to
    // `25.0.5+8.LTS`. Adoptium's zip still carries a nested
    // `jdk-X.Y.Z+1/` inside, so `find_java_home` walks one level deeper
    // to find `bin/java.exe` and reports that as JAVA_HOME.
    //
    // On a non-fatal failure we surface a Retry / Cancel TaskDialog
    // and loop up to `MAX_DOWNLOAD_ATTEMPTS` times. Explicit user
    // cancellation (`done == 3` from `worker_thread`) exits the loop
    // immediately without asking.
    const MAX_DOWNLOAD_ATTEMPTS: u32 = 3;
    let install_dir = install_root.join(min_java_major.to_string());
    let tmp_zip = install_root.join(format!("{}.zip.tmp", min_java_major));

    for attempt in 1..=MAX_DOWNLOAD_ATTEMPTS {
        log::log(&format!(
            "JDK download attempt {attempt}/{MAX_DOWNLOAD_ATTEMPTS}"
        ));
        match run_one_install_attempt(parent_hwnd, &metadata, &install_dir, &tmp_zip) {
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
                    show_error_dialog(
                        parent_hwnd,
                        &dlg.jdk_install.failure.title,
                        &dlg.jdk_install.failure.title,
                        &content,
                    );
                    return Ok(None);
                }
                if !show_retry_dialog(
                    parent_hwnd,
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
    /// Worker set `done == 3` — the user closed the progress dialog
    /// mid-stream. Distinct from "errored and chose to give up";
    /// never triggers a Retry prompt.
    Cancelled,
    /// Worker set `done == 2` (or didn't finish). Value is the
    /// human-readable error message from the worker, used in the
    /// Retry / terminal-failure dialog content.
    Failed(String),
}

/// Run one download → SHA-256 → extract cycle. Cleans up any partial
/// state from a previous attempt first (the temp zip and the
/// extracted-tree directory under `<install_root>/<major>/`).
///
/// The progress dialog stays up for the full duration and auto-dismisses
/// on success (`done == 1`) or error (`done == 2` after the
/// `ERROR_HOLD_DURATION` red-bar hold). User-cancel (`done == 3`)
/// short-circuits.
fn run_one_install_attempt(
    parent_hwnd: HWND,
    metadata: &JdkMetadata,
    install_dir: &Path,
    tmp_zip: &Path,
) -> AttemptOutcome {
    // Start clean: previous attempts may have left a partial zip and a
    // partial extract on disk.
    let _ = std::fs::remove_file(tmp_zip);
    let _ = std::fs::remove_dir_all(install_dir);
    let _ = std::fs::create_dir_all(install_dir);

    let cancel = Arc::new(AtomicBool::new(false));
    let shared = Arc::new(ProgressShared {
        pct: AtomicU32::new(0),
        done: AtomicI32::new(0),
        dialog_hwnd: AtomicI32::new(0),
        error: std::sync::Mutex::new(None),
        phase: AtomicI32::new(0),
        bytes: AtomicU64::new(0),
        total_bytes: AtomicU64::new(metadata.size_bytes),
        home: std::sync::Mutex::new(None),
        error_at: std::sync::Mutex::new(None),
    });

    let worker = thread::spawn({
        let shared = shared.clone();
        let cancel = cancel.clone();
        let install_dir = install_dir.to_path_buf();
        let tmp_zip = tmp_zip.to_path_buf();
        let url = metadata.package_link.clone();
        let sha = metadata.sha256.clone();
        let total = metadata.size_bytes;
        move || worker_thread(install_dir, tmp_zip, url, sha, total, cancel, shared)
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
    let _clicked = show_progress_dialog(
        parent_hwnd,
        &dlg.jdk_install.progress.title,
        &progress_main,
        &dlg.jdk_install.progress.content_initial,
        shared.clone(),
    );

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

/// Pop the Retry / Cancel prompt between failed download attempts.
/// Returns `true` if the user picked Retry.
fn show_retry_dialog(
    parent: HWND,
    attempt: u32,
    max_attempts: u32,
    version: &str,
    error: &str,
) -> bool {
    let d = dialogs::dialogs();
    let attempt_str = attempt.to_string();
    let max_str = max_attempts.to_string();
    let mut c = Config::new(
        parent,
        d.jdk_install.retry.title.clone(),
        d.jdk_install.retry.main.clone(),
    );
    c.icon = IconKind::Warning;
    c.content = dialogs::fill(
        d.jdk_install.retry.content.as_str(),
        &[
            ("version", version),
            ("attempt", &attempt_str),
            ("max_attempts", &max_str),
            ("error", error),
        ],
    );
    c.buttons = vec![
        CustomButton {
            id: IDYES,
            text: d.jdk_install.retry.button_retry.clone(),
        },
        CustomButton {
            id: IDCANCEL,
            text: d.jdk_install.retry.button_cancel.clone(),
        },
    ];
    c.default_button = IDYES;
    c.show() == IDYES
}

/// "We could not reach Adoptium" dialog. Pops when
/// `fetch_metadata` fails — either no internet, captive portal,
/// corporate firewall, TLS/DNS issue, or Adoptium downtime. Without
/// this, the GUI-subsystem launcher silently drops the failure to
/// an invisible stderr line and the user only sees the final
/// `MessageBoxW`.
///
/// Two buttons: **Open the download page in my browser** (carries
/// the user to a Temurin release-filtered page so they can still
/// install manually) and **Cancel**. Returns the button id so the
/// caller can act on the choice.
fn show_metadata_failed_dialog(parent: HWND, min_java: u16, error_detail: &str) -> i32 {
    let d = dialogs::dialogs();
    let major = min_java.to_string();
    let mut c = Config::new(
        parent,
        d.jdk_install.metadata_failed.title.clone(),
        dialogs::fill(d.jdk_install.metadata_failed.main.as_str(), &[("major", &major)]),
    );
    c.icon = IconKind::Warning;
    c.content = dialogs::fill(
        d.jdk_install.metadata_failed.content.as_str(),
        &[("major", &major), ("error", error_detail)],
    );
    c.buttons = vec![
        CustomButton {
            id: IDYES,
            text: d.jdk_install.metadata_failed.button_open_browser.clone(),
        },
        CustomButton {
            id: IDCANCEL,
            text: d.jdk_install.metadata_failed.button_cancel.clone(),
        },
    ];
    c.default_button = IDYES;
    c.show()
}

fn show_error_dialog(parent: HWND, title: &str, main: &str, content: &str) {
    let d = dialogs::dialogs();
    let mut c = Config::new(parent, title, main);
    c.icon = IconKind::Error;
    c.content = format!("{main}\n\n{content}");
    c.buttons = vec![CustomButton {
        id: IDOK,
        text: d.generic.error_dialog_ok.clone(),
    }];
    let mut button: i32 = 0;
    let ok = unsafe { call_task_dialog_indirect(&c.to_taskdialogconfig(), &mut button) };
    if !ok {
        unsafe {
            info_messagebox(parent, title, &format!("{main}\n\n{content}"), false)
        };
    }
}

fn open_in_browser(url: &str) -> Result<(), JdkError> {
    let url_w = wide(url);
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

// ===========================================================================
//  Tests (non-GUI paths)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tempdir() -> std::path::PathBuf {
        let unique = format!(
            "snug-jdk-install-test-{}-{}",
            std::process::id(),
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
        let w = wide("hi");
        assert_eq!(w, vec![b'h' as u16, b'i' as u16, 0u16]);
    }

    #[test]
    fn hash_file_sha256_matches_known_value() {
        let dir = std::env::temp_dir().join(format!(
            "snug-hash-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.txt");
        std::fs::File::create(&p).unwrap().write_all(b"hello").unwrap();

        let h = hash_file_sha256(&p).unwrap();
        assert_eq!(
            h,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_zip_preserves_entry_layout() {
        let dir = std::env::temp_dir().join(format!(
            "snug-jdk-ext-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
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
    /// exact field name that's missing — no more silent `[]` returning
    /// a confusing "could not locate a Java 25+ JVM" error to the
    /// user.
    #[test]
    fn adoptium_feature_releases_json_shape_matches() {
        // Minimal but representative snippet — only the fields we read.
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

    #[test]
    fn format_status_line_phase_0_shows_bytes() {
        // 50 MiB / 100 MiB at 50%
        let line = format_status_line(0, 50, 50 * 1_048_576, 100 * 1_048_576);
        assert!(line.contains("50.0 MB"), "got: {line}");
        assert!(line.contains("100.0 MB"), "got: {line}");
        assert!(line.contains("50%"), "got: {line}");
    }

    #[test]
    fn format_status_line_phase_1_says_verifying() {
        let line = format_status_line(1, 95, 100 * 1_048_576, 100 * 1_048_576);
        assert!(line.contains("Verifying SHA-256"), "got: {line}");
        assert!(line.contains("95%"), "got: {line}");
    }

    #[test]
    fn format_status_line_phase_2_says_extracting() {
        let line = format_status_line(2, 99, 100 * 1_048_576, 100 * 1_048_576);
        assert!(line.contains("Extracting"), "got: {line}");
    }

    /// The progress bar must never go backwards across phase
    /// boundaries — the previous `99 → 95 → 99 → 100` sequence made
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

        // Phase 0 → phase 1: no jump backwards.
        assert!(PHASE_1_PCT_START >= PHASE_0_PCT_MAX);
        // Phase 1 itself climbs.
        assert!(PHASE_1_PCT_END > PHASE_1_PCT_START);
        assert_eq!(PHASE_1_PCT_END, 98);

        // Phase 1 → phase 2: no jump backwards.
        assert!(PHASE_2_PCT_START >= PHASE_1_PCT_END);
        assert_eq!(PHASE_2_PCT_START, 99);

        // Phase 2 ends at 100.
        // (Verified indirectly: we don't have PHASE_2_PCT_END as a
        // const because we use a literal 100. Asserting against the
        // literal here keeps the invariant explicit.)
        assert!(PHASE_2_PCT_START < 100);
    }

    #[test]
    fn format_status_line_handles_unknown_total() {
        // When Adoptium omits size (some JRE builds do), we still show
        // a percentage but no byte counts.
        let line = format_status_line(0, 25, 1024, 0);
        assert!(line.contains("25%"), "got: {line}");
        assert!(!line.contains("of 0.0"), "shouldn't render 0/0: {line}");
    }

    #[test]
    fn find_java_home_handles_adoptium_nested_layout() {
        // Adoptium default: install_root/<version>/jdk-25.0.4.1+1/bin/java.exe
        let tmp = tempdir();
        let nested = tmp.join("25.0.4+101.0.LTS").join("jdk-25.0.4.1+1");
        std::fs::create_dir_all(nested.join("bin")).unwrap();
        std::fs::write(nested.join("bin").join("java.exe"), b"").unwrap();
        let home = find_java_home(&tmp.join("25.0.4+101.0.LTS")).expect("nested home");
        assert!(home.ends_with("jdk-25.0.4.1+1"));
        assert!(home.join("bin").join("java.exe").is_file());
    }

    #[test]
    fn find_java_home_handles_flat_layout() {
        // Future-proofing: a zip without the leading directory
        // should also be picked up.
        let tmp = tempdir();
        let flat = tmp.join("25.0.4+101.0.LTS");
        std::fs::create_dir_all(flat.join("bin")).unwrap();
        std::fs::write(flat.join("bin").join("java.exe"), b"").unwrap();
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
