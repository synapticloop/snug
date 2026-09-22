//! Custom-paint modal prompt dialogs — used as the v5-friendly
//! fallback for the `TaskDialogIndirect`-based prompts in
//! `jdk_install::Config::show`. Same look-and-feel family as
//! `progress_window::show` (mockup-aligned design) but tailored for
//! the prompt pattern: heading + multi-line body + a row of buttons.
//!
//! Layout (480×320):
//! ```text
//! ┌───────────────────────────────────────────────────────────────┐
//! │ [icon] Title…                                       — □ ✕      │ ← title bar (system)
//! │                                                                │
//! │   Heading in 12pt Segoe UI Bold                               │
//! │                                                                │
//! │   Body text — default font, gray, multi-line. Wraps on        │
//! │   whitespace; fixed-height STATIC so very long copy may       │
//! │   clip — sized for the install / retry prompts which both     │
//! │   fit comfortably.                                            │
//! │                                                                │
//! │                                            [ Primary ] [ Cancel ]│
//! └───────────────────────────────────────────────────────────────┘
//! ```

use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, EndPaint, FillRect, FW_BOLD,
    GetStockObject, HBRUSH, HFONT, NULL_BRUSH, PAINTSTRUCT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetSystemMetrics, KillTimer, MSG, PostQuitMessage, RegisterClassExW, SendMessageW,
    SetWindowTextW, SetWindowLongPtrW, GetWindowLongPtrW, TranslateMessage,
    CW_USEDEFAULT, ICON_BIG, ICON_SMALL, SM_CXSCREEN, SM_CYSCREEN, BS_DEFPUSHBUTTON,
    WM_COMMAND, WM_CREATE, WM_CLOSE, WM_NCDESTROY, WM_PAINT, WM_SETICON, WM_TIMER,
    WNDCLASSEXW, WS_CAPTION, WS_CHILD, WS_SYSMENU, WS_VISIBLE, WS_OVERLAPPED, WS_EX_TOPMOST,
};

use crate::jdk_install::load_exe_main_icon_hicon;
use crate::log;

// ===========================================================================
//  Layout constants
// ===========================================================================

const CLASS_NAME: &str = "snug_custom_dialog_v1\0";

const WINDOW_W: i32 = 480;
const WINDOW_H: i32 = 320;
const MARGIN: i32 = 16;

const HEADING_Y: i32 = 24;
const HEADING_H: i32 = 22;

const BODY_Y: i32 = 56;
const BODY_W: i32 = WINDOW_W - MARGIN * 2;
// Reserve 56 px at the bottom for the button row.
const BODY_H: i32 = WINDOW_H - BODY_Y - 56;

const BUTTON_Y: i32 = WINDOW_H - 40;
const BUTTON_H: i32 = 24;
const BUTTON_W: i32 = 96;
const BUTTON_GAP: i32 = 8;

const IDC_HEADING: i32 = 1001;
const IDC_BODY: i32 = 1002;
const IDC_BUTTON_BASE: i32 = 1010;

const GWLP_USERDATA: i32 = -21;

// `SS_LEFT` lets the body STATIC word-wrap (the `_NOWORDWRAP` variant
// is for the truncating variants). Hard-coded because windows-sys
// 0.59 doesn't export either.
const SS_LEFT: u32 = 0x0000;
// `WM_SETFONT` likewise.
const WM_SETFONT: u32 = 0x0030;

// Colours (COLORREF = 0x00BBGGRR).
const COLOR_BG: u32 = 0x00FFFFFF;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ===========================================================================
//  Per-window state
// ===========================================================================

struct PromptState {
    button_count: usize,
    hwnd_heading: HWND,
    hwnd_body: HWND,
    hwnd_buttons: Vec<HWND>,
    hfont_heading: HFONT,
    /// 1-based button index the user clicked. `0` until the WM_COMMAND
    /// handler sets it.
    selected: i32,
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
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(prompt_wndproc),
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

unsafe extern "system" fn prompt_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => unsafe {
            let create_struct =
                lparam as *const windows_sys::Win32::UI::WindowsAndMessaging::CREATESTRUCTW;
            let state = (*create_struct).lpCreateParams as *mut PromptState;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);

