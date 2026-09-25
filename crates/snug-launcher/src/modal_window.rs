//! Unified custom-painted Win32 modal dialog. Replaces the four
//! near-identical dialogs that previously lived in `error_window.rs`,
//! `retry_window.rs`, `metadata_failed_window.rs`, and (partially)
//! `progress_window.rs`. All four now construct a [`ModalDialog`]
//! and call [`show`].
//!
//! Layout (640×320) is the canonical mockup-aligned shape used
//! everywhere in the launcher:
//!
//! ```text
//! ┌───────────────────────────────────────────────────────────────┐
//! │ [icon] Dialog title                                  — □ ✕      │ ← title bar (system)
//! │                                                                │
//! │  ┌──────────────┐  Heading (large bold)                       │
//! │  │              │  Subheading (grey)                           │
//! │  │   [mascot]   │                                               │
//! │  │              │  Content (multi-line)                         │
//! │  │              │                                               │
//! │  │              │                                               │
//! │  └──────────────┘                                               │
//! │  ┌──────────────────────────────────────┐  ┌────────────┐       │
//! │  │ ⓘ  Info heading                      │  │  Primary   │       │
//! │  │     Info subtext                     │  │  Secondary │       │
//! │  └──────────────────────────────────────┘  └────────────┘       │
//! │  [optional "Check for a newer version:" link]                    │
//! └───────────────────────────────────────────────────────────────┘
//! ```
//!
//! **One static class** (`"snug_modal_dialog_v1\0"`) is registered
//! lazily on first use and shared by every dialog instance. A single
//! [`wndproc`] branches on the per-window `state.buttons.len()` and
//! `state.link_url` to decide what to create in `WM_CREATE` and what
//! to paint in `WM_PAINT`.
//!
//! **One font helper** [`create_font_pt`] (with an `underline: bool`
//! variant) drives every text element. **One icon helper**
//! [`load_info_icon`] returns `IDI_INFORMATION` / `IDI_WARNING` /
//! `IDI_ERROR` via `LoadIconW`. **One mascot path** falls back to the
//! EXE main icon when `mascot_hbitmap == 0`.

use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleDC, CreateFontW, CreateRoundRectRgn, CreateSolidBrush, DeleteDC,
    DeleteObject, DrawTextW, EndPaint, FillRect, FillRgn, FW_BOLD, FW_NORMAL, FW_SEMIBOLD,
    GetStockObject, GetTextMetricsW, HBRUSH, HDC, HFONT, NULL_BRUSH, PAINTSTRUCT, SelectObject,
    SetBkMode, SetTextColor, TEXTMETRICW, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow, DispatchMessageW, DrawIconEx, DI_NORMAL,
    GetMessageW, GetSystemMetrics, GetWindowLongPtrW, HICON, IDCANCEL, IDI_ERROR,
    IDI_INFORMATION, IDI_WARNING, LoadCursorW, LoadIconW, MSG, PostQuitMessage,
    RegisterClassExW, SendMessageW, SetCursor, SetWindowLongPtrW, SM_CXSCREEN, SM_CYSCREEN,
    TranslateMessage, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, WM_CLOSE, WM_COMMAND, WM_CREATE,
    WM_CTLCOLORSTATIC, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_NCDESTROY, WM_PAINT, WM_SETCURSOR,
    WNDCLASSEXW, WS_CAPTION, WS_CHILD, WS_EX_TOPMOST, WS_OVERLAPPED, WS_SYSMENU, WS_VISIBLE,
};

use crate::jdk_install::{find_best_icon_hicon, load_exe_main_icon_hicon};
use crate::log;

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
// `IDC_ARROW` is `MAKEINTRESOURCE(32512)`. Restored on hover-exit.
const IDC_ARROW: *const u16 = 32512 as *const u16;
// `SW_SHOWNORMAL` per winuser.h / shellapi.h.
const SW_SHOWNORMAL: i32 = 1;
// `DT_*` constants used by `DrawTextW`. Values from winuser.h.
const DT_CALCRECT: u32 = 0x0000_0004;
const DT_SINGLELINE: u32 = 0x0000_0020;
const DT_NOPREFIX: u32 = 0x0000_0800;

// ============================================================================
//  Result codes
// ============================================================================

