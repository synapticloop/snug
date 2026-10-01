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
//! # then copy / rename to "Build with Snug.exe" beside snug.exe
//! ```
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

#![cfg(windows)]

use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (exe_path, png_path) = match args.len() {
        // `cargo run --bin stamp_dropper_icon -- <exe> <png>`
        3 => (PathBuf::from(&args[1]), PathBuf::from(&args[2])),
        _ => (default_exe_path(), default_png_path()),
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
}

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

fn default_png_path() -> PathBuf {
    workspace_root().join("assets").join("snug-dropper.png")
}

/// `CARGO_MANIFEST_DIR` at compile time is the snug-dropper crate root
/// (`crates/snug-dropper/`); the workspace root is two levels up (`snug/`).
/// `target/` and `assets/` both live at the workspace root.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

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
