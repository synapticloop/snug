//! Native splash window shown before the JVM takes over the screen.
//!
//! The splash is a topmost, borderless, toolwindow rendered with
//! `UpdateLayeredWindow` + `ULW_ALPHA`. The pixel data is supplied as
//! pre-converted `BGRA premultiplied` bytes — the `snug` CLI does the
//! PNG → DIB conversion at build time so the launcher doesn't need an
//! image codec dependency. A dedicated thread owns the window and
//! pumps messages; the launcher calls [`SplashHandle::dismiss`] once
//! the JVM thread is attached (i.e. JavaFX is about to start).
//!
//! Semantics: the splash stays visible for at least `duration_ms`
//! after [`show`] returns; after that it dismisses as soon as the
//! launcher signals via [`SplashHandle::dismiss`]. A hard cap of
//! `duration_ms + HARD_CAP_GRACE` prevents hangs if the JVM startup
//! path wedges.
//!
//! **Thread model:** the splash window is created, shown, and
//! destroyed entirely on the splash thread. Win32 requires
//! `DestroyWindow` to be called from the thread that created the
//! window, so we can't split that responsibility across threads.

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, CreateCompatibleDC, CreateDIBSection, DeleteDC,
    DeleteObject, GetDC, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetSystemMetrics,
    LoadCursorW, MSG, PeekMessageW, PostMessageW, RegisterClassExW, ShowWindow, TranslateMessage,
    UpdateLayeredWindow, WNDCLASSEXW, CS_HREDRAW, CS_OWNDC, CS_VREDRAW, IDC_ARROW, PM_REMOVE,
    SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, ULW_ALPHA, WM_DESTROY, WM_QUIT, WS_EX_LAYERED,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

/// How long the splash may stay up after `duration_ms` if the main
/// thread never signals dismiss.
const HARD_CAP_GRACE: Duration = Duration::from_secs(30);

/// Pump granularity for the splash thread.
const TICK: Duration = Duration::from_millis(15);

const CLASS_NAME: &str = "SnugSplash\0";

/// Errors that can arise while bringing the splash window up. All
/// are non-fatal: the launcher should proceed without a splash if
/// the window can't be created (e.g. GDI failure).
#[derive(Debug)]
pub enum SplashError {
    /// `width * height * 4` would overflow usize on this platform.
    Overflow,
    /// Pixel buffer length doesn't match `width * height * 4`.
    BufferLength { expected: usize, actual: usize },
    /// Width or height is zero.
    Empty,
    /// Failed to spawn the splash thread.
    ThreadSpawn(std::io::Error),
    /// `CreateDIBSection` returned a null bitmap handle.
    Bitmap,
    /// `CreateWindowExW` returned a null HWND.
    Window,
    /// `UpdateLayeredWindow` failed.
    UpdateLayered,
}

impl std::fmt::Display for SplashError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Localized via the `splash.err.*` keys in the
        // `snug-localisations.<tag>.txt` bundle. The English baseline
        // ships the same wording as the original literals, so the
        // user-visible text is unchanged when no other locale is
        // bundled.
        let s = crate::localize::t_positional(
            match self {
                SplashError::Overflow => "splash.err.overflow",
                SplashError::BufferLength { .. } => "splash.err.buffer_length",
                SplashError::Empty => "splash.err.empty",
                SplashError::ThreadSpawn(_) => "splash.err.thread_spawn",
                SplashError::Bitmap => "splash.err.bitmap",
                SplashError::Window => "splash.err.window",
                SplashError::UpdateLayered => "splash.err.update_layered",
            },
            &[],
        );
        // Fill named placeholders for the variants that have them.
        let rendered: String = match self {
            SplashError::BufferLength { expected, actual } => {
                let expected_str = expected.to_string();
                let actual_str = actual.to_string();
                crate::localize::fill_placeholders(
                    &s,
                    &[("expected", &expected_str), ("actual", &actual_str)],
                )
            }
            SplashError::ThreadSpawn(e) => {
                let msg = e.to_string();
                crate::localize::fill_placeholders(&s, &[("0", &msg)])
            }
            _ => s,
        };
        f.write_str(&rendered)
    }
}

impl std::error::Error for SplashError {}

/// Opaque handle to a running splash window. Drop without calling
/// [`SplashHandle::dismiss`] will leak the window until the process
/// exits — the launcher must call `dismiss` once the JVM is ready.
pub struct SplashHandle {
    ready: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    dismissed: bool,
}

/// Show the splash. Returns immediately after the splash thread has
/// been spawned; the splash is visible from then on.
///
/// `pixels` is the pre-converted BGRA data (build-time result of
/// `snug`'s PNG → BGRA-premultiplied stage); see
/// [`snug_format::SplashImage`] for the wire-format definition. The
/// launcher does no PNG decoding, channel swapping, or alpha
/// premultiplication — everything is done at build time.
///
/// `duration_ms` is the minimum visible time — the splash will not
/// dismiss before that elapses, even if `dismiss` is called early.
pub fn show(
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    duration_ms: u32,
) -> Result<SplashHandle, SplashError> {
    // Sanity-check the pixel buffer length against width × height × 4.
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or(SplashError::Overflow)?;
    if pixels.len() != expected {
        return Err(SplashError::BufferLength {
            expected,
            actual: pixels.len(),
        });
    }
    if width == 0 || height == 0 {
        return Err(SplashError::Empty);
    }

    let ready = Arc::new(AtomicBool::new(false));
    let ready_thread = ready.clone();
    let min_duration = Duration::from_millis(duration_ms as u64);

    let thread = thread::Builder::new()
        .name("snug-splash".into())
        .spawn(move || {
            run_splash_thread(width, height, pixels, ready_thread, min_duration);
        })
        .map_err(SplashError::ThreadSpawn)?;

    Ok(SplashHandle {
        ready,
        thread: Some(thread),
        dismissed: false,
    })
}

