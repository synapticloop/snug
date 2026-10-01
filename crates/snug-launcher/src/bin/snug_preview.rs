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
//! Command line:
//!
//! ```text
//! snug_preview [OPTIONS]
//!   -h, --help                        print the option list and every
//!                                     previewable dialog
//!       --localisation <FILE|DIR>     load localisations to pick from
//!       --localization <FILE|DIR>     (alias; both spellings accepted)
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

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, DT_LEFT, DT_SINGLELINE, DrawTextW, EndPaint, HBRUSH, HFONT,
    PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CB_ADDSTRING, CB_GETCURSEL, CB_RESETCONTENT, CB_SETCURSEL, CBN_SELCHANGE,
    CBS_DROPDOWNLIST, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, DrawIconEx,
    GetDlgItem, GetMessageW, GetSystemMetrics, LoadCursorW, LoadImageW, MSG, PostThreadMessageW,
    RegisterClassExW, SendMessageW, SM_CXSCREEN, SM_CYSCREEN, TranslateMessage, BS_PUSHBUTTON,
    DI_NORMAL, HICON, ICON_BIG, ICON_SMALL, IDC_ARROW, IMAGE_ICON, LR_SHARED, WM_CLOSE, WM_COMMAND,
    WM_CREATE, WM_DESTROY, WM_NCDESTROY, WM_PAINT, WM_QUIT, WM_SETICON, WNDCLASSEXW, WS_CAPTION,
    WS_CHILD, WS_EX_TOPMOST, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

use snug_format::Localization;
use snug_launcher::dialogs;
use snug_launcher::error_window;
use snug_launcher::jdk_install::{self, ProgressShared};
use snug_launcher::localize;
use snug_launcher::progress_window;

// ============================================================================
//  Constants
// ============================================================================

const CLASS_NAME: &str = "snug_preview_launcher_v1\0";
const WINDOW_TITLE: &str = "snug dialog preview\0";

/// Window dimensions (logical pixels at 96 DPI).
///
/// `LAUNCHER_W` / `launcher_client_height()` describe the **client
/// area**, not the raw window size — the title bar (~30 px) is added
/// on top at `CreateWindowExW` time via `AdjustWindowRectEx`. Treating
/// the values as client dimensions means the body layout
/// (`BTN_FIRST_Y`, `BTN_CLOSE_MARGIN_BOTTOM`) is computed in the same
/// coordinate system the buttons actually live in, so the Close button
/// can never end up below the visible area.
const LAUNCHER_W: i32 = 480;

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
/// (window is 480 px, icon is centred with 176 px on either side).
const HEADER_ICON_SIZE: i32 = 128;
const HEADER_ICON_Y: i32 = 8;

/// Button layout — single column, 24 px outer margin, 8 px gap.
/// `BTN_FIRST_Y` is set below the header icon + subtitle so the
/// icon and one-line copy sit above the buttons.
const BTN_X: i32 = 24;
const BTN_W: i32 = LAUNCHER_W - 48;
const BTN_H: i32 = 38;
const BTN_GAP: i32 = 8;
const BTN_CLOSE_W: i32 = 96;
const BTN_CLOSE_H: i32 = 36;
const BTN_CLOSE_MARGIN_BOTTOM: i32 = 16;
/// Vertical gap between the last dialog row and the Close button.
/// 16 px reads as a clear separator without looking detached.
const BTN_CLOSE_GAP: i32 = 16;

/// Subtitle text painted in `WM_PAINT`. `SUBTITLE_Y` sits directly
/// below the header icon (icon ends at y=136) with a 16 px gap.
const SUBTITLE_TEXT: &str = "Click a button to open the corresponding dialog.";
const SUBTITLE_Y: i32 = 152;

/// Language picker — a `COMBOBOX` between the subtitle and the dialog
/// buttons, so a translation can be flipped on before opening a dialog
/// instead of requiring a rebuild.
///
/// `BTN_FIRST_Y` sits below `COMBO_Y + COMBO_H`, which is why the
/// button block starts where it does.
const COMBO_LABEL_TEXT: &str = "Language:";
const COMBO_LABEL_Y: i32 = 180;
const COMBO_X: i32 = BTN_X;
const COMBO_Y: i32 = 200;
const COMBO_W: i32 = 240;
const COMBO_H: i32 = 34;
/// Grows to fit a few more locales without touching the constants.
const BTN_FIRST_Y: i32 = 248;

/// `WM_SETFONT` isn't exported as a named constant by windows-sys
/// 0.59. Value from winuser.h.
const WM_SETFONT: u32 = 0x0030;

