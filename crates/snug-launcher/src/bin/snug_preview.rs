//! `snug_preview` — the one-click dialog previewer.
//!
//! Renders every launcher dialog so a copy change in
//! `snug-localisations.<tag>.txt` can be judged by eye instead of by
//! rebuilding an app and breaking it to find out. It is a **Windows-only
//! dev tool**: it previews hand-painted Win32 windows, so there is nothing
//! for it to preview on another platform.
//!
//! # Why the implementation lives one directory down
//!
//! This bin used to be a single file with a file-level
//! `#![cfg(windows)]`. That is the tidiest way to say "Windows only", and
//! it is what `jdk_install.rs` and `splash.rs` still do — but it does not
//! work in a **binary** crate root. `#![cfg(windows)]` configures out the
//! whole file, `main` included, and the linker then fails with
//!
//! ```text
//! error[E0601]: `main` function not found in crate `snug_preview`
//! ```
//!
//! So on a macOS dev box `cargo build --workspace` could not complete at
//! all, which defeats the point of a workspace on a platform where snug
//! is *developed* but not *run*. The fix is the one cargo forces on a
//! platform-specific binary: keep a real `main` in the bin root and put
//! the platform body behind `#[cfg]`.
//!
//! Per-item `#[cfg(windows)]` would also work and would avoid the extra
//! file, but this file has ~73 top-level items. A single `#[cfg(windows)]`
//! on the `mod` declaration covers all of them and leaves the
//! implementation byte-identical.
//!
//! The bin *name* is unchanged, so `cargo run --bin snug_preview` and
//! `build-release.cmd` need no edits.

#[cfg(windows)]
#[path = "snug_preview/windows_impl.rs"]
mod imp;

#[cfg(target_os = "macos")]
#[path = "snug_preview/macos_impl.rs"]
mod imp;

/// Windows entry point. Delegates straight through — on Windows this bin
/// behaves exactly as it did when `windows_impl.rs` was its own crate root.
#[cfg(windows)]
fn main() {
    imp::run()
}

/// macOS entry point.
///
/// The macOS implementation is a different shape entirely, not a port:
/// those dialogs are real `NSWindow`s behind ordinary functions, so the
/// previewer calls them rather than reimplementing them. See the file's
/// own header for why that is both shorter and more honest than the
/// Windows one.
#[cfg(target_os = "macos")]
fn main() {
    imp::run()
}

/// Entry point for platforms with no dialogs to preview, so that
/// `cargo build --workspace` and `cargo test --workspace` succeed on a
/// Linux dev box.
///
/// `--help` is answered here rather than refused, because the help text is
/// the one part of this tool that is genuinely platform-independent — and
/// a one-line "not on this platform" beats `error[E0601]`.
#[cfg(not(any(windows, target_os = "macos")))]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "snug_preview - preview the snug launcher dialogs\n\n\
             USAGE:\n    \
             snug_preview [--localisation <DIR> | --localization <DIR>] [--mascot <FILE>]\n\n\
             This is a development tool for the platforms snug has dialogs on: \
             **Windows** and\n**macOS**. It has no Linux implementation, because \
             there are no dialogs to preview\nthere yet.\n\n\
             Windows:\n    \
             cargo run --bin snug_preview -- [--localisation localisations]\n\n\
             macOS:\n    \
             cargo run --bin snug_preview -- --mascot assets/snug-icon.png"
        );
        return;
    }

    eprintln!(
        "snug_preview has no implementation for this platform (it previews the\n\
         Windows and macOS dialogs). Build and run it on one of those:\n    \
         cargo run --bin snug_preview"
    );
    std::process::exit(1);
}