/// Returned when the user activates the **primary** action (or the
/// only button). Same numeric value as Win32 `IDYES` so existing
/// callers can compare against `windows_sys::Win32::UI::WindowsAndMessaging::IDYES`.
pub const IDYES_I32: i32 = 6;

/// Returned when the user activates the **secondary** action, clicks
/// the X button, presses Alt+F4, or any other dismissal. Same
/// numeric value as Win32 `IDCANCEL`.
pub const IDCANCEL_I32: i32 = 2;

// ============================================================================
//  Layout constants — single source of truth
// ============================================================================

const CLASS_NAME: &str = "snug_modal_dialog_v1\0";
const WINDOW_W: i32 = 640;
const WINDOW_H: i32 = 320;

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

/// Primary button width when paired. Matches `error_window` 80 px.
const BUTTON_PRIMARY_W_PAIR: i32 = 100;
const BUTTON_SECONDARY_W_PAIR: i32 = 80;
const BUTTON_SOLO_W: i32 = 80;
const BUTTON_H: i32 = 25;
const BUTTON_GAP: i32 = INFO_PAD;
const BUTTON_SECONDARY_X: i32 = WINDOW_W - MARGIN * 2 - BUTTON_SECONDARY_W_PAIR;
const BUTTON_PRIMARY_X_PAIR: i32 =
    BUTTON_SECONDARY_X - BUTTON_GAP - BUTTON_PRIMARY_W_PAIR;
const BUTTON_SOLO_X: i32 = WINDOW_W - MARGIN * 2 - BUTTON_SOLO_W;
const BUTTON_Y: i32 = INFO_BOX_Y + (INFO_BOX_H - BUTTON_H) / 2;

/// Optional "Check for a newer version" link row, painted below the
/// info box. Reserved only when [`ModalDialog::link_url`] is `Some`
/// and non-empty. The URL is opened via
/// `ShellExecuteW(..., "open", url, ...)`.
const LINK_Y: i32 = 288;
const LINK_H: i32 = 20;
const LINK_TEXT_X: i32 = MARGIN;

/// Width/height we ask for when loading the EXE icon for the mascot
/// slot. Asking for 256 selects a detailed source for the 154 px
/// mascot box rather than enlarging the 32 px title-bar icon.
const MASCOT_LOAD_CX: i32 = 256;
const MASCOT_LOAD_CY: i32 = 256;

// ============================================================================
//  Fonts (per-element)
// ============================================================================

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

// URL portion of the link row — bold + underline. The label portion
// reuses the body font (`hfont_content`).
const LINK_URL_PT: i32 = 9;
const LINK_URL_WEIGHT: i32 = FW_SEMIBOLD as i32;

// ============================================================================
//  Colours (COLORREF = 0x00BBGGRR)
// ============================================================================

const COLOR_BG: u32 = 0x00FFFFFF;
const COLOR_SUBTITLE: u32 = 0x005F6368;
const COLOR_CONTENT: u32 = 0x00303030;
const COLOR_INFO_BG: u32 = 0x00FEF0E8;
// Link URL colour — standard hyperlink blue.
const COLOR_LINK: u32 = 0x00E8731A;

// ============================================================================
//  Corner rounding
// ============================================================================

const INFO_BOX_CORNER_DIAMETER: i32 = INFO_BOX_H / 4;

// ============================================================================
//  Default copy (used when caller passes `None`)
// ============================================================================

const INFO_HEADING_DEFAULT: &str = "What happened?";
const INFO_SUBTEXT_DEFAULT: &str =
    "You can try again, or visit the download page manually.";
const BUTTON_LABEL_DEFAULT: &str = "OK";

// ============================================================================
//  Public API
// ============================================================================

/// Which icon to draw inside the info box. `Info` for informational
/// (ⓘ), `Warning` for transient failures (⚠), `Error` for terminal
/// failures (❌).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InfoIcon {
    Info,
    Warning,
    Error,
}