            let hinst = GetModuleHandleW(std::ptr::null());

            // Heading — 12pt Segoe UI Bold.
            let hfont_heading = create_font_pt(12, FW_BOLD as i32, false, "Segoe UI\0");

            let hwnd_heading = CreateWindowExW(
                0,
                wide("STATIC\0").as_ptr(),
                std::ptr::null(),
                WS_CHILD | WS_VISIBLE | SS_LEFT,
                MARGIN,
                HEADING_Y,
                WINDOW_W - MARGIN * 2,
                HEADING_H,
                hwnd,
                IDC_HEADING as *mut _,
                hinst,
                std::ptr::null(),
            );
            SendMessageW(hwnd_heading, WM_SETFONT as u32, hfont_heading as WPARAM, 1);

            let hwnd_body = CreateWindowExW(
                0,
                wide("STATIC\0").as_ptr(),
                std::ptr::null(),
                WS_CHILD | WS_VISIBLE | SS_LEFT,
                MARGIN,
                BODY_Y,
                BODY_W,
                BODY_H,
                hwnd,
                IDC_BODY as *mut _,
                hinst,
                std::ptr::null(),
            );

            (*state).hwnd_heading = hwnd_heading;
            (*state).hwnd_body = hwnd_body;
            (*state).hfont_heading = hfont_heading;

            0
        },
        WM_PAINT => unsafe {
            // White background. Stock controls paint themselves.
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc: RECT = std::mem::zeroed();
            windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc);
            let bg_brush = CreateSolidBrush(COLOR_BG);
            FillRect(hdc, &rc, bg_brush);
            DeleteObject(bg_brush as _);
            EndPaint(hwnd, &mut ps);
            0
        },
        WM_COMMAND => {
            let id = (wparam as u32) & 0xFFFF;
            let raw =
                unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut PromptState };
            if !raw.is_null() && id >= IDC_BUTTON_BASE as u32 {
                let button_idx = (id - IDC_BUTTON_BASE as u32) as i32 + 1;
                unsafe {
                    if button_idx >= 1 && button_idx <= (*raw).button_count as i32 {
                        (*raw).selected = button_idx;
                        PostQuitMessage(0);
                    }
                }
            }
            0
        }
        WM_CLOSE => unsafe {
            // Treat window close (X button, Alt+F4) as Cancel — same
            // semantics as the rightmost button. Callers handle `0`
            // as "no choice / cancel" by checking the return value.
            let raw =
                GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut PromptState;
            if !raw.is_null() {
                let already = (*raw).selected;
                if already == 0 {
                    let last = (*raw).button_count as i32;
                    (*raw).selected = last;
                }
            }
            PostQuitMessage(0);
            0
        }
        WM_NCDESTROY => unsafe {
            let raw =
                GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut PromptState;
            if !raw.is_null() {
                if !(*raw).hfont_heading.is_null() {
                    DeleteObject((*raw).hfont_heading as _);
                }
                let _ = Box::from_raw(raw);
            }
            0
        }
        // Keep an unused timer-id constant in scope so the future
        // addition of a "details" expander doesn't need new imports.
        WM_TIMER => 0,
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

// ===========================================================================
//  Font creation
// ===========================================================================

fn create_font_pt(point_size: i32, weight: i32, italic: bool, face: &str) -> HFONT {
    let height = -(point_size * 96 / 72);
    let italic_u32 = if italic { 1 } else { 0 };
    unsafe {
        CreateFontW(
            height, 0, 0, 0, weight, italic_u32, 0, 0, 1, 0, 0, 0, 0,
            wide(face).as_ptr(),
        )
    }
}

// ===========================================================================
//  Public API
// ===========================================================================

