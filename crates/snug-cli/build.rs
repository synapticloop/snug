//! Link `assets/snug-runner.png` into this package's binaries as a Windows
//! `MAINICON` resource, at link time.
//!
//! ## Why this exists
//!
//! `snug.exe` is the primary shipping artefact and used to have no icon at
//! all unless `scripts\windows\build.cmd` got as far as step 6, which stamps
//! the PNG in *after* the link with the `stamp_preview_icon` helper. That made
//! the icon a property of one script rather than of the build: a plain
//!
//! ```text
//! cargo build --release -p snug-cli --bin snug
//! ```
//!
//! relinked an iconless `snug.exe` over a correctly stamped one, and nothing
//! said so. Cargo cannot help — it exposes no post-link hook, which is the
//! same constraint `snug-launcher`'s build script documents. What cargo *can*
//! do is put a resource on the link command, so the icon is compiled in and
//! the stamp becomes belt-and-braces rather than the only thing holding it up.
//!
//! ## Matching the stamp exactly
//!
//! The resolutions below are `RESOLUTIONS` from `vendor/editpe/src/resource.rs`
//! — `[256, 128, 48, 32, 24, 16]` — not the `[16, 32, 48, 64, 128, 256]` set
//! `snug-launcher/build.rs` uses. Deliberate: `verify_icons` checks the
//! largest MAINICON entry plus the *set* of widths in the group, and demands
//! exactly editpe's set. Generating the same shapes here is what lets a
//! plain `cargo build --release` produce a binary that already passes
//! `verify_icons`, so step 6 has nothing left to correct.
//!
//! `snug-launcher`'s copy of `write_multi_resolution_ico` is close to
//! identical but is not shared: build scripts cannot depend on a sibling
//! package without adding a workspace member, and the two callers want
//! different resolution sets. The ICO container format is stable and is
//! spelled out in both copies.

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The PNG the icon is derived from.
///
/// Deliberately the repo-root `assets/` copy rather than one inside this
/// package: `build.cmd` stamps this same file, and a second copy in
/// `crates/snug-cli/assets/` could drift from it without anything noticing.
/// Reading outside the package directory is safe here because the workspace
/// sets `publish = false` — there is no packaged crate for the path to escape
/// into.
fn icon_png() -> PathBuf {
    Path::new(&env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo"))
        .join("..")
        .join("..")
        .join("assets")
        .join("snug-runner.png")
}

/// Standard Windows icon sizes, in the order `editpe` writes them.
///
/// 256 covers the Explorer extra-large view and high-DPI tiles; 128/48/32/24/16
/// cover the taskbar, title bar and Alt-Tab tiers. The ICO format stores each
/// dimension in one byte with 0 meaning "256 or larger", so 256 is the cap.
const ICON_SIZES: &[u32] = &[256, 128, 48, 32, 24, 16];

fn main() {
    // Re-run when the PNG changes, and when this script changes. Without the
    // second line the first *restricts* cargo to just the PNG, so edits here
    // are silently ignored until the PNG is touched — which is how a link arg
    // comes to look like it was passed when it never was.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", icon_png().display());

    // Windows only. A Mach-O has no resource directory (`editpe` is PE-only),
    // and the macOS icon route is a `__TEXT,__icns` section belonging to
    // `snug-launcher`'s launcher binary — a `.app` reads `Contents/Resources/
    // App.icns` instead, which is the bundle's business, not this crate's.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR set by cargo"));
    let ico_path = out_dir.join("snug-runner.ico");
    let rc_path = out_dir.join("snug-runner.rc");

    write_multi_resolution_ico(&icon_png(), &ico_path);

    let mut rc = fs::File::create(&rc_path).expect("create snug-runner.rc");
    writeln!(rc, "MAINICON ICON \"snug-runner.ico\"").expect("write snug-runner.rc");

    // `compile_for_everything` runs the toolchain's resource compiler
    // (`rc.exe` on MSVC, `windres` on MinGW/GNU, LLVM's `RC` driver under
    // `cargo zigbuild`) and emits the *un-binned* `cargo:rustc-link-arg=…`.
    //
    // Un-binned is required, not lazy: `embed_resource::compile` emits the
    // plural `cargo:rustc-link-arg-bins=` form, and MSVC's `cvtres` rejects
    // the resulting `icon.lib` with `CVT1100: duplicate resource` +
    // `LNK1123`. `snug-launcher`'s build script records the same finding.
    //
    // The un-binned form also reaches this package's other bin (`verify_icons`)
    // and its test harnesses. That is deliberate and harmless: `verify_icons`
    // is a build tool that never ships, and nothing in the workspace depends on
    // `snug-cli`, so the icon cannot leak into the dropper and fight the
    // `snug-dropper.png` it stamps for itself.
    let _ = embed_resource::compile_for_everything(&rc_path, embed_resource::NONE);
}

/// Build a multi-resolution ICO from `png_src`.
///
/// Each size is Lanczos3-resampled from the source and encoded as its own
/// PNG (Vista+ reads PNG-compressed ICO entries natively, so no BMP encoder is
/// needed), then packed behind an `ICONDIR` with one `ICONDIRENTRY` per size.
///
/// ```text
/// ICONDIR (6 B)            -- reserved(2) + type(2) + count(2)
/// ICONDIRENTRY x count (16 B each)
///   -- width(1) + height(1) + colours(1) + reserved(1)
///   + planes(2) + bit_count(2) + size(4) + offset(4)
/// PNG bytes for entry 0, entry 1, ...
/// ```
fn write_multi_resolution_ico(png_src: &Path, ico_dst: &Path) {
    let src_img = image::open(png_src)
        .unwrap_or_else(|e| panic!("snug-cli build.rs: open {}: {e}", png_src.display()));

    let mut entries: Vec<(u32, u32, Vec<u8>)> = Vec::with_capacity(ICON_SIZES.len());
    for &size in ICON_SIZES {
        let resized = src_img.resize_exact(size, size, image::imageops::FilterType::Lanczos3);
        // `write_to` needs `Write + Seek`; a bare `Vec<u8>` is only `Write`, so
        // borrow it through a `Cursor` and take the bytes back afterwards.
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut cursor = std::io::Cursor::new(&mut buf);
            resized
                .write_to(&mut cursor, image::ImageFormat::Png)
                .unwrap_or_else(|e| panic!("snug-cli build.rs: encode {size}x{size} PNG: {e}"));
        }
        entries.push((size, size, buf));
    }

    let mut ico: Vec<u8> = Vec::new();
    ico.extend_from_slice(&[0, 0]); // reserved
    ico.extend_from_slice(&1u16.to_le_bytes()); // type = 1 (ICO)
    ico.extend_from_slice(&(entries.len() as u16).to_le_bytes());

    // Walk ICONDIRENTRY twice — once to compute each entry's data offset, once
    // to emit it — so entry N's `offset` points past the 6-byte header, the
    // N-1 entry headers and the N-1 PNGs.
    let header_bytes = 6u32;
    let entry_bytes = (entries.len() as u32) * 16;
    let mut data_offset = header_bytes + entry_bytes;
    let mut offsets = Vec::with_capacity(entries.len());
    for (_, _, png) in &entries {
        offsets.push(data_offset);
        data_offset += png.len() as u32;
    }

    for ((w, h, png), offset) in entries.iter().zip(offsets.iter()) {
        // width/height are single bytes, so 256 is written as 0 ("256 or
        // larger"), per the ICO convention.
        let dim_byte = |d: u32| if d >= 256 { 0u8 } else { d as u8 };
        ico.push(dim_byte(*w));
        ico.push(dim_byte(*h));
        ico.push(0); // colour count (0 = no palette)
        ico.push(0); // reserved
        ico.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        ico.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
    }

    for (_, _, png) in &entries {
        ico.extend_from_slice(png);
    }

    fs::write(ico_dst, &ico).unwrap_or_else(|e| panic!("snug-cli build.rs: write ICO: {e}"));
}
