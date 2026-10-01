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
//! │ [icon] Dialog title                                  — □ ✕    │ ← title bar (system)
//! │                                                               │
//! │  ┌──────────────┐  Heading (large bold)                       │
//! │  │              │  Subheading (grey)                          │
//! │  │   [mascot]   │                                             │
//! │  │              │  Content (multi-line)                       │
//! │  │              │  [Show details]   ← only when expanded_*    │
//! │  │              │  [expanded body]  ← present when toggled on │
//! │  │              │  [clickable URL]  ← only when expanded_link │
//! │  └──────────────┘                                             │
//! │  ┌──────────────────────────────────────┐  ┌────────────┐     │
//! │  │ (i)  Info heading                    │  │ Tertiary   │     │
//! │  │      Info subtext                    │  │ Secondary  │     │
//! │  │                                      │  │  Primary   │     │
//! │  └──────────────────────────────────────┘  └────────────┘     │
//! │  [optional "Check for a newer version:" link]                 │
//! └───────────────────────────────────────────────────────────────┘
//! ```
//!
//! Supports 1, 2, or 3 buttons via [`Button::Primary`],
//! [`Button::Secondary`], [`Button::Tertiary`]. Buttons are
//! right-aligned: primary is rightmost (default, Enter), tertiary is
//! leftmost (treats Esc / X the same way). One-button dialogs use
//! the solo position; two-button dialogs get primary + secondary.
//!
//! **Expand/collapse.** A caller can supply optional
//! [`ModalDialog::expanded_content`] (and optional
//! [`ModalDialog::expanded_link_url`]). When present, a "Show
//! details" hyperlink appears below the regular content. Clicking
//! it toggles the expanded body in/out of the content area. The
//! clickable URL, when present, is rendered as a brand-blue
//! underlined link at the bottom of the expanded body and opens via
//! `ShellExecuteW(..., "open", url, ...)`.
//!
//! **One static class** (`"snug_modal_dialog_v1\0"`) is registered
//! lazily on first use and shared by every dialog instance. A single
//! [`wndproc`] branches on the per-window `state.buttons.len()` and
//! `state.expanded_content` to decide what to create in `WM_CREATE`
//! and what to paint in `WM_PAINT`.
//!
//! **One font helper** [`create_font_pt`] (with an `underline: bool`
//! variant) drives every text element. **One icon helper**
//! [`load_info_icon`] returns `IDI_INFORMATION` / `IDI_WARNING` /
//! `IDI_ERROR` via `LoadIconW`. **One mascot path** falls back to the
//! EXE main icon when `mascot_hbitmap == 0`.

use std::sync::OnceLock;

use windows_sys::Win32::Foundation::SIZE;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleDC, CreateFontW, CreateRoundRectRgn, CreateSolidBrush, DeleteDC,
    DeleteObject, DrawTextW, DT_EDITCONTROL, DT_WORDBREAK, EndPaint, FillRect, FillRgn, GetTextExtentPoint32W,
    FW_BOLD, FW_NORMAL,
    FW_SEMIBOLD,
    GetStockObject, GetTextMetricsW, HBRUSH, HDC, HFONT, NULL_BRUSH, PAINTSTRUCT, SelectObject,
    SetBkMode, SetTextColor, TEXTMETRICW, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    DrawIconEx, DI_NORMAL,
    GetMessageW, GetSystemMetrics, GetWindowLongPtrW, HICON, IDCANCEL, IDI_ERROR,
    IDI_INFORMATION, IDI_WARNING, LoadCursorW, LoadIconW, MSG, PostQuitMessage,
    RegisterClassExW, SendMessageW, SetCursor, SetWindowLongPtrW, SetWindowPos, SM_CXSCREEN,
    SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOZORDER,
    TranslateMessage, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, WM_CLOSE, WM_COMMAND, WM_CREATE,
    WM_CTLCOLORSTATIC, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_NCDESTROY, WM_PAINT,
    WM_SETCURSOR,
    WNDCLASSEXW, WS_CAPTION, WS_CHILD, WS_EX_TOPMOST, WS_OVERLAPPED, WS_SYSMENU, WS_VISIBLE,
};

