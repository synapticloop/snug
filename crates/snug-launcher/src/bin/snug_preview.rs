//! One-click dialog preview for the snug launcher.
//!
//! A tiny standalone Win32 GUI binary. Shows a launcher window with
//! one button per dialog kind the production launcher pops. Each
//! button click invokes the **same** code path the production
//! launcher takes — just with hard-coded sample data instead of a
//! real Adoptium fetch / JVM scan. Lets you iterate on dialog
//! layout / copy / button labels without rebuilding `snug-cli`,
//! `snug-format`, or the committed `bin/launcher-stub.exe`, and
//! without having to type
//! `cargo run --example dialogs_preview -- --kind <KIND>` every time.
//!
//! Build:
//!
//! ```bash
//! cargo build --bin snug_preview           # target/debug/snug_preview.exe
//! cargo build --bin snug_preview --release # target/release/snug_preview.exe
//! ```
//!
//! Each dialog is **detached** from the launcher's lifecycle — they
//! are top-level windows (no parent HWND) and run on their own
//! thread, so closing the launcher (X button or "Close") does not
//! destroy them. Click another button while a dialog is still up to
//! spawn a second dialog side-by-side. The process only exits when
//! **every** open window has been dismissed.
//!
//! No `unsafe` crate dependencies beyond what's already pinned in
//! `Cargo.toml` (just `windows-sys`).

#![cfg(windows)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, DT_LEFT, DT_SINGLELINE, DrawTextW, EndPaint, HBRUSH, HFONT,
    PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetSystemMetrics, LoadCursorW, MSG, PostQuitMessage, RegisterClassExW, SendMessageW,
    SM_CXSCREEN, SM_CYSCREEN, TranslateMessage, BS_PUSHBUTTON, IDC_ARROW, WM_CLOSE, WM_COMMAND,
    WM_CREATE, WM_DESTROY, WM_NCDESTROY, WM_PAINT, WNDCLASSEXW, WS_CAPTION, WS_CHILD,
    WS_EX_TOPMOST, WS_OVERLAPPED, WS_SYSMENU, WS_VISIBLE,
};

use snug_launcher::dialogs;
use snug_launcher::error_window;
use snug_launcher::jdk_install::{self, ProgressShared};
use snug_launcher::progress_window;

// ============================================================================
//  Constants
// ============================================================================

const CLASS_NAME: &str = "snug_preview_launcher_v1\0";
const WINDOW_TITLE: &str = "snug dialog preview\0";

/// Window dimensions (logical pixels at 96 DPI).
const LAUNCHER_W: i32 = 480;
const LAUNCHER_H: i32 = 720;

/// Button layout — single column, 24px outer margin, 8px gap.
const BTN_X: i32 = 24;
const BTN_W: i32 = LAUNCHER_W - 48;
const BTN_H: i32 = 38;
const BTN_GAP: i32 = 8;
const BTN_FIRST_Y: i32 = 100;
const BTN_CLOSE_W: i32 = 96;
const BTN_CLOSE_H: i32 = 36;
const BTN_CLOSE_MARGIN_BOTTOM: i32 = 16;

/// Heading text painted in `WM_PAINT`.
const HEADING_TEXT: &str = "snug dialog preview";
const SUBTITLE_TEXT: &str = "Click a button to open the corresponding dialog.";

/// `WM_SETFONT` isn't exported as a named constant by windows-sys
/// 0.59. Value from winuser.h.
const WM_SETFONT: u32 = 0x0030;

// ============================================================================
//  Window counter
// ============================================================================

/// Counts the number of open top-level windows owned by this preview
/// binary — the launcher window plus one per spawned dialog. Initial
/// value of 1 accounts for the launcher itself.
///
/// The process only exits when this hits zero. Decremented when the
/// launcher's `WM_NCDESTROY` fires, and when each spawned dialog's
/// thread finishes its `show()` call.
static OPEN_WINDOWS: AtomicUsize = AtomicUsize::new(1);

