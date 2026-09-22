//! Custom Win32 progress window — modal dialog used as the v5 fallback
//! for the comctl32-v6-only `TaskDialogIndirect` progress dialog
//! (see `jdk_install::show_progress_dialog` for the v6 path).
//!
//! Layout (960×540) mirrors the user-facing mockup:
//!
//! ```text
//! ┌───────────────────────────────────────────────────────────────┐
//! │ [icon] Downloading...                              — □ ✕      │ ← title bar (system)
//! │                                                                │
//! │  ┌──────────────┐  Downloading Runtime Components              │
//! │  │              │  We are downloading the runtime components    │
//! │  │   [mascot]   │  so we can run the application                │
//! │  │              │                                               │
//! │  │              │  [████████░░░░░░░░░░░░░░░░░] 68%              │
//! │  │              │  Downloading runtime components (Windows x64)  │
//! │  │              │  102 MB of 149 MB (12.4 MB/s)   5 sec left    │
//! │  └──────────────┘                                               │
//! │  ┌──────────────────────────────────────┐  ┌────────────┐       │
//! │  │ ⓘ  This only needs to be downloaded…  │  │   Cancel   │       │
//! │  │    We will reuse this runtime…         │  │            │       │
//! │  └──────────────────────────────────────┘  └────────────┘       │
//! └───────────────────────────────────────────────────────────────┘
//! ```
//!
//! Everything that the stock common controls can't express on their
//! own — the white background, the light-blue info box, and the
//! large mascot area — is painted in `WM_PAINT`. The text and
//! progress values are pushed into stock `STATIC`, `msctls_progress32`,
//! and `BUTTON` children via `SetWindowTextW` / `PBM_SETPOS` from the
//! existing `WM_TIMER` poll, so the data path is identical to the v6
//! `TaskDialogIndirect` path (both consume `ProgressShared`).
//!
//! **Mascot asset.** The mockup shows a Java-coffee-jar mascot.
//! We don't carry a separate mascot asset in the payload yet, so
//! the image area renders the EXE's main icon scaled large — same
//! image as the title-bar icon. The scaled 32×32 source looks
//! pixelated at 340 px; shipping a higher-resolution `mascot.png`
//! payload asset is a follow-up slice. See [`MASCOT_NOTE`] below.

use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, BITMAP, CreateCompatibleDC, CreateFontW, CreateSolidBrush, DeleteDC, DeleteObject,
    EndPaint, FillRect, FW_BOLD, GetObjectW, GetStockObject, HBRUSH, HDC, HBITMAP, HFONT,
    NULL_BRUSH, PAINTSTRUCT, SelectObject, SRCCOPY, StretchBlt,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{PBM_SETBARCOLOR, PBM_SETPOS, PBM_SETRANGE};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, DrawIconEx, GetMessageW,
    GetSystemMetrics, HICON, KillTimer, LoadIconW, LoadImageW, MSG, PostQuitMessage,
    RegisterClassExW, SendMessageW, SetTimer, SetWindowTextW, SetWindowLongPtrW,
    GetWindowLongPtrW, TranslateMessage, CW_USEDEFAULT, IDCANCEL, ICON_BIG, IDI_INFORMATION,
    ICON_SMALL, IMAGE_ICON, LR_SHARED, SM_CXSCREEN, SM_CYSCREEN, BS_DEFPUSHBUTTON,
    WM_COMMAND, WM_CREATE, WM_CLOSE, WM_NCDESTROY, WM_CTLCOLORSTATIC, WM_PAINT, WM_SETICON,
    WM_TIMER,
    WNDCLASSEXW, WS_CAPTION, WS_CHILD, WS_SYSMENU, WS_VISIBLE, WS_OVERLAPPED, WS_EX_TOPMOST,
};

use crate::jdk_install::{load_exe_main_icon_hicon, ProgressShared};
use crate::log;

// `SS_*` constants that windows-sys 0.59 doesn't export. Values come
// from winuser.h — kept here rather than enabling the full
// `Win32_UI_WindowsAndMessaging` features just for these.
const SS_LEFT: u32 = 0x0000;
const SS_RIGHT: u32 = 0x0002;
const SS_ICON: u32 = 0x0003;
const SS_CENTER: u32 = 0x0001;
const SS_LEFTNOWORDWRAP: u32 = 0x000C;
// `WM_SETFONT` and `STM_SETICON` likewise: not exported by windows-sys
// 0.59 but documented and stable. Values from winuser.h.
const WM_SETFONT: u32 = 0x0030;
const STM_SETICON: u32 = 0x0170;

