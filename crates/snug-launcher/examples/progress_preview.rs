//! Tiny preview harness for the custom-paint progress window.
//!
//! Run from the workspace root:
//!
//! ```bash
//! cargo run -p snug-launcher --example progress_preview
//! cargo run -p snug-launcher --example progress_preview -- --static
//! cargo run -p snug-launcher --example progress_preview -- 150 8
//! ```
//!
//! The window is driven entirely by the `ProgressShared` you construct
//! here, so layout, font sizes, colours, and animation timing can all
//! be exercised without rebuilding the fat-JAR-using `snug-cli`,
//! `snug-format`, or the committed `bin/launcher-stub.exe`.
//!
//! Behaviour:
//!
//! - Default mode spawns a worker thread that simulates a download at
//!   `<speed_mb_s>` MB/s for a file of `<size_mb>` MB, then advances
//!   through verify (phase 1) and extract (phase 2). The dialog closes
//!   itself when `done != 0`, so just watch it run.
//!
//! - `--static` skips the worker. The dialog opens at 50% and stays
//!   there until you click Cancel or close the window — useful for
//!   inspecting layout without watching animation.
//!
//! - `--pause` starts the worker but doesn't auto-set `started`; the
//!   user must click the "Install" button before the simulated
//!   download begins. Useful for verifying the Install → Cancel label
//!   flip.
//!
//! - `--no-mascot` skips the `assets/snug-icon.png` decode + DIB
//!   section, leaving the mascot slot empty so you can see how the
//!   fallback path (EXE icon) renders.

#![cfg(windows)]

use std::env;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use snug_launcher::jdk_install::ProgressShared;
use snug_launcher::progress_window;