/// Load the EXE's embedded `MAINICON` group at two sizes for the
/// window class. `LR_SHARED` keeps Windows from handing us a
/// per-process copy that we'd have to `DestroyIcon` later — the
/// icon handle stays valid for the lifetime of the EXE module.
fn load_exe_main_icons(
    hinst: windows_sys::Win32::Foundation::HMODULE,
) -> (HICON, HICON) {
    // RT_GROUP_ICON entries are conventionally named "MAINICON"
    // (Windows icon convention). editpe writes the group under the
    // *name* rather than under numeric ID 1, so the primary lookup
    // is by name. The numeric ID 1 fallback covers any future
    // upstream that switches to a numeric MAINICON.
    let mainicon_w: Vec<u16> = "MAINICON\0".encode_utf16().collect();
    let id1: *const u16 = 1 as *const u16;
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

/// The dialog buttons, in display order. Hoisted to module scope so
/// [`launcher_client_height`] can derive the window height from
/// `DIALOG_BUTTONS.len()` — adding a dialog here resizes the window
/// automatically.
const DIALOG_BUTTONS: &[(&str, usize)] = &[
    ("Progress (animated)", ID_BTN_PROGRESS_ANIM),
    ("Progress (static @ 50%)", ID_BTN_PROGRESS_STATIC),
    ("Metadata failed", ID_BTN_METADATA_FAILED),
    ("Retry (try 2 of 3)", ID_BTN_RETRY),
    ("Error (post-install)", ID_BTN_ERROR),
    ("Java error (with update link)", ID_BTN_JAVA_ERROR),
    ("Install prompt (custom-painted)", ID_BTN_PROMPT_V5),
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
    let files = snug_format::discover_localization_files(entries).map_err(|e| e.to_string())?;

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
        user.push(bundle);
    }

    if user.iter().any(|b| b.tag == snug_format::DEFAULT_EN_TAG) {
        // The user's `en` is already the baseline; nothing to append.
        return Ok(user);
    }

    let builtin = Localization::parse(
        snug_format::DEFAULT_EN_TAG,
        snug_format::DEFAULT_EN_TEXT,
    )
    .map_err(|e| format!("parsing the built-in English baseline: {e}"))?;
    user.push(builtin);
    Ok(user)
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
        return;
    }
    let chosen = bundles
        .get(index.min(bundles.len() - 1))
        .cloned()
        .unwrap_or_else(|| bundles[0].clone());
    localize::set_bundles(std::slice::from_ref(&chosen));
}

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
    Run { localisations: Vec<std::path::PathBuf> },
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
            other => {
                // A bare value is as unexpected as a bad flag: every
                // option that takes a value names it explicitly.
                return Cli::Error(format!("unrecognised argument: {other}"));
            }
        }
    }
    Cli::Run { localisations }
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
    out.push_str("\n");
    out.push_str("            Both spellings of the flag are accepted. The built-in\n");
    out.push_str("            English baseline is always in the dropdown; supplying\n");
    out.push_str("            your own snug-localisations.en.txt replaces it, the\n");
    out.push_str("            same rule the build applies to a shipped launcher.\n\n");
    out.push_str("DIALOGS:\n");
    for (label, _) in DIALOG_BUTTONS {
        out.push_str(&format!("    {label}\n"));
    }
    out.push_str("\n");
    out.push_str("NOTES:\n");
    out.push_str("    Each dialog is detached — they are top-level windows, so\n");
    out.push_str("    closing the launcher does not destroy them and you can open\n");
    out.push_str("    several side by side. The process exits only when every\n");
    out.push_str("    open window has been dismissed.\n\n");
    out.push_str("    Changing the language affects dialogs opened afterwards;\n");
    out.push_str("    windows already on screen keep the copy they were built\n");
    out.push_str("    with. Close and reopen them to see the new language.\n\n");
    out.push_str("    Open one dialog directly with:\n");
    out.push_str("        cargo run --example dialogs_preview -- --kind <KIND>\n");
    out
}

// ============================================================================
//  Entry point
// ============================================================================

