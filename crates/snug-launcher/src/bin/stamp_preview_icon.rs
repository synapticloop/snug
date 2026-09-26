//! Stamp `assets/snug-preview.png` into `target/<profile>/snug_preview.exe`
//! via [`editpe`]. Used to apply a per-bin icon override: the default
//! `MAINICON` group (snug-icon.png) linked in by `build.rs` via
//! `embed_resource::compile_for_everything` is replaced wholesale by
//! this PNG-encoded one.
//!
//! Usage (from the workspace root):
//!
//! ```bash
//! # build the preview binary (default icon from snug-icon.png)
//! cargo build --bin snug_preview
//! # replace its MAINICON with assets/snug-preview.png
//! cargo run --bin stamp_preview_icon
//! ```
//!
//! The defaults resolve to:
//! - `<workspace>/target/<profile>/snug_preview.exe`
//! - `<workspace>/assets/snug-preview.png`
//!
//! where `<profile>` is read from `PROFILE` (cargo's standard env var
//! when running `cargo run` / `cargo build`).
//!
//! Pass explicit paths as positional args to override:
//!
//! ```bash
//! cargo run --bin stamp_preview_icon -- <exe_path> <png_path>
//! ```
//!
//! Exit code 0 on success, 1 on failure (with stderr message).

use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (exe_path, png_path) = match args.len() {
        // `cargo run --bin stamp_preview_icon -- <exe> <png>` — explicit
        3 => (PathBuf::from(&args[1]), PathBuf::from(&args[2])),
        // `cargo run --bin stamp_preview_icon` — defaults below
        _ => (
            default_exe_path(),
            default_png_path(),
        ),
    };

    if let Err(e) = stamp_icon(&exe_path, &png_path) {
        eprintln!("stamp_preview_icon: failed: {e}");
        std::process::exit(1);
    }
    println!(
        "stamp_preview_icon: stamped {} from {}",
        exe_path.display(),
        png_path.display(),
    );
}

fn default_exe_path() -> PathBuf {
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    workspace_root().join("target").join(profile).join("snug_preview.exe")
}

fn default_png_path() -> PathBuf {
    workspace_root().join("assets").join("snug-preview.png")
}

/// `CARGO_MANIFEST_DIR` at compile time is the snug-launcher crate
/// root (`crates/snug-launcher/`); the workspace root is two levels
/// up (`snug/`). `target/` and `assets/` both live at the workspace
/// root.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn stamp_icon(exe: &std::path::Path, png: &std::path::Path) -> Result<(), String> {
    let png_str = png
        .to_str()
        .ok_or_else(|| format!("png path {} is not valid UTF-8", png.display()))?;

    let mut image = editpe::Image::parse_file(exe)
        .map_err(|e| format!("parse_file({}): {e}", exe.display()))?;

    // Pull the existing resource directory out of the EXE so any
    // other resources (RT_VERSION, RT_MANIFEST) survive the
    // re-stamp — `set_resource_directory` replaces the whole
    // directory, so we have to keep the old contents around.
    let mut resources = image
        .resource_directory()
        .cloned()
        .unwrap_or_default();

    // Two-step cleanup before adding the new icon:
    //
    //   1. `remove_main_icon` strips the RT_GROUP_ICON MAINICON entry
    //      and the RT_ICON entries it currently references. That's
    //      what editpe's docs explicitly recommend before
    //      `set_main_icon`.
    //
    //   2. **But** every previous stamp (including the ones before
    //      this fix landed) appended new icons with monotonically
    //      higher IDs and left the *previous* MAINICON's icons as
    //      orphans in RT_ICON. `remove_main_icon` doesn't see those
    //      orphans because no group currently points at them, so a
    //      naive stamp grows the resource section by 6 PNGs per run
    //      forever. Clear RT_ICON in full after the group-side
    //      cleanup so only the freshly-added icons survive.
    //
    // `RT_ICON` = id 3 in the Windows resource-type namespace.
    // `Image::set_resource_directory` rebuilds the on-disk section
    // from the in-memory tree, so clearing entries here drops the
    // orphan bytes from the file too — not just the directory view.
    resources
        .remove_main_icon()
        .map_err(|e| format!("remove_main_icon: {e}"))?;

    let icon_table_id = editpe::ResourceEntryName::ID(3);
    if let Some(editpe::ResourceEntry::Table(icon_table)) = resources.root_mut().get_mut(&icon_table_id) {
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