fn main() {
    // ------------------------------------------------------------------
    // Tiny CLI: [--static|--pause] [<size_mb>] [<speed_mb_s>]
    // ------------------------------------------------------------------
    let mut static_mode = false;
    let mut pause_mode = false;
    let mut skip_mascot = false;
    let mut size_mb: u64 = 150;
    let mut speed_mb_s: f64 = 10.0;

    for arg in env::args().skip(1) {
        match arg.as_str() {
            "--static" => static_mode = true,
            "--pause" => pause_mode = true,
            "--no-mascot" => skip_mascot = true,
            "--help" | "-h" => {
                eprintln!(
                    "usage: progress-preview [--static | --pause | --no-mascot] [<size_mb>] [<speed_mb_s>]"
                );
                eprintln!("  --static     don't animate; render at 50% for layout inspection");
                eprintln!("  --pause      simulate but wait for the user to click Install");
                eprintln!("  --no-mascot  skip the mascot PNG decode (EXE-icon fallback)");
                eprintln!("  size_mb      simulated file size in MB (default 150)");
                eprintln!("  speed_mb_s   simulated download speed in MB/s (default 10)");
                return;
            }
            _ => {
                // First positional is size, second is speed. Parse
                // defensively so a typo doesn't silently fall through.
                if let Ok(n) = arg.parse::<u64>() {
                    if size_mb == 150 {
                        size_mb = n;
                        continue;
                    }
                }
                if let Ok(n) = arg.parse::<f64>() {
                    speed_mb_s = n;
                }
            }
        }
    }

    let total_bytes: u64 = size_mb * 1_048_576;
    let bytes_per_ms: u64 = ((speed_mb_s * 1_048_576.0) / 1000.0).round() as u64;

    let shared = Arc::new(ProgressShared::new(total_bytes));

    // Decode the mascot PNG and push the resulting DIB section into
    // the shared state. `progress_window::WM_PAINT` reads
    // `shared.mascot_hbitmap()` and prefers it over the EXE-icon
    // fallback when non-zero.
    if !skip_mascot {
        match load_mascot_from_png(include_bytes!("../../../assets/snug-icon.png")) {
            Ok((hbitmap, w, h)) => {
                shared.set_mascot_hbitmap(hbitmap as i32);
                eprintln!("progress_preview: mascot loaded {w}x{h}");
            }
            Err(e) => {
                eprintln!(
                    "progress_preview: mascot PNG decode failed: {e} — falling back to EXE icon"
                );
            }
        }
    }

    // ------------------------------------------------------------------
    // Worker thread: drive the simulation.
    //
    // Mirrors the real install flow in shape:
    //   phase 0 (download) : bytes 0 → total_bytes, pct 0 → 95
    //   phase 1 (verify)   : pct 95 → 98 over ~1 s
    //   phase 2 (extract)  : pct 98 → 100 over ~2 s
    // The percentage bands match what `progress_window.rs` documents so
    // you can verify the bar actually moves monotonically.
    // ------------------------------------------------------------------
    if !static_mode {
        let shared = shared.clone();
        thread::spawn(move || {
            // If `--pause`, wait until the user clicks Install. The
            // real worker has the same wait at the top of its
            // `worker_thread`.
            if pause_mode {
                while !shared.is_started() {
                    if shared.status() != 0 {
                        return;
                    }
                    thread::sleep(Duration::from_millis(20));
                }
            } else {
                shared.set_started(true);
            }

            // Phase 0: download.
            let tick = Duration::from_millis(50);
            let bytes_per_tick = (bytes_per_ms as f64 * 50.0 / 1000.0).round() as u64;
            loop {
                if shared.status() != 0 {
                    return;
                }
                let b = shared.bytes_done();
                if b >= total_bytes {
                    break;
                }
                let new_b = (b + bytes_per_tick).min(total_bytes);
                // pct = bytes / total * 95 (cap at 95 for phase 0)
                let pct = ((new_b as u128 * 95) / total_bytes as u128) as u32;
                shared.set_pct(pct);
                shared.set_bytes_done(new_b);
                shared.set_phase(0);
                thread::sleep(tick);
            }

            // Phase 1: verify.
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

            // Phase 2: extract.
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
        // Static: pin to 50% so the layout reads naturally without
        // animation noise.
        shared.set_pct(50);
        shared.set_bytes_done(total_bytes / 2);
        shared.set_phase(0);
    }

    // ------------------------------------------------------------------
    // Show the dialog modally. Blocks until the user clicks Cancel,
    // closes the window, or the simulated worker sets done != 0.
    // ------------------------------------------------------------------
    let started_at = Instant::now();
    let result = unsafe { progress_window::show(std::ptr::null_mut(), "Snug — progress dialog preview", "", shared.clone()) };
    let elapsed = started_at.elapsed();

    eprintln!(
        "progress_preview: dialog returned {} after {:?}",
        result, elapsed
    );

    // Free the mascot DIB section to avoid a one-off GDI handle leak
    // each preview run. Done after `show()` returns so the dialog
    // never sees a dangling HBITMAP mid-paint.
    let hbmp = shared.mascot_hbitmap();
    if hbmp != 0 {
        unsafe {
            windows_sys::Win32::Graphics::Gdi::DeleteObject(hbmp as _);
        }
    }
}

/// Decode `png_bytes` into a 32-bpp top-down DIB section suitable for
/// `StretchBlt` straight into the mascot slot. Returns `(hbitmap,
/// width, height)` on success.
///
/// RGBA → BGRA swap is done in place because GDI's `BI_RGB` 32-bpp
/// layout treats each 4-byte pixel as `B, G, R, X`. Alpha is ignored
/// by the `SRCCOPY` raster op used in `draw_mascot_hbitmap`, so a
/// transparent PNG will composite flat over the dialog's white
/// background — same caveat noted in the dialog code.
fn load_mascot_from_png(png_bytes: &[u8]) -> Result<(isize, u32, u32), String> {
    use windows_sys::Win32::Graphics::Gdi::{
        BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CreateDIBSection, DIB_RGB_COLORS,
    };

    let img = image::load_from_memory_with_format(png_bytes, image::ImageFormat::Png)
        .map_err(|e| format!("decode PNG: {e}"))?;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return Err(format!("PNG decoded to {w}x{h}"));
    }
    let mut pixels = img.into_rgba8().into_raw();

    // Swap R and B per pixel; leave the alpha byte alone.
    for chunk in pixels.chunks_exact_mut(4) {
        chunk.swap(0, 2);
    }

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32), // negative = top-down rows
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

    let mut bits_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
    let hbmp = unsafe {
        CreateDIBSection(
            std::ptr::null_mut(),
            &bmi,
            DIB_RGB_COLORS,
            &mut bits_ptr,
            std::ptr::null_mut(),
            0,
        )
    };
    if hbmp.is_null() || bits_ptr.is_null() {
        return Err(format!(
            "CreateDIBSection failed (hbmp={hbmp:p}, bits={bits_ptr:p})"
        ));
    }

    unsafe {
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits_ptr as *mut u8, pixels.len());
    }
    // `pixels` (Vec) drops here, releasing the staging buffer. The
    // DIB section owns its own copy now.

    Ok((hbmp as isize, w, h))
}