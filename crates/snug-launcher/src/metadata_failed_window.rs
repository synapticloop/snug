//! Custom Win32 "couldn't reach Adoptium" dialog — same
//! mockup-aligned layout as [`error_window`] and
//! [`progress_window`], but with **two buttons** (Open in
//! browser + Cancel) and a warning info-icon.
//!
//! Layout (640×320):
//!
//! ```text
//! ┌───────────────────────────────────────────────────────────────┐
//! │ [icon] Could not reach Adoptium                  — □ ✕       │
//! │                                                                │
//! │  ┌──────────────┐  Heading (large bold)                       │
//! │  │              │  Subheading (grey)                           │
//! │  │   [mascot]   │                                               │
//! │  │              │  Error content (multi-line)                  │
//! │  │              │                                               │
//! │  │              │                                               │
//! │  └──────────────┘                                               │
//! │  ┌──────────────────────────────────────┐  ┌────────────┐       │
//! │  │ ⚠  Why did this happen?              │  │ Open in   │       │
//! │  │    See the message above for…          │  │ browser    │       │
//! │  └──────────────────────────────────────┘  │   Cancel   │       │
//! │                                            └────────────┘       │
//! └───────────────────────────────────────────────────────────────┘
//! ```
//!
//! The two buttons sit at the right of the info box — the
//! primary "Open in browser" action on the left, the
//! secondary "Cancel" action on the right. The primary
//! button is `BS_DEFPUSHBUTTON` so Enter activates it.

use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateRoundRectRgn, CreateSolidBrush, DeleteObject, EndPaint,
    FillRect, FillRgn, FW_BOLD, FW_NORMAL, FW_SEMIBOLD, GetObjectW, GetStockObject, HBRUSH,
    HDC, HFONT, NULL_BRUSH, PAINTSTRUCT, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, DrawIconEx, DI_NORMAL, GetMessageW,
    GetSystemMetrics, GetWindowLongPtrW, IDCANCEL, IDI_INFORMATION, IDI_WARNING, LoadIconW,
    PostQuitMessage, RegisterClassExW, SendMessageW, SetWindowLongPtrW, TranslateMessage,
    BS_DEFPUSHBUTTON, BS_PUSHBUTTON, MSG, SM_CXSCREEN, SM_CYSCREEN, WM_CLOSE, WM_COMMAND,
    WM_CREATE, WM_CTLCOLORSTATIC, WM_NCDESTROY, WM_PAINT, WNDCLASSEXW, WS_CAPTION, WS_CHILD,
    WS_EX_TOPMOST, WS_OVERLAPPED, WS_SYSMENU, WS_VISIBLE,
};

use crate::jdk_install::{find_best_icon_hicon, load_exe_main_icon_hicon};
use crate::log;

// ============================================================================
//  EDITABLE CONSTANTS — tweak these to retune the dialog
// ============================================================================

// -- Window / class --------------------------------------------------------

const CLASS_NAME: &str = "snug_metadata_failed_dialog_v1\0";
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

// -- Two-button cluster ----------------------------------------------------

/// Width of the primary action button ("Open in browser").
const BUTTON_PRIMARY_W: i32 = 120;
/// Width of the secondary action button ("Cancel").
const BUTTON_SECONDARY_W: i32 = 80;
const BUTTON_H: i32 = 26;
/// Gap between the two buttons. `INFO_PAD * 2` matches the spacing
/// between the info box and the right window edge, keeping the
/// cluster visually balanced.
const BUTTON_GAP: i32 = INFO_PAD;
/// Rightmost button X (the secondary "Cancel"). The primary button
/// sits to its left.
const BUTTON_SECONDARY_X: i32 = WINDOW_W - MARGIN * 2 - BUTTON_SECONDARY_W;
const BUTTON_PRIMARY_X: i32 = BUTTON_SECONDARY_X - BUTTON_GAP - BUTTON_PRIMARY_W;
const BUTTON_Y: i32 = INFO_BOX_Y + (INFO_BOX_H - BUTTON_H) / 2;

// -- Mascot ----------------------------------------------------------------

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