use crate::jdk_install::{find_best_icon_hicon, load_exe_main_icon_hicon, mascot_icon_override};
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
const DT_SINGLELINE: u32 = 0x0000_0020;
const DT_NOPREFIX: u32 = 0x0000_0800;

// ============================================================================
//  Result codes
// ============================================================================

/// Returned when the user activates the **primary** action (or the
/// only button). Same numeric value as Win32 `IDYES` so existing
/// callers can compare against `windows_sys::Win32::UI::WindowsAndMessaging::IDYES`.
pub const IDYES_I32: i32 = 6;

/// Returned when the user activates the **secondary** action. Only
/// possible when three buttons are present (primary / secondary /
/// tertiary). Same numeric value as Win32 `IDNO`.
pub const IDNO_I32: i32 = 7;

/// Returned when the user activates the **tertiary** action, clicks
/// the X button, presses Alt+F4, or any other dismissal. In a
/// 2-button dialog the secondary action returns this code (so
/// `retry_window` / `metadata_failed_window` callers can keep
/// branching on `IDYES_I32` vs `IDCANCEL_I32` exactly as before).
/// Same numeric value as Win32 `IDCANCEL`.
pub const IDCANCEL_I32: i32 = 2;

// ============================================================================
//  Layout constants — single source of truth
// ============================================================================

const CLASS_NAME: &str = "snug_modal_dialog_v1\0";
const WINDOW_W: i32 = 640;
/// Height of the **client** area, and the same value the whole dialog
/// family uses (`progress_window` mirrors this stack exactly).
///
/// Derived upward from the info box rather than hardcoded, because the
/// whole point of the redesign is that the two gaps below the box are
/// equal: `INFO_BOX_Y` is the anchor both modules share, then the box,
/// `BOTTOM_PAD`, the button row, and `BOTTOM_PAD` again. Change any
/// one of those and the window follows.
const WINDOW_H: i32 = INFO_BOX_Y + INFO_BOX_H + BOTTOM_PAD + BUTTON_H + BOTTOM_PAD;

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
/// Symmetric: the box used to be `WINDOW_W - MARGIN * 3`, which left
/// twice the left margin as whitespace on the right (16 px in, 32 px
/// out) because the buttons used to sit inside the box's band. With the
/// buttons moved to their own row the extra reserve has no owner.
const INFO_BOX_W: i32 = WINDOW_W - MARGIN * 2;
const INFO_BOX_Y: i32 = 220;
/// Height of the light-blue info box. Sized for the heading plus
/// **two** subtext lines: `INFO_HEADING_Y_OFFSET` .. plus two rows of
/// `INFO_SUBTEXT_H`, with `INFO_PAD` top and bottom. `WINDOW_H` is
/// derived from this, so growing the box here grows the dialog.
const INFO_BOX_H: i32 = 74;
const INFO_PAD: i32 = 8;
const INFO_ICON_SIZE: i32 = 16;

const INFO_ICON_Y_OFFSET: i32 = INFO_PAD + 2;
const INFO_HEADING_Y_OFFSET: i32 = INFO_PAD - 2;
const INFO_SUBTEXT_Y_OFFSET: i32 = INFO_PAD + 20;
/// Height reserved for one subtext row. The second line sits directly
/// below the first, so both are laid out from these two constants
/// rather than from a second hand-picked Y.
const INFO_SUBTEXT_H: i32 = 16;
const INFO_SUBTEXT2_Y_OFFSET: i32 = INFO_SUBTEXT_Y_OFFSET + INFO_SUBTEXT_H + 2;
const INFO_TEXT_X: i32 = INFO_BOX_X + INFO_PAD + INFO_ICON_SIZE + 28;
const INFO_TEXT_W: i32 = INFO_BOX_W - (INFO_TEXT_X - INFO_BOX_X) - INFO_PAD;

