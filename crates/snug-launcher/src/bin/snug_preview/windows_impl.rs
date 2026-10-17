//! One-click dialog preview for the snug launcher.
//!
//! A tiny standalone Win32 GUI binary. Shows a launcher window with
//! one button per dialog kind the production launcher pops. Each
//! button click invokes the **same** code path the production
//! launcher takes — just with hard-coded sample data instead of a
//! real Adoptium fetch / JVM scan. Lets you iterate on dialog
//! layout / copy / button labels without rebuilding `snug-cli`,
//! `snug-format`, or the committed `bin/launcher-stub-windows-x86_64.exe`, and
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
//! Command line:
//!
//! ```text
//! snug_preview [OPTIONS]
//!   -h, --help                        print the option list and every
//!                                     previewable dialog
//!       --localisation <FILE|DIR>     load localisations to pick from
//!       --localization <FILE|DIR>     (alias; both spellings accepted)
//!       --icon <FILE>                 swap the dialog icons for a test
//! ```
//!
//! With no options it opens the launcher window. `--help` is handled
//! before any Win32 setup, so it works on a headless CI runner and
//! never opens a window that would then block. Unrecognised arguments
//! print a one-line error plus the `--help` hint and exit 2 — nothing
//! here read `argv` before, so there is no prior meaning for a stray
//! argument to preserve, and silently launching the GUI after a
//! mistyped flag is its own small time-waster.
//!
//! `--localisation` takes a `snug-localisations.<tag>.txt` or a
//! directory of them, and is repeatable. Everything it finds lands in
//! a **Language** dropdown above the dialog buttons; picking one swaps
//! the launcher's active bundle chain, so dialogs opened afterwards
//! render in that language. The built-in English baseline is always in
//! the list, last — unless you supply your own
//! `snug-localisations.en.txt`, which takes its place, matching the rule
//! the build applies to a shipped launcher.
//!
//! Switching affects dialogs opened *afterwards*: a window already on
//! screen captured its copy when it was built and keeps it.
//!
//! `--icon <FILE>` replaces the icon every dialog uses. Normally that
//! artwork comes from the EXE's own `MAINICON` resource, which means
//! changing it would mean relinking `bin/launcher-stub-windows-x86_64.exe` — a
//! cross-compiled, committed binary, and much too slow a loop for
//! "try this icon across all eight dialogs". `--icon` instead installs
//! a process-wide override that the dialogs consult, covering both the
//! large mascot image and the title bar / Alt-Tab / taskbar. It takes
//! a `.png` or a `.ico`: `.ico` goes through `LoadImageW`, and a PNG
//! needs a GDI+ fallback because -- verified, not assumed -- Windows'
//! `LoadImageW` returns NULL for PNG from file despite documenting
//! support since Vista. Neither route links a Rust image crate into the
//! shipped launcher, so its size is untouched.
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
//!
//! This is the whole implementation, reached through `#[path]` from the
//! `snug_preview` bin one directory up. It used to be that bin's own
//! crate root with a file-level `#![cfg(windows)]`; see the parent's
//! module docs for why a Windows-only bin cannot use that form.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateRoundRectRgn, CreateSolidBrush, DeleteObject, DT_LEFT,
    DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, DrawTextW, EndPaint, FillRect, FillRgn,
    HBRUSH, HBITMAP, HFONT, InvalidateRect, PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor,
    TRANSPARENT,
};
use windows_sys::Win32::Graphics::GdiPlus::{
    GdiplusShutdown, GdiplusStartup, GdiplusStartupInput, GdipCreateBitmapFromFile,
    GdipCreateFromHDC, GdipCreateImageAttributes, GdipDeleteGraphics, GdipDisposeImage,
    GdipDisposeImageAttributes, GdipDrawImageRectRectI, GdipGetImageHeight, GdipGetImageWidth,
    GdipSetImageAttributesWrapMode, GpBitmap, GpGraphics, GpImage, GpImageAttributes, UnitPixel,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CB_ADDSTRING, CB_GETCOUNT, CB_GETCURSEL, CB_RESETCONTENT, CB_SETCURSEL, CBN_SELCHANGE,
    CBS_DROPDOWNLIST, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    DrawIconEx, GetClientRect, GetDlgItem, GetMessageW, GetSystemMetrics, LoadCursorW, LoadImageW,
    MoveWindow, MSG, PostThreadMessageW,
    RegisterClassExW, SendMessageW, SetWindowPos, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE,
    SWP_NOZORDER, TranslateMessage, BS_PUSHBUTTON,
    DI_NORMAL, HICON, ICON_BIG, ICON_SMALL, IDC_ARROW, IMAGE_ICON, LR_DEFAULTSIZE,
    LR_LOADFROMFILE, LR_SHARED, WM_CLOSE, WM_COMMAND,
    WM_CREATE, WM_DESTROY, WM_NCDESTROY, WM_PAINT, WM_QUIT, WM_SETICON, WNDCLASSEXW, WS_CAPTION,
    WS_CHILD, WS_EX_TOPMOST, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

use snug_format::Localization;
use snug_launcher::dialogs;
use snug_launcher::error_window;
use snug_launcher::jdk_install::{self, ProgressShared};
use snug_launcher::localize;
use snug_launcher::log::log;
use snug_launcher::progress_window;

// ============================================================================
//  Constants
// ============================================================================

const CLASS_NAME: &str = "snug_preview_launcher_v1\0";
const WINDOW_TITLE: &str = "snug dialog preview\0";

// ============================================================================
//  Layout metrics
// ============================================================================
//
//  Every constant below is a **logical** measurement at 96 DPI. None of
//  them is handed to Win32 directly — `scale_px` converts at the call
//  site — so one number describes the layout at every scale factor
//  instead of needing a second table of 150%-sized copies.
//
//  The pairing is not optional. Opting into per-monitor DPI awareness on
//  its own stops Windows bitmap-stretching the window but leaves these
//  numbers unscaled, which is *worse* than staying unaware: the window
//  comes out physically too small holding the same contents.
//  `snug-dropper`'s `enable_dpi_awareness` records the same trap from
//  the other side.

/// Scale a logical (96 DPI) measurement to `dpi`.
///
/// Rounded half-away-from-zero rather than truncated, so a measurement
/// never loses a pixel to integer division and the hairline gaps
/// between buttons stay even across scale factors instead of drifting
/// by 1 px.
///
/// The sign handling is not decoration. Rust's `/` truncates toward
/// zero, so the obvious `(n + 48) / 96` rounds *negatives the wrong
/// way* — a -13 px offset becomes -12 at 96 DPI, i.e. the overhang
/// silently shrinks by a pixel at every scale factor. Offsets here are
/// legitimately negative (the `--icon` plate hangs outside the mascot's
/// right edge), so the helper has to be symmetric or the overhang is
/// off by one everywhere it is actually used.
const fn scale_px(logical: i32, dpi: u32) -> i32 {
    let n = logical as i64 * dpi as i64;
    const HALF: i64 = 48;
    if n >= 0 {
        ((n + HALF) / 96) as i32
    } else {
        -((-n + HALF) / 96) as i32
    }
}

/// Window dimensions (logical pixels at 96 DPI).
///
/// `LAUNCHER_W` / `launcher_client_height()` describe the **client
/// area**, not the raw window size — the title bar is added on top at
/// `CreateWindowExW` time via `AdjustWindowRectExForDpi`. Treating the
/// values as client dimensions means the body layout
/// (`BTN_FIRST_Y`, `BTN_CLOSE_MARGIN_BOTTOM`) is computed in the same
/// coordinate system the buttons actually live in, so the Close button
/// can never end up below the visible area.
///
/// 520 rather than the old 480: "Java error (with update link)" needs
/// ~190 px of a 432 px column at 10pt, and the leftover was an
/// arbitrary-looking 24 px gutter on both sides rather than a margin.
/// The extra 40 px also lets the Language dropdown sit on a round
/// measure instead of being pinned to the label.
const LAUNCHER_W: i32 = 520;

/// Height of the client area, **derived from the content** so the
/// Close button always sits just below the last dialog row. The
/// previous hardcoded 760 left ~148 px of dead space between the
/// last button (ends at y=560) and Close (started at y=708).
///
/// Deriving it means adding or removing a dialog in
/// [`DIALOG_BUTTONS`] resizes the window automatically — no second
/// edit to forget.
const fn launcher_client_height() -> i32 {
    let rows = DIALOG_BUTTONS.len() as i32;
    // Bottom edge of the last dialog row.
    let last_row_bottom = BTN_FIRST_Y + (rows - 1) * (BTN_H + BTN_GAP) + BTN_H;
    last_row_bottom + BTN_CLOSE_GAP + BTN_CLOSE_H + BTN_CLOSE_MARGIN_BOTTOM
}

/// Header icon painted at the top of the launcher window's client
/// area — the EXE's embedded MAINICON, centred horizontally. 128 px
/// reads well at HiDPI without overshooting the available width
/// (window is 520 px, icon is centred with 196 px on either side).
///
/// This is **always the mascot**, override or not. `--icon` replaces
/// the window's title-bar / taskbar / Alt-Tab icon and the mascot
/// image each dialog draws, but it deliberately does not replace this
/// one — the preview's premise is that the mascot is the inspector and
/// the candidate icon is what it is inspecting. See
/// [`inspected_icon_rect`].
const HEADER_ICON_SIZE: i32 = 128;
const HEADER_ICON_Y: i32 = 12;
const HEADER_ICON_BOTTOM: i32 = HEADER_ICON_Y + HEADER_ICON_SIZE;

/// Button layout — single column, 24 px outer margin, 8 px gap.
///
/// One column rather than eight buttons in a 2×4 grid: each button is a
/// distinct dialog kind, scanning the list is the primary action, and
/// the longest label needs ~190 px — which does not survive two
/// columns plus a gutter once the font grows.
const BTN_X: i32 = 24;
const BTN_W: i32 = LAUNCHER_W - 48;
/// 40 rather than 38: the row pitch is `BTN_H + BTN_GAP`, so this is
/// also the *distance* between button centres, and 38 left the rows
/// visually crowded against each other in a column this long.
const BTN_H: i32 = 40;
const BTN_GAP: i32 = 8;
const BTN_CLOSE_W: i32 = 104;
const BTN_CLOSE_H: i32 = 40;
const BTN_CLOSE_MARGIN_BOTTOM: i32 = 16;
/// Vertical gap between the last dialog row and the Close button.
/// 16 px reads as a clear separator without looking detached.
const BTN_CLOSE_GAP: i32 = 16;

/// Vertical rhythm below the mascot, **derived** rather than a column
/// of independent magic numbers.
///
/// These were 152 / 200 / 248 — three values hand-tuned against each
/// other, so moving the icon meant re-checking all three and nothing
/// stopped them drifting apart. Chaining them off the mascot's bottom
/// edge means the header can grow without anything below it silently
/// overlapping.
const HEADING_Y: i32 = HEADER_ICON_BOTTOM + 16;
const HEADING_H: i32 = 22;
const SUBTITLE_Y: i32 = HEADING_Y + HEADING_H;
/// Two lines at 10pt. The subtitle is the only place the detachment
/// behaviour can be explained, and one line at 520 px fits ~65
/// characters — not enough for a sentence that is actually useful.
const SUBTITLE_H: i32 = 40;
/// Gap from the subtitle to the dropdown row.
const COMBO_GAP: i32 = 24;
const COMBO_Y: i32 = SUBTITLE_Y + SUBTITLE_H + COMBO_GAP;
/// Gap from the dropdown row to the first dialog button. Wider than
/// `COMBO_GAP` so the eight dialogs read as their own group rather
/// than as a continuation of the controls above them.
const BTN_GAP_HEADER: i32 = 40;
const BTN_FIRST_Y: i32 = COMBO_Y + BTN_GAP_HEADER;

/// Close button position, derived so it tracks the last dialog row.
/// Both `WM_CREATE` and the `WM_DPICHANGED` relayout read these two,
/// which is why they are functions rather than inline arithmetic at
/// each site.
const fn close_y() -> i32 {
    BTN_FIRST_Y + (DIALOG_BUTTONS.len() as i32 - 1) * (BTN_H + BTN_GAP) + BTN_H + BTN_CLOSE_GAP
}
const fn close_x() -> i32 {
    BTN_X + BTN_W - BTN_CLOSE_W
}

/// Copy painted in `WM_PAINT`.
///
/// The second sentence is not decoration. Detached dialogs are the
/// single most surprising thing about this tool — the old copy, "Click
/// a button to open the corresponding dialog.", never mentioned that
/// closing the launcher leaves every dialog you have opened alive, and
/// the `--help` NOTES were the only place it was written down.
const HEADING_TEXT: &str = "snug dialog preview";
const SUBTITLE_TEXT: &str =
    "Each dialog opens in its own window and stays open until you close it, \
     so you can compare two side by side.";

/// Language picker — a `COMBOBOX` between the subtitle and the dialog
/// buttons, so a translation can be flipped on before opening a dialog
/// instead of requiring a rebuild.
///
/// The label and the dropdown share **one row**: the label sits to the
/// left, vertically centred against the dropdown's closed field. The
/// vertical centre is measured at paint time from the combo's own
/// client rect rather than hardcoded, because the closed field's
/// height follows the font — pinning a second row of constants to it
/// drifts the moment the font or DPI changes.
///
/// `COMBO_X` is *derived* from the label's reserved width so the two
/// can never overlap, and `BTN_FIRST_Y` sits below `COMBO_Y`, which is
/// why the button block starts where it does.
const COMBO_LABEL_TEXT: &str = "Language:";
const COMBO_LABEL_X: i32 = BTN_X;
/// Reserved width for the label including a trailing gap. "Language:"
/// is ~60 px in the 10pt Segoe UI used here, so 76 leaves a comfortable
/// gutter without crowding the dropdown.
const COMBO_LABEL_W: i32 = 76;
const COMBO_X: i32 = COMBO_LABEL_X + COMBO_LABEL_W;
const COMBO_W: i32 = 240;
/// Height passed to `CreateWindowExW`. For a `CBS_DROPDOWNLIST` combo
/// this is the height of the **dropped list** — the closed field sizes
/// itself to the font (measured at ~25 px for 10pt Segoe UI) and is
/// unaffected by this value.
///
/// It has to fit *several* rows, not one. At 34 px the list was tall
/// enough for exactly one 25 px entry, so opening it showed only the
/// selected item with every other tag scrolled out of sight — `de`
/// looked absent from the dropdown even though `CB_GETCOUNT` proved it
/// was there. 140 px is ~5 rows, which covers a realistic locale set
/// and leaves room to scroll rather than truncate silently.
const COMBO_H: i32 = 140;

/// `WM_SETFONT` isn't exported as a named constant by windows-sys
/// 0.59. Value from winuser.h.
const WM_SETFONT: u32 = 0x0030;

/// `WM_DPICHANGED` isn't exported either. Value from winuser.h.
const WM_DPICHANGED: u32 = 0x02E0;

// ============================================================================
//  Palette and type
// ============================================================================
//
//  `COLORREF` is 0x00BBGGRR, so these read reversed on screen. Each is
//  written here as the hex its author thought in and converted once,
//  rather than scattering BGR literals through the paint code.
//
//  `COLOR_BODY` replaces the old 0x00606060. That grey sat close enough
//  to the heading that the two lines read as the same weight at a
//  glance, which is the thing a two-level hierarchy cannot afford.
const COLOR_BG: u32 = 0x00FF_FFFF; // #FFFFFF
const COLOR_HEADING: u32 = 0x0020_1A15; // #151A20, cool near-black
const COLOR_BODY: u32 = 0x005A_5550; // #50555A
/// Outline of the `--icon` plate.
///
/// This border is doing all of the separating, because the plate is
/// white and sits on white twice over: the mascot artwork is white and
/// so is the window. The previous `#BFC4C8` was present but read as
/// almost nothing at 1 px; a neutral `#ADADAD` is still plainly a
/// *light* grey, and is the first value that actually reads as an edge
/// against both neighbours.
const COLOR_PLATE_EDGE: u32 = 0x00AD_ADAD; // #ADADAD

const HEADING_PT: i32 = 12;
const BODY_PT: i32 = 10;

/// `FW_NORMAL` / `FW_SEMIBOLD`, spelled out rather than imported so the
/// weights read as weights at the call site.
const FONT_WEIGHT_NORMAL: i32 = 400;
const FONT_WEIGHT_SEMIBOLD: i32 = 600;

// ============================================================================
//  `--icon` composed onto the mascot
// ============================================================================
//
//  When `--icon` is given, the candidate icon is painted small in the
//  bottom-right of the mascot: the jar mascot holds its magnifying
//  glass in the upper-right of the artwork (`assets/snug-preview.png`),
//  so that corner is directly under the lens — and it is also the one
//  region of the artwork that is empty white, so nothing has to be
//  moved or redrawn to make room.

/// Side of the candidate icon as a fraction of the mascot's.
///
/// Proportional rather than a fixed pixel count for two reasons: it
/// tracks the mascot through `WM_DPICHANGED` without a second constant
/// to scale, and the "under the lens" relationship survives a change to
/// `HEADER_ICON_SIZE`. A third reads as an object being examined rather
/// than a badge stuck in the corner.
const INSPECTED_NUM: i32 = 1;
const INSPECTED_DEN: i32 = 3;

/// Horizontal offset from the mascot's **right** edge to the plate's.
///
/// Negative on purpose, so the plate overhangs the mascot by 13 px.
/// Started flush inside the corner and read as a badge painted on top
/// of the mascot's foot; pushing it out past the edge makes it an
/// object held up *beside* the mascot instead — which is what the
/// magnifying glass is aimed at. There is room for it: the mascot is
/// centred in a 520 px window and ends at x=324, so the plate clears
/// the window edge by ~178 px even at its widest.
const INSPECTED_RIGHT: i32 = -13;

/// Vertical offset from the mascot's **bottom** edge to the plate's.
///
/// Left positive. Dropping the plate *below* the mascot would detach it
/// from the lens, which sits in the artwork's upper-right — the two
/// offsets are independent for exactly that reason.
const INSPECTED_BOTTOM: i32 = 8;
/// White padding between the plate's edge and the icon inside it, so
/// the icon is not flush against the outline.
const INSPECTED_PLATE_PAD: i32 = 5;
/// Thickness of the plate's grey ring.
///
/// 2 rather than 1: the plate is white on white twice over — the
/// mascot artwork and the window — so at 1 px the ring is a decision
/// about rendering quality the user cannot resolve.
const INSPECTED_PLATE_BORDER: i32 = 2;
/// Corner ellipse **diameter** — GDI takes a diameter, not a radius.
const INSPECTED_PLATE_RADIUS: i32 = 14;

/// Side of the candidate icon for a mascot `mascot` logical px wide.
const fn inspected_icon_side(mascot: i32) -> i32 {
    mascot * INSPECTED_NUM / INSPECTED_DEN
}

/// Where the `--icon` override is painted: `(x, y, side)` in **device**
/// pixels, positioned against the mascot's bottom-right corner by
/// [`INSPECTED_RIGHT`] and [`INSPECTED_BOTTOM`].
///
/// The mascot box is passed in **logical** pixels and everything comes
/// back scaled, so the offsets and the side cannot disagree about which
/// DPI they are in — the bug that a "caller passes device pixels" version
/// of this function invites.
fn inspected_icon_rect(mascot_x: i32, mascot_y: i32, mascot: i32, dpi: u32) -> (i32, i32, i32) {
    let side = scale_px(inspected_icon_side(mascot), dpi);
    let right = scale_px(mascot_x + mascot, dpi) - scale_px(INSPECTED_RIGHT, dpi);
    let bottom = scale_px(mascot_y + mascot, dpi) - scale_px(INSPECTED_BOTTOM, dpi);
    (right - side, bottom - side, side)
}

/// The rounded plate behind the candidate icon, as `(l, t, r, b)` with
/// `r`/`b` **exclusive** — `CreateRoundRectRgn` treats them that way, and
/// an inclusive edge here draws a plate one pixel wider on the right and
/// bottom than on the left and top.
///
/// Separate from [`inspected_icon_rect`] because the padding is the
/// easiest value in this feature to get subtly wrong, and containment
/// is worth asserting on its own.
fn inspected_plate_rect(icon: (i32, i32, i32), dpi: u32) -> (i32, i32, i32, i32) {
    let pad = scale_px(INSPECTED_PLATE_PAD, dpi);
    let (x, y, side) = icon;
    (x - pad, y - pad, x + side + pad, y + side + pad)
}

/// The white centre of the plate, inset from the outer edge by exactly
/// [`INSPECTED_PLATE_BORDER`] on every side.
///
/// The grey ring is the region *between* these two rectangles, so an
/// inset that is not uniform shows up as a visibly lopsided rim. Kept
/// as a separate function precisely so that symmetry is one expression
/// rather than four subtractions to keep in step.
fn inspected_plate_inner(outer: (i32, i32, i32, i32), dpi: u32) -> (i32, i32, i32, i32) {
    let border = scale_px(INSPECTED_PLATE_BORDER, dpi);
    let (l, t, r, b) = outer;
    (l + border, t + border, r - border, b - border)
}

/// Load the EXE's embedded `MAINICON` group at two sizes for the
/// window class. `LR_SHARED` keeps Windows from handing us a
/// per-process copy that we'd have to `DestroyIcon` later — the
/// icon handle stays valid for the lifetime of the EXE module.
fn load_exe_main_icons(
    hinst: windows_sys::Win32::Foundation::HMODULE,
) -> (HICON, HICON) {
    // `--icon` wins for the launcher window too, so the window you're
    // clicking and the dialogs it spawns show the same artwork. The
    // override is already installed by the time we get here (main runs
    // it before `CreateWindowExW`).
    if let Some(hicon) = snug_launcher::jdk_install::window_icon_override() {
        return (hicon, hicon);
    }

    // RT_GROUP_ICON entries are conventionally named "MAINICON"
    // (Windows icon convention). editpe writes the group under the
    // *name* rather than under numeric ID 1, so the primary lookup
    // is by name. The numeric ID 1 fallback covers any future
    // upstream that switches to a numeric MAINICON.
    let mainicon_w: Vec<u16> = "MAINICON\0".encode_utf16().collect();
    let id1: *const u16 = std::ptr::dangling::<u16>();
    unsafe {
        let by_name_big = LoadImageW(
            hinst,
            mainicon_w.as_ptr(),
            IMAGE_ICON,
            32,
            32,
            LR_SHARED,
        );
        let by_name_small = LoadImageW(
            hinst,
            mainicon_w.as_ptr(),
            IMAGE_ICON,
            16,
            16,
            LR_SHARED,
        );
        // Fall back to the numeric ID 1 path. MAKEINTRESOURCE(1)
        // is encoded as the pointer value 1 — the high 16 bits are
        // 0, which Windows uses as the discriminator between
        // numeric IDs and string pointers.
        let by_id_big = LoadImageW(hinst, id1, IMAGE_ICON, 32, 32, LR_SHARED);
        let by_id_small = LoadImageW(hinst, id1, IMAGE_ICON, 16, 16, LR_SHARED);

        (
            if !by_name_big.is_null() { by_name_big } else { by_id_big },
            if !by_name_small.is_null() { by_name_small } else { by_id_small },
        )
    }
}

// ============================================================================
//  Header icon (painted at the top of the launcher window)
// ============================================================================

/// Lazily-loaded handle for the 128×128 MAINICON that the launcher
/// window paints at the top of its client area. The underlying
/// `LoadImageW` call is relatively expensive — multi-millisecond,
/// plus a roundtrip into the resource directory — and the result is
/// invariant for the lifetime of the EXE module, so we cache it in
/// a `OnceLock` and only call LoadImageW once per process. The
/// `LR_SHARED` flag means the handle stays valid forever; we never
/// `DestroyIcon` it (and can't, even if we wanted to — `LR_SHARED`
/// makes the icon owned by the system).
///
/// `HICON` is a raw pointer (`*mut c_void`), which isn't `Sync` and
/// can't sit directly in a `OnceLock` static. We store the bit
/// pattern as `usize` instead and cast on read; the load still
/// happens exactly once.
static HEADER_ICON: OnceLock<usize> = OnceLock::new();

/// Return the cached header-icon handle, loading it on first call.
/// Returns a null `HICON` if LoadImageW fails — the paint path
/// checks for null and skips the draw in that case (we never
/// want a load failure to crash the paint loop).
fn header_icon(hinst: HMODULE) -> HICON {
    let bits = *HEADER_ICON.get_or_init(|| unsafe {
        let mainicon_w: Vec<u16> = "MAINICON\0".encode_utf16().collect();
        LoadImageW(
            hinst,
            mainicon_w.as_ptr(),
            IMAGE_ICON,
            HEADER_ICON_SIZE,
            HEADER_ICON_SIZE,
            LR_SHARED,
        ) as usize
    });
    bits as *mut std::ffi::c_void
}

// ============================================================================
//  Window counter
// ============================================================================

/// Counts the number of open top-level windows owned by this preview
/// binary — the launcher window plus one per spawned dialog. Initial
/// value of 1 accounts for the launcher itself.
///
/// The process only exits when this hits zero. The launcher's slot
/// is released by `WM_NCDESTROY` (one-shot, after the launcher is
/// destroyed); each spawned dialog's slot is released by the
/// `SlotGuard` Drop inside its thread closure. When the count
/// reaches zero we post `WM_QUIT` to the **main** thread (not the
/// calling thread) so the launcher's `GetMessageW` returns 0 and
/// `main` exits. Posting to the spawned thread instead would only
/// kill that one dialog, leaving the launcher blocked forever.
///
/// `PostThreadMessageW` targets the thread's message queue directly,
/// so it works whether the launcher window still exists or has
/// already been destroyed — both the launcher and the dialogs hold
/// their own slots independently.
static OPEN_WINDOWS: AtomicUsize = AtomicUsize::new(1);

/// Win32 thread id of the launcher main thread. Captured at startup
/// via `GetCurrentThreadId`; read by `release_slot` to send the
/// final `WM_QUIT` to the right place.
static MAIN_THREAD_ID: AtomicU32 = AtomicU32::new(0);

/// Release one slot from `OPEN_WINDOWS`. If this was the last slot,
/// post `WM_QUIT` to the launcher main thread so `main` returns.
unsafe fn release_slot() {
    let prev = OPEN_WINDOWS.fetch_sub(1, Ordering::SeqCst);
    if prev == 1 {
        // SAFETY: `PostThreadMessageW` posts to the named thread's
        // message queue. It's safe to call from any thread, and we
        // own the slot we're decrementing (either via the dialog's
        // SlotGuard or the launcher's WM_NCDESTROY path).
        let main_tid = MAIN_THREAD_ID.load(Ordering::SeqCst);
        if main_tid != 0 {
            unsafe { PostThreadMessageW(main_tid, WM_QUIT, 0, 0) };
        }
    }
}

/// Run `f` on a new OS thread, accounting for the new top-level
/// window in `OPEN_WINDOWS` and decrementing when `f` returns (or
/// panics — the `Guard` below releases the slot on drop).
///
/// Dialogs run on their own threads so closing the launcher doesn't
/// destroy them, and so the user can spawn multiple dialogs
/// side-by-side from the launcher. The launcher releases its own
/// slot in `WM_NCDESTROY`; the dialog's slot is released by the
/// `SlotGuard` here.
fn spawn_dialog<F>(f: F)
where
    F: FnOnce() + Send + 'static,
{
    OPEN_WINDOWS.fetch_add(1, Ordering::SeqCst);
    // `Builder::spawn` returns `Result` so we can release the slot
    // if the OS refuses the thread (resource exhaustion, etc.).
    // The common path is `Ok(_)`, where the SlotGuard inside the
    // closure will fire when `f` returns and decrement the counter
    // for us.
    if thread::Builder::new()
        .spawn(move || {
            let _slot = SlotGuard;
            f();
        })
        .is_err()
    {
        // Spawn failed — release the slot manually so the count
        // doesn't leak above the live-window count.
        unsafe { release_slot() };
    }
}

/// `OPEN_WINDOWS` accounting guard. Decremented on drop so a panic
/// in the dialog thread still releases the window slot.
struct SlotGuard;
impl Drop for SlotGuard {
    fn drop(&mut self) {
        unsafe { release_slot() };
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
/// Language dropdown. Distinct id range from the dialog buttons so a
/// `WM_COMMAND` id match can't confuse the two.
const ID_COMBO_LANGUAGE: usize = 1100;

/// The dialog buttons, **in the order a user meets them**, so the list
/// doubles as the install flow: offer the runtime, watch it download,
/// and only then meet the failure dialogs. A reader who works down the
/// window sees the same sequence they would see in a real launch.
///
/// Hoisted to module scope so [`launcher_client_height`] can derive the
/// window height from `DIALOG_BUTTONS.len()` — adding a dialog here
/// resizes the window automatically.
///
/// The order here is presentation only. The `ID_BTN_*` values above are
/// deliberately **not** renumbered to match: they identify a control to
/// `WM_COMMAND`, so renumbering would be churn that could only ever
/// break something.
const DIALOG_BUTTONS: &[(&str, usize)] = &[
    // 1. The runtime offer. Everything below is downstream of it.
    ("Install prompt (custom-painted)", ID_BTN_PROMPT_V5),
    // 2. Then the download the prompt just agreed to.
    ("Progress (animated)", ID_BTN_PROGRESS_ANIM),
    ("Progress (static @ 50%)", ID_BTN_PROGRESS_STATIC),
    // 3. ...and the ways it can go wrong, in the order they can happen.
    ("Metadata failed", ID_BTN_METADATA_FAILED),
    ("Retry (try 2 of 3)", ID_BTN_RETRY),
    ("Error (post-install)", ID_BTN_ERROR),
    ("Java error (with update link)", ID_BTN_JAVA_ERROR),
    // 4. Before a payload is even read: the bare-stub bail-out.
    ("Early bail (MessageBoxW)", ID_BTN_EARLY_BAIL),
];

// ============================================================================
//  Localisation registry
// ============================================================================
//
//  Holds every bundle the user handed us via `--localisation`, in the
//  order it was discovered. The dropdown indexes straight into this
//  vector, so the combo's item `i` is always `LOCALISATIONS[i]`.
//
//  `en` is always present as the last entry: either the built-in
//  baseline, or the user's own `en` bundle when they supplied one —
//  which replaces the baseline rather than shadowing it, matching the
//  build-time rule in `snug-cli`'s `localization::collect`. Having it
//  last means the fallback is the baseline the launcher itself ships.

static LOCALISATIONS: RwLock<Vec<Localization>> = RwLock::new(Vec::new());

/// A snapshot of the registry, in dropdown order.
fn localisations() -> Vec<Localization> {
    LOCALISATIONS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Build the registry from the paths the user passed to
/// `--localisation`, and return it.
///
/// A path may be a single `snug-localisations.<tag>.txt` or a
/// directory of them; directory expansion is the same
/// `snug_format::discover_localization_files` the `snug` CLI uses, so
/// both resolve a given path identically.
///
/// The built-in English baseline is appended last unless the user
/// supplied their own `en`, in which case theirs takes the slot.
fn build_localisations(entries: &[std::path::PathBuf]) -> Result<Vec<Localization>, String> {
    log(&format!(
        "preview i18n: {} requested entry/entries: {}",
        entries.len(),
        entries
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    let files = snug_format::discover_localization_files(entries).map_err(|e| e.to_string())?;
    for path in &files {
        log(&format!(
            "preview i18n:   discovered {}",
            path.display()
        ));
    }

    let mut user: Vec<Localization> = Vec::new();
    for path in &files {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        let tag = snug_format::tag_from_path(path).ok_or_else(|| {
            format!(
                "extracting locale tag from {} (expected `snug-localisations.<tag>.txt`)",
                path.display()
            )
        })?;
        if user.iter().any(|b| b.tag == tag) {
            return Err(format!(
                "duplicate localisation tag `{tag}` — pass each tag exactly once"
            ));
        }
        let bundle =
            Localization::parse(&tag, &text).map_err(|e| format!("parsing {}: {e}", path.display()))?;
        log(&format!(
            "preview i18n:   parsed tag `{tag}` from {} ({} key/value lines)",
            path.display(),
            bundle.entries.len()
        ));
        user.push(bundle);
    }

    if user.iter().any(|b| b.tag == snug_format::DEFAULT_EN_TAG) {
        // The user's `en` is already the baseline; nothing to append.
        log(
            "preview i18n: user supplied an `en` bundle; built-in baseline NOT appended",
        );
        log(&format!(
            "preview i18n: final dropdown order: [{}]",
            user.iter()
                .map(|b| b.tag.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        return Ok(user);
    }

    let builtin = Localization::parse(
        snug_format::DEFAULT_EN_TAG,
        snug_format::DEFAULT_EN_TEXT,
    )
    .map_err(|e| format!("parsing the built-in English baseline: {e}"))?;
    log(&format!(
        "preview i18n: appended built-in baseline `{}`",
        snug_format::DEFAULT_EN_TAG
    ));
    user.push(builtin);
    log(&format!(
        "preview i18n: final dropdown order: [{}]",
        user.iter()
            .map(|b| b.tag.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    Ok(user)
}

/// Which registry entry the preview should open with: the **English
/// baseline**.
///
/// [`build_localisations`] guarantees an `en` entry always exists —
/// either the user's own bundle (which takes the baseline slot) or the
/// built-in one appended last. Defaulting to index 0 instead would pick
/// whichever tag happens to sort first, and `de` sorts before `en`, so
/// anyone with a German bundle landed on German copy and had to switch
/// back by hand before previewing the untranslated dialogs. The
/// baseline is what a shipped launcher falls back to, so it is the
/// honest starting state: translations are something you opt *into*
/// from the dropdown, not something you opt out of on launch.
///
/// The `unwrap_or(0)` is defensive only — `build_localisations` can't
/// return a list without an `en`.
fn default_index(bundles: &[Localization]) -> usize {
    bundles
        .iter()
        .position(|b| b.tag == snug_format::DEFAULT_EN_TAG)
        .unwrap_or(0)
}

/// Install a registry and make `index` the active language.
fn activate_localisations(bundles: Vec<Localization>, index: usize) {
    if let Ok(mut slot) = LOCALISATIONS.write() {
        *slot = bundles;
    }
    select_language(index);
}

/// Point the launcher's bundle chain at registry entry `index`.
///
/// The chain handed to `localize::set_bundles` is just that one
/// bundle: `Bundles::load` appends the built-in baseline behind it
/// automatically, so untranslated keys fall back to English exactly
/// as they would in a shipped build.
fn select_language(index: usize) {
    let bundles = localisations();
    if bundles.is_empty() {
        log(&format!("preview i18n: select_language({index}) ignored — registry empty"));
        return;
    }
    let chosen = bundles
        .get(index.min(bundles.len() - 1))
        .cloned()
        .unwrap_or_else(|| bundles[0].clone());
    log(&format!(
        "preview i18n: select_language({index}) -> tag `{}` ({} key/value lines), chain now [{}]",
        chosen.tag,
        chosen.entries.len(),
        bundles
            .iter()
            .map(|b| b.tag.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    // Force the chosen tag to the head of the chain. `set_bundles` filters
    // against the *machine's* detected locales, so on an English box
    // selecting `de` there would hand it one bundle the machine does
    // not want, `load` would drop it, and every dialog would stay
    // English no matter what was picked in the dropdown.
    localize::set_bundles_for(
        std::slice::from_ref(&chosen.tag),
        std::slice::from_ref(&chosen),
    );
}

// ============================================================================
//  `--icon` override
// ============================================================================

/// Pixel size we request for the in-dialog **mascot** image. The
/// mascot box is ~154 px, and the module's own `MASCOT_LOAD_CX` is 256
/// for the same reason: asking for the larger source beats enlarging
/// the 32 px title-bar icon. Matches the EXE-resource path so a
/// `--icon` run and a normal run look the same size.
const ICON_MASCOT_PX: i32 = 256;

/// GDI+ returns `Status` 0 for success. windows-sys 0.59 does not
/// export `GpStatusOk`, so it is spelled out once here.
const GP_OK: i32 = 0;

/// `GpWrapModeTileFlipXY` = 3, also not exported by windows-sys 0.59.
/// Without it GDI+ wraps the source edges when scaling down and can
/// bleed a dark border around the mascot; this mirrors them instead,
/// which is what every icon scaler does.
const GP_WRAP_TILE_FLIP_XY: i32 = 3;

/// A GDI+ shutdown token, so the one startup covers both conversions
/// (we build two HICONs: title-bar size and mascot size).
///
/// `GdiplusShutdown` takes the token **by value** while
/// `GdiplusStartup` hands it back through an out-pointer, so it has to
/// be an owned `usize` here -- unlike almost every other GDI+ handle.
struct Gdiplus {
    token: usize,
}

impl Drop for Gdiplus {
    fn drop(&mut self) {
        unsafe {
            GdiplusShutdown(self.token);
        }
    }
}

fn gdiplus_start() -> Result<Gdiplus, String> {
    let mut token: usize = 0;
    let input = GdiplusStartupInput {
        GdiplusVersion: 1,
        DebugEventCallback: 0,
        SuppressBackgroundThread: 0,
        SuppressExternalCodecs: 0,
    };
    let status = unsafe { GdiplusStartup(&mut token, &input, std::ptr::null_mut()) };
    if status != GP_OK {
        return Err(format!(
            "--icon: GdiplusStartup failed with status {status}; GDI+ is unavailable"
        ));
    }
    Ok(Gdiplus { token })
}

/// Render `path` into a `px`x`px` 32-bpp `HICON` via GDI+.
///
/// This is the PNG path. `LoadImageW` is documented to decode PNG from
/// file since Vista, but does not actually do so here: a 1254x1254
/// PNG returns NULL on every flag combination (verified directly),
/// while the identical call on a `.ico` succeeds. GDI+ has a real PNG
/// codec, so it is the only way to accept a PNG without linking a Rust
/// image crate into the shipped launcher -- which would fight the
/// "hundreds of KB, not megabytes" budget to serve a dev-only tool.
///
/// The standard GDI+ -> HICON recipe: decode to a `GpBitmap`, blit it
/// into a 32-bpp top-down DIB section so the alpha survives, then hand
/// that DIB plus a 1-bpp mask to `CreateIconIndirect`. A 32-bpp
/// `hbmColor` means Windows uses per-pixel alpha and ignores the mask,
/// so the mask only has to be a valid, non-null bitmap.
///
/// `CreateIconIndirect` does **not** copy the bitmaps, so both handles
/// are parked in a static for the process lifetime alongside the icon.
fn hicon_from_image_via_gdiplus(path: &std::path::Path, px: i32) -> Result<HICON, String> {
    use windows_sys::Win32::Graphics::Gdi::{
        CreateBitmap, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC,
        ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, ICONINFO};

    let src_w = wide(&path.to_string_lossy());
    let mut src: *mut GpBitmap = std::ptr::null_mut();

    unsafe {
        let status = GdipCreateBitmapFromFile(src_w.as_ptr(), &mut src);
        if status != GP_OK || src.is_null() {
            return Err(format!(
                "--icon: GDI+ could not decode `{}` (status {status}); expected a PNG or ICO",
                path.display()
            ));
        }
        let image: *mut GpImage = src.cast();

        let mut iw: u32 = 0;
        let mut ih: u32 = 0;
        GdipGetImageWidth(image, &mut iw);
        GdipGetImageHeight(image, &mut ih);
        if iw == 0 || ih == 0 {
            GdipDisposeImage(image);
            return Err(format!(
                "--icon: `{}` decoded to a zero-sized image",
                path.display()
            ));
        }

        // 32-bpp top-down DIB section, so we own the pixels and can
        // clear them to fully transparent before drawing.
        let screen = GetDC(std::ptr::null_mut());
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: px,
                // Negative height => top-down rows, the orientation the
                // alpha path expects.
                biHeight: -px,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [windows_sys::Win32::Graphics::Gdi::RGBQUAD {
                rgbBlue: 0,
                rgbGreen: 0,
                rgbRed: 0,
                rgbReserved: 0,
            }],
        };
        let color = CreateDIBSection(
            screen,
            &bi,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        );
        ReleaseDC(std::ptr::null_mut(), screen);
        if color.is_null() || bits.is_null() {
            GdipDisposeImage(image);
            return Err("--icon: CreateDIBSection failed".to_string());
        }
        // Transparent everywhere before the image lands.
        std::ptr::write_bytes(bits, 0, (px as usize) * (px as usize) * 4);

        let mask = CreateBitmap(px, px, 1, 1, std::ptr::null_mut());
        if mask.is_null() {
            DeleteObject(color);
            GdipDisposeImage(image);
            return Err("--icon: CreateBitmap failed for the icon mask".to_string());
        }

        let dc = CreateCompatibleDC(std::ptr::null_mut());
        if dc.is_null() {
            DeleteObject(color);
            DeleteObject(mask);
            GdipDisposeImage(image);
            return Err("--icon: CreateCompatibleDC failed".to_string());
        }
        let prev = SelectObject(dc, color);

        let mut graphics: *mut GpGraphics = std::ptr::null_mut();
        let mut attrs: *mut GpImageAttributes = std::ptr::null_mut();
        GdipCreateImageAttributes(&mut attrs);
        GdipSetImageAttributesWrapMode(attrs, GP_WRAP_TILE_FLIP_XY, 0, 0);

        if GdipCreateFromHDC(dc, &mut graphics) == GP_OK {
            // Bicubic downscale from the source's own size into the
            // target rect, so a 1254 px PNG lands crisp in a 256 px
            // mascot box rather than being cropped.
            GdipDrawImageRectRectI(
                graphics,
                image,
                0,
                0,
                px,
                px,
                0,
                0,
                iw as i32,
                ih as i32,
                UnitPixel,
                attrs,
                0,
                std::ptr::null_mut(),
            );
            GdipDeleteGraphics(graphics);
        }
        if !attrs.is_null() {
            GdipDisposeImageAttributes(attrs);
        }
        GdipDisposeImage(image);

        SelectObject(dc, prev);
        DeleteDC(dc);

        let ii = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let hicon = CreateIconIndirect(&ii);
        if hicon.is_null() {
            DeleteObject(color);
            DeleteObject(mask);
            return Err(format!(
                "--icon: CreateIconIndirect failed for `{}` at {px} px",
                path.display()
            ));
        }
        keep_icon_bitmaps(color, mask);
        Ok(hicon)
    }
}

/// Park the source bitmaps behind an `HICON` for the process lifetime:
/// `CreateIconIndirect` does not copy them, and a dialog thread can
/// still be painting the mascot long after the loading window is gone.
fn keep_icon_bitmaps(color: HBITMAP, mask: HBITMAP) {
    BITMAP_OWNERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((color as usize, mask as usize));
}

static BITMAP_OWNERS: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

/// Load `path` and install it as the process-wide icon override for
/// every dialog this preview opens.
///
/// Two decoders, tried in order, so the flag accepts what a user
/// actually has to hand:
///
/// 1. `LoadImageW(..., LR_LOADFROMFILE)` -- cheap, and the only thing
///    that handles `.ico`.
/// 2. GDI+ -- the only way to get a PNG, because Windows'
///    `LoadImageW` does not decode PNG from file on this platform
///    despite being documented to.
///
/// The `image` crate stays a build-only dependency either way, so the
/// shipped launcher's size is untouched.
///
/// Two sizes are built and kept separately, because the two slots want
/// different ones: the mascot box is ~154 px, the title bar wants the
/// system small-icon size. Building once per size avoids both a blurry
/// 32 px stretch in the mascot and a 256 px downscale in the title bar.
///
/// All handles are parked in statics and never destroyed -- the dialogs
/// are top-level windows on detached threads that can outlive `main`,
/// so the process is the only viable owner.
fn install_icon_override(path: &std::path::Path) -> Result<(), String> {
    if !path.is_file() {
        return Err(format!("--icon: `{}` is not a file", path.display()));
    }
    let display = path.display().to_string();
    let src = wide(&path.to_string_lossy());

    unsafe {
        // LR_DEFAULTSIZE takes the system small-icon metrics, which is
        // exactly what a title bar / taskbar wants at any DPI.
        let title = LoadImageW(
            std::ptr::null_mut(),
            src.as_ptr(),
            IMAGE_ICON,
            0,
            0,
            LR_DEFAULTSIZE | LR_LOADFROMFILE,
        );
        if !title.is_null() {
            let mascot = LoadImageW(
                std::ptr::null_mut(),
                src.as_ptr(),
                IMAGE_ICON,
                ICON_MASCOT_PX,
                ICON_MASCOT_PX,
                LR_LOADFROMFILE,
            );
            if mascot.is_null() {
                return Err(format!(
                    "--icon: `{display}` is a .ico, but it has no {ICON_MASCOT_PX} px \
                     entry for the mascot box; re-save it with a {ICON_MASCOT_PX} px image"
                ));
            }
            activate_icon_override(title, mascot, &format!("{display} (via LoadImageW)"));
            return Ok(());
        }
    }

    // Anything LoadImageW would not take -- in practice, a PNG.
    let _gdi = gdiplus_start()?;
    let title_px = unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXICON};
        GetSystemMetrics(SM_CXICON)
    };
    let title = hicon_from_image_via_gdiplus(path, title_px)?;
    let mascot = hicon_from_image_via_gdiplus(path, ICON_MASCOT_PX)?;
    activate_icon_override(
        title,
        mascot,
        &format!("{display} (via GDI+, title bar {title_px} px)"),
    );
    Ok(())
}

/// Park the two icons and point the launcher and its dialogs at them.
fn activate_icon_override(title: HICON, mascot: HICON, what: &str) {
    TITLE_ICON.store(title as usize, Ordering::SeqCst);
    MASCOT_ICON.store(mascot as usize, Ordering::SeqCst);
    snug_launcher::jdk_install::set_window_icon_override(title as isize);
    snug_launcher::jdk_install::set_mascot_icon_override(mascot as isize);
    log(&format!(
        "preview icon: override installed from {what} (mascot slot {ICON_MASCOT_PX} px)"
    ));
}

/// Process-lifetime homes for the two `HICON`s. `HICON` is a raw
/// pointer, which is neither `Send` nor storable in a `OnceLock` we
/// could later read back by value, so the bit pattern is kept as a
/// `usize` and cast on read — the same trick `header_icon` uses. The
/// load still happens exactly once, in [`install_icon_override`].
static TITLE_ICON: AtomicUsize = AtomicUsize::new(0);
static MASCOT_ICON: AtomicUsize = AtomicUsize::new(0);

// ============================================================================
//  Command line
// ============================================================================
//
//  Deliberately hand-rolled rather than pulling in `clap`: this binary
//  exists so the *launcher's* dialogs can be eyeballed without
//  rebuilding anything, and a dev-only bin is the wrong place to add a
//  dependency that would land in the launcher release profile's build
//  graph. The parse surface is two flags, so a match statement is
//  shorter than the struct `clap` would want.
//
//  Parsing is split from `main` and takes an `IntoIterator` so the
//  behaviour is testable without spawning a process — which also means
//  `--help` is verifiable in CI on a headless runner, where actually
//  launching the Win32 message loop is not.

/// What `snug_preview` should do with the arguments it was given.
#[derive(Debug, PartialEq, Eq)]
enum Cli {
    /// Pop the launcher window and wait for its message loop.
    Run {
        localisations: Vec<std::path::PathBuf>,
        icon: Option<std::path::PathBuf>,
    },
    /// Print [`help_text`] on stdout and exit 0.
    ShowHelp,
    /// Unrecognised input: report it, print usage on stderr, exit 2.
    Error(String),
}

/// Parse the arguments after the executable name.
///
/// `--localisation` and `--localization` are accepted as aliases of
/// each other. The CLI elsewhere is American-spelled
/// (`--localization`, `localisations/` for the scaffold directory is
/// the one British holdout), but this tool is used interactively by
/// people who type either, and a flag that only answers to one
/// spelling is a small papercut with no upside.
fn parse_args<I, S>(args: I) -> Cli
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut localisations: Vec<std::path::PathBuf> = Vec::new();
    let mut icon: Option<std::path::PathBuf> = None;
    let mut it = args.into_iter().peekable();
    while let Some(arg) = it.next() {
        match arg.as_ref() {
            "-h" | "--help" => return Cli::ShowHelp,
            "--localisation" | "--localization" => {
                // Both spellings of the flag may be repeated; they
                // accumulate into one list, same as the shipped CLI's
                // repeatable `--localization`.
                match it.next() {
                    Some(value) => localisations.push(std::path::PathBuf::from(value.as_ref())),
                    None => {
                        return Cli::Error(format!(
                            "{} requires a <FILE|DIR> argument",
                            arg.as_ref()
                        ));
                    }
                }
            }
            "--icon" => match it.next() {
                Some(value) => icon = Some(std::path::PathBuf::from(value.as_ref())),
                None => return Cli::Error("--icon requires a <FILE> argument".to_string()),
            },
            other => {
                // A bare value is as unexpected as a bad flag: every
                // option that takes a value names it explicitly.
                return Cli::Error(format!("unrecognised argument: {other}"));
            }
        }
    }
    Cli::Run { localisations, icon }
}

/// The `--help` body.
///
/// The dialog list is generated from [`DIALOG_BUTTONS`] rather than
/// written out, so adding a preview button and forgetting to document
/// it is not a state this file can be in — the same reason
/// [`launcher_client_height`] reads that list. The module docs note
/// that `cargo run --example dialogs_preview -- --kind <KIND>` is the
/// way to open exactly one dialog; these labels are the set that
/// `KIND` accepts.
fn help_text() -> String {
    let mut out = String::new();
    out.push_str("snug_preview — one-click dialog preview for the snug launcher\n\n");
    out.push_str("USAGE:\n    snug_preview [OPTIONS]\n\n");
    out.push_str("OPTIONS:\n");
    out.push_str("    -h, --help    Print this help and exit.\n");
    out.push_str("                  With no options, the launcher window opens with one\n");
    out.push_str("                  button per dialog kind below.\n\n");
    out.push_str("        --localisation <FILE|DIR>    Load localisations to preview, then pick\n");
    out.push_str("        --localization <FILE|DIR>    from the Language dropdown. Repeatable.\n");
    out.push_str("            <FILE>  snug-localisations.<tag>.txt\n");
    out.push_str("            <DIR>   a directory of them, scanned top-level only\n");
    out.push('\n');
    out.push_str("            Both spellings of the flag are accepted. The built-in\n");
    out.push_str("            English baseline is always in the dropdown; supplying\n");
    out.push_str("            your own snug-localisations.en.txt replaces it, the\n");
    out.push_str("            same rule the build applies to a shipped launcher.\n\n");
    out.push_str("        --icon <FILE>            Swap the dialog icons for a test. Overrides\n");
    out.push_str("                                the EXE's MAINICON resource, which the\n");
    out.push_str("                                dialogs read for both the mascot image\n");
    out.push_str("                                and the title bar / taskbar.\n");
    out.push('\n');
    out.push_str("                                The launcher window's own header keeps\n");
    out.push_str("                                the Snug mascot in every case. Your\n");
    out.push_str("                                icon is painted small in the mascot's\n");
    out.push_str("                                bottom-right corner, under the\n");
    out.push_str("                                magnifying glass it is holding, so you\n");
    out.push_str("                                can see it in context.\n");
    out.push('\n');
    out.push_str("                                <FILE>  a .png or .ico. .ico goes through\n");
    out.push_str("                                LoadImageW; a .png needs GDI+, because\n");
    out.push_str("                                Windows' LoadImageW does not decode\n");
    out.push_str("                                PNG from file despite being documented\n");
    out.push_str("                                to. Either way no Rust image crate is\n");
    out.push_str("                                linked into the launcher.\n");
    out.push('\n');
    out.push_str("                                Preview-only: this tool never ships.\n\n");
    out.push_str("DIALOGS:\n");
    for (label, _) in DIALOG_BUTTONS {
        out.push_str(&format!("    {label}\n"));
    }
    out.push('\n');
    out.push_str("NOTES:\n");
    out.push_str("    Each dialog is detached — they are top-level windows, so\n");
    out.push_str("    closing the launcher does not destroy them and you can open\n");
    out.push_str("    several side by side. The process exits only when every\n");
    out.push_str("    open window has been dismissed.\n\n");
    out.push_str("    Changing the language affects dialogs opened afterwards;\n");
    out.push_str("    windows already on screen keep the copy they were built\n");
    out.push_str("    with. Close and reopen them to see the new language.\n\n");
    out.push_str("    The window is per-monitor DPI aware: drag it between\n");
    out.push_str("    displays of different scale and the layout rescales.\n\n");
    out.push_str("    Open one dialog directly with:\n");
    out.push_str("        cargo run --example dialogs_preview -- --kind <KIND>\n");
    out
}

// ============================================================================
//  Entry point
// ============================================================================

pub fn run() {
    // Handled before any Win32 work: `--help` must work on a headless
    // CI runner and must not open a window that then blocks forever.
    // Localisation loading is here too, for the same reason — a bad
    // path should fail loudly in the terminal, not behind a window
    // that swallows the message.
    let (requested, icon) = match parse_args(std::env::args().skip(1)) {
        Cli::ShowHelp => {
            print!("{}", help_text());
            return;
        }
        Cli::Error(msg) => {
            eprintln!("snug_preview: {msg}");
            eprintln!("Try 'snug_preview --help' for more information.");
            std::process::exit(2);
        }
        Cli::Run {
            localisations,
            icon,
        } => (localisations, icon),
    };

    // Icon override first: it must be in place before any window is
    // created, because `register_class` and `CreateWindowExW` both read
    // it. Errors exit here, in the terminal, rather than behind a
    // window that would swallow the message.
    if let Some(path) = &icon {
        if let Err(msg) = install_icon_override(path) {
            eprintln!("snug_preview: {msg}");
            std::process::exit(2);
        }
    }

    // Seed the registry before the window exists, so `WM_CREATE` can
    // populate the dropdown from a fully-built list. With no
    // `--localisation` this is just the built-in English baseline.
    let bundles = match build_localisations(&requested) {
        Ok(bundles) => bundles,
        Err(msg) => {
            eprintln!("snug_preview: {msg}");
            std::process::exit(2);
        }
    };
    // Open on the English baseline rather than the first tag found —
    // see `default_index` for why index 0 was the wrong choice.
    let start = default_index(&bundles);
    log(&format!(
        "preview i18n: default_index -> {start} (tag `{}`)",
        bundles[start].tag
    ));
    activate_localisations(bundles, start);

    // Before any window exists. Paired with `scale_px` at every
    // geometry site below — on its own this call would make the
    // window physically too small, which is the opposite of the
    // intent. Logged rather than enforced: failing to become aware
    // is survivable (everything just stays unscaled), so it is not a
    // reason to refuse to start.
    let aware = enable_dpi_awareness();
    let dpi = system_dpi();
    log(&format!(
        "preview dpi: process DPI-aware = {aware}; initial geometry at {dpi} DPI"
    ));

    unsafe {
        // Capture the launcher main thread id up front so the
        // window-counter accounting can post `WM_QUIT` to *this*
        // thread when the last dialog (and the launcher itself)
        // closes. Posting to the calling thread — which is what
        // `PostQuitMessage` does — would target a spawned dialog
        // thread instead, leaving the main thread blocked forever
        // once the launcher was closed.
        MAIN_THREAD_ID.store(GetCurrentThreadId(), Ordering::SeqCst);

        let hinst = GetModuleHandleW(std::ptr::null());
        let class_name_w = wide(CLASS_NAME);

        // Pull the EXE's embedded MAINICON at 32×32 (title bar /
        // Alt-Tab) and 16×16 (taskbar / window corner). `LR_SHARED`
        // keeps these handles valid for the lifetime of the module —
        // no DestroyIcon needed, even at process exit.
        let (hicon_class, hicon_sm_class) = load_exe_main_icons(hinst);

        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            // No `CS_HREDRAW | CS_VREDRAW` — the window is fixed
            // size, no resize; no flicker.
            style: 0,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: hicon_class,
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            // NULL_BRUSH — we paint the entire background in
            // WM_PAINT so the system default-class brush doesn't
            // flash through.
            hbrBackground: std::ptr::null_mut() as HBRUSH,
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name_w.as_ptr(),
            hIconSm: hicon_sm_class,
        };
        let _atom = RegisterClassExW(&wc);

        // `LAUNCHER_W` / `launcher_client_height()` describe the
        // **client area** in logical pixels. `AdjustWindowRectExForDpi`
        // scales both the client size and the title-bar / non-client
        // chrome together — the plain `AdjustWindowRectEx` assumes
        // 96 DPI for the chrome, which on a 150% display gives a body
        // sized for one scale inside a title bar drawn for another.
        let mut client_rect = RECT {
            left: 0,
            top: 0,
            right: scale_px(LAUNCHER_W, dpi),
            bottom: scale_px(launcher_client_height(), dpi),
        };
        AdjustWindowRectExForDpi(
            &mut client_rect,
            WS_CAPTION | WS_SYSMENU | WS_OVERLAPPED,
            0,
            WS_EX_TOPMOST,
            dpi,
        );
        let win_w = client_rect.right - client_rect.left;
        let win_h = client_rect.bottom - client_rect.top;

        let sw = GetSystemMetrics(SM_CXSCREEN);
        let sh = GetSystemMetrics(SM_CYSCREEN);
        let x = (sw - win_w) / 2;
        let y = (sh - win_h) / 2;

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST,
            class_name_w.as_ptr(),
            wide(WINDOW_TITLE).as_ptr(),
            WS_CAPTION | WS_SYSMENU | WS_OVERLAPPED | WS_VISIBLE,
            x,
            y,
            win_w,
            win_h,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );

        if hwnd.is_null() {
            eprintln!("snug-preview: CreateWindowExW failed");
            std::process::exit(1);
        }

        // Belt-and-suspenders: WNDCLASSEXW.hIcon/hIconSm is supposed
        // to drive the window's title-bar / taskbar icon, but a few
        // shell compositors (and the PS API queries above) report
        // 0 for the class icon even when one was registered. Forcing
        // WM_SETICON with the same handles per-window is the
        // canonical fix — WM_SETICON's `hIcon` parameter is the
        // authoritative source that Explorer and Taskbar read.
        //
        // ICON_BIG = 1 (title bar / Alt-Tab), ICON_SMALL = 0
        // (taskbar overlay + window corner). We use the same handles
        // we loaded into the class — they stay valid for the
        // module lifetime via `LR_SHARED`, no DestroyIcon needed.
        SendMessageW(hwnd, WM_SETICON, ICON_BIG as usize, hicon_class as isize);
        SendMessageW(hwnd, WM_SETICON, ICON_SMALL as usize, hicon_sm_class as isize);

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
            // The window's own DPI, not the process one: this can be
            // the first thing to run after a `WM_DPICHANGED` resize.
            let dpi = window_dpi(hwnd);
            let fonts = fonts_for(dpi);
            let hinst = GetModuleHandleW(std::ptr::null());

            // Language dropdown, above the dialog buttons. Populated
            // from the registry seeded in `main` before the window
            // existed, so item `i` is always `localisations()[i]`.
            let combo = CreateWindowExW(
                0,
                wide("COMBOBOX").as_ptr(),
                std::ptr::null(),
                WS_CHILD
                    | WS_VISIBLE
                    | WS_TABSTOP
                    | WS_VSCROLL
                    | (CBS_DROPDOWNLIST as u32),
                scale_px(COMBO_X, dpi),
                scale_px(COMBO_Y, dpi),
                scale_px(COMBO_W, dpi),
                scale_px(COMBO_H, dpi),
                hwnd,
                ID_COMBO_LANGUAGE as *mut _,
                hinst,
                std::ptr::null(),
            );
            if !combo.is_null() {
                SendMessageW(combo, WM_SETFONT, fonts.body() as usize, 1);
                SendMessageW(combo, CB_RESETCONTENT, 0, 0);
                let bundles = localisations();
                for bundle in &bundles {
                    SendMessageW(
                        combo,
                        CB_ADDSTRING,
                        0,
                        wide(&bundle.tag).as_ptr() as isize,
                    );
                }
                // Pre-select whatever `main` already activated. Both
                // sides derive the index the same way, so the combo
                // and the live bundle chain can't disagree.
                let start = default_index(&bundles);
                SendMessageW(combo, CB_SETCURSEL, start, 0);
                let added = SendMessageW(combo, CB_GETCOUNT, 0, 0);
                log(&format!(
                    "preview i18n: combo filled - CB_GETCOUNT={added}, CB_SETCURSEL={start} (tag `{}`); items: [{}]",
                    bundles.get(start).map(|b| b.tag.as_str()).unwrap_or("<none>"),
                    bundles
                        .iter()
                        .map(|b| b.tag.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }

            // Body buttons. Labels + control IDs come from the
            // module-level `DIALOG_BUTTONS` list so the window height
            // (see `launcher_client_height`) can't drift out of sync
            // with the row count. `hinst` came from above, where the
            // dropdown needed it.
            let buttons: &[(&str, usize)] = DIALOG_BUTTONS;
            for (i, (label, id)) in buttons.iter().enumerate() {
                let y = BTN_FIRST_Y + (i as i32) * (BTN_H + BTN_GAP);
                let btn = CreateWindowExW(
                    0,
                    wide("BUTTON").as_ptr(),
                    wide(label).as_ptr(),
                    WS_CHILD | WS_VISIBLE | (BS_PUSHBUTTON as u32),
                    scale_px(BTN_X, dpi),
                    scale_px(y, dpi),
                    scale_px(BTN_W, dpi),
                    scale_px(BTN_H, dpi),
                    hwnd,
                    *id as *mut _,
                    hinst,
                    std::ptr::null(),
                );
                if !btn.is_null() {
                    SendMessageW(btn, WM_SETFONT, fonts.body() as usize, 1);
                }
            }

            // Close button — bottom right, positioned relative to the
            // last dialog row so it stays put as the list grows.
            let close = CreateWindowExW(
                0,
                wide("BUTTON").as_ptr(),
                wide("Close").as_ptr(),
                WS_CHILD | WS_VISIBLE | (BS_PUSHBUTTON as u32),
                scale_px(close_x(), dpi),
                scale_px(close_y(), dpi),
                scale_px(BTN_CLOSE_W, dpi),
                scale_px(BTN_CLOSE_H, dpi),
                hwnd,
                ID_BTN_CLOSE as *mut _,
                hinst,
                std::ptr::null(),
            );
            if !close.is_null() {
                SendMessageW(close, WM_SETFONT, fonts.body() as usize, 1);
            }

            0
        },
        WM_PAINT => unsafe {
            let dpi = window_dpi(hwnd);
            let fonts = fonts_for(dpi);
            let hinst = GetModuleHandleW(std::ptr::null());
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);

            SetBkMode(hdc, TRANSPARENT as i32);

            // 0. Background. The window class registers a NULL brush,
            //    which tells Windows not to erase the client area at
            //    all — so without this fill the window shows whatever
            //    was in those pixels beforehand, and the rounded
            //    `--icon` plate below has no guaranteed colour to sit
            //    on. `FillRect` takes the brush by value, so it is
            //    created and destroyed per paint rather than cached:
            //    brushes are cheap next to the `CreateFontW` that
            //    used to happen here.
            let bg = CreateSolidBrush(COLOR_BG);
            let mut client: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut client);
            FillRect(hdc, &client, bg);
            DeleteObject(bg as _);

            // Everything below is positioned from the *logical*
            // constants, converted here once per use.
            let mascot_x = scale_px((LAUNCHER_W - HEADER_ICON_SIZE) / 2, dpi);
            let mascot_y = scale_px(HEADER_ICON_Y, dpi);
            let mascot_side = scale_px(HEADER_ICON_SIZE, dpi);

            // 1. Mascot — centred horizontally at the top of the
            //    client area. Always the EXE's MAINICON and never the
            //    `--icon` override: it is the inspector, not the
            //    thing being inspected. The HICON is loaded once via
            //    the `OnceLock` in `header_icon()` and reused on
            //    every subsequent paint (LR_SHARED keeps the handle
            //    valid for the lifetime of the EXE module, so no
            //    DestroyIcon is needed).
            let mascot = header_icon(hinst);
            if !mascot.is_null() {
                DrawIconEx(
                    hdc,
                    mascot_x,
                    mascot_y,
                    mascot,
                    mascot_side,
                    mascot_side,
                    0,
                    std::ptr::null_mut(),
                    DI_NORMAL,
                );
            }

            // 2. The `--icon` candidate, small, in the mascot's
            //    bottom-right corner — under the magnifying glass
            //    the mascot holds. Drawn only when an override was
            //    installed, so a plain run still shows the mascot
            //    alone.
            //
            //    `MASCOT_ICON` rather than the title-bar handle: that
            //    is the 256 px build, so a ~42 px draw is a downscale
            //    and stays crisp — the same reason the dialogs take
            //    it for their mascot slot.
            let candidate = MASCOT_ICON.load(Ordering::SeqCst) as HICON;
            if !candidate.is_null() {
                let icon_rect = inspected_icon_rect(
                    (LAUNCHER_W - HEADER_ICON_SIZE) / 2,
                    HEADER_ICON_Y,
                    HEADER_ICON_SIZE,
                    dpi,
                );
                let outer = inspected_plate_rect(icon_rect, dpi);
                let (l, t, r, b) = outer;
                let radius = scale_px(INSPECTED_PLATE_RADIUS, dpi);
                // Two nested regions, not `FillRgn` + `FrameRgn`.
                //
                // `FrameRgn` turned out to draw nothing usable here:
                // with `dx`/`dy` of 0 its border is one device unit,
                // and on a plate that is white against a white mascot
                // and a white window that hairline is invisible.
                // Verified rather than assumed — rendering the ring in
                // solid red and the frame in black showed the fill and
                // no frame at all.
                //
                // Filling a larger ring and then a smaller plate gives
                // an explicit thickness that scales with the display,
                // which is what "put a border around it" means.
                let ring = CreateRoundRectRgn(l, t, r, b, radius, radius);
                let (il, it, ir, ib) = inspected_plate_inner(outer, dpi);
                let inner_radius = (radius - scale_px(INSPECTED_PLATE_BORDER, dpi)).max(1);
                let plate = CreateRoundRectRgn(il, it, ir, ib, inner_radius, inner_radius);
                if !ring.is_null() && !plate.is_null() {
                    let edge = CreateSolidBrush(COLOR_PLATE_EDGE);
                    FillRgn(hdc, ring, edge);
                    DeleteObject(edge as _);
                    let fill = CreateSolidBrush(COLOR_BG);
                    FillRgn(hdc, plate, fill);
                    DeleteObject(fill as _);
                }
                if !ring.is_null() {
                    DeleteObject(ring as _);
                }
                if !plate.is_null() {
                    DeleteObject(plate as _);
                }
                let (cx, cy, cside) = icon_rect;
                DrawIconEx(
                    hdc,
                    cx,
                    cy,
                    candidate,
                    cside,
                    cside,
                    0,
                    std::ptr::null_mut(),
                    DI_NORMAL,
                );
            }

            // 3. Heading — the window's one line of real hierarchy,
            //    in the semibold 12pt face. The window title bar
            //    already names the tool, so this exists to separate
            //    the brand header from the explanatory copy below it.
            let prev_font = SelectObject(hdc, fonts.heading() as _);
            SetTextColor(hdc, COLOR_HEADING);
            let mut rect_heading = RECT {
                left: scale_px(BTN_X, dpi),
                top: scale_px(HEADING_Y, dpi),
                right: scale_px(LAUNCHER_W - BTN_X, dpi),
                bottom: scale_px(HEADING_Y + HEADING_H, dpi),
            };
            DrawTextW(
                hdc,
                wide(HEADING_TEXT).as_ptr(),
                -1,
                &mut rect_heading,
                DT_LEFT | DT_SINGLELINE | DT_VCENTER,
            );

            // 4. Subtitle — wrapped across the full content width.
            //    `DT_WORDBREAK` rather than `DT_SINGLELINE`, because
            //    the copy is two sentences' worth and clipping the
            //    second would leave the detachment behaviour
            //    undocumented exactly where a user looks first.
            SelectObject(hdc, fonts.body() as _);
            SetTextColor(hdc, COLOR_BODY);
            let mut rect_sub = RECT {
                left: scale_px(BTN_X, dpi),
                top: scale_px(SUBTITLE_Y, dpi),
                right: scale_px(LAUNCHER_W - BTN_X, dpi),
                bottom: scale_px(SUBTITLE_Y + SUBTITLE_H, dpi),
            };
            DrawTextW(
                hdc,
                wide(SUBTITLE_TEXT).as_ptr(),
                -1,
                &mut rect_sub,
                DT_LEFT | DT_WORDBREAK,
            );

            // 5. Language label — to the LEFT of the dropdown, on the
            //    same row and vertically centred against it. The
            //    band is the combo's own client rect (its closed
            //    field sizes itself to the font, so asking beats
            //    hardcoding a second set of Y constants that would
            //    drift on any font or DPI change). `DT_VCENTER`
            //    centres the text in that band; the rect is clipped
            //    to the label's reserved width so a long label can
            //    never bleed under the dropdown.
            let combo = GetDlgItem(hwnd, ID_COMBO_LANGUAGE as i32);
            let combo_top = scale_px(COMBO_Y, dpi);
            let mut combo_band = RECT {
                left: 0,
                top: combo_top,
                right: 0,
                bottom: combo_top + scale_px(COMBO_H, dpi),
            };
            if !combo.is_null() {
                let mut cr: RECT = std::mem::zeroed();
                if GetClientRect(combo, &mut cr) != 0 && cr.bottom > cr.top {
                    combo_band.top = combo_top + cr.top;
                    combo_band.bottom = combo_top + cr.bottom;
                }
            }
            let mut rect_combo_label = RECT {
                left: scale_px(COMBO_LABEL_X, dpi),
                top: combo_band.top,
                right: scale_px(COMBO_X, dpi),
                bottom: combo_band.bottom,
            };
            DrawTextW(
                hdc,
                wide(COMBO_LABEL_TEXT).as_ptr(),
                -1,
                &mut rect_combo_label,
                DT_LEFT | DT_SINGLELINE | DT_VCENTER,
            );

            SelectObject(hdc, prev_font);
            EndPaint(hwnd, &ps);
            0
        },
        WM_COMMAND => unsafe {
            // LOWORD(wparam) is the control / menu id. Mask off
            // the notification code in the high word.
            let id = wparam & 0xFFFF;
            let notification = (wparam >> 16) & 0xFFFF;

            // The dropdown only means something on a selection change
            // — `CBN_SELENDOK` and friends would be redundant work.
            if id == ID_COMBO_LANGUAGE && notification == CBN_SELCHANGE as usize {
                let combo = GetDlgItem(hwnd, ID_COMBO_LANGUAGE as i32);
                if !combo.is_null() {
                    let index = SendMessageW(combo, CB_GETCURSEL, 0, 0);
                    if index >= 0 {
                        // Swaps the launcher's bundle chain and bumps
                        // `localize::generation`, which invalidates the
                        // cached `Dialogs`. Dialogs opened from here on
                        // render in the new language.
                        select_language(index as usize);
                    }
                }
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }

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
                        "java.lang.UnsatisfiedLinkError: C:\\Users\\demo\\.snug\\jdk\\jdk-25\\bin\\jvm.dll: Can't find dependent libraries",
                    );
                }),
                ID_BTN_JAVA_ERROR => spawn_dialog(|| {
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
                    DestroyWindow(hwnd);
                }
                _ => {}
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        },
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
        WM_DPICHANGED => unsafe {
            // `lparam` is the rectangle Windows wants the window moved
            // to. Honouring that rather than recomputing from our own
            // client size is what keeps the window on the monitor the
            // user just dragged it to, instead of snapping to a size
            // that belonged to the old scale factor.
            //
            // `wparam` carries the new DPI in both halves, but after
            // the move `window_dpi` is authoritative — and it is the
            // same value `WM_PAINT` will read a moment later, so the
            // layout cannot disagree with what gets drawn.
            let suggested = &*(lparam as *const RECT);
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                suggested.left,
                suggested.top,
                suggested.right - suggested.left,
                suggested.bottom - suggested.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let dpi = window_dpi(hwnd);
            // Controls first, then fonts: a control repositioned while
            // still holding the previous DPI's font measures its text
            // against the wrong metrics, and `MoveWindow` on a combo
            // can re-measure its dropped-list height.
            layout_controls(hwnd, dpi);
            apply_fonts(hwnd, dpi);
            InvalidateRect(hwnd, std::ptr::null(), 1);
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
            // decrements the counter to 0 and posts `WM_QUIT` to the
            // main thread (via `release_slot`), so the launcher's
            // message loop exits cleanly.
            unsafe {
                release_slot();
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

/// Drives the install-prompt dialog via the same `prompt_window`
/// path the launcher would use if the prompt were ever wired into
/// production. `mascot_hbitmap = 0` makes `modal_window` fall back
/// to the EXE's main icon, so the preview shows the brand-correct
/// Snug character without us having to load anything here.
fn run_install_prompt_v5() {
    let d = dialogs::dialogs();
    let prompt = &d.jdk_install.prompt;
    let choice = unsafe {
        snug_launcher::prompt_window::show(
            std::ptr::null_mut(),
            0,
            prompt,
            "21.0.2",
            192,
            "9c629caaccc4e64aa0ea58bd0a3f43eaf903a4c1a3e2c2a6e9c5b1a8e8b3f1a0",
        )
    };
    eprintln!("snug-preview: install-prompt choice = {choice:?}");
}

/// Mirror of `main.rs::show_error_box` — the `MessageBoxW` shown when
/// the launcher can't even load its embedded payload.
fn run_early_bail() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

    // Mirrors `localize::lookup("launcher.fallback_messagebox.title")`
    // in `main.rs::show_error_box`. That key resolves from the built-in
    // English baseline on this path (`localize::init` has not run yet),
    // so read the baseline rather than hardcoding the string — keeps the
    // preview in step if the key is ever retitled.
    let title = snug_launcher::localize::lookup("launcher.fallback_messagebox.title");
    let body = "FATAL: failed to read snug payload from RCDATA resource.\n\
                This binary may be corrupted or stamped with the wrong manifest.\n\
                Re-download the launcher from the original source.";

    let result = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide(body).as_ptr(),
            wide(&title).as_ptr(),
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

// ============================================================================
//  DPI
// ============================================================================

/// Opt into per-monitor DPI awareness, before any window exists.
///
/// Without this Windows bitmap-stretches the window on a 125–150%
/// display: correctly sized, but soft text. The awareness *call* alone
/// would be worse than nothing — it stops the stretch while leaving the
/// logical constants unscaled, so the window would come out physically
/// too small. The scaling half is `scale_px`, applied at every call
/// site.
///
/// Returns whether the process actually became aware. A `bool` rather
/// than a panic because awareness can legitimately fail — a manifest
/// that already set it, or a pre-1703 Windows without
/// `SetProcessDpiAwarenessContext` — and neither is worth taking this
/// dev tool down over. The layout degrades to unscaled, which is
/// exactly the behaviour before this change.
fn enable_dpi_awareness() -> bool {
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) != 0 }
}

/// Process DPI, floored at 96.
///
/// `GetDpiForSystem` is Windows 10 1703+. On anything older it returns
/// 0, and `.max(96)` turns "unknown" into "unscaled" rather than into a
/// zero-sized window. Used for the *initial* geometry, before there is a
/// window to ask.
fn system_dpi() -> u32 {
    (unsafe { GetDpiForSystem() as u32 }).max(96)
}

/// This window's own DPI, which is what `WM_DPICHANGED` exists to keep
/// current as the window is dragged between monitors.
fn window_dpi(hwnd: HWND) -> u32 {
    (unsafe { GetDpiForWindow(hwnd) as u32 }).max(96)
}

// ============================================================================
//  Fonts
// ============================================================================
//
//  Cached per DPI rather than built per paint.
//
//  The previous `create_button_font()` was called from `WM_PAINT` and
//  its `HFONT` was never deleted, so every repaint — and a repaint
//  happens on every focus change, every hover over a native button, and
//  every language switch — leaked one GDI font object for the life of
//  the process. Caching by DPI fixes the leak and is cheaper anyway:
//  the font is a function of the scale factor and nothing else.

/// `HFONT` is a raw pointer, which is not `Send`, so the handles are
/// held as bit patterns and cast on read — the same trick `header_icon`
/// uses for `HICON`.
struct UiFonts {
    body: usize,
    heading: usize,
}

impl UiFonts {
    fn body(&self) -> HFONT {
        self.body as HFONT
    }
    fn heading(&self) -> HFONT {
        self.heading as HFONT
    }
}

/// The live pair and the DPI it was built for.
static UI_FONTS: Mutex<Option<(u32, UiFonts)>> = Mutex::new(None);

/// The fonts for `dpi`, rebuilt — and the previous pair released — only
/// when the DPI actually changes.
fn fonts_for(dpi: u32) -> UiFonts {
    let mut slot = UI_FONTS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((built, fonts)) = slot.as_ref() {
        if *built == dpi {
            return UiFonts {
                body: fonts.body,
                heading: fonts.heading,
            };
        }
        // Release the old pair. Dropping the cache without deleting the
        // handles is the same leak in a slower form, and this is the
        // only path that could ever accumulate one.
        unsafe {
            DeleteObject(fonts.body as _);
            DeleteObject(fonts.heading as _);
        }
    }
    let fonts = UiFonts {
        body: unsafe { create_font_pt(BODY_PT, FONT_WEIGHT_NORMAL, dpi) as usize },
        heading: unsafe { create_font_pt(HEADING_PT, FONT_WEIGHT_SEMIBOLD, dpi) as usize },
    };
    *slot = Some((
        dpi,
        UiFonts {
            body: fonts.body,
            heading: fonts.heading,
        },
    ));
    fonts
}

/// Push `dpi`'s fonts onto every child control.
///
/// The initial build and `WM_DPICHANGED` both need this, and a control
/// that misses the `WM_SETFONT` keeps rendering in the *old* DPI's font
/// — the classic half-resized-window artefact — so there is exactly one
/// list of children rather than one per caller.
fn apply_fonts(hwnd: HWND, dpi: u32) {
    let fonts = fonts_for(dpi);
    let mut ids: Vec<i32> = vec![ID_COMBO_LANGUAGE as i32, ID_BTN_CLOSE as i32];
    ids.extend(DIALOG_BUTTONS.iter().map(|(_, id)| *id as i32));
    for id in ids {
        let child = unsafe { GetDlgItem(hwnd, id) };
        if !child.is_null() {
            unsafe { SendMessageW(child, WM_SETFONT, fonts.body() as usize, 1) };
        }
    }
}

/// Re-place every child control for `dpi`.
///
/// Reads the same constants `WM_CREATE` does rather than a parallel set
/// of scaled copies, so a DPI change cannot leave the window laid out
/// for a scale factor nothing else is using.
unsafe fn layout_controls(hwnd: HWND, dpi: u32) {
    let place = |id: i32, x: i32, y: i32, w: i32, h: i32| {
        let child = unsafe { GetDlgItem(hwnd, id) };
        if !child.is_null() {
            unsafe {
                MoveWindow(
                    child,
                    scale_px(x, dpi),
                    scale_px(y, dpi),
                    scale_px(w, dpi),
                    scale_px(h, dpi),
                    1,
                );
            }
        }
    };

    place(
        ID_COMBO_LANGUAGE as i32,
        COMBO_X,
        COMBO_Y,
        COMBO_W,
        COMBO_H,
    );
    for (i, (_, id)) in DIALOG_BUTTONS.iter().enumerate() {
        place(
            *id as i32,
            BTN_X,
            BTN_FIRST_Y + i as i32 * (BTN_H + BTN_GAP),
            BTN_W,
            BTN_H,
        );
    }
    place(
        ID_BTN_CLOSE as i32,
        close_x(),
        close_y(),
        BTN_CLOSE_W,
        BTN_CLOSE_H,
    );
}

/// Segoe UI at `pt` points, rendered for `dpi`.
///
/// Points are converted at the target DPI directly (`pt * dpi / 72`)
/// rather than through `scale_px`, which is a 96-DPI-to-`dpi` scale and
/// would apply the 96/72 conversion a second time.
///
/// Segoe UI rather than "Segoe UI Variable": `CreateFontW` fails soft
/// on an unknown face and silently substitutes, so a name that exists
/// only on Windows 11 would quietly render in a fallback on 10 instead
/// of reporting anything.
unsafe fn create_font_pt(pt: i32, weight: i32, dpi: u32) -> HFONT {
    // A negative height requests a character height rather than a cell
    // height, which is what keeps a 10pt label from growing a pixel or
    // two per line against a 12pt heading.
    let height = -(((pt as i64 * dpi as i64) + 36) / 72) as i32;
    let face_w = wide("Segoe UI");
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            weight,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        parse_args(args.iter().copied())
    }

    fn run(args: &[&str]) -> Vec<String> {
        match parse(args) {
            Cli::Run { localisations, .. } => localisations
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    fn icon_of(args: &[&str]) -> Option<String> {
        match parse(args) {
            Cli::Run { icon, .. } => icon.map(|p| p.display().to_string()),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn no_args_runs_the_launcher_window() {
        assert_eq!(
            parse(&[]),
            Cli::Run {
                localisations: vec![],
                icon: None
            }
        );
    }

    #[test]
    fn long_help_is_recognised() {
        assert_eq!(parse(&["--help"]), Cli::ShowHelp);
    }

    #[test]
    fn short_help_is_recognised() {
        assert_eq!(parse(&["-h"]), Cli::ShowHelp);
    }

    #[test]
    fn help_wins_over_a_later_bad_flag() {
        // `snug_preview --help --typo` should still print help: the
        // user got what they asked for, and a stray trailing arg
        // shouldn't turn a help request into a usage error.
        assert_eq!(parse(&["--help", "--typo"]), Cli::ShowHelp);
        assert_eq!(parse(&["-h", "--typo"]), Cli::ShowHelp);
    }

    #[test]
    fn unknown_flag_is_an_error() {
        assert_eq!(
            parse(&["--helpp"]),
            Cli::Error("unrecognised argument: --helpp".to_string())
        );
    }

    #[test]
    fn bare_value_is_an_error() {
        // Every option is a standalone switch; `snug_preview progress`
        // is a mistake, not a request for the progress dialog.
        assert_eq!(
            parse(&["progress"]),
            Cli::Error("unrecognised argument: progress".to_string())
        );
    }

    #[test]
    fn help_text_advertises_both_help_spellings() {
        let h = help_text();
        assert!(h.contains("-h, --help"), "missing the option line: {h}");
        assert!(h.contains("USAGE:"));
    }

    #[test]
    fn help_text_lists_every_dialog_button() {
        // The whole point of generating the list from DIALOG_BUTTONS:
        // a new preview button cannot ship undocumented.
        let h = help_text();
        for (label, _) in DIALOG_BUTTONS {
            assert!(
                h.contains(label),
                "dialog `{label}` is missing from --help output"
            );
        }
    }

    #[test]
    fn help_text_has_a_line_per_dialog() {
        // Guards against the list being collapsed onto one line, which
        // would still satisfy the `contains` check above.
        let h = help_text();
        let listed = DIALOG_BUTTONS
            .iter()
            .filter(|(label, _)| {
                h.lines()
                    .any(|line| line.trim_start().starts_with(label))
            })
            .count();
        assert_eq!(listed, DIALOG_BUTTONS.len());
    }

    // ------------------------------------------------------------------
    // --localisation / --localization
    // ------------------------------------------------------------------

    #[test]
    fn both_spellings_are_accepted() {
        assert_eq!(run(&["--localisation", "de.txt"]), vec!["de.txt"]);
        assert_eq!(run(&["--localization", "de.txt"]), vec!["de.txt"]);
    }

    #[test]
    fn the_flag_repeats_and_accumulates() {
        assert_eq!(
            run(&[
                "--localisation",
                "a.txt",
                "--localization",
                "b.txt",
                "--localisation",
                "c.txt"
            ]),
            vec!["a.txt", "b.txt", "c.txt"]
        );
    }

    #[test]
    fn a_missing_value_is_an_error() {
        assert_eq!(
            parse(&["--localisation"]),
            Cli::Error("--localisation requires a <FILE|DIR> argument".to_string())
        );
        assert_eq!(
            parse(&["--localization"]),
            Cli::Error("--localization requires a <FILE|DIR> argument".to_string())
        );
    }

    #[test]
    fn a_bare_value_is_still_an_error() {
        // Every option that takes a value names it explicitly, so a
        // stray positional can't be mistaken for a path.
        assert_eq!(
            parse(&["progress"]),
            Cli::Error("unrecognised argument: progress".to_string())
        );
    }

    #[test]
    fn help_text_documents_both_spellings() {
        let h = help_text();
        assert!(h.contains("--localisation <FILE|DIR>"), "{h}");
        assert!(h.contains("--localization <FILE|DIR>"), "{h}");
    }

    // ------------------------------------------------------------------
    // --icon
    // ------------------------------------------------------------------

    #[test]
    fn no_icon_by_default() {
        assert_eq!(icon_of(&[]), None);
    }

    #[test]
    fn an_icon_path_is_captured() {
        assert_eq!(
            icon_of(&["--icon", r"C:\tmp\acme.ico"]),
            Some(r"C:\tmp\acme.ico".to_string())
        );
    }

    #[test]
    fn an_icon_combines_with_localisation() {
        // The two flags are independent: a localisation sweep with a
        // candidate icon is the whole point of the flag.
        assert_eq!(
            parse(&[
                "--localisation",
                r"C:\l",
                "--icon",
                r"C:\i.png",
                "--localisation",
                r"C:\l2"
            ]),
            Cli::Run {
                localisations: vec![
                    std::path::PathBuf::from(r"C:\l"),
                    std::path::PathBuf::from(r"C:\l2")
                ],
                icon: Some(std::path::PathBuf::from(r"C:\i.png")),
            }
        );
    }

    #[test]
    fn the_last_icon_wins() {
        // A single `icon` field, not a repeatable list: there is only
        // one window-class icon and one mascot slot to fill.
        assert_eq!(
            icon_of(&["--icon", "a.ico", "--icon", "b.png"]),
            Some("b.png".to_string())
        );
    }

    #[test]
    fn an_icon_without_a_value_is_an_error() {
        assert_eq!(
            parse(&["--icon"]),
            Cli::Error("--icon requires a <FILE> argument".to_string())
        );
    }

    #[test]
    fn a_missing_icon_file_is_a_terminal_error_not_a_silent_fallback() {
        // The whole tool is a test harness: quietly showing the Snug
        // jar because a path was mistyped is exactly the failure this
        // must not produce.
        let err = install_icon_override(std::path::Path::new(r"C:\nope\missing.ico"))
            .expect_err("missing icon");
        assert!(err.starts_with("--icon:"), "{err}");
    }

    /// The canonical 68-byte 1x1 transparent PNG. Hand-inlined
    /// because this bin's tests cannot reach the `image` crate (it is
    /// a build-dependency, and dev-dependencies do not link into a
    /// normal `[[bin]]` target), and hardcoding the bytes is cheaper
    /// than depending on repo layout to find a fixture.
    const ONE_PIXEL_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // signature
        0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1
        0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, // 8bpp RGBA + crc
        0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, // IDAT
        0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, // zlib
        0x0D, 0x0A, 0x2D, 0xB4, // crc
        0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, // IEND
        0xAE, 0x42, 0x60, 0x82, // crc
    ];

    #[test]
    fn a_png_is_accepted_via_the_gdiplus_fallback() {
        // The important one: LoadImageW does NOT decode PNG from file
        // (verified -- it returns NULL on every flag combination), so
        // without the GDI+ fallback --icon would be useless for the
        // PNG a user is most likely to have.
        let dir = tmpdir();
        let png = dir.join("candidate.png");
        std::fs::write(&png, ONE_PIXEL_PNG).unwrap();

        install_icon_override(&png).expect("a real PNG must load");
    }

    #[test]
    fn a_file_that_is_neither_png_nor_ico_is_rejected() {
        // Both decoders must be tried before giving up, and the error
        // has to name `--icon` so it is obvious which flag failed.
        let dir = tmpdir();
        let junk = dir.join("not-an-image.png");
        std::fs::write(&junk, b"this is not an image at all").unwrap();

        let err = install_icon_override(&junk).expect_err("junk must not load");
        assert!(err.starts_with("--icon:"), "{err}");
    }

    #[test]
    fn help_text_documents_the_icon_flag() {
        let h = help_text();
        assert!(h.contains("--icon <FILE>"), "{h}");
        assert!(h.contains(".png") && h.contains(".ico"), "both formats: {h}");
    }

    // ------------------------------------------------------------------
    // Registry construction
    // ------------------------------------------------------------------

    fn tmpdir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "snug-preview-i18n-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn no_paths_yields_just_the_builtin_english() {
        let bundles = build_localisations(&[]).expect("baseline only");
        assert_eq!(bundles.len(), 1);
        assert_eq!(bundles[0].tag, "en");
    }

    #[test]
    fn a_directory_contributes_every_bundle_then_english() {
        let dir = tmpdir();
        std::fs::write(
            dir.join("snug-localisations.de.txt"),
            "jdk_install.prompt.button_cancel = Abbrechen\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("snug-localisations.ja.txt"),
            "jdk_install.prompt.button_cancel = Cancel\n",
        )
        .unwrap();

        let bundles = build_localisations(std::slice::from_ref(&dir)).expect("two bundles");
        let tags: Vec<&str> = bundles.iter().map(|b| b.tag.as_str()).collect();
        assert_eq!(tags, ["de", "ja", "en"]);
        // English last, so the fallback is the baseline the launcher ships.
        assert_eq!(bundles.last().unwrap().tag, "en");
    }

    #[test]
    fn a_user_supplied_english_bundle_replaces_the_baseline() {
        // Same rule the build applies: your own `en` takes the baseline
        // slot rather than colliding with it.
        let dir = tmpdir();
        std::fs::write(
            dir.join("snug-localisations.en.txt"),
            "launcher.error.button_label = Dismiss\n",
        )
        .unwrap();

        let bundles = build_localisations(&[dir]).expect("user english");
        let tags: Vec<&str> = bundles.iter().map(|b| b.tag.as_str()).collect();
        assert_eq!(tags, ["en"], "baseline must not be appended as a second en");
        assert_eq!(
            bundles[0].get("launcher.error.button_label"),
            Some("Dismiss")
        );
    }

    #[test]
    fn the_preview_opens_on_english_not_the_first_tag_found() {
        // Regression guard: `de` sorts before `en`, so an index-0
        // default made every preview open in German. The dropdown and
        // the live chain must both land on the baseline.
        let dir = tmpdir();
        std::fs::write(dir.join("snug-localisations.de.txt"), "x = 1\n").unwrap();
        std::fs::write(dir.join("snug-localisations.en.txt"), "x = 2\n").unwrap();

        let bundles = build_localisations(&[dir]).expect("de + en");
        let tags: Vec<&str> = bundles.iter().map(|b| b.tag.as_str()).collect();
        assert_eq!(tags, ["de", "en"], "both bundles stay selectable");

        let idx = default_index(&bundles);
        assert_eq!(bundles[idx].tag, "en", "must open on English");
        assert_ne!(idx, 0, "index 0 is the German bundle here");
    }

    #[test]
    fn the_baseline_wins_over_any_tag_that_sorts_after_it() {
        // Not just a `de`-beats-`en` accident: the *baseline* is the
        // default, whatever else is in the list.
        let dir = tmpdir();
        for tag in ["fr", "zh-CN", "ja"] {
            std::fs::write(dir.join(format!("snug-localisations.{tag}.txt")), "x = 1\n").unwrap();
        }
        let bundles = build_localisations(&[dir]).expect("three bundles");
        let idx = default_index(&bundles);
        assert_eq!(bundles[idx].tag, "en");
    }

    #[test]
    fn default_index_falls_back_to_the_first_entry_without_english() {
        // Defensive only — `build_localisations` always supplies an
        // `en` — but the function must not panic on one.
        let bundles = vec![
            Localization::parse("fr", "x = 1\n").unwrap(),
            Localization::parse("ja", "x = 2\n").unwrap(),
        ];
        assert_eq!(default_index(&bundles), 0);
    }

    #[test]
    fn the_language_label_and_dropdown_share_one_row() {
        // The label must sit to the left of the combo, and the combo's
        // x must be derived from the label's reserved width so the two
        // can't overlap when the label text changes.
        const { assert!(COMBO_X > COMBO_LABEL_X, "combo starts right of the label") };
        assert_eq!(COMBO_X, COMBO_LABEL_X + COMBO_LABEL_W);
        const { assert!(COMBO_LABEL_W > 60, "must fit \"Language:\" at 10pt") };
    }

    #[test]
    fn duplicate_tags_are_rejected() {
        let a = tmpdir();
        let b = tmpdir();
        std::fs::write(a.join("snug-localisations.de.txt"), "x = 1\n").unwrap();
        std::fs::write(b.join("snug-localisations.de.txt"), "y = 2\n").unwrap();
        let err = build_localisations(&[a, b]).expect_err("two `de` bundles");
        assert!(err.contains("duplicate localisation tag `de`"), "{err}");
    }

    #[test]
    fn a_stray_file_in_the_directory_is_an_error() {
        // Directory mode is strict on purpose — a README or .bak next
        // to the bundles is a mistake worth surfacing.
        let dir = tmpdir();
        std::fs::write(dir.join("snug-localisations.de.txt"), "x = 1\n").unwrap();
        std::fs::write(dir.join("README.md"), "notes\n").unwrap();
        let err = build_localisations(&[dir]).expect_err("stray file");
        assert!(err.contains("non-matching file"), "{err}");
    }

    #[test]
    fn a_missing_path_is_an_error_not_a_silent_empty_dropdown() {
        let err = build_localisations(&[std::path::PathBuf::from(
            "C:/definitely/not/here.txt",
        )])
        .expect_err("missing path");
        assert!(err.contains("stat-ing localization entry"), "{err}");
    }

    // ------------------------------------------------------------------
    // Layout geometry
    // ------------------------------------------------------------------

    /// The mascot's horizontal origin in logical pixels, exactly as
    /// `WM_PAINT` passes it to [`inspected_icon_rect`].
    const MASCOT_X: i32 = (LAUNCHER_W - HEADER_ICON_SIZE) / 2;

    #[test]
    fn the_mascot_is_centred_horizontally() {
        assert_eq!(MASCOT_X * 2 + HEADER_ICON_SIZE, LAUNCHER_W);
    }

    #[test]
    fn the_vertical_rhythm_is_derived_not_hand_tuned() {
        // These were three independent magic numbers (152 / 200 / 248)
        // tuned against each other, so growing the header could push
        // into the controls below with nothing to catch it. Chaining
        // them off the mascot's bottom edge is the fix, and this is
        // the guard that keeps it a chain.
        assert_eq!(HEADING_Y, HEADER_ICON_BOTTOM + 16);
        assert_eq!(SUBTITLE_Y, HEADING_Y + HEADING_H);
        assert_eq!(COMBO_Y, SUBTITLE_Y + SUBTITLE_H + COMBO_GAP);
        assert_eq!(BTN_FIRST_Y, COMBO_Y + BTN_GAP_HEADER);
        assert!(HEADER_ICON_BOTTOM <= HEADING_Y);
        assert!(SUBTITLE_Y + SUBTITLE_H <= COMBO_Y);
    }

    #[test]
    fn the_close_button_sits_below_the_last_dialog_row() {
        // `close_y` is read by both `WM_CREATE` and the
        // `WM_DPICHANGED` relayout, so a dialog added to
        // `DIALOG_BUTTONS` moves Close in both paths or in neither.
        let last_bottom =
            BTN_FIRST_Y + (DIALOG_BUTTONS.len() as i32 - 1) * (BTN_H + BTN_GAP) + BTN_H;
        assert_eq!(close_y(), last_bottom + BTN_CLOSE_GAP);
        assert_eq!(
            close_y() + BTN_CLOSE_H + BTN_CLOSE_MARGIN_BOTTOM,
            launcher_client_height()
        );
    }

    #[test]
    fn the_close_button_is_right_aligned_with_the_button_column() {
        assert_eq!(close_x() + BTN_CLOSE_W, BTN_X + BTN_W);
    }

    #[test]
    fn scale_px_is_the_identity_at_96_dpi() {
        for v in [0, 1, 7, 24, 128, 520] {
            assert_eq!(scale_px(v, 96), v, "{v} px at 96 DPI must be unchanged");
        }
    }

    #[test]
    fn scale_px_doubles_at_192_dpi() {
        assert_eq!(scale_px(128, 192), 256);
        assert_eq!(scale_px(520, 192), 1040);
    }

    #[test]
    fn scale_px_never_loses_a_pixel_to_truncation() {
        // `v * dpi / 96` truncates, so an odd value at 150% comes out
        // half a pixel short and the gaps between buttons drift as the
        // scale factor changes. Half-up rounding is the entire point of
        // the helper.
        for v in 1..=64 {
            assert_eq!(
                scale_px(v, 144),
                (v * 144 + 48) / 96,
                "{v} at 144 DPI must round half up"
            );
        }
    }

    // ------------------------------------------------------------------
    // `--icon` composed onto the mascot
    // ------------------------------------------------------------------

    #[test]
    fn the_inspected_icon_is_a_third_of_the_mascot() {
        assert_eq!(inspected_icon_side(HEADER_ICON_SIZE), 42);
        // Proportional, so it tracks a change to `HEADER_ICON_SIZE`.
        assert_eq!(inspected_icon_side(240), 80);
    }

    #[test]
    fn the_inspected_icon_overhangs_the_mascot_to_the_right() {
        // The negative offset is the mechanism, so assert both the
        // sign and the arithmetic — a later "tidy this up" that pushes
        // it back flush would put the plate on the mascot's foot
        // again, which is the look this replaced.
        let (x, _, side) = inspected_icon_rect(MASCOT_X, HEADER_ICON_Y, HEADER_ICON_SIZE, 96);
        let mascot_right = MASCOT_X + HEADER_ICON_SIZE;
        assert!(INSPECTED_RIGHT < 0, "the offset must stay negative");
        assert_eq!(x + side, mascot_right - INSPECTED_RIGHT);
        // `x + side` is *past* `mascot_right`, so the overhang is the
        // other subtraction. Getting this backwards is what made the
        // first run of this test fail while the geometry was right.
        assert_eq!(
            (x + side) - mascot_right,
            13,
            "the plate must overhang the mascot's right edge, not sit inside it"
        );
    }

    #[test]
    fn the_inspected_icon_stays_inside_the_mascot_box_vertically() {
        // Only the horizontal offset moved out. Dropping the plate
        // below the mascot too would detach it from the lens, which
        // sits in the artwork's upper-right.
        let (_, y, side) = inspected_icon_rect(MASCOT_X, HEADER_ICON_Y, HEADER_ICON_SIZE, 96);
        let mascot_bottom = HEADER_ICON_Y + HEADER_ICON_SIZE;
        assert!(y >= HEADER_ICON_Y);
        assert!(y + side <= mascot_bottom);
        assert_eq!(mascot_bottom - (y + side), INSPECTED_BOTTOM);
    }

    #[test]
    fn the_plate_stays_inside_the_window() {
        // The overhang has a budget — the content margin, not the raw
        // client width.
        let icon = inspected_icon_rect(MASCOT_X, HEADER_ICON_Y, HEADER_ICON_SIZE, 96);
        let (_, _, _, r) = inspected_plate_rect(icon, 96);
        assert!(
            r <= LAUNCHER_W - BTN_X,
            "plate right edge {r} must stay inside the content margin {}",
            LAUNCHER_W - BTN_X
        );
    }

    #[test]
    fn the_plate_border_is_a_uniform_ring() {
        // The grey is the region *between* the outer rect and the inner
        // one, so a non-uniform inset reads as a lopsided rim. That is
        // the whole reason `inspected_plate_inner` exists rather than
        // four inline subtractions.
        let icon = inspected_icon_rect(MASCOT_X, HEADER_ICON_Y, HEADER_ICON_SIZE, 96);
        let (l, t, r, b) = inspected_plate_rect(icon, 96);
        let (il, it, ir, ib) = inspected_plate_inner((l, t, r, b), 96);
        let border = scale_px(INSPECTED_PLATE_BORDER, 96);
        assert_eq!(il - l, border);
        assert_eq!(it - t, border);
        assert_eq!(r - ir, border);
        assert_eq!(b - ib, border);
        // The inner plate must stay inside the icon it backs.
        let (x, y, _) = icon;
        assert!(il <= x && it <= y, "ring must not intrude on the icon");
    }

    #[test]
    fn the_plate_border_is_a_readable_light_grey() {
        // The plate is white on white twice over — the mascot artwork
        // and the window — so this border is the only edge a user
        // sees. Guard the luminance rather than the literal value, so
        // a future tweak cannot quietly drop it to near-white or push
        // it to a hard black outline.
        let b = (COLOR_PLATE_EDGE >> 16) & 0xFF;
        let g = (COLOR_PLATE_EDGE >> 8) & 0xFF;
        let r = COLOR_PLATE_EDGE & 0xFF;
        let min = r.min(g).min(b);
        let max = r.max(g).max(b);
        assert!(min > 0x60, "too dark to be a light grey: {COLOR_PLATE_EDGE:#010x}");
        assert!(max < 0xF0, "too light to read as an edge: {COLOR_PLATE_EDGE:#010x}");
        assert!(max - min <= 0x08, "expected a neutral grey: {COLOR_PLATE_EDGE:#010x}");
    }

    #[test]
    fn the_inspected_icon_lands_in_the_bottom_right_quadrant() {
        // The artwork's magnifying glass is upper-right, so the
        // candidate has to be the bottom-*right* corner or it is not
        // under the lens at all. Asserting the quadrant separately
        // from the offsets catches a sign flip in either axis.
        let (x, y, side) = inspected_icon_rect(MASCOT_X, HEADER_ICON_Y, HEADER_ICON_SIZE, 96);
        let mascot_centre_x = MASCOT_X + HEADER_ICON_SIZE / 2;
        let mascot_centre_y = HEADER_ICON_Y + HEADER_ICON_SIZE / 2;
        assert!(x + side / 2 > mascot_centre_x, "must be right of centre");
        assert!(y + side / 2 > mascot_centre_y, "must be below centre");
    }

    #[test]
    fn the_inspected_icon_scales_with_the_display() {
        let (x96, _, s96) = inspected_icon_rect(MASCOT_X, HEADER_ICON_Y, HEADER_ICON_SIZE, 96);
        let (x192, _, s192) = inspected_icon_rect(MASCOT_X, HEADER_ICON_Y, HEADER_ICON_SIZE, 192);
        assert_eq!(s192, s96 * 2);
        // Scaled about the same corner, so the overhang past the
        // mascot's right edge doubles along with everything else.
        let overhang96 = MASCOT_X + HEADER_ICON_SIZE - (x96 + s96);
        let mascot_right192 = scale_px(MASCOT_X + HEADER_ICON_SIZE, 192);
        let overhang192 = mascot_right192 - (x192 + s192);
        assert_eq!(overhang192, overhang96 * 2);
    }

    #[test]
    fn scale_px_rounds_negatives_away_from_zero() {
        // Rust's `/` truncates toward zero, so the naive
        // `(n + 48) / 96` makes a negative offset shrink instead of
        // grow — the -13 px overhang would come out at -12 at 96 DPI
        // and be wrong at every scale factor. This is the guard for
        // that, and it is why `scale_px` has a sign branch at all.
        assert_eq!(scale_px(-13, 96), -13);
        assert_eq!(scale_px(-13, 192), -26);
        assert_eq!(scale_px(-13, 144), -20); // 19.5 px, away from zero
        // Symmetry: +n and -n must be exact negatives of each other.
        for v in 1..=64 {
            assert_eq!(scale_px(-v, 96), -scale_px(v, 96), "{v}");
            assert_eq!(scale_px(-v, 144), -scale_px(v, 144), "{v} at 144 DPI");
        }
    }

    #[test]
    fn the_plate_contains_the_icon_with_symmetric_padding() {
        let icon = inspected_icon_rect(MASCOT_X, HEADER_ICON_Y, HEADER_ICON_SIZE, 96);
        let (x, y, side) = icon;
        let (l, t, r, b) = inspected_plate_rect(icon, 96);
        assert!(l < x && t < y, "plate must extend past the icon");
        assert!(r > x + side && b > y + side);
        // Symmetric, so the icon is not off-centre in its card.
        assert_eq!(x - l, r - (x + side));
        assert_eq!(y - t, b - (y + side));
    }

    #[test]
    fn the_plate_padding_scales_with_the_display() {
        let pad_at = |dpi: u32| {
            let icon = inspected_icon_rect(MASCOT_X, HEADER_ICON_Y, HEADER_ICON_SIZE, dpi);
            let (x, _, _) = icon;
            let (l, _, _, _) = inspected_plate_rect(icon, dpi);
            x - l
        };
        assert_eq!(pad_at(96), INSPECTED_PLATE_PAD);
        assert_eq!(pad_at(192), INSPECTED_PLATE_PAD * 2);
    }

    #[test]
    fn help_text_explains_the_mascot_composition() {
        // The composition is the one behaviour of `--icon` a user
        // cannot infer from the flag name, so it has to be written
        // down rather than discovered.
        let h = help_text();
        assert!(h.contains("magnifying glass"), "{h}");
        assert!(h.contains("bottom-right"), "{h}");
    }
}