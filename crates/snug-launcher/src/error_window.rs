//! Custom Win32 error dialog — modal dialog mirroring the layout of
//! `progress_window` but without the progress bar / phase state /
//! worker thread.
//!
//! Layout (640×320) is the same six-component shape used everywhere
//! else in the launcher:
//!
//! ```text
//! ┌───────────────────────────────────────────────────────────────┐
//! │ [icon] Something went wrong                       — □ ✕      │ ← title bar (system)
//! │                                                                │
//! │  ┌──────────────┐  Heading (large bold)                       │
//! │  │              │  Subheading (grey)                           │
//! │  │   [mascot]   │                                               │
//! │  │              │  Error content (multi-line)                  │
//! │  │              │                                               │
//! │  │              │                                               │
//! │  └──────────────┘                                               │
//! │  ┌──────────────────────────────────────┐  ┌────────────┐       │
//! │  │ ⓘ  What happened?                    │  │    OK       │       │
//! │  │     You can try again, or…            │  │             │       │
//! │  └──────────────────────────────────────┘  └────────────┘       │
//! └───────────────────────────────────────────────────────────────┘
//! ```
//!
//! Everything that stock common controls can't express — the white
//! background, the light-blue info box, the large mascot area — is
//! painted in `WM_PAINT`. Text and the button are stock `STATIC` /
//! `BUTTON` children driven by `SetWindowTextW`.
//!
//! **Constants.** Layout, colours, fonts, and the static strings all
//! live as `pub const` near the top of this file. Tweak them in
//! place — the WndProc + paint logic reads from them directly.
//!
//! **Mascot asset.** Same convention as `progress_window`: an
//! HBITMAP the caller pushes via the `mascot_hbitmap` parameter is
//! drawn into the 154×154 slot; otherwise we fall back to the EXE's
//! main icon resource loaded at 256 px.

use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleDC, CreateFontW, CreateRoundRectRgn, CreateSolidBrush,
    DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, FillRgn, FW_BOLD, FW_NORMAL,
    FW_SEMIBOLD, GetObjectW, GetStockObject, GetTextMetricsW, HBRUSH, HDC, HFONT, NULL_BRUSH,
    PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, TEXTMETRICW, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, DrawIconEx, DI_NORMAL,
    GetMessageW, GetSystemMetrics, GetWindowLongPtrW, LoadCursorW, LoadIconW, PostQuitMessage,
    RegisterClassExW, SendMessageW, SetCursor, SetWindowLongPtrW, TranslateMessage,
    BS_DEFPUSHBUTTON, IDCANCEL, IDI_ERROR, IDI_INFORMATION, IDI_WARNING, MSG, SM_CXSCREEN,
    SM_CYSCREEN, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORSTATIC, WM_LBUTTONDOWN,
    WM_MOUSEMOVE, WM_NCDESTROY, WM_PAINT, WM_SETCURSOR, WNDCLASSEXW, WS_CAPTION, WS_CHILD,
    WS_EX_TOPMOST, WS_OVERLAPPED, WS_SYSMENU, WS_VISIBLE,
};

// `SS_*` / `BS_DEFPUSHBUTTON` constants that windows-sys 0.59 doesn't
// export as named constants. Values from winuser.h — kept here rather
// than enabling the full `Win32_UI_WindowsAndMessaging` features just
// for these.
const SS_LEFT: u32 = 0x0000;
const SS_LEFTNOWORDWRAP: u32 = 0x000C;
const SS_ICON: u32 = 0x0003;
const WM_SETFONT: u32 = 0x0030;
const STM_SETICON: u32 = 0x0170;
// `IDC_HAND` is `MAKEINTRESOURCE(32649)` per winuser.h.
const IDC_HAND: *const u16 = 32649 as *const u16;
// `IDC_ARROW` is `MAKEINTRESOURCE(32512)`. We restore the arrow
// cursor on hover-exit to avoid leaving the hand cursor stuck if
// the cursor leaves the window while hovering.
const IDC_ARROW: *const u16 = 32512 as *const u16;
// `SW_SHOWNORMAL` per winuser.h / shellapi.h.
const SW_SHOWNORMAL: i32 = 1;
// `DT_*` constants used by `DrawTextW`. Values from winuser.h —
// kept here because windows-sys 0.59 doesn't export them.
const DT_CALCRECT: u32 = 0x0000_0004;
const DT_SINGLELINE: u32 = 0x0000_0020;
const DT_NOPREFIX: u32 = 0x0000_0800;

use crate::jdk_install::{find_best_icon_hicon, load_exe_main_icon_hicon};
use crate::log;

// ============================================================================
//  EDITABLE CONSTANTS — tweak these to retune the dialog
// ============================================================================

// -- Window / class --------------------------------------------------------

const CLASS_NAME: &str = "snug_error_dialog_v1\0";
const WINDOW_W: i32 = 640;
const WINDOW_H: i32 = 320;

// -- Layout ----------------------------------------------------------------

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

// Multi-line error content area — fills the slot the progress bar
// occupied in `progress_window`. Sized for ~4 lines at the body
// font size.
const CONTENT_Y: i32 = 108;
const CONTENT_H: i32 = 88;

const INFO_BOX_X: i32 = MARGIN;
const INFO_BOX_W: i32 = WINDOW_W - MARGIN * 3;
const INFO_BOX_Y: i32 = 220;
const INFO_BOX_H: i32 = 50;
const INFO_PAD: i32 = 8;
const INFO_ICON_SIZE: i32 = 16;

