//! Custom Win32 progress window — replaces `TaskDialogIndirect` for the
//! JDK-download flow on systems where comctl32 v5 is the only available
//! version (and v6 — which exports `TaskDialogIndirect` — crashes inside
//! itself when called from a v6-SxS context, as seen on some Win10
//! installs).
//!
//! Owns a top-level window with a `msctls_progress32` child control, a
//! `STATIC` status label, and a `BUTTON` cancel. Runs a self-contained
//! message loop that updates the controls from the same `ProgressShared`
//! state the worker thread writes to. No `TaskDialogIndirect`, no
//! comctl32 v6 manifest dependency.
//!
//! Layout (window 460×130):
//! ```text
//! ┌──────────────────────────────────────┐
//! │ Downloading Eclipse Temurin…          │
//! │                                      │
//! │ [████████████████░░░░░░░░░░░░░░░░░]    │  progress bar
//! │ Downloaded 47 MB of 141 MB (33%)     │  status label
//! │                            [ Cancel ] │  button
//! └──────────────────────────────────────┘
//! ```

use std::sync::atomic::Ordering;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{COLOR_BTNFACE, HBRUSH};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{PBM_SETPOS, PBM_SETRANGE};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BS_PUSHBUTTON, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetDlgItem, GetMessageW, GetSystemMetrics, KillTimer, MSG, PostQuitMessage,
    RegisterClassExW, SendMessageW, SetTimer, SetWindowTextW, SetWindowLongPtrW,
    GetWindowLongPtrW, TranslateMessage, CW_USEDEFAULT, IDCANCEL, SM_CXSCREEN, SM_CYSCREEN,
    WM_COMMAND, WM_CREATE, WM_CLOSE, WM_NCDESTROY, WM_TIMER, WNDCLASSEXW, WS_CAPTION,
    WS_CHILD, WS_SYSMENU, WS_VISIBLE, WS_OVERLAPPED, WS_EX_TOPMOST,
};

use crate::jdk_install::ProgressShared;
use crate::log;

// `SS_LEFT` lives in a module we don't enable. The value is documented
// as 0x0000L in winuser.h, so we hard-code it.
const SS_LEFT: u32 = 0x0000;

const CLASS_NAME: &str = "snug_progress_dialog_v1\0";
const TIMER_ID: usize = 1;
const TIMER_MS: u32 = 200;
const WINDOW_W: i32 = 460;
const WINDOW_H: i32 = 130;
const IDC_PROGRESS: i32 = 1001;
const IDC_STATUS: i32 = 1002;

const IDOK_I32: i32 = 1;
const IDCANCEL_I32: i32 = 2;
const GWLP_USERDATA: i32 = -21;

const PROGRESS_CLASS_NAME: &str = "msctls_progress32\0";
const STATIC_CLASS_NAME: &str = "STATIC\0";
const BUTTON_CLASS_NAME: &str = "BUTTON\0";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Per-window state passed via `lpCreateParams` and recovered through
/// `GWL_USERDATA`. Owned by the window — `Box::from_raw` in WM_NCDESTROY.
struct ProgressState {
    shared: std::sync::Arc<ProgressShared>,
    hwnd_progress: HWND,
    hwnd_status: HWND,
}

// ===========================================================================
//  Window class registration (one-time)
// ===========================================================================

static CLASS_ATOM: OnceLock<u16> = OnceLock::new();

unsafe fn register_class() -> u16 {
    *CLASS_ATOM.get_or_init(|| {
        // Bind the wide class name to a local that outlives `wc`.
        let class_name_w = wide(CLASS_NAME);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(progress_wndproc),
            cbClsExtra: 0,
            cbWndExtra: std::mem::size_of::<isize>() as i32,
            hInstance: unsafe { GetModuleHandleW(std::ptr::null()) },
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: COLOR_BTNFACE as HBRUSH,
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name_w.as_ptr(),
            hIconSm: std::ptr::null_mut(),
        };
        unsafe { RegisterClassExW(&wc) as u16 }
    })
}

