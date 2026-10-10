//! The only Win32 code in the crate: dialogs, and the marquee window that
//! covers a running build.
//!
//! Deliberately thin. Every string is passed in, every decision was made
//! by [`crate::decide`], and every child-process detail belongs to
//! [`crate::build`] — so the parts worth testing are all elsewhere and
//! this file is left as untested glue.
//!
//! Plain `MessageBoxW` rather than a custom-painted dialog: this is a
//! build tool, the system dialog is what a user's expectations are
//! calibrated to, and the launcher's own chrome (`jdk_install.rs`,
//! `modal_window.rs`) lives in `snug-launcher`, where it is wired to JVM
//! state this crate has no access to.

use std::sync::mpsc::{self, TryRecvError};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{GetSysColorBrush, COLOR_WINDOW};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{PBM_SETMARQUEE, PBM_SETPOS, PBM_SETRANGE32, PBS_MARQUEE};
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForSystem, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetMessageW, GetSystemMetrics, LoadCursorW, LoadImageW, MessageBoxW, RegisterClassExW,
    SendMessageW, SetTimer, TranslateMessage, ICON_BIG, ICON_SMALL, IDC_ARROW, IDYES, IMAGE_ICON,
    LR_DEFAULTSIZE, LR_LOADFROMFILE, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK,
    MB_YESNO, MSG, SM_CXSCREEN, SM_CYSCREEN, WNDCLASSEXW, WS_CAPTION, WS_CHILD, WS_OVERLAPPED,
    WS_VISIBLE, WM_CLOSE, WM_CREATE, WM_SETICON, WM_TIMER,
};

/// Caption shared by every dialog this tool shows.
pub const TITLE: &str = "Build with Snug";

// ---- DPI ----------------------------------------------------------------

/// Opt into per-monitor DPI awareness, before any window exists.
///
/// Without this, Windows bitmap-stretches our window on a 125–150%
/// display: correctly sized, but soft text. That reads as "cheap tool" to
/// exactly the audience this exists for, so the layout is scaled
/// explicitly by [`px`] instead. The awareness *call* alone would be worse
/// than nothing — it stops the stretch while leaving our hardcoded
/// logical pixels unscaled, so the window would come out too small.
pub fn enable_dpi_awareness() {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// Process DPI, floored at 96 so a pre-1703 Windows that doesn't export
/// `GetDpiForSystem` degrades to unscaled rather than to a zero-sized
/// window.
fn dpi() -> i32 {
    static DPI: OnceLock<i32> = OnceLock::new();
    *DPI.get_or_init(|| unsafe { GetDpiForSystem() as i32 }.max(96))
}

/// Scale a 96-DPI layout constant to the current display.
fn px(v: i32) -> i32 {
    v * dpi() / 96
}

// ---- Dialogs ------------------------------------------------------------

/// Informational dialog with a single Close button.
pub fn info(text: &str) {
    message_box(TITLE, text, MB_OK | MB_ICONINFORMATION);
}

/// Error dialog with a single Close button.
///
/// Used for build failures *and* for a missing `snug.exe`. Neither path
/// falls back to a terminal, so this dialog and the log it names are the
/// only recourse the user has — the message has to stand on its own.
pub fn error(text: &str) {
    message_box(TITLE, text, MB_OK | MB_ICONERROR);
}

/// Yes/No dialog. `true` for Yes.
///
/// Takes its own caption because the overwrite prompt names the file it is
/// about to replace, which is more use in the title bar than the tool's
/// name — "Build with Snug" is already on every other dialog here.
pub fn confirm(caption: &str, text: &str) -> bool {
    message_box(caption, text, MB_YESNO | MB_ICONQUESTION) == IDYES
}

/// `caption` is the title bar, `text` the body.
///
/// The parameter order deliberately leads with the caption, the way the
/// dialog reads on screen. `MessageBoxW` itself puts the body first, so the
/// swap happens exactly once, here, rather than at three call sites.
fn message_box(caption: &str, text: &str, kind: u32) -> i32 {
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide(text).as_ptr(),
            wide(caption).as_ptr(),
            kind,
        )
    }
}

// ---- Marquee window -----------------------------------------------------

/// Client area, in logical pixels at 96 DPI.
const CLIENT_W: i32 = 400;
const CLIENT_H: i32 = 150;