const INFO_ICON_Y_OFFSET: i32 = INFO_PAD + 2;
const INFO_HEADING_Y_OFFSET: i32 = INFO_PAD - 2;
const INFO_SUBTEXT_Y_OFFSET: i32 = INFO_PAD + 20;
const INFO_TEXT_X: i32 = INFO_BOX_X + INFO_PAD + INFO_ICON_SIZE + 28;
const INFO_TEXT_W: i32 = INFO_BOX_W - (INFO_TEXT_X - INFO_BOX_X) - INFO_PAD;

const BUTTON_W: i32 = 80;
const BUTTON_H: i32 = 25;
const BUTTON_X: i32 = WINDOW_W - MARGIN * 2 - BUTTON_W - INFO_PAD * 2;
const BUTTON_Y: i32 = INFO_BOX_Y + (INFO_BOX_H - BUTTON_H) / 2;

// Optional "Check for a newer version" link row, painted below the
// info box. Reserved only when the caller supplies a non-empty
// `update_check_url`; otherwise this region is left blank.
//
// Layout-wise the row is a single line of text — the label
// (e.g. "Check for a newer version:") renders in regular grey, and the
// URL renders blue + underlined. Both halves sit inside `LINK_RECT` for
// the purpose of hit-testing: clicking anywhere on the line opens the
// URL. The link row never paints when the URL is empty, so the
// existing layout is unchanged for callers that don't opt in.
const LINK_Y: i32 = 288;
const LINK_H: i32 = 20;
const LINK_TEXT_X: i32 = MARGIN;

// -- Mascot ----------------------------------------------------------------

/// Width/height we ask for when loading the EXE icon for the mascot
/// slot. Windows ICO files typically carry 16/32/48/256 px sizes —
/// asking for 256 selects a detailed source for the 154 px mascot
/// box rather than enlarging the 32 px title-bar icon.
const MASCOT_LOAD_CX: i32 = 256;
const MASCOT_LOAD_CY: i32 = 256;

// -- Fonts (per-element) ---------------------------------------------------

const FONT_FACE: &str = "Segoe UI\0";

const HEADING_PT: i32 = 22;
const HEADING_WEIGHT: i32 = FW_SEMIBOLD as i32;

const SUBTITLE_PT: i32 = 14;
const SUBTITLE_WEIGHT: i32 = FW_NORMAL as i32;

const CONTENT_PT: i32 = 13;
const CONTENT_WEIGHT: i32 = FW_NORMAL as i32;

const INFO_HEADING_PT: i32 = 12;
const INFO_HEADING_WEIGHT: i32 = FW_BOLD as i32;

const INFO_SUBTEXT_PT: i32 = 9;
const INFO_SUBTEXT_WEIGHT: i32 = FW_NORMAL as i32;

// URL portion of the link row — bold + underline. The label
// portion reuses the body font (hfont_content) so no separate
// LINK_LABEL_PT / LINK_LABEL_WEIGHT constants are needed.
const LINK_URL_PT: i32 = 9;
const LINK_URL_WEIGHT: i32 = FW_SEMIBOLD as i32;

// -- Colours (COLORREF = 0x00BBGGRR) ---------------------------------------

const COLOR_BG: u32 = 0x00FFFFFF;
const COLOR_SUBTITLE: u32 = 0x005F6368;
const COLOR_CONTENT: u32 = 0x00303030;
const COLOR_INFO_BG: u32 = 0x00FEF0E8;
// Link URL colour — standard hyperlink blue (matches the system
// `HYPERLINK` token in modern Windows).
const COLOR_LINK: u32 = 0x00E8731A; // RGB(26, 115, 232) = #1A73E8

// -- Corner rounding -------------------------------------------------------

const INFO_BOX_CORNER_DIAMETER: i32 = INFO_BOX_H / 4;

// -- Strings (overridable per call) ---------------------------------------

/// Default text inside the info box. Editable in place; the caller
/// can override via `info_heading` / `info_subtext` if needed. These
/// fall back when the `failure.info_heading` / `failure.info_subtext`
/// TOML strings are empty, so they're the literal source-of-truth
/// for the "no TOML override" path.
const INFO_HEADING_DEFAULT: &str = "What happened?";
const INFO_SUBTEXT_DEFAULT: &str =
    "You can try the install again, or visit the download page manually.";

/// Default button label. Caller can override via `button_label`.
const BUTTON_LABEL_DEFAULT: &str = "OK";

// ============================================================================
//  Public API
// ============================================================================

/// Which icon to draw inside the info box. Today only the
/// `progress_window` defaults to `Info`; the failure dialog uses
/// `Error`; the metadata-fetch-failure dialog uses `Warning`. Pick
/// to match the dialog's tone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InfoIcon {
    Info,
    Warning,
    Error,
}