/// Decrement the window counter; if it was the last window, post
/// `WM_QUIT` to the calling thread's message queue so the message
/// loop (or, on a dialog thread, the modal `show()` loop) exits.
///
/// Safe to call from any thread: `PostQuitMessage` targets the
/// current thread.
unsafe fn maybe_quit_on_last() {
    let prev = OPEN_WINDOWS.fetch_sub(1, Ordering::SeqCst);
    if prev == 1 {
        // SAFETY: `PostQuitMessage` targets the **calling** thread's
        // message queue; safe to call from any thread as long as we
        // own the slot we're decrementing (which we do, via the
        // guard on the spawn-dialog closure).
        unsafe { PostQuitMessage(0) };
    }
}

/// Run `f` on a new OS thread, accounting for the new top-level
/// window in `OPEN_WINDOWS` and decrementing when `f` returns (or
/// panics — the `Guard` below releases the slot on drop).
///
/// Dialogs run on their own threads so closing the launcher doesn't
/// destroy them, and so the user can spawn multiple dialogs
/// side-by-side from the launcher.
fn spawn_dialog<F>(f: F)
where
    F: FnOnce() + Send + 'static,
{
    OPEN_WINDOWS.fetch_add(1, Ordering::SeqCst);
    let _slot = SlotGuard;
    thread::spawn(move || {
        let _slot = SlotGuard;
        f();
    });
}

/// `OPEN_WINDOWS` accounting guard. Decremented on drop so a panic in
/// the dialog thread still releases the window slot.
struct SlotGuard;
impl Drop for SlotGuard {
    fn drop(&mut self) {
        unsafe { maybe_quit_on_last() };
    }
}

// Button control IDs. Cast to `usize` for the `wparam & 0xFFFF` mask
// in `WM_COMMAND`.
const ID_BTN_PROGRESS_ANIM: usize = 1001;
const ID_BTN_PROGRESS_STATIC: usize = 1002;
const ID_BTN_METADATA_FAILED: usize = 1003;
const ID_BTN_RETRY: usize = 1004;
const ID_BTN_ERROR: usize = 1005;
const ID_BTN_JAVA_ERROR: usize = 1006;
const ID_BTN_PROMPT_V5: usize = 1007;
const ID_BTN_EARLY_BAIL: usize = 1008;
const ID_BTN_CLOSE: usize = 1099;

// ============================================================================
//  Entry point
// ============================================================================

