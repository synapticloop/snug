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
//! own — the white background, the light-blue info box, the large
//! mascot area, and the progress bar — is painted in `WM_PAINT`.
//! The text and progress values are pushed into stock `STATIC` and
//! `BUTTON` children via `SetWindowTextW` from the existing
//! `WM_TIMER` poll, so the data path is identical to the v6
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
    AC_SRC_ALPHA, AC_SRC_OVER, AlphaBlend, BeginPaint, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
    BLENDFUNCTION, BI_RGB, CreateCompatibleDC, CreateDIBSection, CreateFontW,
    CreateRoundRectRgn, CreateSolidBrush, DIB_RGB_COLORS, DeleteDC, DeleteObject, EndPaint,
    FillRect, FillRgn, FW_BOLD, FW_NORMAL, GetObjectW, GetStockObject, HBRUSH, HDC, HBITMAP,
    HFONT, InvalidateRect, NULL_BRUSH, PAINTSTRUCT, SelectObject, FW_SEMIBOLD, WHITE_BRUSH,
};
use windows_sys::Win32::System::LibraryLoader::{
    FindResourceW, GetModuleHandleW, LoadResource, LockResource,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, DrawIconEx, GetMessageW,
    GetSystemMetrics, HICON, KillTimer, LoadIconW, MSG, PostQuitMessage,
    RegisterClassExW, SendMessageW, SetTimer, SetWindowTextW, SetWindowLongPtrW,
    GetWindowLongPtrW, TranslateMessage, CW_USEDEFAULT, IDCANCEL, ICON_BIG, IDI_INFORMATION,
    ICON_SMALL, SM_CXSCREEN, SM_CYSCREEN, BS_DEFPUSHBUTTON,
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

const WINDOW_W: i32 = 640;
const WINDOW_H: i32 = 300;

const MARGIN: i32 = 16;
const MASCOT_X: i32 = MARGIN;
const MASCOT_Y: i32 = MARGIN;
const MASCOT_W: i32 = 154;
const MASCOT_H: i32 = 154;

const TEXT_X: i32 = MASCOT_X + MASCOT_W + MARGIN;
const TEXT_W: i32 = WINDOW_W - TEXT_X - MARGIN - MARGIN;

const HEADING_Y: i32 = 8;
const HEADING_H: i32 = 48;

const SUBTITLE_Y: i32 = 50;
const SUBTITLE_H: i32 = 50;

const PROGRESS_X: i32 = TEXT_X;
const PROGRESS_Y: i32 = 108;
const PROGRESS_H: i32 = 18;
const PROGRESS_W: i32 = TEXT_W - 52;
const PCT_X: i32 = TEXT_X + PROGRESS_W + 6;
const PCT_W: i32 = TEXT_X + TEXT_W - PCT_X;
const PCT_H: i32 = 20;

const PHASE_Y: i32 = 132;
const PHASE_H: i32 = 50;
const DETAIL_Y: i32 = 154;
const DETAIL_H: i32 = 16;

const INFO_BOX_X: i32 = MARGIN;
const INFO_BOX_W: i32 = WINDOW_W - MARGIN * 3;
const INFO_BOX_Y: i32 = 186;
const INFO_BOX_H: i32 = 50;
const INFO_PAD: i32 = 8;
const INFO_ICON_SIZE: i32 = 12;

// Vertical offsets inside the info box. Currently the icon is
// aligned with the heading baseline (`INFO_PAD - 2` nudges up by 2
// px so the 12 px icon top sits level with the 12 pt heading cap).
// Set `INFO_ICON_Y_OFFSET` to `(INFO_BOX_H - INFO_ICON_SIZE) / 2`
// for vertical centering; `INFO_PAD` for top-aligned with full
// padding; `INFO_BOX_H - INFO_ICON_SIZE - INFO_PAD` for bottom-
// aligned.
const INFO_ICON_Y_OFFSET: i32 = INFO_PAD + 2;
const INFO_HEADING_Y_OFFSET: i32 = INFO_PAD - 2;
const INFO_SUBTEXT_Y_OFFSET: i32 = INFO_PAD + 20;
const INFO_TEXT_X: i32 = INFO_BOX_X + INFO_PAD + INFO_ICON_SIZE + 28;
const INFO_TEXT_W: i32 = INFO_BOX_W - (INFO_TEXT_X - INFO_BOX_X) - INFO_PAD;

const CANCEL_W: i32 = 80;
const CANCEL_H: i32 = 25;
const CANCEL_X: i32 = WINDOW_W - MARGIN * 2 - CANCEL_W - INFO_PAD - INFO_PAD;
const CANCEL_Y: i32 = INFO_BOX_Y + (INFO_BOX_H - CANCEL_H) / 2;

// Control IDs
const IDC_HEADING: i32 = 1001;
const IDC_SUBTITLE: i32 = 1002;
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

const STATIC_CLASS_NAME: &str = "STATIC\0";
const BUTTON_CLASS_NAME: &str = "BUTTON\0";

// ===========================================================================
//  Font configuration
// ===========================================================================
//
// Per-text-element font size and weight. Tweak these to retune
// typography without touching the control-creation / paint logic.
//
// `FONT_FACE` is shared across all elements. Two visual tiers today
// — "heading" (12pt bold) for the top heading + info-box heading, and
// "body" (9pt regular) for every other text element — but each
// element has its own `_PT` / `_WEIGHT` constants so you can split one
// element away from its tier without touching siblings. Every element
// gets its own HFONT in `ProgressState` (allocated in `WM_CREATE`,
// freed in `WM_NCDESTROY`) so the constants actually drive the
// rendering.
//
// `FW_NORMAL` = 400, `FW_BOLD` = 700, per `winuser.h`.

const FONT_FACE: &str = "Segoe UI\0";

// "Downloading Runtime Components" — main heading.
const HEADING_PT: i32 = 26;
const HEADING_WEIGHT: i32 = FW_SEMIBOLD as i32;

// "We are downloading the runtime components..." — subtitle.
const SUBTITLE_PT: i32 = 14;
const SUBTITLE_WEIGHT: i32 = FW_NORMAL as i32;

// "0%", "47%", etc. — to the right of the progress bar.
const PCT_PT: i32 = 14;
const PCT_WEIGHT: i32 = FW_NORMAL as i32;

// "Downloading runtime components (Windows x64)" — below the bar.
const PHASE_PT: i32 = 9;
const PHASE_WEIGHT: i32 = FW_NORMAL as i32;

// "102 MB of 149 MB (12.4 MB/s)" — left detail line.
const DETAIL_LEFT_PT: i32 = 9;
const DETAIL_LEFT_WEIGHT: i32 = FW_NORMAL as i32;

// "5 sec left" — right detail line.
const DETAIL_RIGHT_PT: i32 = 9;
const DETAIL_RIGHT_WEIGHT: i32 = FW_NORMAL as i32;

// "This only needs to be downloaded..." — info-box heading.
const INFO_HEADING_PT: i32 = 12;
const INFO_HEADING_WEIGHT: i32 = FW_BOLD as i32;

// "We will reuse this runtime..." — info-box subtext.
const INFO_SUBTEXT_PT: i32 = 9;
const INFO_SUBTEXT_WEIGHT: i32 = FW_NORMAL as i32;

// Colours (COLORREF = 0x00BBGGRR).
const COLOR_BG: u32 = 0x00FFFFFF;
const COLOR_SUBTITLE: u32 = 0x005F6368;
const COLOR_PROGRESS_FILL: u32 = 0x00E8731A; // RGB(0x1A, 0x73, 0xE8) — brand blue
const COLOR_PROGRESS_TRACK: u32 = 0x00E0E0E0; // RGB(0xE0, 0xE0, 0xE0) — neutral light grey
const COLOR_INFO_BG: u32 = 0x00FEF0E8; // RGB(0xE8, 0xF0, 0xFE) — info-box blue

// Corner rounding for the progress bar. ~`PROGRESS_H / 3` diameter
// gives a subtle, modern Windows 11 look (radius ~1 px on a 6 px bar).
// Set to `PROGRESS_H` for full pill ends; `0` for sharp corners.
const PROGRESS_CORNER_DIAMETER: i32 = PROGRESS_H / 3;

// Corner rounding for the info box. ~`INFO_BOX_H / 4` diameter
// (radius ~4 px on a 36 px box) — visible without intruding on the
// info icon / text inside the box.
const INFO_BOX_CORNER_DIAMETER: i32 = INFO_BOX_H / 4;

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
    hwnd_pct: HWND,
    hwnd_phase: HWND,
    hwnd_detail_left: HWND,
    hwnd_detail_right: HWND,
    hwnd_info_icon: HWND,
    hwnd_info_heading: HWND,
    hwnd_info_subtext: HWND,
    hwnd_cancel: HWND,
    /// One HFONT per text element, all created from the per-element
    /// `*_PT` / `*_WEIGHT` constants near the top of the file. Today
    /// heading + info-heading share the same 12pt-bold values, and
    /// every body element shares 9pt-regular — but each element gets
    /// its own handle so a future tune that splits one out doesn't
    /// affect its siblings. Freed in `WM_NCDESTROY`.
    hfont_heading: HFONT,
    hfont_subtitle: HFONT,
    hfont_pct: HFONT,
    hfont_phase: HFONT,
    hfont_detail_left: HFONT,
    hfont_detail_right: HFONT,
    hfont_info_heading: HFONT,
    hfont_info_subtext: HFONT,
    started_at: Instant,
    /// Last phase value we wrote the cancel button label for. Lets
    /// `WM_TIMER` rewrite the label only on the 0→1 transition
    /// (Install → Cancel) instead of every 200 ms tick.
    last_label_phase: i32,
    /// Last pct value we invalidated the bar rect for. Lets
    /// `WM_TIMER` skip the `InvalidateRect` call on ticks where
    /// the worker hasn't moved the percentage (e.g. during the
    /// verify phase where `pct` is pinned at 95 for ~1 s while
    /// SHA-256 runs).
    last_pct: u32,
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
            let hfont_heading = create_font_pt(
                HEADING_PT,
                HEADING_WEIGHT,
                false,
                FONT_FACE,
            );
            let hfont_subtitle = create_font_pt(
                SUBTITLE_PT,
                SUBTITLE_WEIGHT,
                false,
                FONT_FACE,
            );
            let hfont_pct = create_font_pt(PCT_PT, PCT_WEIGHT, false, FONT_FACE);
            let hfont_phase =
                create_font_pt(PHASE_PT, PHASE_WEIGHT, false, FONT_FACE);
            let hfont_detail_left = create_font_pt(
                DETAIL_LEFT_PT,
                DETAIL_LEFT_WEIGHT,
                false,
                FONT_FACE,
            );
            let hfont_detail_right = create_font_pt(
                DETAIL_RIGHT_PT,
                DETAIL_RIGHT_WEIGHT,
                false,
                FONT_FACE,
            );
            let hfont_info_heading = create_font_pt(
                INFO_HEADING_PT,
                INFO_HEADING_WEIGHT,
                false,
                FONT_FACE,
            );
            let hfont_info_subtext = create_font_pt(
                INFO_SUBTEXT_PT,
                INFO_SUBTEXT_WEIGHT,
                false,
                FONT_FACE,
            );

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
            apply_font(hwnd_subtitle, hfont_subtitle);

            // The progress bar is no longer a `msctls_progress32`
            // child window — it's custom-painted inside the parent
            // `WM_PAINT` (see the `// 2. Progress bar` block there),
            // so we can drive the look without fighting the system
            // theme. `PROGRESS_X`/`_Y`/`_W`/`_H` below drive both the
            // track + fill rectangles in WM_PAINT and the
            // `InvalidateRect` region used by `WM_TIMER` to push a
            // repaint when the worker thread updates `shared.pct`.

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
            apply_font(hwnd_pct, hfont_pct);

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
            apply_font(hwnd_phase, hfont_phase);

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
            apply_font(hwnd_detail_left, hfont_detail_left);

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
            apply_font(hwnd_detail_right, hfont_detail_right);

            let hwnd_info_icon = CreateWindowExW(
                0,
                wide(STATIC_CLASS_NAME).as_ptr(),
                std::ptr::null(),
                WS_CHILD | WS_VISIBLE | SS_ICON | SS_CENTER,
                INFO_BOX_X + INFO_PAD,
                INFO_BOX_Y + INFO_ICON_Y_OFFSET,
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
                INFO_BOX_Y + INFO_HEADING_Y_OFFSET,
                INFO_TEXT_W,
                22,
                hwnd,
                IDC_INFO_HEADING as *mut _,
                hinst,
                std::ptr::null(),
            );
            apply_font(hwnd_info_heading, hfont_info_heading);

            let hwnd_info_subtext = CreateWindowExW(
                0,
                wide(STATIC_CLASS_NAME).as_ptr(),
                wide(&d.jdk_install.progress.info_subtext).as_ptr(),
                WS_CHILD | WS_VISIBLE | SS_LEFT,
                INFO_TEXT_X,
                INFO_BOX_Y + INFO_SUBTEXT_Y_OFFSET,
                INFO_TEXT_W,
                22,
                hwnd,
                IDC_INFO_SUBTEXT as *mut _,
                hinst,
                std::ptr::null(),
            );
            apply_font(hwnd_info_subtext, hfont_info_subtext);

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
            (*state).hwnd_pct = hwnd_pct;
            (*state).hwnd_phase = hwnd_phase;
            (*state).hwnd_detail_left = hwnd_detail_left;
            (*state).hwnd_detail_right = hwnd_detail_right;
            (*state).hwnd_info_icon = hwnd_info_icon;
            (*state).hwnd_info_heading = hwnd_info_heading;
            (*state).hwnd_info_subtext = hwnd_info_subtext;
            (*state).hwnd_cancel = hwnd_cancel;
            (*state).hfont_heading = hfont_heading;
            (*state).hfont_subtitle = hfont_subtitle;
            (*state).hfont_pct = hfont_pct;
            (*state).hfont_phase = hfont_phase;
            (*state).hfont_detail_left = hfont_detail_left;
            (*state).hfont_detail_right = hfont_detail_right;
            (*state).hfont_info_heading = hfont_info_heading;
            (*state).hfont_info_subtext = hfont_info_subtext;
            (*state).started_at = Instant::now();

            SetTimer(hwnd, TIMER_ID, TIMER_MS, None);
            SetFocus(hwnd_cancel);

            0
        },
        WM_PAINT => unsafe {
            // Custom-paint the white background, the progress bar,
            // the light-blue info box, and the mascot icon. Child
            // controls (heading, subtitle, percent label, etc.) are
            // painted by their own WM_PAINT handlers.
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);

            let mut rc: RECT = std::mem::zeroed();
            windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc);

            // 1. White background.
            let bg_brush = CreateSolidBrush(COLOR_BG);
            FillRect(hdc, &rc, bg_brush);
            DeleteObject(bg_brush as _);

            // 2. Progress bar — track + fill, custom-painted so the
            // look doesn't depend on the system theme. Both are
            // rounded rectangles using `PROGRESS_CORNER_DIAMETER` for
            // a consistent, subtle curve on track and fill — the fill
            // gets the same corners at every percentage (no special
            // case at 100%).
            //
            // `pct` is read straight off `shared` so the repaint is
            // always in sync with whatever the worker wrote last.
            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ProgressState;
            let pct = if !raw.is_null() {
                (&(*raw).shared).pct.load(Ordering::SeqCst)
            } else {
                0
            };

            // Track.
            let track_rgn = CreateRoundRectRgn(
                PROGRESS_X,
                PROGRESS_Y,
                PROGRESS_X + PROGRESS_W,
                PROGRESS_Y + PROGRESS_H,
                PROGRESS_CORNER_DIAMETER,
                PROGRESS_CORNER_DIAMETER,
            );
            if !track_rgn.is_null() {
                let track_brush = CreateSolidBrush(COLOR_PROGRESS_TRACK);
                FillRgn(hdc, track_rgn, track_brush);
                DeleteObject(track_brush as _);
                DeleteObject(track_rgn as _);
            }

            // Fill — same corner radius as the track at every pct.
            let fill_w =
                (PROGRESS_W as i32 * pct as i32 / 100).max(0).min(PROGRESS_W as i32);
            if fill_w > 0 {
                let fill_rgn = CreateRoundRectRgn(
                    PROGRESS_X,
                    PROGRESS_Y,
                    PROGRESS_X + fill_w,
                    PROGRESS_Y + PROGRESS_H,
                    PROGRESS_CORNER_DIAMETER,
                    PROGRESS_CORNER_DIAMETER,
                );
                if !fill_rgn.is_null() {
                    let fill_brush = CreateSolidBrush(COLOR_PROGRESS_FILL);
                    FillRgn(hdc, fill_rgn, fill_brush);
                    DeleteObject(fill_brush as _);
                    DeleteObject(fill_rgn as _);
                }
            }

            // 3. Light-blue info box — rounded.
            let info_rgn = CreateRoundRectRgn(
                INFO_BOX_X,
                INFO_BOX_Y,
                INFO_BOX_X + INFO_BOX_W,
                INFO_BOX_Y + INFO_BOX_H,
                INFO_BOX_CORNER_DIAMETER,
                INFO_BOX_CORNER_DIAMETER,
            );
            if !info_rgn.is_null() {
                let info_brush = CreateSolidBrush(COLOR_INFO_BG);
                FillRgn(hdc, info_rgn, info_brush);
                DeleteObject(info_brush as _);
                DeleteObject(info_rgn as _);
            }

            // 4. Mascot. Prefer an HBITMAP the caller pushed via
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
                crate::log::log(&format!(
                    "WM_PAINT mascot: PNG bitmap path hbitmap={}",
                    mascot_hbitmap
                ));
                draw_mascot_hbitmap(hdc, mascot_hbitmap as _);
            } else {
                // Production path: read the EXE icon resource and
                // AlphaBlend it onto the mascot slot. `DrawIconEx`
                // doesn't honour 32-bit alpha for these icons (the
                // icons are encoded as PNG-in-ICO and `DrawIconEx`
                // falls back to the 1-bit AND mask, which renders the
                // soft-alpha background as fully opaque). Going
                // through `AlphaBlend` directly gives the user the
                // same per-pixel transparency the preview shows for
                // the PNG mascot.
                let hinst = unsafe { GetModuleHandleW(std::ptr::null()) };
                if !hinst.is_null() {
                    let best_id = crate::jdk_install::best_icon_id_for_size(hinst, MASCOT_LOAD_CX, MASCOT_LOAD_CY);
                    match best_id {
                        Some(id) => unsafe {
                            crate::log::log(&format!(
                                "WM_PAINT mascot: AlphaBlend path, RT_ICON id={}",
                                id
                            ));
                            draw_exe_mascot_with_alpha(hdc, hinst, id, MASCOT_X, MASCOT_Y, MASCOT_W, MASCOT_H);
                        },
                        None => crate::log::log("WM_PAINT mascot: no RT_ICON id found"),
                    }
                } else {
                    crate::log::log("WM_PAINT mascot: GetModuleHandleW returned NULL");
                }
            }

            EndPaint(hwnd, &ps);
            0
        },
        WM_CTLCOLORSTATIC => unsafe {
            // Every text-bearing STATIC paints transparent so the
            // parent's painted background (white for the main body,
            // light blue for the info box) shows through — except
            // for the dynamic text controls (percent, detail-left,
            // detail-right) which update 5×/sec. For those we
            // return `WHITE_BRUSH` so the control rect is fully
            // repainted with the dialog fill before the new text
            // is drawn; otherwise the previous value's pixels stay
            // visible behind the new value ("12%" becoming "13%"
            // with a ghost "2" still showing, etc.).
            //
            // The info icon is the exception — it's the only STATIC
            // carrying its own image content (via `STM_SETICON`), so
            // we let the default brush stand for it.
            //
            // Subtitle, phase label, and detail-right line get the
            // lighter-weight grey text colour. Heading, info heading,
            // percent label, detail-left, and info subtext keep the
            // system default (black / COLOR_WINDOWTEXT).
            use windows_sys::Win32::Graphics::Gdi::{
                SetBkMode, SetTextColor, HDC, TRANSPARENT,
            };
            let hdc: HDC = wparam as HDC;
            let hwnd_child = lparam as HWND;
            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ProgressState;
            if !raw.is_null() && hwnd_child != (*raw).hwnd_info_icon {
                let grey_text = hwnd_child == (*raw).hwnd_subtitle
                    || hwnd_child == (*raw).hwnd_phase
                    || hwnd_child == (*raw).hwnd_detail_right;
                if grey_text {
                    SetTextColor(hdc, COLOR_SUBTITLE);
                }
                let dynamic = hwnd_child == (*raw).hwnd_pct
                    || hwnd_child == (*raw).hwnd_detail_left
                    || hwnd_child == (*raw).hwnd_detail_right;
                if dynamic {
                    return GetStockObject(WHITE_BRUSH) as LRESULT;
                }
                SetBkMode(hdc, TRANSPARENT as i32);
                return GetStockObject(NULL_BRUSH) as LRESULT;
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

            // Invalidate just the progress bar rect when pct moved.
            // Skipping ticks where pct didn't change avoids a
            // redundant full WM_PAINT (and re-blitting the mascot /
            // text) on every 200 ms poll.
            if pct != (*raw).last_pct {
                (*raw).last_pct = pct;
                let bar_rc = RECT {
                    left: PROGRESS_X,
                    top: PROGRESS_Y,
                    right: PROGRESS_X + PROGRESS_W,
                    bottom: PROGRESS_Y + PROGRESS_H,
                };
                InvalidateRect(hwnd, &bar_rc, 0);
            }

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
                if !(*raw).hfont_subtitle.is_null() {
                    DeleteObject((*raw).hfont_subtitle as _);
                }
                if !(*raw).hfont_pct.is_null() {
                    DeleteObject((*raw).hfont_pct as _);
                }
                if !(*raw).hfont_phase.is_null() {
                    DeleteObject((*raw).hfont_phase as _);
                }
                if !(*raw).hfont_detail_left.is_null() {
                    DeleteObject((*raw).hfont_detail_left as _);
                }
                if !(*raw).hfont_detail_right.is_null() {
                    DeleteObject((*raw).hfont_detail_right as _);
                }
                if !(*raw).hfont_info_heading.is_null() {
                    DeleteObject((*raw).hfont_info_heading as _);
                }
                if !(*raw).hfont_info_subtext.is_null() {
                    DeleteObject((*raw).hfont_info_subtext as _);
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
/// BGRA byte order and **straight per-pixel alpha** (the format
/// `CreateDIBSection` + `BI_RGB` produces when the caller writes
/// raw pixel bytes — see `progress_preview` for the conversion
/// routine). `AlphaBlend` with `AC_SRC_OVER` + `AC_SRC_ALPHA`
/// honours the per-pixel alpha so a PNG with a transparent
/// background composites correctly over the dialog's white fill
/// rather than rendering its RGB values flat.
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
    // BlendOp = AC_SRC_OVER, SourceConstantAlpha = 255 (use the
    // source's per-pixel alpha as the multiplier), AlphaFormat =
    // AC_SRC_ALPHA (source 32-bpp DIB has straight alpha in the
    // high byte). With `BI_RGB` + 32-bpp, GDI treats the alpha as
    // straight (non-premultiplied), which matches what the PNG
    // decoder hands us.
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    unsafe {
        AlphaBlend(
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
            blend,
        );
        SelectObject(hdc_mem, old);
        DeleteDC(hdc_mem);
    }
}

/// Look up the EXE's main icon at the entry whose dimensions best match
/// `(cx, cy)`, build a top-down DIB section from its raw
/// `BITMAPINFOHEADER` + BGRA pixel data, and `AlphaBlend` it onto the
/// dialog at `(x, y)` with size `(w, h)`.
///
/// This is the production equivalent of `draw_mascot_hbitmap` (which
/// takes an HBITMAP from the PNG preview path). `DrawIconEx` honours
/// `LR_SHARED` + the 1-bit AND mask for transparency, but a 32-bit
/// BGRA icon with a soft alpha background (like `assets/snug-icon.png`)
/// drawn via `DrawIconEx` ends up either fully opaque or fully
/// transparent depending on `editpe`'s icon encoding, neither of
/// which matches the mockup. Reading the raw RT_ICON bytes and
/// `AlphaBlend`-ing with `AC_SRC_ALPHA` gives full 32-bit alpha
/// transparency the way the user expects.
fn draw_exe_mascot_with_alpha(
    hdc: HDC,
    hinst: windows_sys::Win32::Foundation::HINSTANCE,
    icon_id: u16,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) {
    const RT_ICON: u16 = 3;
    unsafe {
        let hres = FindResourceW(
            hinst,
            icon_id as usize as *const u16,
            RT_ICON as *const u16,
        );
        if hres.is_null() {
            crate::log::log(&format!(
                "draw_exe_mascot_with_alpha: FindResourceW(RT_ICON id={}) returned NULL",
                icon_id
            ));
            return;
        }
        let hmem = LoadResource(hinst, hres);
        let pdata = if !hmem.is_null() {
            LockResource(hmem)
        } else {
            crate::log::log("draw_exe_mascot_with_alpha: LoadResource returned NULL");
            std::ptr::null_mut()
        };
        if pdata.is_null() {
            return;
        }

        // Parse BITMAPINFOHEADER. `biHeight` is doubled for icons
        // (color + AND mask); we want just the color portion.
        let header = pdata as *const BITMAPINFOHEADER;
        let bi_width = (*header).biWidth;
        let bi_height_full = (*header).biHeight.unsigned_abs();
        let bi_bit_count = (*header).biBitCount as u32;
        let bi_height = bi_height_full as i32 / 2;

        if bi_bit_count != 32 || bi_width <= 0 || bi_height == 0 {
            crate::log::log(&format!(
                "draw_exe_mascot_with_alpha: unsupported icon format (w={} h={} bpp={})",
                bi_width, bi_height, bi_bit_count
            ));
            return;
        }

        // Pixel data follows the BITMAPINFOHEADER (typically 40 bytes).
        let pixels = (pdata as *const u8).add((*header).biSize as usize);

        // Build a top-down DIB section so AlphaBlend reads BGRA rows
        // in the natural top-to-bottom order without an extra flip.
let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: bi_width,
                biHeight: -bi_height, // negative = top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [std::mem::zeroed(); 1],
        };

        let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
        let hbmp = CreateDIBSection(
            core::ptr::null_mut(),
            &bmi,
            DIB_RGB_COLORS,
            &mut bits,
            core::ptr::null_mut(),
            0,
        );
        if hbmp.is_null() || bits.is_null() {
            crate::log::log("draw_exe_mascot_with_alpha: CreateDIBSection failed");
            return;
        }
        let row_bytes = bi_width as usize * 4;
        core::ptr::copy_nonoverlapping(pixels, bits as *mut u8, row_bytes * bi_height as usize);

        // AlphaBlend onto the dialog HDC.
        let mem_dc = CreateCompatibleDC(hdc);
        if mem_dc.is_null() {
            DeleteObject(hbmp);
            return;
        }
        let old = SelectObject(mem_dc, hbmp);
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let ok = AlphaBlend(
            hdc,
            x,
            y,
            w,
            h,
            mem_dc,
            0,
            0,
            bi_width,
            bi_height as i32,
            blend,
        );
        if ok == 0 {
            crate::log::log("draw_exe_mascot_with_alpha: AlphaBlend returned 0");
        } else {
            crate::log::log(&format!(
                "draw_exe_mascot_with_alpha: AlphaBlend OK ({}x{} -> {}x{} at {}, {})",
                bi_width, bi_height, w, h, x, y
            ));
        }
        SelectObject(mem_dc, old);
        DeleteDC(mem_dc);
        DeleteObject(hbmp);
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
        hwnd_pct: std::ptr::null_mut(),
        hwnd_phase: std::ptr::null_mut(),
        hwnd_detail_left: std::ptr::null_mut(),
        hwnd_detail_right: std::ptr::null_mut(),
        hwnd_info_icon: std::ptr::null_mut(),
        hwnd_info_heading: std::ptr::null_mut(),
        hwnd_info_subtext: std::ptr::null_mut(),
        hwnd_cancel: std::ptr::null_mut(),
        hfont_heading: std::ptr::null_mut(),
        hfont_subtitle: std::ptr::null_mut(),
        hfont_pct: std::ptr::null_mut(),
        hfont_phase: std::ptr::null_mut(),
        hfont_detail_left: std::ptr::null_mut(),
        hfont_detail_right: std::ptr::null_mut(),
        hfont_info_heading: std::ptr::null_mut(),
        hfont_info_subtext: std::ptr::null_mut(),
        started_at: Instant::now(),
        // Start in phase 0 — `WM_TIMER`'s first tick will rewrite
        // the cancel button to "Install" when the worker hasn't
        // moved on yet, and to "Cancel" once phase 1 begins.
        last_label_phase: -1,
        last_pct: 0,
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