fn main() {
    // Handled before any Win32 work: `--help` must work on a headless
    // CI runner and must not open a window that then blocks forever.
    // Localisation loading is here too, for the same reason — a bad
    // path should fail loudly in the terminal, not behind a window
    // that swallows the message.
    let requested = match parse_args(std::env::args().skip(1)) {
        Cli::ShowHelp => {
            print!("{}", help_text());
            return;
        }
        Cli::Error(msg) => {
            eprintln!("snug_preview: {msg}");
            eprintln!("Try 'snug_preview --help' for more information.");
            std::process::exit(2);
        }
        Cli::Run { localisations } => localisations,
    };

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
    activate_localisations(bundles, 0);

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
        // **client area**. `AdjustWindowRectEx` adds the title-bar /
        // non-client chrome to that rectangle, so we hand the
        // *expanded* dimensions to `CreateWindowExW`. Centring on the
        // primary monitor uses the expanded width/height (which is
        // what the user actually sees on the desktop), not the client
        // area.
        let mut client_rect = RECT {
            left: 0,
            top: 0,
            right: LAUNCHER_W,
            bottom: launcher_client_height(),
        };
        AdjustWindowRectEx(
            &mut client_rect,
            WS_CAPTION | WS_SYSMENU | WS_OVERLAPPED,
            0,
            WS_EX_TOPMOST,
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
            // Create one Segoe UI 10pt font used by every button.
            // Leaked intentionally — the launcher window is the
            // last user-owned resource, the OS reclaims everything
            // on process exit.
            let hfont = create_button_font();
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
                COMBO_X,
                COMBO_Y,
                COMBO_W,
                COMBO_H,
                hwnd,
                ID_COMBO_LANGUAGE as *mut _,
                hinst,
                std::ptr::null(),
            );
            if !combo.is_null() {
                SendMessageW(combo, WM_SETFONT, hfont as usize, 1);
                SendMessageW(combo, CB_RESETCONTENT, 0, 0);
                for bundle in localisations() {
                    SendMessageW(
                        combo,
                        CB_ADDSTRING,
                        0,
                        wide(&bundle.tag).as_ptr() as isize,
                    );
                }
                // Pre-select whatever `main` already activated.
                SendMessageW(combo, CB_SETCURSEL, 0, 0);
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

            // Close button — bottom right, positioned relative to the
            // last dialog row so it stays put as the list grows.
            let close_y = BTN_FIRST_Y
                + (DIALOG_BUTTONS.len() as i32 - 1) * (BTN_H + BTN_GAP)
                + BTN_H
                + BTN_CLOSE_GAP;
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
            let hinst = GetModuleHandleW(std::ptr::null());
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            let prev_font = SelectObject(hdc, hfont as _);

            SetBkMode(hdc, TRANSPARENT as i32);

            // 1. Header icon — centred horizontally at the top of
            //    the client area. The HICON is loaded once via the
            //    `OnceLock` in `header_icon()` and reused on every
            //    subsequent paint (LR_SHARED keeps the handle valid
            //    for the lifetime of the EXE module, so no
            //    DestroyIcon is needed).
            let hicon = header_icon(hinst);
            if !hicon.is_null() {
                let icon_x = (LAUNCHER_W - HEADER_ICON_SIZE) / 2;
                DrawIconEx(
                    hdc,
                    icon_x,
                    HEADER_ICON_Y,
                    hicon,
                    HEADER_ICON_SIZE,
                    HEADER_ICON_SIZE,
                    0,
                    std::ptr::null_mut(),
                    DI_NORMAL,
                );
            }

            // 2. Subtitle — one line of mid-grey copy below the icon.
            SetTextColor(hdc, 0x00606060);
            let mut rect_sub = RECT {
                left: BTN_X,
                top: SUBTITLE_Y,
                right: LAUNCHER_W - BTN_X,
                bottom: SUBTITLE_Y + 24,
            };
            DrawTextW(
                hdc,
                wide(SUBTITLE_TEXT).as_ptr(),
                -1,
                &mut rect_sub,
                DT_LEFT | DT_SINGLELINE,
            );

            // 3. Language label — sits directly above the dropdown.
            SetTextColor(hdc, 0x00606060);
            let mut rect_combo_label = RECT {
                left: COMBO_X,
                top: COMBO_LABEL_Y,
                right: LAUNCHER_W - COMBO_X,
                bottom: COMBO_LABEL_Y + 20,
            };
            DrawTextW(
                hdc,
                wide(COMBO_LABEL_TEXT).as_ptr(),
                -1,
                &mut rect_combo_label,
                DT_LEFT | DT_SINGLELINE,
            );

            SelectObject(hdc, prev_font);
            EndPaint(hwnd, &ps);
            0
        },
        WM_COMMAND => unsafe {
            // LOWORD(wparam) is the control / menu id. Mask off
            // the notification code in the high word.
            let id = (wparam & 0xFFFF) as usize;
            let notification = ((wparam >> 16) & 0xFFFF) as usize;

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
                        "",
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
    let choice = snug_launcher::prompt_window::show(
        std::ptr::null_mut(),
        0,
        prompt,
        "21.0.2",
        192,
        "https://api.adoptium.net/v3/binary/latest/21/ga/windows/x64/jdk/hotspot/normal/eclipse",
        "9c629caaccc4e64aa0ea58bd0a3f43eaf903a4c1a3e2c2a6e9c5b1a8e8b3f1a0",
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        parse_args(args.iter().copied())
    }

    fn run(args: &[&str]) -> Vec<String> {
        match parse(args) {
            Cli::Run { localisations } => localisations
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn no_args_runs_the_launcher_window() {
        assert_eq!(parse(&[]), Cli::Run { localisations: vec![] });
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

        let bundles = build_localisations(&[dir.clone()]).expect("two bundles");
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
}