/// Inputs to `show`. Caller fills the four text fields; icon +
/// optional info overrides default to taste.
pub struct ErrorDialog<'a> {
    pub title: &'a str,
    pub heading: &'a str,
    pub subheading: &'a str,
    pub error_content: &'a str,
    pub info_icon: InfoIcon,
    /// Optional override for the info-box heading. `None` ⇒
    /// `INFO_HEADING_DEFAULT`.
    pub info_heading: Option<&'a str>,
    /// Optional override for the info-box subtext. `None` ⇒
    /// `INFO_SUBTEXT_DEFAULT`.
    pub info_subtext: Option<&'a str>,
    /// Optional override for the button label. `None` ⇒
    /// `BUTTON_LABEL_DEFAULT`.
    pub button_label: Option<&'a str>,
    /// Optional HBITMAP (cast to `isize`) for the mascot slot. `0` ⇒
    /// fall back to the EXE icon resource.
    pub mascot_hbitmap: isize,
    /// Optional "Check for a newer version" URL. When `Some` and
    /// non-empty, the dialog paints a clickable link below the info
    /// box that opens via `ShellExecuteW(..., "open", url, ...)`.
    /// `None` or empty → the link row is hidden and the URL handler
    /// is never invoked.
    pub update_check_url: Option<&'a str>,
    /// Optional override for the label rendered before the URL
    /// (e.g. `"Check for a newer version:"`). `None` or empty ⇒
    /// the TOML `[launcher.error].update_check_label` (or empty).
    pub update_check_label: Option<&'a str>,
}

/// Show the error dialog modally. Returns `IDOK_I32` (1) on OK.
pub unsafe fn show(parent: HWND, dlg: ErrorDialog<'_>) -> i32 {
    let _ = parent; // Parent unused today; reserved for future owned-window parents.

    register_class();

    let title_w = wide(dlg.title);

    // Resolve info-box / button copy up front: caller override → TOML
    // → module default. Stashing the resolved `String`s in the state
    // means `WM_CREATE` doesn't need access to the borrow `dlg`
    // (which lives on `show`'s stack).
    let dialogs = crate::dialogs::dialogs();
    let info_heading_text: String = dlg
        .info_heading
        .map(String::from)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let from_toml = dialogs.jdk_install.failure.info_heading.clone();
            if from_toml.is_empty() {
                INFO_HEADING_DEFAULT.to_string()
            } else {
                from_toml
            }
        });
    let info_subtext_text: String = dlg
        .info_subtext
        .map(String::from)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let from_toml = dialogs.jdk_install.failure.info_subtext.clone();
            if from_toml.is_empty() {
                INFO_SUBTEXT_DEFAULT.to_string()
            } else {
                from_toml
            }
        });
    let button_label: String = dlg
        .button_label
        .map(String::from)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let from_toml = dialogs.jdk_install.failure.button_label.clone();
            if from_toml.is_empty() {
                BUTTON_LABEL_DEFAULT.to_string()
            } else {
                from_toml
            }
        });
    let heading_text = if dlg.heading.is_empty() {
        dialogs.jdk_install.failure.heading.clone()
    } else {
        dlg.heading.to_string()
    };
    let subheading_text = if dlg.subheading.is_empty() {
        dialogs.jdk_install.failure.subheading.clone()
    } else {
        dlg.subheading.to_string()
    };
    let error_content_text = dlg.error_content.to_string();

    // Resolve link-row fields. Both pieces are independently
    // optional: an empty URL hides the row regardless of label, and
    // an empty label still renders the URL (so callers that
    // pre-format their own label aren't double-printed).
    let link_url_text: String = dlg
        .update_check_url
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_default();
    let link_label_text: String = if link_url_text.is_empty() {
        String::new()
    } else {
        dlg.update_check_label
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| dialogs.launcher.error.update_check_label.clone())
    };

    let state = Box::new(ErrorState {
        heading_text,
        subheading_text,
        error_content_text,
        info_heading_text,
        info_subtext_text,
        button_label,
        link_label_text,
        link_url_text,
        hwnd_heading: std::ptr::null_mut(),
        hwnd_subtitle: std::ptr::null_mut(),
        hwnd_content: std::ptr::null_mut(),
        hwnd_info_icon: std::ptr::null_mut(),
        hwnd_info_heading: std::ptr::null_mut(),
        hwnd_info_subtext: std::ptr::null_mut(),
        hwnd_button: std::ptr::null_mut(),
        hfont_heading: std::ptr::null_mut(),
        hfont_subtitle: std::ptr::null_mut(),
        hfont_content: std::ptr::null_mut(),
        hfont_info_heading: std::ptr::null_mut(),
        hfont_info_subtext: std::ptr::null_mut(),
        hfont_link_url: std::ptr::null_mut(),
        link_url_rect: RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        },
        link_hover: false,
        info_icon_kind: dlg.info_icon,
        mascot_hbitmap: dlg.mascot_hbitmap as i32,
    });
    let state_ptr = Box::into_raw(state);

    // Centre the dialog on the primary monitor. `SM_CXSCREEN` /
    // `SM_CYSCREEN` give the screen size; we offset by half the
    // window's width/height so the dialog appears in the visual
    // centre rather than top-left.
    let sx = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let sy = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    let x = (sx - WINDOW_W) / 2;
    let y = (sy - WINDOW_H) / 2;

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST,
            wide(CLASS_NAME).as_ptr(),
            title_w.as_ptr(),
            // `WS_VISIBLE` is required — without it the window is
            // created but never shown and the user just sees
            // nothing. (Matches `progress_window::show`.)
            WS_CAPTION | WS_SYSMENU | WS_OVERLAPPED | WS_VISIBLE,
            x,
            y,
            WINDOW_W,
            WINDOW_H,
            parent,
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            state_ptr as *const _ as *mut _,
        )
    };
    if hwnd.is_null() {
        log::log("error_window: CreateWindowExW returned NULL — aborting");
        unsafe {
            drop(Box::from_raw(state_ptr));
        }
        return 0;
    }

    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        let r = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if r == 0 || r == -1 {
            break;
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    msg.wParam as i32
}