const MARGIN: i32 = 20;
const HEADING_Y: i32 = 24;
const HEADING_H: i32 = 22;
const SUB_Y: i32 = 54;
const SUB_H: i32 = 34;
const BAR_Y: i32 = 104;
const BAR_H: i32 = 22;

const CLASS_NAME: &str = "snug_dropper_progress_v1";

/// Timer that polls the worker thread. 100 ms is well below the point
/// where the hand-off would be perceptible.
const POLL_MS: u32 = 100;
const TIMER_ID: usize = 1;

/// Heading and sub-text for the window currently being created.
///
/// The window procedure is a plain `extern "system"` function with no
/// user-data pointer, so the strings are staged here before
/// `CreateWindowExW` and read back from `WM_CREATE`. `OnceLock` over
/// `static mut`: there is exactly one window per process, so the `set`
/// never actually loses a race — this makes that a non-issue rather than
/// a soundness one.
static PROGRESS_TEXT: OnceLock<(Vec<u16>, Vec<u16>)> = OnceLock::new();

/// Run `work` on a worker thread behind an indeterminate progress window,
/// returning its result once that window is gone.
///
/// The worker thread is not optional polish: a 200 MB fat JAR takes long
/// enough to hash, encode and stamp that doing this inline leaves the
/// window frozen and Windows paints "Not responding". Polling a channel
/// on `WM_TIMER` — rather than having the worker `PostMessage` — keeps
/// the cross-thread signalling in safe code, at the cost of a ≤100 ms
/// delay nobody can see.
pub fn with_marquee<T: Send + 'static>(
    heading: &str,
    sub: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (tx, rx) = mpsc::channel::<T>();
    std::thread::Builder::new()
        .name("snug-build".to_string())
        .spawn(move || {
            let value = work();
            let _ = tx.send(value);
        })
        .expect("spawning the build thread");

    let _ = PROGRESS_TEXT.set((wide(heading), wide(sub)));

    let hwnd = create_progress_window();
    unsafe { SetTimer(hwnd, TIMER_ID, POLL_MS, None) };

    let mut outcome: Option<T> = None;
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        let r = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if r <= 0 {
            break;
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        if msg.message == WM_TIMER && msg.wParam == TIMER_ID {
            match rx.try_recv() {
                Ok(value) => {
                    outcome = Some(value);
                    break;
                }
                // Still building.
                Err(TryRecvError::Empty) => {}
                // The worker died without reporting — only reachable if
                // it panicked. Break and let the `expect` below report
                // it, which beats a window that spins forever.
                Err(TryRecvError::Disconnected) => break,
            }
        }
    }

    // `WM_DESTROY` may have arrived via the message loop's own quit
    // path; either way this is idempotent.
    unsafe { DestroyWindow(hwnd) };
    outcome.expect("the build thread ended without reporting a result")
}

fn create_progress_window() -> HWND {
    unsafe {
        let class = wide(CLASS_NAME);
        let hinst: HINSTANCE = GetModuleHandleW(std::ptr::null_mut());

        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: std::ptr::null_mut(),
            // The stock arrow. A `null` cursor makes the whole window
            // show the busy hourglass, which is actively wrong for a
            // window that is blocked on purpose.
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
            hIconSm: std::ptr::null_mut(),
        };
        let _ = RegisterClassExW(&wc);

        // `CLIENT_W` / `CLIENT_H` describe the **client** area; the
        // caption bar is added on top here, the same way `snug_preview`
        // derives its window size. Centring uses the expanded size, which
        // is what the user actually sees on the desktop.
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: px(CLIENT_W),
            bottom: px(CLIENT_H),
        };
        let style = WS_OVERLAPPED | WS_CAPTION;
        AdjustWindowRectEx(&mut rect, style, 0, 0);
        let total_w = rect.right - rect.left;
        let total_h = rect.bottom - rect.top;

        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide(TITLE).as_ptr(),
            style | WS_VISIBLE,
            (GetSystemMetrics(SM_CXSCREEN) - total_w) / 2,
            (GetSystemMetrics(SM_CYSCREEN) - total_h) / 3,
            total_w,
            total_h,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null_mut(),
        );

        apply_exe_icon(hwnd);

        hwnd
    }
}

