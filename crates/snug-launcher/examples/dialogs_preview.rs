//! Tiny preview harness for every dialog the launcher currently shows.
//!
//! Run from the workspace root:
//!
//! ```bash
//! cargo run -p snug-launcher --example dialogs_preview -- --help
//! cargo run -p snug-launcher --example dialogs_preview -- --kind progress
//! cargo run -p snug-launcher --example dialogs_preview -- --kind progress --static
//! cargo run -p snug-launcher --example dialogs_preview -- --kind metadata-failed
//! cargo run -p snug-launcher --example dialogs_preview -- --kind retry
//! cargo run -p snug-launcher --example dialogs_preview -- --kind error
//! cargo run -p snug-launcher --example dialogs_preview -- --kind early-bail
//! ```
//!
//! Mirrors `progress_preview` — every kind invokes the **same** code
//! path the production launcher takes, just with hard-coded sample
//! data instead of a real Adoptium fetch. Lets you iterate on dialog
//! layout / copy / button labels without rebuilding `snug-cli`,
//! `snug-format`, or the committed `bin/launcher-stub.exe`.
//!
//! `--kind` selects which dialog:
//!
//! - `progress`        — `progress_window::show` (the mockup-aligned
//!                        download bar window). Accepts `--static`,
//!                        `--pause`, `--no-mascot`, `<size_mb>`,
//!                        `<speed_mb_s>` like `progress_preview`.
//! - `metadata-failed` — `jdk_install::show_metadata_failed_dialog`.
//!                        Renders the "Couldn't reach Adoptium" prompt
//!                        with a fake network error in the expanded
//!                        details.
//! - `retry`           — `jdk_install::show_retry_dialog`. Renders
//!                        the "Try again / Cancel" prompt with a fake
//!                        transient-failure error.
//! - `error`           — `jdk_install::show_error_dialog`. Renders the
//!                        terminal post-install-failure dialog (single
//!                        OK) with a fake fatal error.
//! - `early-bail`      — `MessageBoxW` from `src/main.rs`. The fallback
//!                        shown when the launcher can't even load its
//!                        embedded payload.
//!
//! `--icon normal|warning|error|info` overrides the icon-kind for the
//! `error` kind. The other kinds pin to whatever the production code
//! uses (Warning for metadata-failed, none for retry, Error for
//! `show_error_dialog`).

#![cfg(windows)]

use std::env;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use snug_launcher::jdk_install::{self, ProgressShared};
use snug_launcher::progress_window;

fn main() {
    let args: Vec<String> = env::args().collect();
    let opts = match Options::parse(&args) {
        Ok(o) => o,
        Err(msg) => {
            eprintln!("dialogs_preview: {msg}");
            eprintln!("run with --help for usage");
            std::process::exit(2);
        }
    };

    let result = match opts.kind {
        Kind::Progress => run_progress(opts),
        Kind::MetadataFailed => run_metadata_failed(),
        Kind::Retry => run_retry(),
        Kind::Error => run_error(),
        Kind::EarlyBail => run_early_bail(),
    };

    if let Some(code) = result {
        eprintln!("dialogs_preview: dialog returned {code}");
    }
}

// =============================================================================
//  CLI
// =============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Progress,
    MetadataFailed,
    Retry,
    Error,
    EarlyBail,
}

impl Kind {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "progress" => Some(Self::Progress),
            "metadata-failed" => Some(Self::MetadataFailed),
            "retry" => Some(Self::Retry),
            "error" => Some(Self::Error),
            "early-bail" => Some(Self::EarlyBail),
            _ => None,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Progress => "progress",
            Self::MetadataFailed => "metadata-failed",
            Self::Retry => "retry",
            Self::Error => "error",
            Self::EarlyBail => "early-bail",
        }
    }
}