// -- Colours (COLORREF = 0x00BBGGRR) ---------------------------------------

const COLOR_BG: u32 = 0x00FFFFFF;
const COLOR_SUBTITLE: u32 = 0x005F6368;
const COLOR_CONTENT: u32 = 0x00303030;
const COLOR_INFO_BG: u32 = 0x00FEF0E8;

// -- Corner rounding -------------------------------------------------------

const INFO_BOX_CORNER_DIAMETER: i32 = INFO_BOX_H / 4;

// ============================================================================
//  Public API
// ============================================================================

/// Inputs to `show`. Caller fills the four text fields; the mascot
/// hbitmap is optional (`0` falls back to the EXE-icon resource).
pub struct MetadataFailedDialog<'a> {
    pub title: &'a str,
    pub heading: &'a str,
    pub subheading: &'a str,
    pub error_content: &'a str,
    /// Optional override for the info-box heading. `None` ⇒
    /// `[jdk_install.metadata_failed].info_heading` from `dialogs.toml`.
    pub info_heading: Option<&'a str>,
    /// Optional override for the info-box subtext. `None` ⇒ TOML.
    pub info_subtext: Option<&'a str>,
    /// Label for the primary button (left of the pair).
    pub primary_label: &'a str,
    /// Label for the secondary button (rightmost).
    pub secondary_label: &'a str,
    /// Optional HBITMAP (cast to `isize`) for the mascot slot.
    pub mascot_hbitmap: isize,
}

/// Result of `show` — which button the user pressed.
///
/// `IDYES_I32` (6) = primary action (Open in browser). `IDCANCEL_I32`
/// (2) = secondary action. The launcher calls `open_in_browser(...)`
/// on `IDYES_I32` and aborts the install on `IDCANCEL_I32`.
pub const IDYES_I32: i32 = 6;
pub const IDCANCEL_I32: i32 = 2;

/// Show the dialog modally. Returns the button id the user pressed.
pub unsafe fn show(parent: HWND, dlg: MetadataFailedDialog<'_>) -> i32 {
    let _ = parent;

    register_class();

    let title_w = wide(dlg.title);

    // Resolve strings: caller override → TOML → module default.
    let dialogs = crate::dialogs::dialogs();
    let info_heading_text: String = dlg
        .info_heading
        .map(String::from)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let from_toml = dialogs.jdk_install.metadata_failed.info_heading.clone();
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
            let from_toml = dialogs.jdk_install.metadata_failed.info_subtext.clone();
            if from_toml.is_empty() {
                INFO_SUBTEXT_DEFAULT.to_string()
            } else {
                from_toml
            }
        });
    let heading_text = if dlg.heading.is_empty() {
        dialogs.jdk_install.metadata_failed.heading.clone()
    } else {
        dlg.heading.to_string()
    };
    let subheading_text = if dlg.subheading.is_empty() {
        dialogs.jdk_install.metadata_failed.subheading.clone()
    } else {
        dlg.subheading.to_string()
    };
    let error_content_text = dlg.error_content.to_string();
    let primary_label = dlg.primary_label.to_string();
    let secondary_label = dlg.secondary_label.to_string();

    let state = Box::new(MfState {
        heading_text,
        subheading_text,
        error_content_text,
        info_heading_text,
        info_subtext_text,
        primary_label,
        secondary_label,
        hwnd_heading: std::ptr::null_mut(),
        hwnd_subtitle: std::ptr::null_mut(),
        hwnd_content: std::ptr::null_mut(),
        hwnd_info_icon: std::ptr::null_mut(),
        hwnd_info_heading: std::ptr::null_mut(),
        hwnd_info_subtext: std::ptr::null_mut(),
        hwnd_primary: std::ptr::null_mut(),
        hwnd_secondary: std::ptr::null_mut(),
        hfont_heading: std::ptr::null_mut(),
        hfont_subtitle: std::ptr::null_mut(),
        hfont_content: std::ptr::null_mut(),
        hfont_info_heading: std::ptr::null_mut(),
        hfont_info_subtext: std::ptr::null_mut(),
        mascot_hbitmap: dlg.mascot_hbitmap as i32,
    });
    let state_ptr = Box::into_raw(state);

    let sx = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let sy = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    let x = (sx - WINDOW_W) / 2;
    let y = (sy - WINDOW_H) / 2;

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST,
            wide(CLASS_NAME).as_ptr(),
            title_w.as_ptr(),
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
        log::log("metadata_failed_window: CreateWindowExW returned NULL — aborting");
        unsafe { drop(Box::from_raw(state_ptr)); }
        return IDCANCEL_I32;
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