/// Uniform padding around the bottom of the content stack. Used
/// **twice** on purpose: once between the info box and the button row,
/// and once between the button row and the bottom of the window. That
/// is what makes the two gaps provably equal rather than equal by
/// hand-tuning two numbers that would drift apart.
const BOTTOM_PAD: i32 = 16;

/// Buttons sit in their own row *below* the info box, right-aligned.
/// Widths are measured from each label (see `measure_button_width`)
/// rather than hardcoded, so a translated label of any length fits
/// without a width table to maintain.
/// Button captions get a real font of their own rather than the
/// system default. That is not cosmetic: sizing a button from the
/// stock GUI font while it *renders* in the themed UI font
/// under-measures by ~15%, which clipped the longest labels. Applying
/// and measuring the same HFONT makes the two agree by construction.
const BUTTON_PT: i32 = 9;
const BUTTON_WEIGHT: i32 = FW_NORMAL as i32;
const BUTTON_H: i32 = 25;
/// Symmetric horizontal padding inside a button, added to the measured
/// text width.
const BUTTON_PAD_X: i32 = 20;
/// Floor, so a two-letter label like `OK` still looks like a button
/// rather than a sliver.
const BUTTON_MIN_W: i32 = 80;
const BUTTON_GAP: i32 = INFO_PAD;
/// Right edge of the button group, and the leftward step between
/// buttons.
const BUTTON_RIGHT: i32 = WINDOW_W - MARGIN;
const BUTTON_Y: i32 = INFO_BOX_Y + INFO_BOX_H + BOTTOM_PAD;