// ===========================================================================
//  Window procedure
// ===========================================================================

unsafe extern "system" fn progress_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => unsafe {
            let create_struct = lparam
                as *const windows_sys::Win32::UI::WindowsAndMessaging::CREATESTRUCTW;
            let state = (*create_struct).lpCreateParams as *mut ProgressState;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);

            let hinst = GetModuleHandleW(std::ptr::null());

            let hwnd_progress = CreateWindowExW(
                0,
                wide(PROGRESS_CLASS_NAME).as_ptr(),
                std::ptr::null(),
                WS_CHILD | WS_VISIBLE,
                10,
                12,
                WINDOW_W - 20,
                24,
                hwnd,
                IDC_PROGRESS as *mut _,
                hinst,
                std::ptr::null(),
            );
            // PBM_SETRANGE: lParam = MAKELPARAM(min, max) = (max << 16) | min.
            SendMessageW(
                hwnd_progress,
                PBM_SETRANGE,
                0,
                ((100u32 << 16) | 0u32) as isize,
            );

            let hwnd_status = CreateWindowExW(
                0,
                wide(STATIC_CLASS_NAME).as_ptr(),
                wide("Starting...").as_ptr(),
                WS_CHILD | WS_VISIBLE | SS_LEFT,
                10,
                44,
                WINDOW_W - 20,
                22,
                hwnd,
                IDC_STATUS as *mut _,
                hinst,
                std::ptr::null(),
            );

            let hwnd_cancel = CreateWindowExW(
                0,
                wide(BUTTON_CLASS_NAME).as_ptr(),
                wide("Cancel").as_ptr(),
                WS_CHILD | WS_VISIBLE | BS_PUSHBUTTON as u32,
                WINDOW_W - 100,
                78,
                90,
                28,
                hwnd,
                IDCANCEL as *mut _,
                hinst,
                std::ptr::null(),
            );

            (*state).hwnd_progress = hwnd_progress;
            (*state).hwnd_status = hwnd_status;

            SetTimer(hwnd, TIMER_ID, TIMER_MS, None);
            SetFocus(hwnd_cancel);

            0
        },
        WM_TIMER => unsafe {
            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ProgressState;
            if raw.is_null() {
                return 0;
            }
            // Read every value out of the shared state into locals so
            // we don't hold the auto-ref through `raw` for long. Rust
            // 2024's `dangerous_implicit_autorefs` lint requires that
            // raw-pointer accesses don't create implicit references
            // through their target — we use `(&(*raw).shared).field`
            // to be explicit, then move out of the struct.
            let pct = (&(*raw).shared).pct.load(Ordering::SeqCst);
            let phase = (&(*raw).shared).phase.load(Ordering::SeqCst);
            let bytes = (&(*raw).shared).bytes.load(Ordering::SeqCst);
            let total = (&(*raw).shared).total_bytes.load(Ordering::SeqCst);
            let done = (&(*raw).shared).done.load(Ordering::SeqCst);
            let hwnd_progress = (*raw).hwnd_progress;
            let hwnd_status = (*raw).hwnd_status;

            let status = format_status_line(phase, pct, bytes, total);

            SendMessageW(hwnd_progress, PBM_SETPOS, pct as WPARAM, 0);
            let status_w = wide(&status);
            SetWindowTextW(hwnd_status, status_w.as_ptr());

            if done != 0 {
                PostQuitMessage(0);
            }
            0
        },
        WM_COMMAND => {
            // LOWORD(wparam) holds the control id; HIWORD(wparam) holds
            // the notification code (BN_CLICKED for buttons).
            let id = (wparam as u32) & 0xFFFF;
            if id == IDCANCEL as u32 {
                unsafe {
                    let raw =
                        GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ProgressState;
                    if !raw.is_null() {
                        (&(*raw).shared).done.store(3, Ordering::SeqCst); // 3 = cancelled
                    }
                    PostQuitMessage(0);
                }
            }
            0
        }
        WM_CLOSE => unsafe {
            PostQuitMessage(0);
            0
        }
        WM_NCDESTROY => unsafe {
            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ProgressState;
            if !raw.is_null() {
                let _ = Box::from_raw(raw);
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

// ===========================================================================
//  Live status text — mirrors `jdk_install::format_status_line` so the
//  fallback window tells the same story the TaskDialog would.
// ===========================================================================

fn format_status_line(phase: i32, pct: u32, bytes: u64, total: u64) -> String {
    let pct = pct.to_string();
    let mib = |n: u64| -> String { format!("{:.1}", n as f64 / 1_048_576.0) };
    let d = crate::dialogs::dialogs();
    match phase {
        0 if total > 0 => crate::dialogs::fill(
            d.jdk_install.progress.status_phase_0_with_size.as_str(),
            &[("done_mb", &mib(bytes)), ("total_mb", &mib(total)), ("pct", &pct)],
        ),
        0 => crate::dialogs::fill(
            d.jdk_install.progress.status_phase_0_no_size.as_str(),
            &[("pct", &pct)],
        ),
        1 => crate::dialogs::fill(
            d.jdk_install.progress.status_phase_1.as_str(),
            &[("pct", &pct)],
        ),
        2 => crate::dialogs::fill(
            d.jdk_install.progress.status_phase_2.as_str(),
            &[("pct", &pct)],
        ),
        _ => crate::dialogs::fill(
            d.jdk_install.progress.status_other.as_str(),
            &[("pct", &pct)],
        ),
    }
}

// ===========================================================================
//  Public API
// ===========================================================================

/// Show the progress window modally. Returns the picked button id
/// (`IDOK_I32` on success, `IDCANCEL_I32` on user cancel or worker error).
///
/// `shared` must be the same `Arc<ProgressShared>` that the worker
/// thread is updating.
pub unsafe fn show(
    parent: HWND,
    title: &str,
    initial_status: &str,
    shared: std::sync::Arc<ProgressShared>,
) -> i32 {
    unsafe { register_class() };

    let title_w = wide(title);
    let hinst = unsafe { GetModuleHandleW(std::ptr::null()) };

    let state_box = Box::new(ProgressState {
        shared: shared.clone(),
        hwnd_progress: std::ptr::null_mut(),
        hwnd_status: std::ptr::null_mut(),
    });
    let state_ptr = Box::into_raw(state_box);

    // Centre on the primary monitor if `parent` is null.
    let (x, y) = if parent.is_null() {
        let sw = unsafe { GetSystemMetrics(SM_CXSCREEN) };
        let sh = unsafe { GetSystemMetrics(SM_CYSCREEN) };
        ((sw - WINDOW_W) / 2, (sh - WINDOW_H) / 2)
    } else {
        (CW_USEDEFAULT, CW_USEDEFAULT)
    };

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST,
            wide(CLASS_NAME).as_ptr(),
            title_w.as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_VISIBLE,
            x,
            y,
            WINDOW_W,
            WINDOW_H,
            parent,
            std::ptr::null_mut(),
            hinst,
            state_ptr as *mut _,
        )
    };

    if hwnd.is_null() {
        let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        log::log(&format!(
            "progress_window::show: CreateWindowExW failed (GetLastError={})",
            err
        ));
        let _ = unsafe { Box::from_raw(state_ptr) };
        return IDCANCEL_I32;
    }

    let status_hwnd = unsafe { GetDlgItem(hwnd, IDC_STATUS) };
    if !status_hwnd.is_null() {
        let s = wide(initial_status);
        unsafe { SetWindowTextW(status_hwnd, s.as_ptr()) };
    }

    // Modal message loop. Exits when `PostQuitMessage` is called from
    // WM_TIMER (worker finished) or WM_COMMAND (user clicked Cancel).
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    unsafe {
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    unsafe { KillTimer(hwnd, TIMER_ID) };
    unsafe { DestroyWindow(hwnd) };

    let done = shared.done.load(Ordering::SeqCst);
    match done {
        1 => IDOK_I32,
        _ => IDCANCEL_I32,
    }
}