struct Options {
    kind: Kind,
    /// `--static`: pin the progress dialog at 50% with no worker.
    static_mode: bool,
    /// `--pause`: start the worker but wait for the user to click
    /// Install before the simulated download begins.
    pause_mode: bool,
    /// `--no-mascot`: skip the `assets/snug-icon.png` decode and let
    /// the mascot slot fall back to the EXE icon.
    skip_mascot: bool,
    /// Simulated file size in MB (progress only). Default 150.
    size_mb: u64,
    /// Simulated download speed in MB/s (progress only). Default 10.
    speed_mb_s: f64,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut kind: Option<Kind> = None;
        let mut static_mode = false;
        let mut pause_mode = false;
        let mut skip_mascot = false;
        let mut size_mb: u64 = 150;
        let mut speed_mb_s: f64 = 10.0;

        let mut i = 1;
        while i < args.len() {
            let arg = &args[i];
            match arg.as_str() {
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                "--kind" => {
                    let v = args
                        .get(i + 1)
                        .ok_or_else(|| "--kind needs a value".to_string())?;
                    kind = Some(
                        Kind::parse(v)
                            .ok_or_else(|| format!("unknown --kind {v:?}; expected one of: progress, metadata-failed, retry, error, early-bail"))?,
                    );
                    i += 2;
                }
                "--static" => {
                    static_mode = true;
                    i += 1;
                }
                "--pause" => {
                    pause_mode = true;
                    i += 1;
                }
                "--no-mascot" => {
                    skip_mascot = true;
                    i += 1;
                }
                _ => {
                    if let Ok(n) = arg.parse::<u64>() {
                        if size_mb == 150 {
                            size_mb = n;
                            i += 1;
                            continue;
                        }
                    }
                    if let Ok(n) = arg.parse::<f64>() {
                        speed_mb_s = n;
                        i += 1;
                        continue;
                    }
                    return Err(format!("unexpected argument: {arg:?}"));
                }
            }
        }

        let kind = kind.ok_or_else(|| "--kind is required".to_string())?;
        Ok(Self {
            kind,
            static_mode,
            pause_mode,
            skip_mascot,
            size_mb,
            speed_mb_s,
        })
    }
}

fn print_help() {
    eprintln!("usage: dialogs_preview --kind <KIND> [options]");
    eprintln!();
    eprintln!("KIND:");
    eprintln!("  progress         JDK download progress dialog (progress_window::show)");
    eprintln!("  metadata-failed  Could not reach Adoptium prompt (show_metadata_failed_dialog)");
    eprintln!("  retry            Try again / Cancel prompt (show_retry_dialog)");
    eprintln!("  error            Terminal post-install-failure dialog (show_error_dialog)");
    eprintln!("  early-bail       MessageBoxW from src/main.rs (show_error_box)");
    eprintln!();
    eprintln!("options:");
    eprintln!("  --static         (progress only) pin at 50%, no animation");
    eprintln!("  --pause          (progress only) wait for user to click Install");
    eprintln!("  --no-mascot      (progress only) skip PNG mascot decode");
    eprintln!("  <size_mb>        (progress only) simulated file size, default 150");
    eprintln!("  <speed_mb_s>     (progress only) simulated download speed, default 10");
}

// =============================================================================
//  Per-kind dispatchers
// =============================================================================