// -- Default info strings (used when TOML has empty values) ----------------

const INFO_HEADING_DEFAULT: &str = "Why did this happen?";
const INFO_SUBTEXT_DEFAULT: &str =
    "Your network connection may be down, or Adoptium's API may be temporarily unreachable.";

// ============================================================================
//  Per-window state
// ============================================================================

struct MfState {
    heading_text: String,
    subheading_text: String,
    error_content_text: String,
    info_heading_text: String,
    info_subtext_text: String,
    primary_label: String,
    secondary_label: String,
    hwnd_heading: HWND,
    hwnd_subtitle: HWND,
    hwnd_content: HWND,
    hwnd_info_icon: HWND,
    hwnd_info_heading: HWND,
    hwnd_info_subtext: HWND,
    hwnd_primary: HWND,
    hwnd_secondary: HWND,
    hfont_heading: HFONT,
    hfont_subtitle: HFONT,
    hfont_content: HFONT,
    hfont_info_heading: HFONT,
    hfont_info_subtext: HFONT,
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
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(mf_wndproc),
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
const IDC_HEADING: i32 = 3001;
const IDC_SUBTITLE: i32 = 3002;
const IDC_CONTENT: i32 = 3003;
const IDC_INFO_ICON: i32 = 3004;
const IDC_INFO_HEADING: i32 = 3005;
const IDC_INFO_SUBTEXT: i32 = 3006;
const IDC_PRIMARY: i32 = 3007;
const IDC_SECONDARY: i32 = 3008;
const IDOK_I32: i32 = 1;
const STM_SETICON: u32 = 0x0170;
const WM_SETFONT: u32 = 0x0030;
const STATIC_CLASS: &str = "STATIC\0";
const BUTTON_CLASS: &str = "BUTTON\0";
const SS_LEFT: u32 = 0x0000;
const SS_LEFTNOWORDWRAP: u32 = 0x000C;

unsafe extern "system" fn mf_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => unsafe {
            let create_struct =
                lparam as *const windows_sys::Win32::UI::WindowsAndMessaging::CREATESTRUCTW;
            let state = (*create_struct).lpCreateParams as *mut MfState;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);

            let hinst = GetModuleHandleW(std::ptr::null());

            let hfont_heading = create_font_pt(HEADING_PT, HEADING_WEIGHT, FONT_FACE);
            let hfont_subtitle = create_font_pt(SUBTITLE_PT, SUBTITLE_WEIGHT, FONT_FACE);
            let hfont_content = create_font_pt(CONTENT_PT, CONTENT_WEIGHT, FONT_FACE);
            let hfont_info_heading =
                create_font_pt(INFO_HEADING_PT, INFO_HEADING_WEIGHT, FONT_FACE);
            let hfont_info_subtext =
                create_font_pt(INFO_SUBTEXT_PT, INFO_SUBTEXT_WEIGHT, FONT_FACE);

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