fn main() {
    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        let class_name_w = wide(CLASS_NAME);

        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            // No `CS_HREDRAW | CS_VREDRAW` — the window is fixed
            // size, no resize; no flicker.
            style: 0,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: std::ptr::null_mut(),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            // NULL_BRUSH — we paint the entire background in
            // WM_PAINT so the system default-class brush doesn't
            // flash through.
            hbrBackground: std::ptr::null_mut() as HBRUSH,
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name_w.as_ptr(),
            hIconSm: std::ptr::null_mut(),
        };
        let _atom = RegisterClassExW(&wc);

        // Centre on primary monitor.
        let sw = GetSystemMetrics(SM_CXSCREEN);
        let sh = GetSystemMetrics(SM_CYSCREEN);
        let x = (sw - LAUNCHER_W) / 2;
        let y = (sh - LAUNCHER_H) / 2;

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST,
            class_name_w.as_ptr(),
            wide(WINDOW_TITLE).as_ptr(),
            WS_CAPTION | WS_SYSMENU | WS_OVERLAPPED | WS_VISIBLE,
            x,
            y,
            LAUNCHER_W,
            LAUNCHER_H,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );

        if hwnd.is_null() {
            eprintln!("snug-preview: CreateWindowExW failed");
            std::process::exit(1);
        }

        // Pump messages until PostQuitMessage.
        let mut msg: MSG = std::mem::zeroed();
        loop {
            let r = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
            if r == 0 || r == -1 {
                break;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

// ============================================================================
//  Window procedure
// ============================================================================

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => unsafe {
            // Create one Segoe UI 10pt font used by every button.
            // Leaked intentionally — the launcher window is the
            // last user-owned resource, the OS reclaims everything
            // on process exit.
            let hfont = create_button_font();

            // Body buttons (8 stacked).
            let hinst = GetModuleHandleW(std::ptr::null());
            let buttons: &[(&str, usize)] = &[
                ("Progress (animated)", ID_BTN_PROGRESS_ANIM),
                ("Progress (static @ 50%)", ID_BTN_PROGRESS_STATIC),
                ("Metadata failed", ID_BTN_METADATA_FAILED),
                ("Retry (try 2 of 3)", ID_BTN_RETRY),
                ("Error (post-install)", ID_BTN_ERROR),
                ("Java error (with update link)", ID_BTN_JAVA_ERROR),
                ("Install prompt v5 (MessageBoxW)", ID_BTN_PROMPT_V5),
                ("Early bail (MessageBoxW)", ID_BTN_EARLY_BAIL),
            ];
            for (i, (label, id)) in buttons.iter().enumerate() {
                let y = BTN_FIRST_Y + (i as i32) * (BTN_H + BTN_GAP);
                let btn = CreateWindowExW(
                    0,
                    wide("BUTTON").as_ptr(),
                    wide(label).as_ptr(),
                    WS_CHILD | WS_VISIBLE | (BS_PUSHBUTTON as u32),
                    BTN_X,
                    y,
                    BTN_W,
                    BTN_H,
                    hwnd,
                    *id as *mut _,
                    hinst,
                    std::ptr::null(),
                );
                if !btn.is_null() {
                    SendMessageW(btn, WM_SETFONT, hfont as usize, 1);
                }
            }

            // Close button — bottom right.
            let close_y = LAUNCHER_H - BTN_CLOSE_MARGIN_BOTTOM - BTN_CLOSE_H;
            let close = CreateWindowExW(
                0,
                wide("BUTTON").as_ptr(),
                wide("Close").as_ptr(),
                WS_CHILD | WS_VISIBLE | (BS_PUSHBUTTON as u32),
                LAUNCHER_W - BTN_X - BTN_CLOSE_W,
                close_y,
                BTN_CLOSE_W,
                BTN_CLOSE_H,
                hwnd,
                ID_BTN_CLOSE as *mut _,
                hinst,
                std::ptr::null(),
            );
            if !close.is_null() {
                SendMessageW(close, WM_SETFONT, hfont as usize, 1);
            }

            0
        },
        WM_PAINT => unsafe {
            // Paint the heading + subtitle above the buttons. Same
            // font as the buttons (10pt Segoe UI), reused via
            // SelectObject so the system font doesn't bleed through.
            let hfont = create_button_font();
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            let prev_font = SelectObject(hdc, hfont as _);

            SetBkMode(hdc, TRANSPARENT as i32);
            // Heading — slightly darker than the subtitle.
            SetTextColor(hdc, 0x00202020);
            let mut rect_head = RECT {
                left: BTN_X,
                top: 16,
                right: LAUNCHER_W - BTN_X,
                bottom: 48,
            };
            DrawTextW(
                hdc,
                wide(HEADING_TEXT).as_ptr(),
                -1,
                &mut rect_head,
                DT_LEFT | DT_SINGLELINE,
            );

            // Subtitle — mid grey.
            SetTextColor(hdc, 0x00606060);
            let mut rect_sub = RECT {
                left: BTN_X,
                top: 52,
                right: LAUNCHER_W - BTN_X,
                bottom: 88,
            };
            DrawTextW(
                hdc,
                wide(SUBTITLE_TEXT).as_ptr(),
                -1,
                &mut rect_sub,
                DT_LEFT | DT_SINGLELINE,
            );

            SelectObject(hdc, prev_font);
            EndPaint(hwnd, &ps);
            0
        },
        WM_COMMAND => {
            // LOWORD(wparam) is the control / menu id. Mask off
            // the notification code in the high word.
            let id = (wparam & 0xFFFF) as usize;
            match id {
                ID_BTN_PROGRESS_ANIM => spawn_dialog(|| run_progress(false)),
                ID_BTN_PROGRESS_STATIC => spawn_dialog(|| run_progress(true)),
                ID_BTN_METADATA_FAILED => spawn_dialog(|| {
                    let _ = jdk_install::show_metadata_failed_dialog(
                        std::ptr::null_mut(),
                        25,
                        "DNS resolution failed: no such host is known",
                    );
                }),
                ID_BTN_RETRY => spawn_dialog(|| {
                    let _ = jdk_install::show_retry_dialog(
                        std::ptr::null_mut(),
                        2,
                        3,
                        "25.0.1",
                        "TLS handshake timeout after 30s",
                    );
                }),
                ID_BTN_ERROR => spawn_dialog(|| {
                    jdk_install::show_error_dialog(
                        std::ptr::null_mut(),
                        "Sample post-install error",
                        "",
                        "java.lang.UnsatisfiedLinkError: C:\\Users\\demo\\.snug\\jdk\\jdk-25\\bin\\jvm.dll: Can't find dependent libraries",
                    );
                }),
                ID_BTN_JAVA_ERROR => spawn_dialog(|| unsafe {
                    error_window::show_launcher_error(
                        std::ptr::null_mut(),
                        "java.lang.NoClassDefFoundError: com/example/Main",
                        Some("https://github.com/adoptium/temurin25-binaries/releases"),
                    );
                }),
                ID_BTN_PROMPT_V5 => spawn_dialog(run_install_prompt_v5),
                ID_BTN_EARLY_BAIL => spawn_dialog(run_early_bail),
                ID_BTN_CLOSE => {
                    // Same as the X button: destroy the launcher
                    // window. Any dialogs spawned before this point
                    // are on their own threads and survive.
                    unsafe {
                        DestroyWindow(hwnd);
                    }
                }
                _ => {}
            }
            0
        }
        WM_CLOSE => {
            // Closing the launcher destroys **only the launcher**.
            // Any dialogs already spawned are independent top-level
            // windows on their own threads; they keep running until
            // the user dismisses them.
            unsafe {
                DestroyWindow(hwnd);
            }
            0
        }
        WM_DESTROY => {
            // Nothing to do — let `WM_NCDESTROY` handle the counter
            // decrement once the window is fully torn down.
            0
        }
        WM_NCDESTROY => {
            // Launcher window is fully gone — release its slot in the
            // window counter. If no dialogs are still running, this
            // posts `WM_QUIT` to the launcher thread and the process
            // exits.
            unsafe {
                maybe_quit_on_last();
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

// ============================================================================
//  Per-button handlers
// ============================================================================

/// Open the progress dialog. `static_mode=true` pins at 50% with no
/// worker thread; `false` spawns a worker that drives
/// `phase 0 → 95% → phase 1 → 98% → phase 2 → 100%` like the real
/// install flow.
///
/// Always invoked via `spawn_dialog` so it runs on its own OS thread
/// — closing the launcher doesn't take this dialog down. The dialog
/// itself is **null-parented** (top-level), so it's also independent
/// visually (can be moved to a different monitor, doesn't follow the
/// launcher's z-order).
fn run_progress(static_mode: bool) {
    let total_bytes: u64 = 150 * 1_048_576;
    let bytes_per_ms: u64 = ((10.0_f64 * 1_048_576.0) / 1000.0) as u64;
    let shared = Arc::new(ProgressShared::new(total_bytes));

    if !static_mode {
        let shared = shared.clone();
        thread::spawn(move || {
            shared.set_started(true);

            // Phase 0: download 0% → 95%.
            let tick = Duration::from_millis(50);
            let bytes_per_tick = (bytes_per_ms as f64 * 50.0 / 1000.0) as u64;
            loop {
                if shared.status() != 0 {
                    return;
                }
                let b = shared.bytes_done();
                if b >= total_bytes {
                    break;
                }
                let new_b = (b + bytes_per_tick).min(total_bytes);
                let pct = ((new_b as u128 * 95) / total_bytes as u128) as u32;
                shared.set_pct(pct);
                shared.set_bytes_done(new_b);
                shared.set_phase(0);
                thread::sleep(tick);
            }

            // Phase 1: verify 95% → 98%.
            if shared.status() != 0 {
                return;
            }
            for p in 95..=98 {
                shared.set_pct(p);
                shared.set_bytes_done(total_bytes);
                shared.set_phase(1);
                thread::sleep(Duration::from_millis(250));
                if shared.status() != 0 {
                    return;
                }
            }

            // Phase 2: extract 99% → 100%.
            for p in 99..=100 {
                shared.set_pct(p);
                shared.set_bytes_done(total_bytes);
                shared.set_phase(2);
                thread::sleep(Duration::from_millis(200));
                if shared.status() != 0 {
                    return;
                }
            }

            shared.set_status(1); // success
        });
    } else {
        shared.set_pct(50);
        shared.set_bytes_done(total_bytes / 2);
        shared.set_phase(0);
    }

    // Blocks until the user dismisses (Cancel, X, or worker sets
    // status != 0). Runs on the spawn-dialog thread (not the launcher
    // thread) so the launcher can accept more button clicks while
    // this dialog is up.
    unsafe {
        progress_window::show(
            std::ptr::null_mut(),
            "Snug — progress dialog preview",
            "",
            shared.clone(),
        );
    }
}

/// Mirror of `jdk_install::prompt_messagebox` — the comctl32-v5
/// fallback the launcher shows when `TaskDialogIndirect` isn't
/// available. We can't reach the private function from this binary
/// (it's private to `jdk_install`), so duplicate the same
/// `MessageBoxW` call shape here. Sample copy comes from
/// `[jdk_install.prompt]` in `dialogs.toml` so iterating on the
/// production strings shows up here too.
fn run_install_prompt_v5() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_DEFBUTTON1, MB_ICONQUESTION, MB_YESNOCANCEL,
    };

    let d = dialogs::dialogs();
    let prompt = &d.jdk_install.prompt;
    let mut text = String::new();
    text.push_str(&prompt.main);
    text.push_str("\n\n");
    text.push_str(&prompt.content);

    let result = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide(&text).as_ptr(),
            wide(&prompt.title).as_ptr(),
            MB_YESNOCANCEL | MB_ICONQUESTION | MB_DEFBUTTON1,
        )
    };
    eprintln!("snug-preview: install-prompt-v5 MessageBoxW returned {result}");
}