/// Width/height we ask for when loading the EXE icon for the mascot
/// slot. Windows ICO files typically contain 16, 32, 48, and 256 px
/// sizes — asking for 256 lets `LoadImageW` pick the largest
/// available, which survives being scaled to the 340 px mascot box
/// much better than the 32 px the title bar uses.
const MASCOT_LOAD_CX: i32 = 256;
const MASCOT_LOAD_CY: i32 = 256;

// ===========================================================================
//  Layout constants
// ===========================================================================

const CLASS_NAME: &str = "snug_progress_dialog_v2\0";

const TIMER_ID: usize = 1;
const TIMER_MS: u32 = 200;

const WINDOW_W: i32 = 480;
const WINDOW_H: i32 = 270;

const MARGIN: i32 = 16;
const MASCOT_X: i32 = MARGIN;
const MASCOT_Y: i32 = 36;
const MASCOT_W: i32 = 140;
const MASCOT_H: i32 = 140;

const TEXT_X: i32 = MASCOT_X + MASCOT_W + 18;
const TEXT_W: i32 = WINDOW_W - TEXT_X - MARGIN;

const HEADING_Y: i32 = 30;
const HEADING_H: i32 = 18;

const SUBTITLE_Y: i32 = 65;
const SUBTITLE_H: i32 = 25;

const PROGRESS_Y: i32 = 110;
const PROGRESS_H: i32 = 6;
const PROGRESS_W: i32 = TEXT_W - 32;
const PCT_X: i32 = TEXT_X + PROGRESS_W + 6;
const PCT_W: i32 = TEXT_X + TEXT_W - PCT_X;
const PCT_H: i32 = 12;

const PHASE_Y: i32 = 125;
const PHASE_H: i32 = 11;
const DETAIL_Y: i32 = 139;
const DETAIL_H: i32 = 11;

const INFO_BOX_X: i32 = MARGIN;
const INFO_BOX_W: i32 = WINDOW_W - MARGIN * 2 - 70;
const INFO_BOX_Y: i32 = 220;
const INFO_BOX_H: i32 = 36;
const INFO_PAD: i32 = 8;
const INFO_ICON_SIZE: i32 = 12;
const INFO_TEXT_X: i32 = INFO_BOX_X + INFO_PAD + INFO_ICON_SIZE + 6;
const INFO_TEXT_W: i32 = INFO_BOX_W - (INFO_TEXT_X - INFO_BOX_X) - INFO_PAD;

const CANCEL_W: i32 = 60;
const CANCEL_H: i32 = 18;
const CANCEL_X: i32 = WINDOW_W - MARGIN - CANCEL_W;
const CANCEL_Y: i32 = INFO_BOX_Y + (INFO_BOX_H - CANCEL_H) / 2;

// Control IDs
const IDC_HEADING: i32 = 1001;
const IDC_SUBTITLE: i32 = 1002;
const IDC_PROGRESS: i32 = 1003;
const IDC_PCT: i32 = 1004;
const IDC_PHASE: i32 = 1005;
const IDC_DETAIL_LEFT: i32 = 1006;
const IDC_DETAIL_RIGHT: i32 = 1007;
const IDC_INFO_ICON: i32 = 1008;
const IDC_INFO_HEADING: i32 = 1009;
const IDC_INFO_SUBTEXT: i32 = 1010;

const IDOK_I32: i32 = 1;
const IDCANCEL_I32: i32 = 2;
const GWLP_USERDATA: i32 = -21;

const PROGRESS_CLASS_NAME: &str = "msctls_progress32\0";
const STATIC_CLASS_NAME: &str = "STATIC\0";
const BUTTON_CLASS_NAME: &str = "BUTTON\0";

// Colours (COLORREF = 0x00BBGGRR).
const COLOR_BG: u32 = 0x00FFFFFF;
const COLOR_SUBTITLE: u32 = 0x005F6368;
const COLOR_PROGRESS_FILL: u32 = 0x00E8731A; // RGB(0x1A, 0x73, 0xE8) — brand blue
const COLOR_INFO_BG: u32 = 0x00FEF0E8; // RGB(0xE8, 0xF0, 0xFE) — info-box blue

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Per-window state passed via `lpCreateParams` and recovered through
/// `GWLP_USERDATA`. Owned by the window — `Box::from_raw` in
/// `WM_NCDESTROY` along with its HFONT handles.
struct ProgressState {
    shared: Arc<ProgressShared>,
    hwnd_heading: HWND,
    hwnd_subtitle: HWND,
    hwnd_progress: HWND,
    hwnd_pct: HWND,
    hwnd_phase: HWND,
    hwnd_detail_left: HWND,
    hwnd_detail_right: HWND,
    hwnd_info_icon: HWND,
    hwnd_info_heading: HWND,
    hwnd_info_subtext: HWND,
    hwnd_cancel: HWND,
    hfont_heading: HFONT,
    started_at: Instant,
    /// Last phase value we wrote the cancel button label for. Lets
    /// `WM_TIMER` rewrite the label only on the 0→1 transition
    /// (Install → Cancel) instead of every 200 ms tick.
    last_label_phase: i32,
}