// `GetSystemMetrics` is imported above as part of the shared windows-sys
// prelude; no extra wrappers needed.

// ============================================================================
//  Per-window state
// ============================================================================

struct ErrorState {
    /// Resolved heading text (caller override → TOML → default).
    heading_text: String,
    /// Resolved subheading text (same resolution order).
    subheading_text: String,
    /// Resolved error content (always supplied by the caller).
    error_content_text: String,
    /// Resolved info-box heading.
    info_heading_text: String,
    /// Resolved info-box subtext.
    info_subtext_text: String,
    /// Resolved button label.
    button_label: String,
    /// Resolved link-row label (e.g. "Check for a newer version:").
    /// Empty when the link row is hidden.
    link_label_text: String,
    /// Resolved link URL. Empty when the link row is hidden.
    link_url_text: String,
    hwnd_heading: HWND,
    hwnd_subtitle: HWND,
    hwnd_content: HWND,
    hwnd_info_icon: HWND,
    hwnd_info_heading: HWND,
    hwnd_info_subtext: HWND,
    hwnd_button: HWND,
    hfont_heading: HFONT,
    hfont_subtitle: HFONT,
    hfont_content: HFONT,
    hfont_info_heading: HFONT,
    hfont_info_subtext: HFONT,
    /// Underlined HFONT used to paint the URL portion of the link row.
    /// `std::ptr::null_mut()` when the link row is hidden.
    hfont_link_url: HFONT,
    /// Right- and bottom-edge of the URL hit-test rect in client
    /// coordinates. Computed once in `WM_CREATE` from the label
    /// width + measured URL width. `(0,0,0,0)` when the link row is
    /// hidden so `PtInRect` short-circuits to false.
    link_url_rect: RECT,
    /// Whether the cursor is currently over the URL portion. Drives
    /// the `IDC_HAND` cursor in `WM_SETCURSOR` and (future) hover
    /// repaint. False when the link row is hidden.
    link_hover: bool,
    info_icon_kind: InfoIcon,
    mascot_hbitmap: i32,
}

// ============================================================================
//  Window class registration (one-time)
// ============================================================================

static CLASS_ATOM: OnceLock<u16> = OnceLock::new();

fn register_class() -> u16 {
    *CLASS_ATOM.get_or_init(|| unsafe {
        let class_name_w = wide(CLASS_NAME);
        let hicon = load_exe_main_icon_hicon().unwrap_or(std::ptr::null_mut());
        let hinst = GetModuleHandleW(std::ptr::null());
        // NULL_BRUSH lets us paint the entire background in WM_PAINT
        // without flicker from the default class brush.
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(error_wndproc),
            cbClsExtra: 0,
            cbWndExtra: std::mem::size_of::<isize>() as i32,
            hInstance: hinst,
            hIcon: hicon,
            hCursor: std::ptr::null_mut(),
            hbrBackground: GetStockObject(NULL_BRUSH) as HBRUSH,
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name_w.as_ptr(),
            hIconSm: hicon,
        };
        RegisterClassExW(&wc) as u16
    })
}

// ============================================================================
//  Window procedure
// ============================================================================

const GWLP_USERDATA: i32 = -21;
const IDC_HEADING: i32 = 2001;
const IDC_SUBTITLE: i32 = 2002;
const IDC_CONTENT: i32 = 2003;
const IDC_INFO_ICON: i32 = 2004;
const IDC_INFO_HEADING: i32 = 2005;
const IDC_INFO_SUBTEXT: i32 = 2006;
const IDC_BUTTON: i32 = 2007;
const IDOK_I32: i32 = 1;
const STATIC_CLASS: &str = "STATIC\0";
const BUTTON_CLASS: &str = "BUTTON\0";