/// A button on the dialog.
///
/// [`Button::Primary`] is the left button (or the only button); Enter
/// activates it. [`Button::Secondary`] sits to its right; Esc and the
/// X button dismiss via it. Up to two buttons today.
#[derive(Clone, Copy)]
pub enum Button<'a> {
    Primary(&'a str),
    Secondary(&'a str),
}

/// Inputs to [`show`]. Caller fills the text fields + button list;
/// optional info / link overrides default to taste.
pub struct ModalDialog<'a> {
    /// Title-bar text.
    pub title: &'a str,
    /// Heading — large bold line at the top of the dialog body.
    pub heading: &'a str,
    /// Subheading — smaller grey line below the heading.
    pub subheading: &'a str,
    /// Body text — multi-line, fills the CONTENT_Y slot.
    pub content: &'a str,
    /// Icon to draw inside the info box (and painted nowhere else).
    pub info_icon: InfoIcon,
    /// Info-box heading. `None` ⇒ [`INFO_HEADING_DEFAULT`].
    pub info_heading: Option<&'a str>,
    /// Info-box subtext. `None` ⇒ [`INFO_SUBTEXT_DEFAULT`].
    pub info_subtext: Option<&'a str>,
    /// Button list. One or two entries; the first is always
    /// [`Button::Primary`].
    pub buttons: &'a [Button<'a>],
    /// Optional HBITMAP (cast to `isize`) for the mascot slot. `0` ⇒
    /// fall back to the EXE icon resource.
    pub mascot_hbitmap: isize,
    /// Optional "Check for a newer version" URL. When `Some` and
    /// non-empty, paints a clickable link below the info box that
    /// opens via `ShellExecuteW(..., "open", url, ...)`. `None` or
    /// empty → the link row is hidden.
    pub link_url: Option<&'a str>,
    /// Optional override for the label rendered before the URL
    /// (e.g. "Check for a newer version:"). `None` ⇒ the TOML
    /// `[launcher.error].update_check_label` (or empty when the
    /// TOML is unset).
    pub link_label: Option<&'a str>,
}

/// Show the modal dialog. Returns [`IDYES_I32`] when the user
/// activates the primary button (or the only button), [`IDCANCEL_I32`]
/// when they activate the secondary, click X, or press Alt+F4.
///
/// TOML fallback for the link-label (when `link_url` is non-empty
/// and `link_label` is `None`) is resolved here from
/// `[launcher.error].update_check_label` — that's the cross-cutting
/// label the launcher renders across the family. Per-dialog info /
/// button fallbacks are resolved by each wrapper module since each
/// reads a different TOML table (`failure` / `retry` /
/// `metadata_failed`).
pub unsafe fn show<'a>(parent: HWND, dlg: &'a ModalDialog<'a>) -> i32 {
    let _ = parent; // Reserved for future owned-window parents.

    register_class();

    let title_w = wide(dlg.title);

    // Resolve link-row fields up front so WM_CREATE doesn't need
    // access to the borrow `dlg`. An empty URL hides the row
    // regardless of label; an empty label still renders the URL
    // (callers that pre-formatted their own label aren't
    // double-printed).
    let link_url_text: String = dlg
        .link_url
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_default();
    let link_label_text: String = if link_url_text.is_empty() {
        String::new()
    } else {
        dlg.link_label
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                crate::dialogs::dialogs()
                    .launcher
                    .error
                    .update_check_label
                    .clone()
            })
    };

    // Resolve info / button labels (caller → TOML → default).
    let info_heading_text: String = dlg
        .info_heading
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| INFO_HEADING_DEFAULT.to_string());
    let info_subtext_text: String = dlg
        .info_subtext
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| INFO_SUBTEXT_DEFAULT.to_string());

    let state = Box::new(State {
        heading_text: dlg.heading.to_string(),
        subheading_text: dlg.subheading.to_string(),
        content_text: dlg.content.to_string(),
        info_heading_text,
        info_subtext_text,
        buttons: dlg
            .buttons
            .iter()
            .map(|b| match b {
                Button::Primary(s) => (true, (*s).to_string()),
                Button::Secondary(s) => (false, (*s).to_string()),
            })
            .collect(),
        link_label_text,
        link_url_text,
        hwnd_heading: std::ptr::null_mut(),
        hwnd_subtitle: std::ptr::null_mut(),
        hwnd_content: std::ptr::null_mut(),
        hwnd_info_icon: std::ptr::null_mut(),
        hwnd_info_heading: std::ptr::null_mut(),
        hwnd_info_subtext: std::ptr::null_mut(),
        hwnd_button_primary: std::ptr::null_mut(),
        hwnd_button_secondary: std::ptr::null_mut(),
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
        mascot_hicon: std::ptr::null_mut(),
    });
    let state_ptr = Box::into_raw(state);

    // Centre on the primary monitor.
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
        log::log("modal_window: CreateWindowExW returned NULL — aborting");
        unsafe {
            drop(Box::from_raw(state_ptr));
        }
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