impl SplashHandle {
    /// Signal the splash thread that the JVM is up. The window stays
    /// visible for at least the original `duration_ms`; after that it
    /// dismisses as soon as this method runs. Blocks until the splash
    /// thread has torn the window down.
    pub fn dismiss(mut self) {
        self.do_dismiss();
    }

    fn do_dismiss(&mut self) {
        if self.dismissed {
            return;
        }
        self.dismissed = true;
        self.ready.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for SplashHandle {
    fn drop(&mut self) {
        // Auto-dismiss on scope exit so error paths in the launcher
        // don't leak the splash thread / window. Idempotent.
        self.do_dismiss();
    }
}

fn run_splash_thread(
    width: u32,
    height: u32,
    mut pixels: Vec<u8>,
    ready: Arc<AtomicBool>,
    min_duration: Duration,
) {
    let hard_cap = min_duration + HARD_CAP_GRACE;
    let start = Instant::now();

    // Top-down DIB section (negative biHeight). 32-bit BI_RGB so the
    // build-time BGRA-premultiplied bytes land in the bitmap
    // unmodified.
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
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

    let screen_dc = unsafe { GetDC(std::ptr::null_mut()) };
    let mem_dc = unsafe { CreateCompatibleDC(screen_dc) };
    unsafe { ReleaseDC(std::ptr::null_mut(), screen_dc) };
    if mem_dc.is_null() {
        eprintln!("snug-splash: {}", crate::localize::lookup("splash.err.create_compatible_dc"));
        return;
    }

    let mut bits_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
    let bitmap = unsafe {
        CreateDIBSection(
            mem_dc,
            &bmi,
            DIB_RGB_COLORS,
            &mut bits_ptr,
            std::ptr::null_mut(),
            0,
        )
    };
    if bitmap.is_null() || bits_ptr.is_null() {
        unsafe { DeleteDC(mem_dc) };
        eprintln!("snug-splash: {}", crate::localize::lookup("splash.err.create_dib_section"));
        return;
    }

    unsafe {
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits_ptr as *mut u8, pixels.len());
    }
    pixels.clear();

    let old_bitmap = unsafe { SelectObject(mem_dc, bitmap) };

    // Register the window class on this thread (the thread that owns
    // the window).
    let module = unsafe { GetModuleHandleW(std::ptr::null()) };
    let class_name_w: Vec<u16> = CLASS_NAME.encode_utf16().collect();
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW | CS_OWNDC,
        lpfnWndProc: Some(splash_wnd_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: module,
        hIcon: std::ptr::null_mut(),
        hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
        hbrBackground: std::ptr::null_mut(),
        lpszMenuName: std::ptr::null(),
        lpszClassName: class_name_w.as_ptr(),
        hIconSm: std::ptr::null_mut(),
    };
    unsafe { RegisterClassExW(&wc) };

    let screen_w = unsafe { GetSystemMetrics(SM_CXSCREEN) } as i32;
    let screen_h = unsafe { GetSystemMetrics(SM_CYSCREEN) } as i32;
    let x = (screen_w - width as i32) / 2;
    let y = (screen_h - height as i32) / 2;

    let title_w: Vec<u16> = format!("{}\0", crate::localize::lookup("splash.title"))
        .encode_utf16()
        .collect();

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class_name_w.as_ptr(),
            title_w.as_ptr(),
            WS_POPUP,
            x,
            y,
            width as i32,
            height as i32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            module,
            std::ptr::null_mut(),
        )
    };
    if hwnd.is_null() {
        unsafe {
            SelectObject(mem_dc, old_bitmap);
            DeleteObject(bitmap);
            DeleteDC(mem_dc);
        }
        eprintln!("snug-splash: {}", crate::localize::lookup("splash.err.create_window"));
        return;
    }

    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let pt_src = POINT { x: 0, y: 0 };
    let pt_dst = POINT { x, y };
    let size = SIZE {
        cx: width as i32,
        cy: height as i32,
    };
    let ok = unsafe {
        UpdateLayeredWindow(
            hwnd,
            std::ptr::null_mut(),
            &pt_dst,
            &size,
            mem_dc,
            &pt_src,
            0,
            &blend,
            ULW_ALPHA,
        )
    };
    if ok == 0 {
        unsafe {
            DestroyWindow(hwnd);
            SelectObject(mem_dc, old_bitmap);
            DeleteObject(bitmap);
            DeleteDC(mem_dc);
        }
        eprintln!("snug-splash: {}", crate::localize::lookup("splash.err.update_layered_window"));
        return;
    }

    unsafe { ShowWindow(hwnd, SW_SHOW) };

    // Pump messages on this thread (the owner of the splash window).
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        let elapsed = start.elapsed();
        let min_met = elapsed >= min_duration;
        let signalled = ready.load(Ordering::SeqCst);

        if elapsed >= hard_cap || (min_met && signalled) {
            break;
        }

        unsafe {
            let r = PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE);
            if r != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
                if msg.message == WM_QUIT {
                    break;
                }
            } else {
                thread::sleep(TICK);
            }
        }
    }

    // Tear down on this thread (the window owner).
    unsafe {
        DestroyWindow(hwnd);
        SelectObject(mem_dc, old_bitmap);
        DeleteObject(bitmap);
        DeleteDC(mem_dc);
    }
}

unsafe extern "system" fn splash_wnd_proc(
    hwnd: HWND,
    msg: u32,
    _wparam: WPARAM,
    _lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_DESTROY => {
            unsafe {
                PostMessageW(std::ptr::null_mut(), WM_QUIT, 0, 0);
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, _wparam, _lparam) },
    }
}
