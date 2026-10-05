//! Stamp `assets/snug-dropper.png` into `target/<profile>/snug-dropper.exe`
//! via [`editpe`].
//!
//! Mirrors `snug-launcher`'s `stamp_preview_icon` helper, for the same
//! reason: it applies a **per-binary icon override** to an artefact that
//! is renamed at packaging time into `Build with Snug.exe`, so the icon
//! can be swapped (and re-judged against a 16 px taskbar slot) without a
//! Rust rebuild.
//!
//! This matters more here than for the preview tool. The dropper is the
//! one artefact whose *whole interface* is its icon: a user identifies it
//! by the artwork they drag a JAR onto, and a 240 KB binary with a
//! generic default icon is easy to mis-drop onto something else.
//!
//! Usage (from the workspace root):
//!
//! ```bash
//! # build the dropper
//! cargo build --release --bin snug-dropper
//! # replace its MAINICON with assets/snug-dropper.png
//! cargo run --release --bin stamp_dropper_icon
//! ```
//!
//! To also emit the shipped, correctly-named copy in one go:
//!
//! ```bash
//! cargo run --release --bin stamp_dropper_icon -- --package
//! # -> target\release\Build with Snug.exe, beside snug.exe
//! ```
//!
//! `--package` takes an optional destination directory; it defaults to
//! the built exe's own directory, which on a native build is already
//! where `snug.exe` sits — and the dropper resolves `snug.exe` relative
//! to its *own* location at runtime, so those two have to ship in the
//! same folder.
//!
//! Defaults resolve to:
//! - `<workspace>/target/<profile>/snug-dropper.exe`, where `<profile>` is
//!   the profile this helper was itself built with
//! - `<workspace>/assets/snug-dropper.png`
//!
//! Pass explicit paths as positional args to override:
//!
//! ```bash
//! cargo run --release --bin stamp_dropper_icon -- <exe_path> <png_path>
//! ```
//!
//! Exit code 0 on success, 1 on failure (with a stderr message).
//!
//! **Windows-only**, for the same reason as the dropper itself: it stamps
//! a `.exe` that only ever ships to Windows users. The items below carry
//! per-item `#[cfg(windows)]` rather than one file-level
//! `#![cfg(windows)]`, so the crate still builds on a macOS / Linux dev
//! box — a bin whose whole body is configured out has no `main`, which
//! fails `cargo build --workspace` with `E0601`. See `../main.rs` for the
//! longer version of this note.

#[cfg(windows)]
use std::path::{Path, PathBuf};

/// The name the dropper ships under. Windows shows this in the taskbar
/// and to anyone browsing the folder, so it has to be a real display
/// name rather than the crate's target name.
#[cfg(windows)]
const SHIPPED_NAME: &str = "Build with Snug.exe";

#[cfg(windows)]
fn main() {
    let mut package: Option<Option<PathBuf>> = None;
    let mut positional: Vec<String> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--package" || arg == "-p" {
            // The destination is optional: `--package`, `--package DIR`.
            // A following token that isn't a flag is taken as the dir.
            let dir = args
                .next()
                .filter(|next| !next.starts_with('-'))
                .map(PathBuf::from);
            package = Some(dir);
        } else if let Some(dir) = arg.strip_prefix("--package=") {
            package = Some(Some(PathBuf::from(dir)));
        } else if arg == "--help" || arg == "-h" {
            print_help();
            return;
        } else {
            positional.push(arg);
        }
    }

    let (exe_path, png_path) = match positional.len() {
        0 => (default_exe_path(), default_png_path()),
        2 => (PathBuf::from(&positional[0]), PathBuf::from(&positional[1])),
        n => {
            eprintln!(
                "stamp_dropper_icon: expected 0 or 2 positional paths \
                 (<exe> <png>), got {n}"
            );
            std::process::exit(2);
        }
    };

    if !png_path.is_file() {
        eprintln!(
            "stamp_dropper_icon: no icon at {}\n\
             hint: save the artwork there, or pass <exe_path> <png_path>",
            png_path.display()
        );
        std::process::exit(1);
    }

    if let Err(e) = stamp_icon(&exe_path, &png_path) {
        eprintln!("stamp_dropper_icon: failed: {e}");
        std::process::exit(1);
    }
    println!(
        "stamp_dropper_icon: stamped {} from {}",
        exe_path.display(),
        png_path.display()
    );

    let Some(dest) = package else {
        return;
    };

    // Default to the exe's own folder, which is where `snug.exe`
    // already lives on a native build.
    let dest = dest.unwrap_or_else(|| {
        exe_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    });
    let out = dest.join(SHIPPED_NAME);

    if let Err(e) = emit_package(&exe_path, &dest, &out) {
        eprintln!("stamp_dropper_icon: packaging failed: {e}");
        std::process::exit(1);
    }
    println!(
        "stamp_dropper_icon: packaged {} (stamped icon included)",
        out.display()
    );
}

#[cfg(windows)]
fn print_help() {
    println!(
        "stamp_dropper_icon - stamp an icon into snug-dropper.exe and optionally\n\
         emit the shipped copy.\n\
         \n\
         USAGE:\n    \
         stamp_dropper_icon [--package [DIR]] [<exe_path> <png_path>]\n\
         \n\
         OPTIONS:\n    \
         -p, --package [DIR]  Copy the stamped binary to DIR\\{SHIPPED_NAME}.\n                       \
         DIR defaults to the exe's own folder.\n    \
         -h, --help           Show this message.\n\
         \n\
         DEFAULTS:\n    \
         exe: <workspace>/target/<profile>/snug-dropper.exe\n    \
         png: <workspace>/assets/snug-dropper.png"
    );
}