/// Re-exported for the wrappers — keeps callers that used
/// `error_window::show`'s default label from breaking when the
/// button label is empty.
pub fn default_button_label() -> &'static str {
    BUTTON_LABEL_DEFAULT
}

// ============================================================================
//  Per-window state
// ============================================================================

/// Per-window state passed via `lpCreateParams` and recovered through
/// `GWLP_USERDATA`. Owned by the window — `Box::from_raw` in
/// `WM_NCDESTROY` along with its HFONT handles.
///
/// `buttons` is `(is_primary, label)` so the WndProc can dispatch
/// `WM_COMMAND` correctly without re-parsing the public `Button`
/// enum.
struct State {
    /// Resolved heading text (caller-supplied).
    heading_text: String,
    /// Resolved subheading text (caller-supplied).
    subheading_text: String,
    /// Resolved body content (caller-supplied).
    content_text: String,
    /// Resolved info-box heading (caller → default).
    info_heading_text: String,
    /// Resolved info-box subtext (caller → default).
    info_subtext_text: String,
    /// Resolved buttons: `(is_primary, label)`. Always at least one.
    buttons: Vec<(bool, String)>,
    /// Resolved link-row label. Empty when the link row is hidden.
    link_label_text: String,
    /// Resolved link URL. Empty when the link row is hidden.
    link_url_text: String,

    hwnd_heading: HWND,
    hwnd_subtitle: HWND,
    hwnd_content: HWND,
    hwnd_info_icon: HWND,
    hwnd_info_heading: HWND,
    hwnd_info_subtext: HWND,
    hwnd_button_primary: HWND,
    /// `std::ptr::null_mut()` when there's only one button.
    hwnd_button_secondary: HWND,

    hfont_heading: HFONT,
    hfont_subtitle: HFONT,
    hfont_content: HFONT,
    hfont_info_heading: HFONT,
    hfont_info_subtext: HFONT,
    /// Underlined HFONT used to paint the URL portion of the link row.
    /// `std::ptr::null_mut()` when the link row is hidden.
    hfont_link_url: HFONT,
    /// Right- and bottom-edge of the URL hit-test rect in client
    /// coordinates. `(0,0,0,0)` when the link row is hidden.
    link_url_rect: RECT,
    /// Whether the cursor is currently over the URL. Drives the
    /// `IDC_HAND` cursor in `WM_SETCURSOR`.
    link_hover: bool,
    info_icon_kind: InfoIcon,
    mascot_hbitmap: i32,
    /// Cached `HICON` for the mascot slot — loaded **once** in
    /// `WM_CREATE` (when the caller didn't supply an `HBITMAP`) and
    /// reused across every `WM_PAINT`. `std::ptr::null_mut()` when
    /// the caller pushed a bitmap (we never need the icon then) or
    /// when icon loading failed at startup.
    ///
    /// Caching is the whole point: `find_best_icon_hicon` walks
    /// `FindResourceW → LoadResource → CreateIconFromResourceEx`,
    /// which used to spam the log at ~20 Hz during the progress-bar
    /// animation (the old code reloaded on every paint).
    mascot_hicon: HICON,
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
            lpfnWndProc: Some(wndproc),
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
// Control IDs. Stable per-dispatcher — WM_COMMAND uses these.
const IDC_HEADING: i32 = 5001;
const IDC_SUBTITLE: i32 = 5002;
const IDC_CONTENT: i32 = 5003;
const IDC_INFO_ICON: i32 = 5004;
const IDC_INFO_HEADING: i32 = 5005;
const IDC_INFO_SUBTEXT: i32 = 5006;
const IDC_BUTTON_PRIMARY: i32 = 5007;
const IDC_BUTTON_SECONDARY: i32 = 5008;

const STATIC_CLASS: &str = "STATIC\0";
const BUTTON_CLASS: &str = "BUTTON\0";

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => unsafe {
            let create_struct =
                lparam as *const windows_sys::Win32::UI::WindowsAndMessaging::CREATESTRUCTW;
            let state = (*create_struct).lpCreateParams as *mut State;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);

            let hinst = GetModuleHandleW(std::ptr::null());