/// Show a modal prompt dialog. Returns the 1-based index of the
/// button the user clicked (1 = first button, 2 = second, etc.). The
/// last button is treated as "cancel" — pressing Esc or closing the
/// window returns its index. Returns `0` if the window couldn't be
/// created (callers should treat that as cancel).
pub unsafe fn show_prompt(
    parent: HWND,
    title: &str,
    heading: &str,
    body: &str,
    buttons: &[&str],
) -> i32 {
    if buttons.is_empty() {
        return 0;
    }

    unsafe { register_class() };

    let title_w = wide(title);
    let hinst = unsafe { GetModuleHandleW(std::ptr::null()) };

    let state_box = Box::new(PromptState {
        button_count: buttons.len(),
        hwnd_heading: std::ptr::null_mut(),
        hwnd_body: std::ptr::null_mut(),
        hwnd_buttons: vec![std::ptr::null_mut(); buttons.len()],
        hfont_heading: std::ptr::null_mut(),
        selected: 0,
    });
    let state_ptr = Box::into_raw(state_box);

    // Centre on the primary monitor when no parent is supplied.
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
            "custom_dialog::show_prompt: CreateWindowExW failed (GetLastError={})",
            err
        ));
        let _ = unsafe { Box::from_raw(state_ptr) };
        return 0;
    }

    // Apply the EXE's main icon to the title bar + taskbar.
    if let Some(hicon) = load_exe_main_icon_hicon() {
        unsafe {
            SendMessageW(hwnd, WM_SETICON, ICON_SMALL as WPARAM, hicon as LPARAM);
            SendMessageW(hwnd, WM_SETICON, ICON_BIG as WPARAM, hicon as LPARAM);
        }
    }

    // Heading and body text.
    unsafe {
        let s = wide(heading);
        SetWindowTextW((*state_ptr).hwnd_heading, s.as_ptr());
        let s = wide(body);
        SetWindowTextW((*state_ptr).hwnd_body, s.as_ptr());
    }
    // (state_ptr is dereferenced further down — also wrapped in unsafe.)

    // Buttons — right-aligned at the bottom. First button is the
    // default (Enter activates it).
    let total_button_w =
        (buttons.len() as i32) * BUTTON_W + (buttons.len() as i32 - 1) * BUTTON_GAP;
    let buttons_start_x = WINDOW_W - MARGIN - total_button_w;
    for (i, label) in buttons.iter().enumerate() {
        let button_x = buttons_start_x + (i as i32) * (BUTTON_W + BUTTON_GAP);
        let hwnd_btn = unsafe {
            CreateWindowExW(
                0,
                wide("BUTTON\0").as_ptr(),
                wide(label).as_ptr(),
                WS_CHILD | WS_VISIBLE | BS_DEFPUSHBUTTON as u32,
                button_x,
                BUTTON_Y,
                BUTTON_W,
                BUTTON_H,
                hwnd,
                (IDC_BUTTON_BASE + i as i32) as *mut _,
                hinst,
                std::ptr::null(),
            )
        };
        unsafe {
            // `hwnd_buttons[i] = hwnd_btn` would be an implicit `&mut`
            // through the raw pointer (Rust 2024
            // `dangerous_implicit_autorefs`). Write through an
            // explicit reference.
            (&mut (*state_ptr).hwnd_buttons)[i] = hwnd_btn;
        }
        if i == 0 {
            unsafe { SetFocus(hwnd_btn) };
        }
    }

    // Modal message loop. Exits when PostQuitMessage is called from
    // a button click or window close.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    unsafe {
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // Best-effort timer kill; the window is being destroyed anyway,
    // but killing any timers we set (none today, kept here for the
    // future "details" expander) avoids a stray WM_TIMER landing in
    // the freed state.
    unsafe { KillTimer(hwnd, 1) };
    unsafe { DestroyWindow(hwnd) };

    unsafe { (*state_ptr).selected }
}