fn run_progress(opts: Options) -> Option<i32> {
    // Mirrors `progress_preview` exactly so this preview stays in sync
    // with that one. If you change the simulation here, mirror it
    // there.
    let total_bytes: u64 = opts.size_mb * 1_048_576;
    let bytes_per_ms: u64 = ((opts.speed_mb_s * 1_048_576.0) / 1000.0).round() as u64;

    let shared = Arc::new(ProgressShared::new(total_bytes));

    if !opts.skip_mascot {
        match load_mascot_from_png(include_bytes!("../../../assets/snug-icon.png")) {
            Ok((hbitmap, w, h)) => {
                shared.set_mascot_hbitmap(hbitmap as i32);
                eprintln!("dialogs_preview: mascot loaded {w}x{h}");
            }
            Err(e) => {
                eprintln!(
                    "dialogs_preview: mascot PNG decode failed: {e} — falling back to EXE icon"
                );
            }
        }
    }

    if !opts.static_mode {
        let shared = shared.clone();
        thread::spawn(move || {
            if opts.pause_mode {
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

            shared.set_status(1);
        });
    } else {
        shared.set_pct(50);
        shared.set_bytes_done(total_bytes / 2);
        shared.set_phase(0);
    }

    let started_at = Instant::now();
    let result = unsafe {
        progress_window::show(
            std::ptr::null_mut(),
            "Snug — progress dialog preview",
            "",
            shared.clone(),
        )
    };
    eprintln!(
        "dialogs_preview: kind={} returned {} after {:?}",
        Kind::Progress.as_str(),
        result,
        started_at.elapsed()
    );

    let hbmp = shared.mascot_hbitmap();
    if hbmp != 0 {
        unsafe {
            windows_sys::Win32::Graphics::Gdi::DeleteObject(hbmp as _);
        }
    }
    None
}

fn run_metadata_failed() -> Option<i32> {
    // Sample network error detail so the user can see how the
    // expanded section reads. The production code passes a real
    // `JdkError` Display here.
    let fake_error = "DNS error: no such host is known. (api.adoptium.net:443)";
    let result = jdk_install::show_metadata_failed_dialog(
        std::ptr::null_mut(),
        25,
        fake_error,
    );
    eprintln!(
        "dialogs_preview: kind={} button={result}",
        Kind::MetadataFailed.as_str()
    );
    Some(result)
}

fn run_retry() -> Option<i32> {
    // Sample transient-failure error. The production code passes a
    // real `JdkError` Display here.
    let fake_error = "Connection reset by peer (HTTP 0 after 47.2 MB)";
    let retry_again = jdk_install::show_retry_dialog(
        std::ptr::null_mut(),
        1,
        3,
        "25.0.1+8.LTS",
        fake_error,
    );
    eprintln!(
        "dialogs_preview: kind={} retry_again={retry_again}",
        Kind::Retry.as_str()
    );
    Some(if retry_again { 1 } else { 2 })
}

fn run_error() -> Option<i32> {
    jdk_install::show_error_dialog(
        std::ptr::null_mut(),
        "Could not download or install Eclipse Temurin — Snug",
        "Eclipse Temurin 25.0.1+8.LTS",
        "All 3 download attempts failed.\n\n\
         Most recent error:\n\
         Connection reset by peer (HTTP 0 after 47.2 MB)\n\n\
         The launch will continue using whichever Java (if any) is already \
         installed on this machine.",
    );
    eprintln!(
        "dialogs_preview: kind={} dismissed",
        Kind::Error.as_str()
    );
    None
}

fn run_early_bail() -> Option<i32> {
    // Mirror `src/main.rs::show_error_box` exactly — that's what the
    // launcher shows when it can't even load its embedded payload.
    let title: Vec<u16> = OsStr::new("snug launcher")
        .encode_wide()
        .chain(Some(0))
        .collect();
    let body: Vec<u16> = OsStr::new(
        "FATAL: failed to read snug payload from RCDATA resource.\n\
         This binary may be corrupted or stamped with the wrong manifest.\n\
         Re-download the launcher from the original source.",
    )
    .encode_wide()
    .chain(Some(0))
    .collect();
    let result = unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
            std::ptr::null_mut(),
            body.as_ptr(),
            title.as_ptr(),
            windows_sys::Win32::UI::WindowsAndMessaging::MB_OK
                | windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
        )
    };
    eprintln!(
        "dialogs_preview: kind={} MessageBoxW returned {result}",
        Kind::EarlyBail.as_str()
    );
    Some(result)
}

// =============================================================================
//  Mascot PNG → DIB section
// =============================================================================
//
// Same loader as `progress_preview`. Duplicated rather than shared so
// each example stays self-contained — neither imports the other.

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
            biHeight: -(h as i32),
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

    Ok((hbmp as isize, w, h))
}