            // ----- Fonts -----
            let hfont_heading = create_font_pt(HEADING_PT, HEADING_WEIGHT, false);
            let hfont_subtitle = create_font_pt(SUBTITLE_PT, SUBTITLE_WEIGHT, false);
            let hfont_content = create_font_pt(CONTENT_PT, CONTENT_WEIGHT, false);
            let hfont_info_heading =
                create_font_pt(INFO_HEADING_PT, INFO_HEADING_WEIGHT, false);
            let hfont_info_subtext =
                create_font_pt(INFO_SUBTEXT_PT, INFO_SUBTEXT_WEIGHT, false);
            let hfont_link_url = if !(&(*state).link_url_text).is_empty() {
                create_font_pt(LINK_URL_PT, LINK_URL_WEIGHT, true)
            } else {
                std::ptr::null_mut()
            };

            // Compute the URL hit-test rect when the link row is
            // visible. We use a memory DC + `DrawTextW(DT_CALCRECT)`
            // to measure both halves in pixels: the label paints in
            // `hfont_content` (same metrics as subtitle / body), the
            // URL paints in the underlined HFONT. `LINK_TEXT_X` +
            // label_width gives the URL's left edge; URL width is
            // added to find the right edge. `LINK_Y` is the text
            // top; bottom is `LINK_Y + tmHeight`.
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

            // ----- Mascot icon (cached for paint) -----
            // Only load the EXE-icon fallback when the caller didn't
            // supply an `HBITMAP`. Doing this here, **once**, rather
            // than inside `WM_PAINT`, keeps the
            // `FindResourceW → LoadResource → CreateIconFromResourceEx`
            // pipeline from running at every repaint — the old code
            // spammed the log at ~20 Hz during the progress-bar
            // animation.
            if (*state).mascot_hbitmap == 0 {
                if let Some(hicon) = find_best_icon_hicon(MASCOT_LOAD_CX, MASCOT_LOAD_CY) {
                    (*state).mascot_hicon = hicon;
                }
            }

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

            // ----- Content -----
            let hwnd_content = CreateWindowExW(
                0,
                wide(STATIC_CLASS).as_ptr(),
                wide((*state).content_text.as_str()).as_ptr(),
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

            // ----- Info icon -----
            let hwnd_info_icon = CreateWindowExW(
                0,
                wide(STATIC_CLASS).as_ptr(),
                std::ptr::null(),
                WS_CHILD | WS_VISIBLE | SS_ICON,
                INFO_BOX_X + INFO_PAD,
                INFO_BOX_Y + INFO_ICON_Y_OFFSET,
                INFO_ICON_SIZE,
                INFO_ICON_SIZE,
                hwnd,
                IDC_INFO_ICON as *mut _,
                hinst,
                std::ptr::null(),
            );
            // `STM_SETICON` expects an `HICON` in `wParam`, not the
            // integer resource id. `LoadIconW(NULL, MAKEINTRESOURCE(id))`
            // resolves the standard predefined icon to a real handle.
            let icon_handle = load_info_icon((*state).info_icon_kind);
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

            // ----- Buttons -----
            let buttons_len = (&(*state).buttons).len();
            let (hwnd_primary, hwnd_secondary) = match buttons_len {
                1 => {
                    let (is_primary, label) = (&(*state).buttons)[0].clone();
                    let _ = is_primary;
                    let hwnd_button = CreateWindowExW(
                        0,
                        wide(BUTTON_CLASS).as_ptr(),
                        wide(label.as_str()).as_ptr(),
                        WS_CHILD | WS_VISIBLE | (BS_DEFPUSHBUTTON as u32),
                        BUTTON_SOLO_X,
                        BUTTON_Y,
                        BUTTON_SOLO_W,
                        BUTTON_H,
                        hwnd,
                        IDC_BUTTON_PRIMARY as *mut _,
                        hinst,
                        std::ptr::null(),
                    );
                    (hwnd_button, std::ptr::null_mut())
                }
                _ => {
                    // Two buttons (the only other supported size today).
                    let (is_primary_0, label_0) = (&(*state).buttons)[0].clone();
                    let (is_primary_1, label_1) = (&(*state).buttons)[1].clone();
                    let _ = (is_primary_0, is_primary_1);
                    let hwnd_primary = CreateWindowExW(
                        0,
                        wide(BUTTON_CLASS).as_ptr(),
                        wide(label_0.as_str()).as_ptr(),
                        WS_CHILD | WS_VISIBLE | (BS_DEFPUSHBUTTON as u32),
                        BUTTON_PRIMARY_X_PAIR,
                        BUTTON_Y,
                        BUTTON_PRIMARY_W_PAIR,
                        BUTTON_H,
                        hwnd,
                        IDC_BUTTON_PRIMARY as *mut _,
                        hinst,
                        std::ptr::null(),
                    );
                    let hwnd_secondary = CreateWindowExW(
                        0,
                        wide(BUTTON_CLASS).as_ptr(),
                        wide(label_1.as_str()).as_ptr(),
                        WS_CHILD | WS_VISIBLE | (BS_PUSHBUTTON as u32),
                        BUTTON_SECONDARY_X,
                        BUTTON_Y,
                        BUTTON_SECONDARY_W_PAIR,
                        BUTTON_H,
                        hwnd,
                        IDC_BUTTON_SECONDARY as *mut _,
                        hinst,
                        std::ptr::null(),
                    );
                    (hwnd_primary, hwnd_secondary)
                }
            };

            // Stash handles back into the state struct so the rest
            // of the WndProc can reach them via
            // `GetWindowLongPtrW(hwnd, GWLP_USERDATA)`.
            (*state).hwnd_heading = hwnd_heading;
            (*state).hwnd_subtitle = hwnd_subtitle;
            (*state).hwnd_content = hwnd_content;
            (*state).hwnd_info_icon = hwnd_info_icon;
            (*state).hwnd_info_heading = hwnd_info_heading;
            (*state).hwnd_info_subtext = hwnd_info_subtext;
            (*state).hwnd_button_primary = hwnd_primary;
            (*state).hwnd_button_secondary = hwnd_secondary;
            (*state).hfont_heading = hfont_heading;
            (*state).hfont_subtitle = hfont_subtitle;
            (*state).hfont_content = hfont_content;
            (*state).hfont_info_heading = hfont_info_heading;
            (*state).hfont_info_subtext = hfont_info_subtext;
            (*state).hfont_link_url = hfont_link_url;

            // Initial focus on the primary button so Enter
            // activates it.
            windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus(hwnd_primary);

            0
        },
        WM_PAINT => unsafe {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);

