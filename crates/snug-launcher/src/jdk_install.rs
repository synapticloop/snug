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
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use windows_sys::Win32::Foundation::{HWND, LPARAM, WPARAM, S_OK};
use windows_sys::Win32::UI::Controls::{
    TD_ERROR_ICON, TD_INFORMATION_ICON, TD_SHIELD_ICON, TD_WARNING_ICON,
    TDF_ALLOW_DIALOG_CANCELLATION, TDF_CALLBACK_TIMER, TDF_ENABLE_HYPERLINKS,
    TDF_SHOW_PROGRESS_BAR, TDF_USE_COMMAND_LINKS, TASKDIALOG_BUTTON,
    TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TASKDIALOGCONFIG_1,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SendMessageW, IDCANCEL, IDNO, IDOK, IDYES, SW_SHOWNORMAL,
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
    binary: AdoptiumBinary,
}

#[derive(Debug, Deserialize)]
struct AdoptiumBinary {
    package_link: String,
    #[serde(default)]
    sha256sum: Option<String>,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    version: Option<AdoptiumVersion>,
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

    let url = format!(
        "https://api.adoptium.net/v3/assets/latest/{maj}/hotspots\
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

    let package_link = asset.binary.package_link;
    let sha256 = asset
        .binary
        .sha256sum
        .ok_or_else(|| JdkError::BadField { name: "sha256sum", value: "missing".into() })?;
    let size = asset.binary.size.unwrap_or(0);
    let version = asset
        .binary
        .version
        .and_then(|v| v.semver)
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
            return None;
        }
        let proc = windows_sys::Win32::System::LibraryLoader::GetProcAddress(
            lib,
            b"TaskDialogIndirect\0".as_ptr() as *const u8,
        );
        if proc.is_none() {
            return None;
        }
        Some(std::mem::transmute(proc))
    });
    match cell {
        Some(f) => {
            // SAFETY: `f` was returned by `GetProcAddress` for the
            // v6 comctl32 `TaskDialogIndirect` entry.
            unsafe { f(cfg, button, std::ptr::null_mut(), std::ptr::null_mut()) };
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
/// `TaskDialogIndirect` callback (reads).
struct ProgressShared {
    /// 0..=100 download percent.
    pct: AtomicU32,
    /// 0 = running, 1 = success, 2 = error, 3 = cancelled.
    done: AtomicI32,
    /// Captured in `TDN_CREATED`; written by the callback.
    dialog_hwnd: AtomicI32,
    /// Set on failure.
    error: std::sync::Mutex<Option<String>>,
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
        TDN_CREATED, TDN_TIMER, TDM_CLICK_BUTTON, TDM_SET_PROGRESS_BAR_POS,
    };
    let Some(arc) = ACTIVE_PROGRESS.with(|c| c.borrow().clone()) else {
        return S_OK;
    };
    if msg == TDN_CREATED {
        arc.dialog_hwnd.store(hwnd as i32, Ordering::SeqCst);
    } else if msg == TDN_TIMER {
        let pct = arc.pct.load(Ordering::SeqCst);
        unsafe {
            SendMessageW(hwnd, TDM_SET_PROGRESS_BAR_POS as u32, pct as usize, 0);
            match arc.done.load(Ordering::SeqCst) {
                1 => {
                    SendMessageW(hwnd, TDM_CLICK_BUTTON as u32, IDOK as usize, 0);
                }
                2 | 3 => {
                    SendMessageW(hwnd, TDM_CLICK_BUTTON as u32, IDCANCEL as usize, 0);
                }
                _ => {}
            }
        }
    }
    S_OK
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

    let dw_flags: i32 = TDF_SHOW_PROGRESS_BAR
        | TDF_CALLBACK_TIMER
        | TDF_ALLOW_DIALOG_CANCELLATION;

    let cfg = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: parent,
        hInstance: std::ptr::null_mut(),
        dwFlags: dw_flags,
        dwCommonButtons: 0,
        pszWindowTitle: title_w.as_ptr(),
        Anonymous1: TASKDIALOGCONFIG_0 {
            pszMainIcon: TD_INFORMATION_ICON_H,
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
        cxWidth: 0,
    };

    ACTIVE_PROGRESS.with(|c| *c.borrow_mut() = Some(shared.clone()));
    let mut button: i32 = 0;
    let dialog_ok = unsafe { call_task_dialog_indirect(&cfg, &mut button) };
    if !dialog_ok {
        // Pre-Vista fallback: no progress UI. Block on the worker
        // directly, then return IDOK so the caller proceeds as if
        // the dialog auto-dismissed on success. The result dialogs
        // (info / error) below also degrade to `MessageBoxW`.
        use std::sync::atomic::Ordering;
        loop {
            let done = shared.done.load(Ordering::SeqCst);
            if done != 0 {
                button = if done == 1 { IDOK } else { IDCANCEL };
                break;
            }
            let pct = shared.pct.load(Ordering::SeqCst);
            eprint!("\rsnug: downloading Temurin… {pct:3}%   ");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        ACTIVE_PROGRESS.with(|c| *c.borrow_mut() = None);
        eprintln!();
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

fn locate_java_exe(home_dir: &Path) -> Option<PathBuf> {
    let java = home_dir.join("bin").join("java.exe");
    if java.exists() {
        Some(java)
    } else {
        None
    }
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
fn find_cached_jdk(min_java_major: u16, install_root: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(install_root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(java) = locate_java_exe(&path) else {
            continue;
        };
        let home = java.parent().and_then(|p| p.parent())?.to_path_buf();
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

    let on_progress = |written: u64| {
        if total_bytes > 0 {
            let pct = ((written as f64 / total_bytes as f64 * 100.0) as u32).min(99);
            shared.pct.store(pct, Ordering::SeqCst);
        }
    };

    match download_to_disk(&url, &tmp_zip, &cancel, total_bytes, on_progress) {
        Ok(_) => {}
        Err(e) => {
            set_error(format!("download: {e}"));
            shared.done.store(2, Ordering::SeqCst);
            return;
        }
    }
    if cancel.load(Ordering::SeqCst) {
        shared.done.store(3, Ordering::SeqCst);
        return;
    }
    shared.pct.store(95, Ordering::SeqCst);

    let computed = match hash_file_sha256(&tmp_zip) {
        Ok(h) => h,
        Err(e) => {
            set_error(format!("hash: {e}"));
            shared.done.store(2, Ordering::SeqCst);
            return;
        }
    };
    if !computed.eq_ignore_ascii_case(&expected_sha) {
        set_error(format!(
            "SHA-256 mismatch — declared {}, computed {}",
            expected_sha, computed
        ));
        shared.done.store(2, Ordering::SeqCst);
        return;
    }
    shared.pct.store(98, Ordering::SeqCst);

    if let Err(e) = extract_jdk_zip(&tmp_zip, &install_dir) {
        set_error(format!("extract: {e}"));
        shared.done.store(2, Ordering::SeqCst);
        return;
    }
    if locate_java_exe(&install_dir).is_none() {
        set_error(format!(
            "extracted to {} but missing bin\\java.exe",
            install_dir.display()
        ));
        shared.done.store(2, Ordering::SeqCst);
        return;
    }
    shared.pct.store(100, Ordering::SeqCst);
    shared.done.store(1, Ordering::SeqCst);
}

// ===========================================================================
//  Top-level
// ===========================================================================

/// Run the "no JDK found" recovery. Returns `Ok(Some(<jdk_home>))` on
/// success (cache hit or fresh install), `Ok(None)` if the user
/// cancelled or chose to open the URL in a browser.
pub fn maybe_install(
    parent_hwnd: HWND,
    min_java_major: u16,
    install_root: &Path,
) -> Result<Option<PathBuf>, JdkError> {
    std::fs::create_dir_all(install_root)?;

    // 1. Cache hit — silent reuse.
    if let Some(home) = find_cached_jdk(min_java_major, install_root) {
        return Ok(Some(home));
    }

    // 2. Fetch metadata, prompt the user.
    let metadata = fetch_metadata(min_java_major)?;

    let mut prompt = Config::new(
        parent_hwnd,
        "Java Runtime Required — Snug",
        "Eclipse Temurin JDK was not found on this machine",
    );
    prompt.icon = IconKind::Shield;
    prompt.content = format!(
        "This application needs a Java {} or higher. Snug can download the \
         official Eclipse Temurin {} (~{:.0} MB) and install it to a per-user \
         location, or open the download page in your browser.\n\n\
         Download will be verified against the official SHA-256.",
        metadata.version,
        metadata.version,
        metadata.size_bytes as f64 / 1_048_576.0,
    );
    prompt.psz_expanded_information = Some(format!(
        "Direct download URL (also clickable below):\n{}\n\n\
         SHA-256: {}\n\n\
         What's a JDK? Java applications need a Java Development Kit to run. \
         Eclipse Temurin is the official OpenJDK distribution from the Eclipse \
         Adoptium working group — same Java you'd get from any vendor, but \
         freely redistributable.",
        metadata.package_link, metadata.sha256
    ));
    prompt.buttons = vec![
        CustomButton {
            id: IDYES,
            text: format!("Download Temurin {} now", metadata.version),
        },
        CustomButton {
            id: IDNO,
            text: "Open the download page in my browser".to_string(),
        },
        CustomButton {
            id: IDCANCEL,
            text: "Cancel".to_string(),
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

    // 3. Download with progress + SHA-on-disk + extract.
    let install_dir = install_root.join(&metadata.version);
    let cancel = Arc::new(AtomicBool::new(false));
    let shared = Arc::new(ProgressShared {
        pct: AtomicU32::new(0),
        done: AtomicI32::new(0),
        dialog_hwnd: AtomicI32::new(0),
        error: std::sync::Mutex::new(None),
    });

    let tmp_zip = install_root.join(format!("{}.zip.tmp", metadata.version));
    let url = metadata.package_link.clone();
    let sha = metadata.sha256.clone();
    let total = metadata.size_bytes;
    let worker = thread::spawn({
        let shared = shared.clone();
        let cancel = cancel.clone();
        let install_dir = install_dir.clone();
        let tmp_zip = tmp_zip.clone();
        move || worker_thread(install_dir, tmp_zip, url, sha, total, cancel, shared)
    });

    let clicked = show_progress_dialog(
        parent_hwnd,
        "Downloading Eclipse Temurin…",
        &format!(
            "Temurin {} (~{:.0} MB)",
            metadata.version,
            metadata.size_bytes as f64 / 1_048_576.0
        ),
        "Verifying SHA-256 against the file on disk once complete.",
        shared.clone(),
    );

    let _ = worker.join();

    // 4. Show result dialog + return.
    let done = shared.done.load(Ordering::SeqCst);
    if done == 1 {
        if let Some(java) = locate_java_exe(&install_dir) {
            let home = java
                .parent()
                .and_then(|p| p.parent())
                .unwrap_or(&install_dir)
                .to_path_buf();
            let _ = std::fs::remove_file(&tmp_zip);
            show_info_dialog(
                parent_hwnd,
                "JDK ready — Snug",
                "Eclipse Temurin was installed",
                &format!(
                    "OpenJDK {} is now available at:\n{}\n\nSnug will continue launching.",
                    metadata.version,
                    home.display()
                ),
            );
            return Ok(Some(home));
        }
    }

    // Failure / cancel.
    let _ = std::fs::remove_dir_all(&install_dir);
    let _ = std::fs::remove_file(&tmp_zip);
    let err = shared.error.lock().ok().and_then(|g| g.clone());
    let msg = err.unwrap_or_else(|| {
        if clicked == IDCANCEL {
            "Download cancelled.".to_string()
        } else {
            format!("Download did not finish (status={done}).")
        }
    });
    show_error_dialog(
        parent_hwnd,
        "JDK download failed — Snug",
        "Could not download or install Eclipse Temurin",
        &format!("{msg}\n\nPlease set JAVA_HOME manually and re-launch."),
    );
    Ok(None)
}

fn show_info_dialog(parent: HWND, title: &str, main: &str, content: &str) {
    // Try the full TaskDialog first, fall back to MessageBoxW on
    // pre-Vista systems.
    let mut c = Config::new(parent, title, main);
    c.icon = IconKind::Info;
    c.content = format!("{main}\n\n{content}");
    c.buttons = vec![CustomButton {
        id: IDOK,
        text: "Continue".into(),
    }];
    let mut button: i32 = 0;
    let ok = unsafe { call_task_dialog_indirect(&c.to_taskdialogconfig(), &mut button) };
    if !ok {
        unsafe {
            info_messagebox(parent, title, &format!("{main}\n\n{content}"), true)
        };
    }
}

fn show_error_dialog(parent: HWND, title: &str, main: &str, content: &str) {
    let mut c = Config::new(parent, title, main);
    c.icon = IconKind::Error;
    c.content = format!("{main}\n\n{content}");
    c.buttons = vec![CustomButton {
        id: IDOK,
        text: "OK".into(),
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
}