// ===========================================================================
//  Window class registration (one-time)
// ===========================================================================

static CLASS_ATOM: OnceLock<u16> = OnceLock::new();

unsafe fn register_class() -> u16 {
    *CLASS_ATOM.get_or_init(|| {
        let class_name_w = wide(CLASS_NAME);
        let hicon = load_exe_main_icon_hicon().unwrap_or(std::ptr::null_mut());
        let hinst = unsafe { GetModuleHandleW(std::ptr::null()) };
        // NULL_BRUSH lets us paint the entire background in WM_PAINT
        // without flicker from the default class brush.
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(progress_wndproc),
            cbClsExtra: 0,
            cbWndExtra: std::mem::size_of::<isize>() as i32,
            hInstance: hinst,
            hIcon: hicon,
            hCursor: std::ptr::null_mut(),
            hbrBackground: unsafe { GetStockObject(NULL_BRUSH) as HBRUSH },
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name_w.as_ptr(),
            hIconSm: hicon,
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
            let create_struct =
                lparam as *const windows_sys::Win32::UI::WindowsAndMessaging::CREATESTRUCTW;
            let state = (*create_struct).lpCreateParams as *mut ProgressState;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);

            let hinst = GetModuleHandleW(std::ptr::null());
            let d = crate::dialogs::dialogs();

            // Heading — 12pt Segoe UI Bold (scaled to match the
            // 480×270 window). Body text uses the default dialog
            // font (hfont = NULL) — keeps DPI behaviour simple and
            // matches the subtitle / info-box copy.
            let hfont_heading =
                create_font_pt(12, FW_BOLD as i32, false, "Segoe UI\0");

            let hwnd_heading = CreateWindowExW(
                0,
                wide(STATIC_CLASS_NAME).as_ptr(),
                wide(&d.jdk_install.progress.heading).as_ptr(),
                WS_CHILD | WS_VISIBLE | SS_LEFTNOWORDWRAP,
                TEXT_X,
                HEADING_Y,
                TEXT_W,
                HEADING_H,
                hwnd,
                IDC_HEADING as *mut _,
                hinst,
                std::ptr::null(),
            );
            apply_font(hwnd_heading, hfont_heading);

            let hwnd_subtitle = CreateWindowExW(
                0,
                wide(STATIC_CLASS_NAME).as_ptr(),
                wide(&d.jdk_install.progress.subtitle).as_ptr(),
                WS_CHILD | WS_VISIBLE | SS_LEFT,
                TEXT_X,
                SUBTITLE_Y,
                TEXT_W,
                SUBTITLE_H,
                hwnd,
                IDC_SUBTITLE as *mut _,
                hinst,
                std::ptr::null(),
            );

            let hwnd_progress = CreateWindowExW(
                0,
                wide(PROGRESS_CLASS_NAME).as_ptr(),
                std::ptr::null(),
                WS_CHILD | WS_VISIBLE,
                TEXT_X,
                PROGRESS_Y,
                PROGRESS_W,
                PROGRESS_H,
                hwnd,
                IDC_PROGRESS as *mut _,
                hinst,
                std::ptr::null(),
            );
            SendMessageW(hwnd_progress, PBM_SETRANGE, 0, ((100u32 << 16) | 0u32) as isize);
            // Brand-blue fill. Note: themed progress bars on
            // modern Windows sometimes ignore this and use the
            // system accent colour instead — acceptable fallback.
            SendMessageW(
                hwnd_progress,
                PBM_SETBARCOLOR,
                0,
                COLOR_PROGRESS_FILL as isize,
            );

            let hwnd_pct = CreateWindowExW(
                0,
                wide(STATIC_CLASS_NAME).as_ptr(),
                wide("0%").as_ptr(),
                WS_CHILD | WS_VISIBLE | SS_LEFT,
                PCT_X,
                PROGRESS_Y - 4,
                PCT_W,
                PCT_H,
                hwnd,
                IDC_PCT as *mut _,
                hinst,
                std::ptr::null(),
            );

            let hwnd_phase = CreateWindowExW(
                0,
                wide(STATIC_CLASS_NAME).as_ptr(),
                wide(&d.jdk_install.progress.phase_label).as_ptr(),
                WS_CHILD | WS_VISIBLE | SS_LEFTNOWORDWRAP,
                TEXT_X,
                PHASE_Y,
                TEXT_W,
                PHASE_H,
                hwnd,
                IDC_PHASE as *mut _,
                hinst,
                std::ptr::null(),
            );

            let hwnd_detail_left = CreateWindowExW(
                0,
                wide(STATIC_CLASS_NAME).as_ptr(),
                wide("").as_ptr(),
                WS_CHILD | WS_VISIBLE | SS_LEFTNOWORDWRAP,
                TEXT_X,
                DETAIL_Y,
                TEXT_W * 3 / 5,
                DETAIL_H,
                hwnd,
                IDC_DETAIL_LEFT as *mut _,
                hinst,
                std::ptr::null(),
            );

            let hwnd_detail_right = CreateWindowExW(
                0,
                wide(STATIC_CLASS_NAME).as_ptr(),
                wide("").as_ptr(),
                WS_CHILD | WS_VISIBLE | SS_RIGHT,
                TEXT_X + TEXT_W * 3 / 5,
                DETAIL_Y,
                TEXT_W - TEXT_W * 3 / 5,
                DETAIL_H,
                hwnd,
                IDC_DETAIL_RIGHT as *mut _,
                hinst,
                std::ptr::null(),
            );

            let hwnd_info_icon = CreateWindowExW(
                0,
                wide(STATIC_CLASS_NAME).as_ptr(),
                std::ptr::null(),
                WS_CHILD | WS_VISIBLE | SS_ICON | SS_CENTER,
                INFO_BOX_X + INFO_PAD,
                INFO_BOX_Y + (INFO_BOX_H - INFO_ICON_SIZE) / 2,
                INFO_ICON_SIZE,
                INFO_ICON_SIZE,
                hwnd,
                IDC_INFO_ICON as *mut _,
                hinst,
                std::ptr::null(),
            );
            // System info icon — same one the TaskDialog footer
            // uses via `IDI_INFORMATION`.
            SendMessageW(
                hwnd_info_icon,
                STM_SETICON as u32,
                LoadIconW(std::ptr::null_mut(), IDI_INFORMATION) as WPARAM,
                0,
            );

            let hwnd_info_heading = CreateWindowExW(
                0,
                wide(STATIC_CLASS_NAME).as_ptr(),
                wide(&d.jdk_install.progress.info_heading).as_ptr(),
                WS_CHILD | WS_VISIBLE | SS_LEFTNOWORDWRAP,
                INFO_TEXT_X,
                INFO_BOX_Y + INFO_PAD - 2,
                INFO_TEXT_W,
                22,
                hwnd,
                IDC_INFO_HEADING as *mut _,
                hinst,
                std::ptr::null(),
            );
            apply_font(hwnd_info_heading, hfont_heading); // bold for emphasis

            let hwnd_info_subtext = CreateWindowExW(
                0,
                wide(STATIC_CLASS_NAME).as_ptr(),
                wide(&d.jdk_install.progress.info_subtext).as_ptr(),
                WS_CHILD | WS_VISIBLE | SS_LEFT,
                INFO_TEXT_X,
                INFO_BOX_Y + INFO_PAD + 20,
                INFO_TEXT_W,
                22,
                hwnd,
                IDC_INFO_SUBTEXT as *mut _,
                hinst,
                std::ptr::null(),
            );

            let hwnd_cancel = CreateWindowExW(
                0,
                wide(BUTTON_CLASS_NAME).as_ptr(),
                wide(&d.jdk_install.progress.cancel_button_during_download).as_ptr(),
                WS_CHILD | WS_VISIBLE | BS_DEFPUSHBUTTON as u32,
                CANCEL_X,
                CANCEL_Y,
                CANCEL_W,
                CANCEL_H,
                hwnd,
                IDCANCEL as *mut _,
                hinst,
                std::ptr::null(),
            );
            // The button text was set in CreateWindowExW above
            // (localized "Cancel" from the install-prompt section).

            (*state).hwnd_heading = hwnd_heading;
            (*state).hwnd_subtitle = hwnd_subtitle;
            (*state).hwnd_progress = hwnd_progress;
            (*state).hwnd_pct = hwnd_pct;
            (*state).hwnd_phase = hwnd_phase;
            (*state).hwnd_detail_left = hwnd_detail_left;
            (*state).hwnd_detail_right = hwnd_detail_right;
            (*state).hwnd_info_icon = hwnd_info_icon;
            (*state).hwnd_info_heading = hwnd_info_heading;
            (*state).hwnd_info_subtext = hwnd_info_subtext;
            (*state).hwnd_cancel = hwnd_cancel;
            (*state).hfont_heading = hfont_heading;
            (*state).started_at = Instant::now();

            SetTimer(hwnd, TIMER_ID, TIMER_MS, None);
            SetFocus(hwnd_cancel);

            0
        },
        WM_PAINT => unsafe {
            // Custom-paint the white background and the light-blue
            // info box, then draw the mascot icon. Child controls
            // (heading, subtitle, progress, etc.) are painted by
            // their own WM_PAINT handlers.
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);

            let mut rc: RECT = std::mem::zeroed();
            windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc);

            // 1. White background.
            let bg_brush = CreateSolidBrush(COLOR_BG);
            FillRect(hdc, &rc, bg_brush);
            DeleteObject(bg_brush as _);

            // 2. Light-blue info box.
            let info_rc = RECT {
                left: INFO_BOX_X,
                top: INFO_BOX_Y,
                right: INFO_BOX_X + INFO_BOX_W,
                bottom: INFO_BOX_Y + INFO_BOX_H,
            };
            let info_brush = CreateSolidBrush(COLOR_INFO_BG);
            FillRect(hdc, &info_rc, info_brush);
            DeleteObject(info_brush as _);

            // 3. Mascot. Prefer an HBITMAP the caller pushed via
            // `ProgressShared::set_mascot_hbitmap()` — `progress_preview`
            // uses this to draw `assets/snug-icon.png` directly. Falls
            // back to the EXE's main icon resource when no bitmap is
            // set (production path).
            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ProgressState;
            let mascot_hbitmap = if !raw.is_null() {
                (&(*raw).shared).mascot_hbitmap()
            } else {
                0
            };
            if mascot_hbitmap != 0 {
                draw_mascot_hbitmap(hdc, mascot_hbitmap as _);
            } else {
                let mascot_hicon = load_mascot_hicon().unwrap_or(std::ptr::null_mut());
                if !mascot_hicon.is_null() {
                    DrawIconEx(
                        hdc,
                        MASCOT_X,
                        MASCOT_Y,
                        mascot_hicon,
                        MASCOT_W,
                        MASCOT_H,
                        0,
                        std::ptr::null_mut(),
                        0,
                    );
                }
            }

            EndPaint(hwnd, &ps);
            0
        },
        WM_CTLCOLORSTATIC => unsafe {
            // Apply the gray subtitle / detail colour to the controls
            // the mockup shows in lighter weight, and use a
            // transparent background so the parent's painted
            // background (white, or the light-blue info box) shows
            // through behind the text.
            use windows_sys::Win32::Graphics::Gdi::{
                SetBkMode, SetTextColor, HDC, TRANSPARENT,
            };
            let hdc: HDC = wparam as HDC;
            let hwnd_child = lparam as HWND;
            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ProgressState;
            if !raw.is_null() {
                let gray_target = hwnd_child == (*raw).hwnd_subtitle
                    || hwnd_child == (*raw).hwnd_detail_right;
                if gray_target {
                    SetTextColor(hdc, COLOR_SUBTITLE);
                    SetBkMode(hdc, TRANSPARENT as i32);
                    return GetStockObject(NULL_BRUSH) as LRESULT;
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_TIMER => unsafe {
            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ProgressState;
            if raw.is_null() {
                return 0;
            }
            // Read shared state into locals (see note in the prior
            // version about Rust 2024's `dangerous_implicit_autorefs`).
            let pct = (&(*raw).shared).pct.load(Ordering::SeqCst);
            let phase = (&(*raw).shared).phase.load(Ordering::SeqCst);
            let bytes = (&(*raw).shared).bytes.load(Ordering::SeqCst);
            let total = (&(*raw).shared).total_bytes.load(Ordering::SeqCst);
            let done = (&(*raw).shared).done.load(Ordering::SeqCst);
            let started = (&(*raw).shared).started.load(Ordering::SeqCst);
            let started_at = (*raw).started_at;
            let hwnd_progress = (*raw).hwnd_progress;
            let hwnd_pct = (*raw).hwnd_pct;
            let hwnd_detail_left = (*raw).hwnd_detail_left;
            let hwnd_detail_right = (*raw).hwnd_detail_right;
            let hwnd_cancel = (*raw).hwnd_cancel;
            // Track the label we last wrote to the cancel button so
            // we don't spam `SetWindowTextW` every 200 ms tick. We
            // key off `started` (idempotent flip Install → Cancel)
            // rather than `phase`, since the label change is a
            // one-shot transition triggered by the user's first
            // click.
            let last_label_phase = (*raw).last_label_phase;

            let d = crate::dialogs::dialogs();
            let elapsed = started_at.elapsed().as_secs_f64();
            let view = format_view(phase, pct, bytes, total, elapsed, &d.jdk_install.progress);

            SendMessageW(hwnd_progress, PBM_SETPOS, pct as WPARAM, 0);
            set_static_text(hwnd_pct, &view.pct_label);
            set_static_text(hwnd_detail_left, &view.detail_left);
            set_static_text(hwnd_detail_right, &view.detail_right);

            // The button is "Install" before the user clicks and
            // "Cancel" once the install is in flight. The WM_COMMAND
            // handler also rewrites the label on the click for
            // snappiness; this tick is the safety net in case
            // WM_COMMAND fired before the dialog was fully wired up.
            let desired_started: i32 = if started { 1 } else { 0 };
            if desired_started != last_label_phase {
                let label = if started {
                    d.jdk_install.prompt.button_cancel.as_str()
                } else {
                    d.jdk_install.progress.cancel_button_during_download.as_str()
                };
                if !hwnd_cancel.is_null() {
                    let s = wide(label);
                    SetWindowTextW(hwnd_cancel, s.as_ptr());
                }
                (*raw).last_label_phase = desired_started;
            }

            if done != 0 {
                PostQuitMessage(0);
            }
            0
        },
        WM_COMMAND => {
            let id = (wparam as u32) & 0xFFFF;
            if id == IDCANCEL as u32 {
                unsafe {
                    let raw =
                        GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ProgressState;
                    if !raw.is_null() {
                        let started = (&(*raw).shared).started.load(Ordering::SeqCst);
                        if started {
                            // Already running — cancel the in-flight
                            // download / verify / extract.
                            (&(*raw).shared).done.store(3, Ordering::SeqCst); // 3 = cancelled
                        } else {
                            // First click — kick off the install.
                            // The worker is spinning on `started` at
                            // the top of `worker_thread`; setting it
                            // unblocks the download within ~50 ms.
                            (&(*raw).shared).started.store(true, Ordering::SeqCst);
                            // Flip the button label to "Cancel"
                            // immediately so the user sees the
                            // change without waiting for the next
                            // 200 ms tick.
                            let cancel_label = crate::dialogs::dialogs()
                                .jdk_install
                                .prompt
                                .button_cancel
                                .clone();
                            let s = wide(&cancel_label);
                            SetWindowTextW((*raw).hwnd_cancel, s.as_ptr());
                        }
                    }
                    PostQuitMessage(0);
                }
            }
            0
        }
        WM_CLOSE => unsafe {
            // Window closed (X button, Alt+F4). Make sure the worker
            // exits cleanly — its wait loop on `started` only checks
            // `started` and `done`, so we set done=3 here to wake it
            // up. Without this the worker would spin forever if the
            // user closed the window before clicking Install.
            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ProgressState;
            if !raw.is_null() {
                (&(*raw).shared).done.store(3, Ordering::SeqCst); // 3 = cancelled
                // Also unblock the worker in case it's mid-download;
                // the cancel callback in `download_to_disk` checks
                // `cancel`, but if the worker is past phase 0 the
                // `done` flag is what triggers the early return.
            }
            PostQuitMessage(0);
            0
        }
        WM_NCDESTROY => unsafe {
            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ProgressState;
            if !raw.is_null() {
                if !(*raw).hfont_heading.is_null() {
                    DeleteObject((*raw).hfont_heading as _);
                }
                let _ = Box::from_raw(raw);
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

// ===========================================================================
//  Static text helpers
// ===========================================================================

unsafe fn set_static_text(hwnd: HWND, text: &str) {
    if hwnd.is_null() {
        return;
    }
    let w = wide(text);
    unsafe { SetWindowTextW(hwnd, w.as_ptr()) };
}

unsafe fn apply_font(hwnd: HWND, hfont: HFONT) {
    if hwnd.is_null() || hfont.is_null() {
        return;
    }
    unsafe {
        SendMessageW(hwnd, WM_SETFONT as u32, hfont as WPARAM, 1);
    }
}

// ===========================================================================
//  View-model — what's currently shown in the right-column labels.
// ===========================================================================

struct LiveView {
    pct_label: String,
    detail_left: String,
    detail_right: String,
}

fn format_view(
    phase: i32,
    pct: u32,
    bytes: u64,
    total: u64,
    elapsed_secs: f64,
    strings: &crate::dialogs::ProgressDialog,
) -> LiveView {
    let mib = |n: u64| -> String { format!("{:.1}", n as f64 / 1_048_576.0) };
    let speed_mb_s = if elapsed_secs > 0.5 {
        format!("{:.1}", bytes as f64 / 1_048_576.0 / elapsed_secs)
    } else {
        "—".to_string()
    };
    let eta_secs: Option<u64> = if total > bytes && elapsed_secs > 0.5 && bytes > 0 {
        let bps = bytes as f64 / elapsed_secs;
        if bps > 1.0 {
            Some(((total - bytes) as f64 / bps).round() as u64)
        } else {
            None
        }
    } else if total > 0 && bytes >= total {
        Some(0)
    } else {
        None
    };

    let pct_label = crate::dialogs::fill(
        strings.pct_label.as_str(),
        &[("pct", &pct.to_string())],
    );

    // Phase 1 (verify SHA) and phase 2 (extract) don't have a useful
    // "X MB of Y MB" reading — the byte counter has gone stale. Show
    // `detail_no_size` with a percentage instead, matching the
    // TaskDialog's `status_phase_*` strings.
    let detail_left = if phase == 0 && total > 0 {
        crate::dialogs::fill(
            strings.detail_with_size.as_str(),
            &[
                ("done_mb", &mib(bytes)),
                ("total_mb", &mib(total)),
                ("speed_mb_s", &speed_mb_s),
            ],
        )
    } else {
        crate::dialogs::fill(
            strings.detail_no_size.as_str(),
            &[("pct", &pct.to_string())],
        )
    };

    let detail_right = match eta_secs {
        Some(0) | Some(1) if phase == 0 => strings.detail_eta_done.clone(),
        Some(1) => strings.detail_eta_second.clone(),
        Some(n) => crate::dialogs::fill(
            strings.detail_eta_seconds.as_str(),
            &[("n", &n.to_string())],
        ),
        None => String::new(),
    };

    LiveView {
        pct_label,
        detail_left,
        detail_right,
    }
}

// ===========================================================================
//  Window icon + mascot
// ===========================================================================

/// Apply the EXE's main icon to a freshly-created window — covers
/// both the title-bar slot (class) and the taskbar / Alt-Tab slot
/// (per-window `WM_SETICON`). Used right after `CreateWindowExW`.
unsafe fn apply_window_icon(hwnd: HWND) {
    if let Some(hicon) = load_exe_main_icon_hicon() {
        unsafe {
            SendMessageW(hwnd, WM_SETICON, ICON_SMALL as WPARAM, hicon as LPARAM);
            SendMessageW(hwnd, WM_SETICON, ICON_BIG as WPARAM, hicon as LPARAM);
        }
    }
}

/// Stretch a caller-supplied `HBITMAP` into the mascot slot. Used by
/// `progress_preview`, which decodes `assets/snug-icon.png` into a
/// top-down DIB section at startup and pushes the handle into
/// `ProgressShared::mascot` via `set_mascot_hbitmap()`. Falls back to
/// the EXE-icon path in `load_mascot_hicon` when the handle is
/// `NULL`.
///
/// The bitmap is assumed to be a 32-bpp top-down DIB section with
/// BGRA byte order (the format `CreateDIBSection` + `BI_RGB` produces
/// when the caller writes raw pixel bytes — see `progress_preview`
/// for the conversion routine). `StretchBlt` with `SRCCOPY` ignores
/// the alpha channel and treats each 32-bit pixel as opaque, so a
/// PNG with a transparent background will composite over the dialog's
/// white background instead of looking correct — that's a known
/// limitation noted for the follow-up `mascot.png` payload slice.
unsafe fn draw_mascot_hbitmap(hdc_dest: HDC, hbitmap: HBITMAP) {
    if hbitmap.is_null() {
        return;
    }
    let mut bmp: BITMAP = unsafe { std::mem::zeroed() };
    let got = unsafe {
        GetObjectW(
            hbitmap as _,
            std::mem::size_of::<BITMAP>() as i32,
            &mut bmp as *mut _ as *mut _,
        )
    };
    if got == 0 {
        return;
    }
    let src_w = bmp.bmWidth;
    let src_h = bmp.bmHeight;
    if src_w <= 0 || src_h <= 0 {
        return;
    }
    let hdc_mem = unsafe { CreateCompatibleDC(hdc_dest) };
    if hdc_mem.is_null() {
        return;
    }
    let old = unsafe { SelectObject(hdc_mem, hbitmap as _) };
    unsafe {
        StretchBlt(
            hdc_dest,
            MASCOT_X,
            MASCOT_Y,
            MASCOT_W,
            MASCOT_H,
            hdc_mem,
            0,
            0,
            src_w,
            src_h,
            SRCCOPY,
        );
        SelectObject(hdc_mem, old);
        DeleteDC(hdc_mem);
    }
}

/// Try to load the largest available size of the EXE's main icon for
/// the mascot slot. ICO files typically carry 16/32/48/256 px sizes —
/// asking for 256 lets `LoadImageW` pick the largest. Returns
/// `None` when the EXE has no icon (bare stub, no `--icon` input).
fn load_mascot_hicon() -> Option<HICON> {
    // `LoadImageW` with `IMAGE_ICON` + a fixed cx/cy walks the ICO
    // directory and picks the entry whose dimensions are ≥ the
    // requested size, downscaling if nothing larger is available.
    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        if hinst.is_null() {
            return load_exe_main_icon_hicon();
        }
        let hicon = LoadImageW(
            hinst,
            1usize as *const u16,
            IMAGE_ICON,
            MASCOT_LOAD_CX,
            MASCOT_LOAD_CY,
            LR_SHARED,
        );
        if !hicon.is_null() {
            Some(hicon)
        } else {
            load_exe_main_icon_hicon()
        }
    }
}

// ===========================================================================
//  DWM rounded corners
// ===========================================================================
//
// The Win11 `DWMWA_WINDOW_CORNER_PREFERENCE` attribute would let the
// window paint with rounded corners. windows-sys 0.59 gates the
// Dwm module behind the `Win32_Graphics_Dwm` feature which the
// workspace doesn't currently enable — enabling it just for this
// visual nicety would bloat the launcher binary. Left as a TODO
// follow-up; the mockup's window has square corners anyway.

unsafe fn _try_apply_dwm_rounded_corners_unused(_hwnd: HWND) {
    // intentionally empty
}

// ===========================================================================
//  Font creation
// ===========================================================================

/// Create a `HFONT` at the given point size with the given weight and
/// italic flag, using the system UI face name (`Segoe UI` on modern
/// Windows). The face string must be null-terminated.
fn create_font_pt(point_size: i32, weight: i32, italic: bool, face: &str) -> HFONT {
    // Point → pixel at 96 DPI. The window itself handles HiDPI by
    // scaling the font once it draws into the window's DC.
    let height = -(point_size * 96 / 72);
    let italic_u32 = if italic { 1 } else { 0 };
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            weight,
            italic_u32,
            0,
            0,
            1, // DEFAULT_CHARSET
            0, // OUT_DEFAULT_PRECIS
            0, // CLIP_DEFAULT_PRECIS
            0, // DEFAULT_QUALITY
            0, // DEFAULT_PITCH | FF_DONTCARE
            wide(face).as_ptr(),
        )
    }
}

// ===========================================================================
//  Public API
// ===========================================================================

/// Show the progress window modally. Returns the picked button id
/// (`IDOK_I32` on success, `IDCANCEL_I32` on user cancel or worker
/// error).
///
/// `shared` must be the same `Arc<ProgressShared>` that the worker
/// thread is updating. `title` becomes the window title-bar text;
/// the body content is driven entirely from `shared` via the
/// `format_view` helper.
pub unsafe fn show(
    parent: HWND,
    title: &str,
    _initial_status: &str,
    shared: Arc<ProgressShared>,
) -> i32 {
    unsafe { register_class() };

    let title_w = wide(title);
    let hinst = unsafe { GetModuleHandleW(std::ptr::null()) };

    let state_box = Box::new(ProgressState {
        shared: shared.clone(),
        hwnd_heading: std::ptr::null_mut(),
        hwnd_subtitle: std::ptr::null_mut(),
        hwnd_progress: std::ptr::null_mut(),
        hwnd_pct: std::ptr::null_mut(),
        hwnd_phase: std::ptr::null_mut(),
        hwnd_detail_left: std::ptr::null_mut(),
        hwnd_detail_right: std::ptr::null_mut(),
        hwnd_info_icon: std::ptr::null_mut(),
        hwnd_info_heading: std::ptr::null_mut(),
        hwnd_info_subtext: std::ptr::null_mut(),
        hwnd_cancel: std::ptr::null_mut(),
        hfont_heading: std::ptr::null_mut(),
        started_at: Instant::now(),
        // Start in phase 0 — `WM_TIMER`'s first tick will rewrite
        // the cancel button to "Install" when the worker hasn't
        // moved on yet, and to "Cancel" once phase 1 begins.
        last_label_phase: -1,
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

    unsafe { apply_window_icon(hwnd) };

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