            // 1. White background.
            let bg_brush = CreateSolidBrush(COLOR_BG);
            let bg_rc = RECT {
                left: 0,
                top: 0,
                right: WINDOW_W,
                bottom: WINDOW_H,
            };
            FillRect(hdc, &bg_rc, bg_brush);
            DeleteObject(bg_brush as _);

            // 2. Light-blue info box — rounded.
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

            // 3. Mascot. Bitmap if non-zero, else EXE main icon (cached
            //    on State during WM_CREATE — see State::mascot_hicon).
            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
            let (mascot_hbitmap, mascot_hicon) = if !raw.is_null() {
                ((*raw).mascot_hbitmap, (*raw).mascot_hicon)
            } else {
                (0, std::ptr::null_mut())
            };
            if mascot_hbitmap != 0 {
                draw_mascot_hbitmap(hdc, mascot_hbitmap as _);
            } else if !mascot_hicon.is_null() {
                if DrawIconEx(
                    hdc,
                    MASCOT_X,
                    MASCOT_Y,
                    mascot_hicon,
                    MASCOT_W,
                    MASCOT_H,
                    0,
                    std::ptr::null_mut(),
                    DI_NORMAL,
                ) == 0
                {
                    log::log("modal_window WM_PAINT: DrawIconEx failed");
                }
            } else {
                log::log("modal_window WM_PAINT: no usable EXE icon found");
            }