/// Mirror of `main.rs::show_error_box` — the `MessageBoxW` shown when
/// the launcher can't even load its embedded payload.
fn run_early_bail() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

    let title = "snug launcher";
    let body = "FATAL: failed to read snug payload from RCDATA resource.\n\
                This binary may be corrupted or stamped with the wrong manifest.\n\
                Re-download the launcher from the original source.";

    let result = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide(body).as_ptr(),
            wide(title).as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
    eprintln!("snug-preview: early-bail MessageBoxW returned {result}");
}

// ============================================================================
//  Helpers
// ============================================================================

/// UTF-16 null-terminated wide string for Win32 APIs. Mirrors the
/// `wide` helper used elsewhere in this crate (e.g.
/// `progress_window::wide`).
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Create the Segoe UI 10pt font used by every launcher button.
/// Same shape as `modal_window::create_font_pt` with weight
/// `FW_NORMAL` (=400). No underline. Leaked; see `WM_CREATE`.
unsafe fn create_button_font() -> HFONT {
    // `-pt * 96 / 72` converts points to a 96-DPI pixel height.
    let h = -((10 * 96) / 72);
    let face_w = wide("Segoe UI");
    unsafe {
        CreateFontW(
            h,
            0,
            0,
            0,
            400, // FW_NORMAL
            0,
            0,
            0,
            0, // DEFAULT_CHARSET — windows-sys constant is `0`
            0,
            0,
            0, // DEFAULT_QUALITY — windows-sys constant is `0`
            0, // DEFAULT_PITCH | FF_DONTCARE — both `0`
            face_w.as_ptr(),
        )
    }
}