/// Copy the stamped binary to its shipped name under `dest`.
///
/// Copies rather than moves: `target\` is cargo's output and is
/// routinely wiped by `cargo clean`, so the shipping artefact is a
/// derived copy. Stamping is idempotent, so re-running the whole
/// pipeline over an already-stamped `target\` binary is safe.
#[cfg(windows)]
fn emit_package(exe: &Path, dest: &Path, out: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest)
        .map_err(|e| format!("creating {}: {e}", dest.display()))?;

    // If the dropper is currently running, Windows refuses the copy with
    // a sharing violation. That gets a hint rather than a bare errno,
    // because "cannot access the file" is otherwise a very confusing
    // way to learn that you left the tool open.
    std::fs::copy(exe, out).map_err(|e| {
        format!(
            "copying {} to {}: {e}\n\
             hint: close any running copy of the dropper first",
            exe.display(),
            out.display()
        )
    })?;

    Ok(())
}

#[cfg(windows)]
fn default_exe_path() -> PathBuf {
    // Derive the profile from this helper's *own* build rather than the
    // `PROFILE` env var. `cfg!(debug_assertions)` is always the profile
    // cargo just built this helper with, and it is the same profile as
    // the dropper binary sitting next to it. `PROFILE` is documented as
    // "set by cargo when building" and is not reliably forwarded into
    // the `cargo run` child environment -- when it is absent the old
    // `unwrap_or("debug")` fallback made `cargo run --release --bin
    // stamp_dropper_icon` stamp the *debug* exe and report success while
    // leaving the release one untouched.
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    workspace_root()
        .join("target")
        .join(profile)
        .join("snug-dropper.exe")
}

#[cfg(windows)]
fn default_png_path() -> PathBuf {
    workspace_root().join("assets").join("snug-dropper.png")
}

/// `CARGO_MANIFEST_DIR` at compile time is the snug-dropper crate root
/// (`crates/snug-dropper/`); the workspace root is two levels up (`snug/`).
/// `target/` and `assets/` both live at the workspace root.
#[cfg(windows)]
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(windows)]
fn stamp_icon(exe: &Path, png: &Path) -> Result<(), String> {
    let png_str = png
        .to_str()
        .ok_or_else(|| format!("png path {} is not valid UTF-8", png.display()))?;

    let mut image =
        editpe::Image::parse_file(exe).map_err(|e| format!("parse_file({}): {e}", exe.display()))?;

    // Pull the existing resource directory out of the EXE so any other
    // resources (RT_VERSION, RT_MANIFEST) survive the re-stamp —
    // `set_resource_directory` replaces the whole directory, so the old
    // contents have to be kept around.
    let mut resources = image
        .resource_directory()
        .cloned()
        .unwrap_or_default();

    // Two-step cleanup before adding the new icon:
    //
    //   1. `remove_main_icon` strips the RT_GROUP_ICON MAINICON entry
    //      and the RT_ICON entries it currently references — which is
    //      what editpe's docs recommend before `set_main_icon_file`.
    //
    //   2. **But** every previous stamp appended icons with
    //      monotonically higher IDs and left the *previous* MAINICON's
    //      icons as orphans in RT_ICON. `remove_main_icon` can't see
    //      those (no group points at them), so a naive re-stamp grows
    //      the resource section by one icon per run forever. Clear
    //      RT_ICON wholesale so only the freshly-added icons survive.
    //
    //      This is inherited from `stamp_preview_icon`, where a repeated
    //      stamp was a developer habit; here it matters more, because the
    //      shipping step is `stamp -> rename -> ship` and a rebuild is
    //      expected between icon iterations.
    //
    // `RT_ICON` = id 3 in the Windows resource-type namespace.
    // `set_resource_directory` rebuilds the on-disk section from the
    // in-memory tree, so clearing entries here drops the orphan bytes
    // from the file too, not just the directory view.
    resources
        .remove_main_icon()
        .map_err(|e| format!("remove_main_icon: {e}"))?;

    let icon_table_id = editpe::ResourceEntryName::ID(3);
    if let Some(editpe::ResourceEntry::Table(icon_table)) =
        resources.root_mut().get_mut(&icon_table_id)
    {
        let keys: Vec<_> = icon_table.entries().into_iter().cloned().collect();
        for k in keys {
            icon_table.remove(k);
        }
    }

    resources
        .set_main_icon_file(png_str)
        .map_err(|e| format!("set_main_icon_file({png_str}): {e}"))?;

    image
        .set_resource_directory(resources)
        .map_err(|e| format!("set_resource_directory: {e}"))?;

    image
        .write_file(exe)
        .map_err(|e| format!("write_file({}): {e}", exe.display()))?;
    Ok(())
}

/// Non-Windows entry point — see the module docs. The stamp helper runs
/// as part of `build-windows.cmd` against a natively-built `.exe`; there
/// is no macOS / Linux equivalent to run.
#[cfg(not(windows))]
fn main() {
    eprintln!(
        "stamp_dropper_icon is a Windows-only build helper. It stamps a \
         natively-built snug-dropper.exe via build-windows.cmd."
    );
    std::process::exit(1);
}