            // ----- Info icon (warning — yellow ⚠) -----
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
            let icon_handle = LoadIconW(std::ptr::null_mut(), IDI_WARNING as *const u16);
            if !icon_handle.is_null() {
                SendMessageW(hwnd_info_icon, STM_SETICON, icon_handle as usize, 0);
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

            // ----- Primary button (left of the pair, "Open in browser") -----
            let hwnd_primary = CreateWindowExW(
                0,
                wide(BUTTON_CLASS).as_ptr(),
                wide((*state).primary_label.as_str()).as_ptr(),
                WS_CHILD | WS_VISIBLE | (BS_DEFPUSHBUTTON as u32),
                BUTTON_PRIMARY_X,
                BUTTON_Y,
                BUTTON_PRIMARY_W,
                BUTTON_H,
                hwnd,
                IDC_PRIMARY as *mut _,
                hinst,
                std::ptr::null(),
            );

            // ----- Secondary button (rightmost, "Cancel") -----
            let hwnd_secondary = CreateWindowExW(
                0,
                wide(BUTTON_CLASS).as_ptr(),
                wide((*state).secondary_label.as_str()).as_ptr(),
                WS_CHILD | WS_VISIBLE | (BS_PUSHBUTTON as u32),
                BUTTON_SECONDARY_X,
                BUTTON_Y,
                BUTTON_SECONDARY_W,
                BUTTON_H,
                hwnd,
                IDC_SECONDARY as *mut _,
                hinst,
                std::ptr::null(),
            );

            (*state).hwnd_heading = hwnd_heading;
            (*state).hwnd_subtitle = hwnd_subtitle;
            (*state).hwnd_content = hwnd_content;
            (*state).hwnd_info_icon = hwnd_info_icon;
            (*state).hwnd_info_heading = hwnd_info_heading;
            (*state).hwnd_info_subtext = hwnd_info_subtext;
            (*state).hwnd_primary = hwnd_primary;
            (*state).hwnd_secondary = hwnd_secondary;
            (*state).hfont_heading = hfont_heading;
            (*state).hfont_subtitle = hfont_subtitle;
            (*state).hfont_content = hfont_content;
            (*state).hfont_info_heading = hfont_info_heading;
            (*state).hfont_info_subtext = hfont_info_subtext;

            // Initial focus on the primary button so Enter
            // activates "Open in browser" and Esc dismisses via
            // the secondary.
            windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus(hwnd_primary);

            0
        },
        WM_PAINT => unsafe {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);

            let bg_brush = CreateSolidBrush(COLOR_BG);
            let bg_rc = RECT {
                left: 0,
                top: 0,
                right: WINDOW_W,
                bottom: WINDOW_H,
            };
            FillRect(hdc, &bg_rc, bg_brush);
            DeleteObject(bg_brush as _);

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

            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut MfState;
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
                    log::log("metadata_failed_window WM_PAINT: DrawIconEx failed");
                }
            } else {
                log::log("metadata_failed_window WM_PAINT: no usable EXE icon found");
            }

            EndPaint(hwnd, &ps);
            0
        },
        WM_CTLCOLORSTATIC => unsafe {
            let hdc: HDC = wparam as HDC;
            let hwnd_child = lparam as HWND;
            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut MfState;
            if !raw.is_null() && hwnd_child != (*raw).hwnd_info_icon {
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
        WM_COMMAND => {
            let id = (wparam as u32) & 0xFFFF;
            if id == IDC_PRIMARY as u32 {
                unsafe { PostQuitMessage(IDYES_I32); }
            } else if id == IDC_SECONDARY as u32 || id == IDCANCEL as u32 {
                unsafe { PostQuitMessage(IDCANCEL_I32); }
            }
            0
        }
        WM_CLOSE => {
            // X button / Alt+F4. Treat as dismissal equivalent to
            // clicking the secondary button — `DefWindowProcW`
            // would only call `DestroyWindow` and the message
            // loop in `show()` would spin forever. Pressing X
            // means "I don't want to do either action", which
            // matches the Cancel / IDCANCEL semantic.
            unsafe {
                PostQuitMessage(IDCANCEL_I32);
            }
            0
        }
        WM_NCDESTROY => unsafe {
            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut MfState;
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
            0, // DEFAULT_CHARSET
            0,
            0,
            0, // DEFAULT_QUALITY
            0, // DEFAULT_PITCH | FF_DONTCARE
            face_w.as_ptr(),
        )
    }
}

/// Same drawing routine as `progress_window` / `error_window`.
/// Duplicated rather than shared so each module is self-contained.
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
        log::log("metadata_failed_window mascot: GetObjectW failed");
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

#[allow(dead_code)]
fn _unused() {
    let _ = IDI_INFORMATION;
    let _ = IDOK_I32;
}