            // 4. Optional link row.
            //
            // Rust 2024 forbids auto-ref through a raw pointer
            // deref, so we read the relevant fields into locals
            // inside one `unsafe` block, then use the locals freely.
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
            }

            EndPaint(hwnd, &ps);
            0
        },
        WM_CTLCOLORSTATIC => unsafe {
            let hdc: HDC = wparam as HDC;
            let hwnd_child = lparam as HWND;
            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
            if !raw.is_null() && hwnd_child != (*raw).hwnd_info_icon {
                // Subtitle + content use the lighter grey text colour;
                // everything else (heading, info-heading, info-subtext)
                // keeps the default black.
                let grey_text = hwnd_child == (*raw).hwnd_subtitle;
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
            // Cursor handling for the link row. If the cursor is
            // over the URL portion of the link, switch to the
            // system hand cursor (`IDC_HAND`); otherwise let the
            // default arrow stand.
            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
            let link_visible =
                !raw.is_null() && !(&(*raw).link_url_text).is_empty();
            if link_visible {
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
            // Track whether the cursor is over the URL hit-test
            // rect so `WM_SETCURSOR` can flip to IDC_HAND. No
            // hover-paint effect today; reserved for future
            // "underline darkens" tweak.
            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
            let link_visible =
                !raw.is_null() && !(&(*raw).link_url_text).is_empty();
            if !link_visible {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
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
            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
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
                    "modal_window: opening update URL: {url_text}"
                ));
                let rc = ShellExecuteW(
                    std::ptr::null_mut(),
                    wide("open").as_ptr(),
                    url_w.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    SW_SHOWNORMAL,
                );
                let rc = rc as isize;
                if rc <= 32 {
                    log::log(&format!(
                        "modal_window: ShellExecuteW failed (code {rc})"
                    ));
                }
            }
            0
        }
        WM_COMMAND => {
            let id = (wparam as u32) & 0xFFFF;
            if id == IDC_BUTTON_PRIMARY as u32 {
                unsafe {
                    PostQuitMessage(IDYES_I32);
                }
            } else if id == IDC_BUTTON_SECONDARY as u32 || id == IDCANCEL as u32 {
                unsafe {
                    PostQuitMessage(IDCANCEL_I32);
                }
            }
            0
        }
        WM_CLOSE => {
            // X button / Alt+F4. Treat as dismissal equivalent to
            // the secondary button — `DefWindowProcW` would only
            // `DestroyWindow`, which leaves the message loop in
            // `show()` spinning forever waiting for `WM_QUIT`.
            unsafe {
                PostQuitMessage(IDCANCEL_I32);
            }
            0
        }
        WM_NCDESTROY => unsafe {
            let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
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
                if !(*raw).mascot_hicon.is_null() {
                    DestroyIcon((*raw).mascot_hicon);
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

/// Win32 `CreateFontW` with a point-size + weight + face. `underline`
/// sets the `lfUnderline` bit — used for the URL portion of the
/// optional link row.
fn create_font_pt(pt: i32, weight: i32, underline: bool) -> HFONT {
    // `-pt * 96 / 72` converts points to a 96-DPI pixel height. The
    // resulting HFONT reads at 1×96 DPI on a 96-DPI monitor and
    // scales proportionally on HiDPI (the dialog is system-DPI-aware
    // via the manifest).
    let h = -((pt * 96) / 72);
    let face_w = wide(FONT_FACE);
    let lf_underline = if underline { 1 } else { 0 };
    unsafe {
        CreateFontW(
            h,
            0,
            0,
            0,
            weight,
            0,
            lf_underline,
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

/// Resolve an [`InfoIcon`] to a system predefined `HICON`. Returns
/// the `IDI_INFORMATION` / `IDI_WARNING` / `IDI_ERROR` standard icon.
fn load_info_icon(kind: InfoIcon) -> HICON {
    let id = match kind {
        InfoIcon::Info => IDI_INFORMATION,
        InfoIcon::Warning => IDI_WARNING,
        InfoIcon::Error => IDI_ERROR,
    };
    unsafe { LoadIconW(std::ptr::null_mut(), id as *const u16) }
}

/// Stretch-draw an existing DIB section (`HBITMAP`) into the mascot
/// slot. Mirrors `progress_window::draw_mascot_hbitmap`; the modal
/// path always uses the icon fallback so this is the only place we
/// need the bitmap branch today.
unsafe fn draw_mascot_hbitmap(hdc: HDC, hbitmap: isize) {
    use windows_sys::Win32::Graphics::Gdi::{
        BITMAP, StretchDIBits, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
    };
    let mut bm: BITMAP = unsafe { std::mem::zeroed() };
    let bm_ok = unsafe {
        windows_sys::Win32::Graphics::Gdi::GetObjectW(
            hbitmap as _,
            std::mem::size_of::<BITMAP>() as i32,
            &mut bm as *mut _ as *mut std::ffi::c_void,
        )
    };
    if bm_ok == 0 {
        log::log("modal_window mascot: GetObjectW failed");
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
}