unsafe extern "system" fn error_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => unsafe {
            let create_struct =
                lparam as *const windows_sys::Win32::UI::WindowsAndMessaging::CREATESTRUCTW;
            let state = (*create_struct).lpCreateParams as *mut ErrorState;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);

            let hinst = GetModuleHandleW(std::ptr::null());

            // ----- Fonts -----
            let hfont_heading = create_font_pt(HEADING_PT, HEADING_WEIGHT, FONT_FACE);
            let hfont_subtitle = create_font_pt(SUBTITLE_PT, SUBTITLE_WEIGHT, FONT_FACE);
            let hfont_content = create_font_pt(CONTENT_PT, CONTENT_WEIGHT, FONT_FACE);
            let hfont_info_heading =
                create_font_pt(INFO_HEADING_PT, INFO_HEADING_WEIGHT, FONT_FACE);
            let hfont_info_subtext =
                create_font_pt(INFO_SUBTEXT_PT, INFO_SUBTEXT_WEIGHT, FONT_FACE);
            // Underlined font for the URL portion of the link row.
            // `None` when the row is hidden.
            let hfont_link_url = if !(&(*state).link_url_text).is_empty() {
                create_font_pt_underline(LINK_URL_PT, LINK_URL_WEIGHT, FONT_FACE)
            } else {
                std::ptr::null_mut()
            };

            // Compute the URL hit-test rect when the link row is
            // visible. We use a memory DC + `DrawTextW(DT_CALCRECT)`
            // to measure both halves in pixels: the label paints in
            // `hfont_content` (same metrics as subtitle / body), the
            // URL paints in the underlined HFONT above. `LINK_TEXT_X`
            // + label_width gives the URL's left edge; URL width is
            // added to find the right edge. `LINK_Y` is the text top;
            // bottom is `LINK_Y + tmHeight` (font ascent + descent)
            // — measured via `GetTextMetricsW` on the underlined
            // font. Without this rect the cursor wouldn't change to
            // IDC_HAND and clicks would no-op.
            if !(&(*state).link_url_text).is_empty() {
                let mem_dc = CreateCompatibleDC(std::ptr::null_mut());
                if !mem_dc.is_null() {
                    let prev_label = SelectObject(mem_dc, hfont_content as _);
                    let mut label_rc = RECT {
                        left: 0,
                        top: 0,
                        right: 0,
                        bottom: 0,
                    };
                    let label_str = wide(&(*state).link_label_text);
                    let _ = DrawTextW(
                        mem_dc,
                        label_str.as_ptr(),
                        -1,
                        &mut label_rc,
                        DT_CALCRECT,
                    );
                    let label_w = label_rc.right - label_rc.left;

                    SelectObject(mem_dc, hfont_link_url as _);
                    let mut url_rc = RECT {
                        left: 0,
                        top: 0,
                        right: 0,
                        bottom: 0,
                    };
                    let url_str = wide(&(*state).link_url_text);
                    let _ = DrawTextW(mem_dc, url_str.as_ptr(), -1, &mut url_rc, DT_CALCRECT);
                    let url_w = url_rc.right - url_rc.left;

                    let mut tm: TEXTMETRICW = std::mem::zeroed();
                    GetTextMetricsW(mem_dc, &mut tm);
                    let url_h = tm.tmHeight;

                    SelectObject(mem_dc, prev_label);
                    let _ = DeleteDC(mem_dc);

                    (*state).link_url_rect = RECT {
                        left: LINK_TEXT_X + label_w,
                        top: LINK_Y,
                        right: LINK_TEXT_X + label_w + url_w,
                        bottom: LINK_Y + url_h,
                    };
                }
            }

            // Strings were resolved in `show()` (caller override →
            // TOML → module default) and snapshotted into `state`
            // before `CreateWindowExW`. `WM_CREATE` reads them back
            // here so the WndProc doesn't need access to `dlg`'s
            // borrow.

            // ----- Heading -----
            let hwnd_heading = CreateWindowExW(
                0,
                wide(STATIC_CLASS).as_ptr(),
                wide((*state).heading_text.as_str()).as_ptr(),
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

            // ----- Subheading -----
            let hwnd_subtitle = CreateWindowExW(
                0,
                wide(STATIC_CLASS).as_ptr(),
                wide((*state).subheading_text.as_str()).as_ptr(),
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

            // ----- Error content -----
            let hwnd_content = CreateWindowExW(
                0,
                wide(STATIC_CLASS).as_ptr(),
                wide((*state).error_content_text.as_str()).as_ptr(),
                WS_CHILD | WS_VISIBLE | SS_LEFT,
                TEXT_X,
                CONTENT_Y,
                TEXT_W,
                CONTENT_H,
                hwnd,
                IDC_CONTENT as *mut _,
                hinst,
                std::ptr::null(),
            );
            apply_font(hwnd_content, hfont_content);

            // ----- Info box icon -----
            let hwnd_info_icon = CreateWindowExW(
                0,
                wide(STATIC_CLASS).as_ptr(),
                std::ptr::null(),
                WS_CHILD | WS_VISIBLE | 0x0003, // SS_ICON
                INFO_BOX_X + INFO_PAD,
                INFO_BOX_Y + INFO_ICON_Y_OFFSET,
                INFO_ICON_SIZE,
                INFO_ICON_SIZE,
                hwnd,
                IDC_INFO_ICON as *mut _,
                hinst,
                std::ptr::null(),
            );
            let icon_id = match (*state).info_icon_kind {
                InfoIcon::Info => IDI_INFORMATION,
                InfoIcon::Warning => IDI_WARNING,
                InfoIcon::Error => IDI_ERROR,
            };
            // `STM_SETICON` expects an `HICON` in `wParam`, not the
            // integer resource id. `LoadIconW(NULL, MAKEINTRESOURCE(id))`
            // resolves the standard predefined icon (e.g. `IDI_ERROR`)
            // to a real handle we can pass to the static. Without this
            // step the static is created with a `NULL` text and the
            // integer wParam is silently ignored, leaving the info
            // box empty.
            let icon_handle = LoadIconW(std::ptr::null_mut(), icon_id as *const u16);
            if !icon_handle.is_null() {
                SendMessageW(
                    hwnd_info_icon,
                    STM_SETICON,
                    icon_handle as usize,
                    0,
                );
            }

            // ----- Info heading + subtext -----
            let hwnd_info_heading = CreateWindowExW(
                0,
                wide(STATIC_CLASS).as_ptr(),
                wide((*state).info_heading_text.as_str()).as_ptr(),
                WS_CHILD | WS_VISIBLE | SS_LEFT,
                INFO_TEXT_X,
                INFO_BOX_Y + INFO_HEADING_Y_OFFSET,
                INFO_TEXT_W,
                20,
                hwnd,
                IDC_INFO_HEADING as *mut _,
                hinst,
                std::ptr::null(),
            );
            apply_font(hwnd_info_heading, hfont_info_heading);

            let hwnd_info_subtext = CreateWindowExW(
                0,
                wide(STATIC_CLASS).as_ptr(),
                wide((*state).info_subtext_text.as_str()).as_ptr(),
                WS_CHILD | WS_VISIBLE | SS_LEFT,
                INFO_TEXT_X,
                INFO_BOX_Y + INFO_SUBTEXT_Y_OFFSET,
                INFO_TEXT_W,
                20,
                hwnd,
                IDC_INFO_SUBTEXT as *mut _,
                hinst,
                std::ptr::null(),
            );
            apply_font(hwnd_info_subtext, hfont_info_subtext);

            // ----- Button -----
            let hwnd_button = CreateWindowExW(
                0,
                wide(BUTTON_CLASS).as_ptr(),
                wide((*state).button_label.as_str()).as_ptr(),
                WS_CHILD | WS_VISIBLE | (BS_DEFPUSHBUTTON as u32),
                BUTTON_X,
                BUTTON_Y,
                BUTTON_W,
                BUTTON_H,
                hwnd,
                IDC_BUTTON as *mut _,
                hinst,
                std::ptr::null(),
            );

            // Stash the handles back into the state struct so the
            // rest of the WndProc can reach them via
            // `GetWindowLongPtrW(hwnd, GWLP_USERDATA)`.
            (*state).hwnd_heading = hwnd_heading;
            (*state).hwnd_subtitle = hwnd_subtitle;
            (*state).hwnd_content = hwnd_content;
            (*state).hwnd_info_icon = hwnd_info_icon;
            (*state).hwnd_info_heading = hwnd_info_heading;
            (*state).hwnd_info_subtext = hwnd_info_subtext;
            (*state).hwnd_button = hwnd_button;
            (*state).hfont_heading = hfont_heading;
            (*state).hfont_subtitle = hfont_subtitle;
            (*state).hfont_content = hfont_content;
            (*state).hfont_info_heading = hfont_info_heading;
            (*state).hfont_info_subtext = hfont_info_subtext;
            (*state).hfont_link_url = hfont_link_url;

            // Initial focus on the OK button so Enter dismisses.
            // `SetFocus` is in `Win32::UI::Input::KeyboardAndMouse`.
            windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus(hwnd_button);

            0
        },
        WM_PAINT => unsafe {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);

            // ----- 1. White background -----
            let bg_brush = CreateSolidBrush(COLOR_BG);
            let bg_rc = RECT {
                left: 0,
                top: 0,
                right: WINDOW_W,
                bottom: WINDOW_H,
            };
            FillRect(hdc, &bg_rc, bg_brush);
            DeleteObject(bg_brush as _);

            // ----- 2. Light-blue info box — rounded -----
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

            // ----- 3. Mascot -----
            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ErrorState;
            let mascot_hbitmap = if !raw.is_null() {
                (*raw).mascot_hbitmap
            } else {
                0
            };
            if mascot_hbitmap != 0 {
                draw_mascot_hbitmap(hdc, mascot_hbitmap as _);
            } else if let Some(hicon) =
                find_best_icon_hicon(MASCOT_LOAD_CX, MASCOT_LOAD_CY)
            {
                if DrawIconEx(
                    hdc,
                    MASCOT_X,
                    MASCOT_Y,
                    hicon,
                    MASCOT_W,
                    MASCOT_H,
                    0,
                    std::ptr::null_mut(),
                    DI_NORMAL,
                ) == 0
                {
                    log::log("error_window WM_PAINT: DrawIconEx failed");
                }
            } else {
                log::log("error_window WM_PAINT: no usable EXE icon found");
            }

            // ----- 4. Link row (optional) -----
            // Painted below the info box when the launcher was given a
            // non-empty `update_check_url`. The label paints in
            // regular grey, the URL paints in blue + underline. Both
            // halves sit inside `link_url_rect` for hit-testing; see
            // `WM_LBUTTONDOWN` for the click handler.
            //
            // Rust 2024 forbids auto-ref through a raw pointer
            // deref, so we read the relevant `String`s + `RECT` +
            // `HFONT`s into locals inside one `unsafe` block, then
            // use the locals freely for the rest of the paint arm.
            let link_paint: Option<(String, String, RECT, isize, isize)> =
                if raw.is_null() || (&(*raw).link_url_text).is_empty() {
                    None
                } else {
                    Some((
                        (*raw).link_label_text.clone(),
                        (*raw).link_url_text.clone(),
                        (*raw).link_url_rect,
                        (*raw).hfont_content as isize,
                        (*raw).hfont_link_url as isize,
                    ))
                };
            if let Some((label_text, url_text, url_rect, hfont_content, hfont_link_url)) =
                link_paint
            {
                SetBkMode(hdc, TRANSPARENT as i32);

                // Label (regular).
                let label_str = wide(&label_text);
                let prev_font = SelectObject(hdc, hfont_content as _);
                SetTextColor(hdc, COLOR_SUBTITLE);
                let mut label_rc = RECT {
                    left: LINK_TEXT_X,
                    top: LINK_Y,
                    right: url_rect.left,
                    bottom: LINK_Y + LINK_H,
                };
                let label_flags = DT_SINGLELINE | DT_NOPREFIX;
                let _ = DrawTextW(
                    hdc,
                    label_str.as_ptr(),
                    -1,
                    &mut label_rc,
                    label_flags,
                );

                // URL (blue, underlined).
                SelectObject(hdc, hfont_link_url as _);
                SetTextColor(hdc, COLOR_LINK);
                let url_str = wide(&url_text);
                let mut url_paint_rc = RECT {
                    left: url_rect.left,
                    top: LINK_Y,
                    right: url_rect.right,
                    bottom: LINK_Y + LINK_H,
                };
                let _ = DrawTextW(
                    hdc,
                    url_str.as_ptr(),
                    -1,
                    &mut url_paint_rc,
                    label_flags,
                );

                SelectObject(hdc, prev_font as _);
                // (Color resets in WM_CTLCOLORSTATIC later in the
                // paint cycle; leaving the DC in link-paint state
                // here is harmless because no further GDI calls use
                // the text colour.)
            }

            EndPaint(hwnd, &ps);
            0
        },
        WM_CTLCOLORSTATIC => unsafe {
            let hdc: HDC = wparam as HDC;
            let hwnd_child = lparam as HWND;
            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ErrorState;
            if !raw.is_null() && hwnd_child != (*raw).hwnd_info_icon {
                // Subtitle + content + info-heading use the lighter
                // grey text colour; info-subtext stays default black.
                let grey_text = hwnd_child == (*raw).hwnd_subtitle
                    || hwnd_child == (*raw).hwnd_content;
                if grey_text {
                    SetTextColor(hdc, COLOR_SUBTITLE);
                } else if hwnd_child == (*raw).hwnd_content {
                    SetTextColor(hdc, COLOR_CONTENT);
                }
                SetBkMode(hdc, TRANSPARENT as i32);
                return GetStockObject(NULL_BRUSH) as LRESULT;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        },
        WM_SETCURSOR => unsafe {
            // Cursor handling for the link row. If the cursor is over
            // the URL portion of the link, switch to the system hand
            // cursor (`IDC_HAND`); otherwise let the default arrow
            // stand.
            //
            // Returning `1` from `WM_SETCURSOR` tells Windows we've
            // set the cursor ourselves and it shouldn't run the
            // default cursor-for-window handler.
            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ErrorState;
            let link_visible =
                !raw.is_null() && !(&(*raw).link_url_text).is_empty();
            if link_visible {
                // The cursor is in our client area iff the LOWORD of
                // `lparam` is `HTCLIENT`.
                let ht = (lparam & 0xFFFF) as u16;
                if ht == 1 /* HTCLIENT */ {
                    let cursor = LoadCursorW(std::ptr::null_mut(), IDC_HAND);
                    if !cursor.is_null() {
                        SetCursor(cursor);
                    }
                    return 1;
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_MOUSEMOVE => unsafe {
            // Track whether the cursor is over the URL hit-test rect
            // so `WM_SETCURSOR` can flip to IDC_HAND. We don't paint
            // a hover effect today; reserved for future "underline
            // darkens" tweak.
            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ErrorState;
            let link_visible =
                !raw.is_null() && !(&(*raw).link_url_text).is_empty();
            if !link_visible {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            // `GET_X_Y_PARAM` macro: x in LOWORD, y in HIWORD.
            let x = (wparam & 0xFFFF) as i16 as i32;
            let y = ((wparam >> 16) & 0xFFFF) as i16 as i32;
            let pt = POINT { x, y };
            let (rect, was_hover) =
                ((*raw).link_url_rect, (*raw).link_hover);
            let is_hover = windows_sys::Win32::Graphics::Gdi::PtInRect(
                &rect, pt,
            ) != 0;
            if is_hover != was_hover {
                (*raw).link_hover = is_hover;
                // Force a SETCURSOR cycle so the cursor flips
                // immediately on entry / exit.
                let cursor = if is_hover {
                    LoadCursorW(std::ptr::null_mut(), IDC_HAND)
                } else {
                    LoadCursorW(std::ptr::null_mut(), IDC_ARROW)
                };
                if !cursor.is_null() {
                    SetCursor(cursor);
                }
            }
            0
        }
        WM_LBUTTONDOWN => unsafe {
            // Click handler for the URL. Open in the user's default
            // browser via `ShellExecuteW(..., "open", url, ...)` —
            // same path the Windows shell uses for hyperlinks in
            // `SysLink` controls. `ShellExecuteW` is best-effort: a
            // non-zero return value > 32 means success; anything
            // else is an error code we log and silently ignore.
            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ErrorState;
            let url_text: Option<String> = if raw.is_null()
                || (&(*raw).link_url_text).is_empty()
            {
                None
            } else {
                Some((*raw).link_url_text.clone())
            };
            let Some(url_text) = url_text else {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            };
            let x = (wparam & 0xFFFF) as i16 as i32;
            let y = ((wparam >> 16) & 0xFFFF) as i16 as i32;
            let pt = POINT { x, y };
            let rect = (*raw).link_url_rect;
            let over_link =
                windows_sys::Win32::Graphics::Gdi::PtInRect(&rect, pt) != 0;
            if over_link {
                let url_w = wide(&url_text);
                log::log(&format!(
                    "error_window: opening update URL: {url_text}"
                ));
                let rc = ShellExecuteW(
                    std::ptr::null_mut(),
                    wide("open").as_ptr(),
                    url_w.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    SW_SHOWNORMAL,
                );
                // `ShellExecuteW` returns `HINSTANCE`; values > 32
                // mean success, lower values are error codes.
                let rc = rc as isize;
                if rc <= 32 {
                    log::log(&format!(
                        "error_window: ShellExecuteW failed (code {rc})"
                    ));
                }
            }
            0
        }
        WM_COMMAND => {
            let id = (wparam as u32) & 0xFFFF;
            if id == IDC_BUTTON as u32 || id == IDCANCEL as u32 {
                unsafe {
                    PostQuitMessage(IDOK_I32 as i32);
                }
            }
            0
        }
        WM_CLOSE => {
            // X button / Alt+F4 / system "Close" command. Treat as a
            // dismissal equivalent to clicking OK — `DefWindowProcW`
            // would only `DestroyWindow`, which leaves the
            // message loop in `show()` spinning forever waiting
            // for a `WM_QUIT` that never arrives.
            unsafe {
                PostQuitMessage(IDOK_I32 as i32);
            }
            0
        }
        WM_NCDESTROY => unsafe {
            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ErrorState;
            if !raw.is_null() {
                if !(*raw).hfont_heading.is_null() {
                    DeleteObject((*raw).hfont_heading as _);
                }
                if !(*raw).hfont_subtitle.is_null() {
                    DeleteObject((*raw).hfont_subtitle as _);
                }
                if !(*raw).hfont_content.is_null() {
                    DeleteObject((*raw).hfont_content as _);
                }
                if !(*raw).hfont_info_heading.is_null() {
                    DeleteObject((*raw).hfont_info_heading as _);
                }
                if !(*raw).hfont_info_subtext.is_null() {
                    DeleteObject((*raw).hfont_info_subtext as _);
                }
                if !(*raw).hfont_link_url.is_null() {
                    DeleteObject((*raw).hfont_link_url as _);
                }
                drop(Box::from_raw(raw));
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

// ============================================================================
//  Helpers
// ============================================================================

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn apply_font(hwnd: HWND, hfont: HFONT) {
    unsafe {
        SendMessageW(hwnd, WM_SETFONT, hfont as usize, 1);
    }
}

fn create_font_pt(pt: i32, weight: i32, face: &str) -> HFONT {
    // `-pt` because Win32 `CreateFontW` height is in *pixels* when
    // `lfHeight < 0`; the mapping from points is
    // `-pt * GetDeviceCaps(LOGPIXELSY) / 72`. `GetDeviceCaps` is
    // heavyweight — pass `-MulDiv(pt, 96, 72)` to get a 96-DPI
    // value, then let the system scale via DPI awareness. The
    // resulting HFONT reads at 1×96 DPI on a 96-DPI monitor and
    // scales proportionally on HiDPI.
    let h = -((pt * 96) / 72);
    let face_w = wide(face);
    unsafe {
        CreateFontW(
            h,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            0, // DEFAULT_CHARSET — windows-sys constant is `0`.
            0,
            0,
            0, // DEFAULT_QUALITY — windows-sys constant is `0`.
            0, // DEFAULT_PITCH | FF_DONTCARE — both `0`.
            face_w.as_ptr(),
        )
    }
}

/// Same as [`create_font_pt`] but with the underline bit set on
/// `lfUnderline`. Used for the URL portion of the optional link row.
fn create_font_pt_underline(pt: i32, weight: i32, face: &str) -> HFONT {
    let h = -((pt * 96) / 72);
    let face_w = wide(face);
    unsafe {
        CreateFontW(
            h,
            0,
            0,
            0,
            weight,
            0,
            1, // lfUnderline = TRUE
            0,
            0,
            0,
            0,
            0,
            0,
            face_w.as_ptr(),
        )
    }
}

/// Stretch-draw an existing DIB section (`HBITMAP`) into the mascot
/// slot. Mirrors `progress_window::draw_mascot_hbitmap`; duplicated
/// here so each module stays self-contained.
unsafe fn draw_mascot_hbitmap(hdc: HDC, hbitmap: isize) {
    use windows_sys::Win32::Graphics::Gdi::{
        BITMAP, StretchDIBits, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
    };
    let mut bm: BITMAP = unsafe { std::mem::zeroed() };
    let bm_ok = unsafe {
        GetObjectW(
            hbitmap as _,
            std::mem::size_of::<BITMAP>() as i32,
            &mut bm as *mut _ as *mut std::ffi::c_void,
        )
    };
    if bm_ok == 0 {
        log::log("error_window mascot: GetObjectW failed");
        return;
    }
    let w = bm.bmWidth as i32;
    let h = bm.bmHeight.unsigned_abs() as i32;

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: [unsafe { std::mem::zeroed() }; 1],
    };
    let _ = unsafe {
        StretchDIBits(
            hdc,
            MASCOT_X,
            MASCOT_Y,
            MASCOT_W,
            MASCOT_H,
            0,
            0,
            w,
            h,
            std::ptr::null(),
            &bmi,
            DIB_RGB_COLORS,
            0x00CC0020, /* SRCCOPY */
        )
    };
}

// Silence the unused-import warning for `DestroyWindow` and friends
// that are pulled in via the shared `windows_sys::Win32::UI::WindowsAndMessaging`
// prelude but only used in some arms of the WndProc.
#[allow(dead_code)]
fn _unused() {
    let _ = DestroyWindow;
    let _ = SS_ICON;
    let _ = SS_LEFT;
    let _ = SS_LEFTNOWORDWRAP;
}