/// Optional "Check for a newer version" link row, painted below the
/// info box. Reserved only when [`ModalDialog::link_url`] is `Some`
/// and non-empty. The URL is opened via
/// `ShellExecuteW(..., "open", url, ...)`.
///
/// It shares the `BOTTOM_PAD` gap between the box and the buttons, so
/// under the mascot: this is the one dialog in the family
/// with a link, the row is single-line, and the buttons are pinned to
/// the window bottom, so the link has to fit the gap rather than
/// claim a row of its own.
/// Optional "Check for a newer version" block, painted as **two
/// stacked rows under the mascot**: the label, then the URL beneath it.
/// The URL is opened via `ShellExecuteW(..., "open", url, ...)`.
///
/// Only the URL row is clickable; the label is plain text.
///
/// `LINK_URL_W` is the mascot's width, and it is a real constraint
/// rather than a preference: the error-content control occupies
/// `CONTENT_Y .. CONTENT_Y + CONTENT_H` (108..196) in the column to
/// the right, and this block starts at y=170. Letting the URL run wider
/// than the mascot would push it into that control, so it wraps inside
/// the mascot column instead -- which also keeps it visually grouped
/// under the artwork. The label sits on its own row at the content
/// font; it measures ~165 px, which just fits the inter-column gap
/// before `TEXT_X` (186), so it needs no clipping of its own.
const LINK_TEXT_X: i32 = MARGIN;
/// The label gets the full content width rather than the mascot
/// column: at the content font "Check for a newer version:" measures
/// ~180 px, so capping it at the mascot width (154 px) clipped it
/// mid-word and capping it at the inter-column gap (170 px) clipped
/// it too. The URL below it still wraps inside the mascot column --
/// see `LINK_URL_W`.
const LINK_LABEL_W: i32 = WINDOW_W - MARGIN * 2;
const LINK_LABEL_Y: i32 = MASCOT_Y + MASCOT_H;
const LINK_LABEL_H: i32 = INFO_SUBTEXT_H;
const LINK_URL_Y: i32 = LINK_LABEL_Y + LINK_LABEL_H;
const LINK_URL_H: i32 = INFO_SUBTEXT_H * 2;
const LINK_URL_W: i32 = MASCOT_W;

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
// Link URL colour — brand blue (`#1A73E8`), identical to
// `COLOR_PROGRESS_FILL`. We deliberately reuse the brand colour
// for the "Check for a newer version" link row rather than
// introducing a second accent so the dialog reads as a single
// chromatic family.
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
/// [`Button::Primary`] is the rightmost (default, Enter); in a
/// 1-button dialog it's the only button and sits at the solo
/// position. [`Button::Secondary`] sits to its left when present.
/// [`Button::Tertiary`] sits to the left of the secondary; Esc, the
/// X button, and Alt+F4 all dismiss as the tertiary (or as the
/// secondary in a 2-button dialog, or as the primary in a
/// 1-button dialog). Up to three buttons total.
#[derive(Clone, Copy)]
pub enum Button<'a> {
    Primary(&'a str),
    Secondary(&'a str),
    Tertiary(&'a str),
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
    /// Optional **third** line in the info box, printed under
    /// `info_subtext`. `None` or empty skips creating the control
    /// entirely, so a dialog that does not want the line pays nothing
    /// beyond the taller box.
    pub info_subtext_2: Option<&'a str>,
    /// Button list. One, two, or three entries; the first is always
    /// [`Button::Primary`], the last is always either
    /// [`Button::Secondary`] (2-button) or [`Button::Tertiary`]
    /// (3-button).
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
    // --- Expand/collapse + content-area link support removed:
    //     the modal_window family is now strictly 1-3 button +
    //     optional link row. The install prompt lives in
    //     `prompt_window` and handles its own content-area link
    //     + expandable body.
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
    // Unlike the two above, the third line has no module default: it
    // is genuinely optional, and an absent key must leave the control
    // uncreated rather than inventing copy a dialog never asked for.
    let info_subtext2_text: String = dlg
        .info_subtext_2
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .unwrap_or_default();

    let state = Box::new(State {
        heading_text: dlg.heading.to_string(),
        subheading_text: dlg.subheading.to_string(),
        content_text: dlg.content.to_string(),
        info_heading_text,
        info_subtext_text,
        info_subtext2_text,
        buttons: dlg
            .buttons
            .iter()
            .map(|b| match b {
                Button::Primary(s) => (true, (*s).to_string()),
                Button::Secondary(s) => (false, (*s).to_string()),
                Button::Tertiary(s) => (false, (*s).to_string()),
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
        hwnd_info_subtext2: std::ptr::null_mut(),
        hwnd_button_primary: std::ptr::null_mut(),
        hwnd_button_secondary: std::ptr::null_mut(),
        hwnd_button_tertiary: std::ptr::null_mut(),
        hfont_heading: std::ptr::null_mut(),
        hfont_subtitle: std::ptr::null_mut(),
        hfont_content: std::ptr::null_mut(),
        hfont_button: std::ptr::null_mut(),
        hfont_info_heading: std::ptr::null_mut(),
        hfont_info_subtext: std::ptr::null_mut(),
        hfont_info_subtext2: std::ptr::null_mut(),
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

    // WINDOW_W / WINDOW_H describe the **client** area, like every
    // other constant in this file. The styles below carry a caption,
    // so the size handed to CreateWindowExW has to be the expanded
    // window rect -- otherwise the title bar (~31 px) silently comes
    // out of the client height and the bottom of the stack (the info
    // box, then the update-check link below it) is clipped. That was
    // invisible while the box was 50 px tall; growing it for a third
    // line pushed its last row off the client area. Same conversion
    // snug_preview already does for its launcher window.
    let mut client = RECT {
        left: 0,
        top: 0,
        right: WINDOW_W,
        bottom: WINDOW_H,
    };
    unsafe {
        AdjustWindowRectEx(
            &mut client,
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
            0,
            WS_EX_TOPMOST,
        );
    }
    let win_w = client.right - client.left;
    let win_h = client.bottom - client.top;

    // Centre on the primary monitor, using the *window* size -- that
    // is what the user actually sees on screen.
    let sx = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let sy = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    let x = (sx - win_w) / 2;
    let y = (sy - win_h) / 2;

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST,
            wide(CLASS_NAME).as_ptr(),
            title_w.as_ptr(),
            WS_CAPTION | WS_SYSMENU | WS_OVERLAPPED | WS_VISIBLE,
            x,
            y,
            win_w,
            win_h,
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
    /// Third info-box line. Empty when the caller passed none,
    /// which is what suppresses the control in WM_CREATE.
    info_subtext2_text: String,
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
    /// Third info-box line; null when the caller passed none.
    hwnd_info_subtext2: HWND,
    hwnd_button_primary: HWND,
    /// `std::ptr::null_mut()` when there's only one button.
    hwnd_button_secondary: HWND,
    /// `std::ptr::null_mut()` when there are fewer than three buttons.
    hwnd_button_tertiary: HWND,

    hfont_heading: HFONT,
    hfont_subtitle: HFONT,
    hfont_content: HFONT,
    hfont_button: HFONT,
    hfont_info_heading: HFONT,
    hfont_info_subtext: HFONT,
    hfont_info_subtext2: HFONT,
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
/// Third info-box line. A distinct id so `GetDlgItem` in the cleanup
/// path can't ever alias the first subtext.
const IDC_INFO_SUBTEXT2: i32 = 5010;
const IDC_BUTTON_PRIMARY: i32 = 5007;
const IDC_BUTTON_SECONDARY: i32 = 5008;
const IDC_BUTTON_TERTIARY: i32 = 5009;

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
            let hfont_button = create_font_pt(BUTTON_PT, BUTTON_WEIGHT, false);
            let hfont_info_heading =
                create_font_pt(INFO_HEADING_PT, INFO_HEADING_WEIGHT, false);
            let hfont_info_subtext =
                create_font_pt(INFO_SUBTEXT_PT, INFO_SUBTEXT_WEIGHT, false);
            // Third line reuses the first subtext's font, and is only
            // allocated when the text exists — otherwise the cleanup
            // path would be deleting a null it never created.
            let hfont_info_subtext2 = if (&(*state).info_subtext2_text).is_empty() {
                std::ptr::null_mut()
            } else {
                create_font_pt(INFO_SUBTEXT_PT, INFO_SUBTEXT_WEIGHT, false)
            };
            let hfont_link_url = if !(&(*state).link_url_text).is_empty() {
                create_font_pt(LINK_URL_PT, LINK_URL_WEIGHT, true)
            } else {
                std::ptr::null_mut()
            };

            // Compute the URL hit-test rect when the link row is
            // visible. A memory DC plus `GetTextExtentPoint32W`
            // measures both halves in pixels: the label paints in
            // `hfont_content` (same metrics as subtitle / body), the
            // URL paints in the underlined HFONT. `LINK_TEXT_X` +
            // label_width gives the URL's left edge; URL width is
            // added to find the right edge. `LINK_Y` is the text top;
            // bottom is `LINK_Y + tmHeight`.
            //
            // This used to use `DrawTextW(DT_CALCRECT)` against an
            // all-zero rect, which reports 0 px -- so both widths were
            // zero and the hit-test rect collapsed to a zero-width
            // sliver, making the link unclickable. `GetTextExtentPoint32W`
            // has no rect to clip against, and needs an explicit
            // character count rather than -1.
            if !(&(*state).link_url_text).is_empty() {
                let mem_dc = CreateCompatibleDC(std::ptr::null_mut());
                if !mem_dc.is_null() {
                    let prev_label = SelectObject(mem_dc, hfont_content as _);
                    let label_str = wide(&(*state).link_label_text);
                    let mut label_size = SIZE { cx: 0, cy: 0 };
                    let _ = GetTextExtentPoint32W(
                        mem_dc,
                        label_str.as_ptr(),
                        (label_str.len() as i32) - 1,
                        &mut label_size,
                    );
                    let label_w = label_size.cx;

                    SelectObject(mem_dc, hfont_link_url as _);
                    let url_str = wide(&(*state).link_url_text);
                    let mut url_size = SIZE { cx: 0, cy: 0 };
                    let _ = GetTextExtentPoint32W(
                        mem_dc,
                        url_str.as_ptr(),
                        (url_str.len() as i32) - 1,
                        &mut url_size,
                    );
                    let url_w = url_size.cx;

                    let mut tm: TEXTMETRICW = std::mem::zeroed();
                    GetTextMetricsW(mem_dc, &mut tm);
                    let _ = tm.tmHeight;

                    SelectObject(mem_dc, prev_label);
                    let _ = DeleteDC(mem_dc);

                    // The clickable target is the URL row, clamped to
                    // the width it actually paints in: it wraps inside
                    // `LINK_URL_W`, so a single-line measurement can
                    // overshoot and must not be trusted as-is.
                    (*state).link_url_rect = RECT {
                        left: LINK_TEXT_X,
                        top: LINK_URL_Y,
                        right: LINK_TEXT_X + url_w.min(LINK_URL_W),
                        bottom: LINK_URL_Y + LINK_URL_H,
                    };
                    let _ = label_w;
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
                // An explicit override (snug_preview --icon) wins over
                // the EXE resource; otherwise read the EXE icon. A
                // caller-supplied HBITMAP still wins over both, which
                // is the precedence the WM_PAINT branch implements.
                if let Some(hicon) = mascot_icon_override()
                    .or_else(|| find_best_icon_hicon(MASCOT_LOAD_CX, MASCOT_LOAD_CY))
                {
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

            // ----- Third info-box line (optional) -----
            // Created only when the caller supplied non-empty copy, so
            // a dialog that omits `info_subtext_2` has no control and
            // no font to leak. Positioned from `INFO_SUBTEXT2_Y_OFFSET`
            // rather than a hand-picked Y so it cannot drift from the
            // first subtext line when `INFO_BOX_H` changes.
            let hwnd_info_subtext2 = if (&(*state).info_subtext2_text).is_empty() {
                std::ptr::null_mut()
            } else {
                let h = CreateWindowExW(
                    0,
                    wide(STATIC_CLASS).as_ptr(),
                    wide((*state).info_subtext2_text.as_str()).as_ptr(),
                    WS_CHILD | WS_VISIBLE | SS_LEFT,
                    INFO_TEXT_X,
                    INFO_BOX_Y + INFO_SUBTEXT2_Y_OFFSET,
                    INFO_TEXT_W,
                    INFO_SUBTEXT_H,
                    hwnd,
                    IDC_INFO_SUBTEXT2 as *mut _,
                    hinst,
                    std::ptr::null(),
                );
                apply_font(h, hfont_info_subtext2);
                h
            };

            // ----- Buttons -----
            // Content-sized, right-aligned, flowing **left**, with the
            // primary action at the far right (the Windows task-dialog
            // convention). Widths are measured from each label rather
            // than assumed, so a translated string of any length fits
            // without a width table to maintain -- which is the whole
            // point of moving them out of fixed X/W constants.
            //
            // Buttons used to be vertically centred *inside* the info
            // box's band, overlapping it. They now have their own row
            // below it.
            let mut created: [(HWND, i32); 3] =
                [(std::ptr::null_mut(), 0); 3];
            let button_ids = [IDC_BUTTON_PRIMARY, IDC_BUTTON_SECONDARY, IDC_BUTTON_TERTIARY];
            let button_count = (&(*state).buttons).len().min(3);
            for i in 0..button_count {
                let (is_primary, label) = (&(*state).buttons)[i].clone();
                let style = if is_primary {
                    BS_DEFPUSHBUTTON as u32
                } else {
                    BS_PUSHBUTTON as u32
                };
                // Created at x = 0 and repositioned below, once its
                // width is known.
                let hwnd_button = CreateWindowExW(
                    0,
                    wide(BUTTON_CLASS).as_ptr(),
                    wide(label.as_str()).as_ptr(),
                    WS_CHILD | WS_VISIBLE | style,
                    0,
                    BUTTON_Y,
                    BUTTON_MIN_W,
                    BUTTON_H,
                    hwnd,
                    button_ids[i] as *mut _,
                    hinst,
                    std::ptr::null(),
                );
                apply_font(hwnd_button, hfont_button);
                let w = measure_button_width(hfont_button, &label);
                created[i] = (hwnd_button, w);
            }

            // Pack right to left: entry 0 (primary) hugs the right
            // margin and each later entry steps left by its own width
            // plus BUTTON_GAP, so the group is flush right whatever the
            // label widths turn out to be.
            let mut right = BUTTON_RIGHT;
            for i in 0..button_count {
                let (hwnd_button, w) = created[i];
                SetWindowPos(
                    hwnd_button,
                    std::ptr::null_mut(),
                    right - w,
                    BUTTON_Y,
                    w,
                    BUTTON_H,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                right -= w + BUTTON_GAP;
            }

            let (hwnd_primary, hwnd_secondary, hwnd_tertiary) =
                (created[0].0, created[1].0, created[2].0);

            // Stash handles back into the state struct so the rest
            // of the WndProc can reach them via
            // `GetWindowLongPtrW(hwnd, GWLP_USERDATA)`.
            (*state).hwnd_heading = hwnd_heading;
            (*state).hwnd_subtitle = hwnd_subtitle;
            (*state).hwnd_content = hwnd_content;
            (*state).hwnd_info_icon = hwnd_info_icon;
            (*state).hwnd_info_heading = hwnd_info_heading;
            (*state).hwnd_info_subtext = hwnd_info_subtext;
            (*state).hwnd_info_subtext2 = hwnd_info_subtext2;
            (*state).hwnd_button_primary = hwnd_primary;
            (*state).hwnd_button_secondary = hwnd_secondary;
            (*state).hwnd_button_tertiary = hwnd_tertiary;
            (*state).hfont_heading = hfont_heading;
            (*state).hfont_subtitle = hfont_subtitle;
            (*state).hfont_content = hfont_content;
            (*state).hfont_button = hfont_button;
            (*state).hfont_info_heading = hfont_info_heading;
            (*state).hfont_info_subtext = hfont_info_subtext;
            (*state).hfont_info_subtext2 = hfont_info_subtext2;
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

                // Row 1: the label, on its own line under the mascot.
                let label_str = wide(&label_text);
                let prev_font = SelectObject(hdc, hfont_content as _);
                SetTextColor(hdc, COLOR_SUBTITLE);
                let mut label_rc = RECT {
                    left: LINK_TEXT_X,
                    top: LINK_LABEL_Y,
                    right: LINK_TEXT_X + LINK_LABEL_W,
                    bottom: LINK_LABEL_Y + LINK_LABEL_H,
                };
                let _ = DrawTextW(
                    hdc,
                    label_str.as_ptr(),
                    -1,
                    &mut label_rc,
                    DT_SINGLELINE | DT_NOPREFIX,
                );

                // Row 2: the URL, beneath it. DT_WORDBREAK so a long
                // URL wraps inside the mascot column instead of running
                // into the error-content control to the right.
                SelectObject(hdc, hfont_link_url as _);
                SetTextColor(hdc, COLOR_LINK);
                let url_str = wide(&url_text);
                let mut url_paint_rc = RECT {
                    left: LINK_TEXT_X,
                    top: LINK_URL_Y,
                    right: LINK_TEXT_X + LINK_URL_W,
                    bottom: LINK_URL_Y + LINK_URL_H,
                };
                let _ = DrawTextW(
                    hdc,
                    url_str.as_ptr(),
                    -1,
                    &mut url_paint_rc,
                    // DT_EDITCONTROL is what makes the wrap happen: DT_WORDBREAK
                    // on its own only breaks at existing word boundaries, and
                    // a URL has none, so the text was clipped instead.
                    DT_EDITCONTROL | DT_WORDBREAK | DT_NOPREFIX,
                );

                let _ = url_rect;
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
            let state_ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const State;
            let buttons_len = if !state_ptr.is_null() {
                unsafe { (&(*state_ptr)).buttons.len() }
            } else {
                0
            };
            if id == IDC_BUTTON_PRIMARY as u32 {
                unsafe {
                    PostQuitMessage(IDYES_I32);
                }
            } else if id == IDC_BUTTON_SECONDARY as u32 && buttons_len == 3 {
                // Middle button on a 3-button dialog → IDNO. 2-button
                // dialogs (where `IDC_BUTTON_SECONDARY` is also wired
                // up) fall through to the cancel branch below to
                // preserve the historical return code.
                unsafe {
                    PostQuitMessage(IDNO_I32);
                }
            } else if id == IDC_BUTTON_SECONDARY as u32
                || id == IDC_BUTTON_TERTIARY as u32
                || id == IDCANCEL as u32
            {
                // 3-button: tertiary dismissal. 2-button: secondary
                // dismissal. 1-button: X / Alt+F4 dismissal. All → IDCANCEL_I32.
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
                if !(*raw).hfont_button.is_null() {
                    DeleteObject((*raw).hfont_button as _);
                }
                if !(*raw).hfont_info_heading.is_null() {
                    DeleteObject((*raw).hfont_info_heading as _);
                }
                if !(*raw).hfont_info_subtext.is_null() {
                    DeleteObject((*raw).hfont_info_subtext as _);
                }
                if !(*raw).hfont_info_subtext2.is_null() {
                    DeleteObject((*raw).hfont_info_subtext2 as _);
                }
                if !(*raw).hfont_link_url.is_null() {
                    DeleteObject((*raw).hfont_link_url as _);
                }
                // `mascot_hicon` is deliberately NOT destroyed here --
                // see the matching note in `progress_window`. Neither
                // source is owned by the window: `find_best_icon_hicon`
                // returns an `LR_SHARED` system handle, and the
                // `snug_preview --icon` override is shared by every
                // dialog for the process lifetime. Destroying it freed
                // the icon for every dialog after the first.
                let _ = &(*raw).mascot_hicon;
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

/// Width a button needs for `label`: the text as `hfont` renders it,
/// plus symmetric padding, floored at [`BUTTON_MIN_W`].
///
/// Takes the font as an argument rather than asking the control
/// (`WM_GETFONT`), because during `WM_CREATE` a freshly created
/// `BUTTON` has no font of its own yet and the stock GUI font it
/// would fall back to is the legacy GDI face -- visibly narrower than
/// the themed font a button actually renders in. Measuring the very
/// font we then apply makes the two agree by construction.
///
/// Also switched off `DrawTextW(DT_CALCRECT)`, which measured 0 px for
/// every label: with an all-zero rect DrawText clips to the empty
/// rect, and `GetTextExtentPoint32W` -- the API we want, since it has
/// no rect to clip against -- rejects `c = -1` and returns FALSE.
/// Either mistake silently floors every button to `BUTTON_MIN_W`.
pub(crate) fn measure_button_width(hfont: HFONT, label: &str) -> i32 {
    if hfont.is_null() {
        return BUTTON_MIN_W;
    }
    unsafe {
        let mem_dc = CreateCompatibleDC(std::ptr::null_mut());
        if mem_dc.is_null() {
            return BUTTON_MIN_W;
        }
        let prev = SelectObject(mem_dc, hfont as _);
        let mut size = SIZE { cx: 0, cy: 0 };
        let s = wide(label);
        // Explicit character count, minus the NUL `wide` appended.
        let count = (s.len() as i32) - 1;
        let _ = GetTextExtentPoint32W(mem_dc, s.as_ptr(), count, &mut size);
        SelectObject(mem_dc, prev);
        let _ = DeleteDC(mem_dc);
        (size.cx + BUTTON_PAD_X * 2).max(BUTTON_MIN_W)
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