/// Put our own `MAINICON` on the window — title bar, Alt-Tab and taskbar.
///
/// The `MessageBoxW` dialogs pick the EXE's icon up for free, but a
/// custom-painted window gets nothing, and this is the window the user
/// stares at for the whole build. Reading the icon back out of our own
/// file is what lets the single `stamp_dropper_icon` step cover the whole
/// tool: swap the PNG, restamp, and the marquee follows.
///
/// Leaks one icon handle per process — at most two, on a window that
/// lives for the length of a build. Not worth a `WM_DESTROY` cleanup
/// path.
fn apply_exe_icon(hwnd: HWND) {
    let Ok(path) = std::env::current_exe() else {
        return;
    };
    let src = wide(&path.to_string_lossy());

    unsafe {
        // `LR_DEFAULTSIZE` takes the system small-icon metrics, which is
        // the right read for a title bar. Reused for `ICON_BIG` — the
        // taskbar resamples it, and a second `LoadImageW` for a window
        // that lives seconds isn't worth the code.
        let hicon = LoadImageW(
            std::ptr::null_mut(),
            src.as_ptr(),
            IMAGE_ICON,
            0,
            0,
            LR_LOADFROMFILE | LR_DEFAULTSIZE,
        );
        if !hicon.is_null() {
            SendMessageW(hwnd, WM_SETICON, ICON_BIG as WPARAM, hicon as LPARAM);
            SendMessageW(hwnd, WM_SETICON, ICON_SMALL as WPARAM, hicon as LPARAM);
        }
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => unsafe {
            let (heading, sub) = PROGRESS_TEXT
                .get()
                .cloned()
                .unwrap_or_else(|| (wide("Building..."), wide("")));

            let bar_w = px(CLIENT_W - MARGIN * 2);
            let hinst = GetModuleHandleW(std::ptr::null_mut());

            for (class, text, y, h) in [
                ("STATIC", heading, HEADING_Y, HEADING_H),
                ("STATIC", sub, SUB_Y, SUB_H),
            ] {
                CreateWindowExW(
                    0,
                    wide(class).as_ptr(),
                    text.as_ptr(),
                    WS_CHILD | WS_VISIBLE,
                    px(MARGIN),
                    px(y),
                    bar_w,
                    px(h),
                    hwnd,
                    std::ptr::null_mut(),
                    hinst,
                    std::ptr::null_mut(),
                );
            }

            let bar = CreateWindowExW(
                0,
                wide("msctls_progress32").as_ptr(),
                wide("").as_ptr(),
                WS_CHILD | WS_VISIBLE | PBS_MARQUEE,
                px(MARGIN),
                px(BAR_Y),
                bar_w,
                px(BAR_H),
                hwnd,
                std::ptr::null_mut(),
                hinst,
                std::ptr::null_mut(),
            );
            // Range and position are inert in marquee mode, but setting
            // them first means the bar still shows *something* if a
            // driver ignores `PBS_MARQUEE`.
            SendMessageW(bar, PBM_SETRANGE32, 0, 100);
            SendMessageW(bar, PBM_SETPOS, 10, 0);
            SendMessageW(bar, PBM_SETMARQUEE, 1, 30);

            0
        },
        WM_CLOSE => {
            // Ignored. The window carries no `WS_SYSMENU`, so there is no
            // close button, but Alt+F4 still arrives — and honouring it
            // would destroy the window out from under a `snug.exe` that
            // is midway through writing the output file. Blocking is the
            // only safe answer; the build finishes in seconds anyway.
            0
        }
        // Deliberately NO `WM_DESTROY` -> `PostQuitMessage` handler.
        //
        // Our loop ends itself, by breaking out once the worker reports
        // in, so a quit message buys nothing here — and it actively
        // breaks the *next* dialog. `PostQuitMessage` leaves a `WM_QUIT`
        // in the thread queue, and `MessageBoxW` runs its own modal
        // message loop over that same queue: it sees the stale quit,
        // tears its window down, and returns immediately. The effect was
        // the success and error dialogs flashing for a frame and
        // vanishing while the process exited 0 — a silently swallowed
        // result, and the reason the result dialogs are shown from code
        // that runs after the marquee is destroyed.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// UTF-16, NUL-terminated, for every Win32 entry point taking a string.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_is_nul_terminated() {
        assert_eq!(wide("ok"), vec![u16::from(b'o'), u16::from(b'k'), 0]);
    }

    #[test]
    fn wide_encodes_non_ascii() {
        // A stray non-ASCII character must survive rather than being
        // truncated to `?` — paths shown in dialogs routinely contain one.
        assert_eq!(wide("é"), vec![0xe9, 0]);
    }
}
