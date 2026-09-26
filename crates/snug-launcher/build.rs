//! Generate a multi-resolution `snug-icon.ico` from
//! `assets/snug-icon.png` at build time and link it as a Windows
//! `MAINICON` resource so `find_best_icon_hicon` (`jdk_install.rs`)
//! finds the icon when the dialogs need an EXE-icon fallback for
//! the mascot slot.
//!
//! The ICO carries one PNG-encoded entry per standard Windows icon
//! size — 16, 32, 48, 64, 128, 256 px. Vista+ accepts PNG-encoded
//! ICO entries natively so we don't need a BMP encoder. Each
//! resolution is independently encoded (Lanczos3-resampled from
//! the source) so File Explorer / taskbar / Title-bar icon all
//! pick the entry closest to the rendered size — no scaling
//! artefacts from a single over-large source.
//!
//! We write the ICO + a one-line `.rc` to `OUT_DIR`, then hand the
//! `.rc` to `embed_resource::compile_for_everything`, which invokes
//! `rc.exe` (MSVC), `windres` (MinGW), or LLVM's `RC` driver
//! depending on the active toolchain and emits a single
//! un-binned `cargo:rustc-link-arg=…` so the icon is picked up by
//! every rustc invocation in the package (the primary bin,
//! examples, tests, benchmarks) without ever appearing twice on
//! the link command line.
//!
//! Cross-compilation via `cargo zigbuild`: `embed-resource` will
//! pick up `windres` / `x86_64-w64-mingw32-windres` on PATH if
//! available; on hosts without it the build fails loudly with a
//! clear error from `embed-resource`. Dev builds on
//! `windows-latest` runners have `rc.exe` available.
//!
//! **`snug_preview.exe` icon override** (uses
//! `assets/snug-preview.png` instead of `snug-icon.png`). The
//! un-binned MAINICON above applies to every dev-time bin in this
//! crate (primary launcher, examples, tests), including the
//! preview binary. To swap to the preview-specific icon, run the
//! `stamp_preview_icon` helper **after** `cargo build --bin
//! snug_preview`:
//!
//! ```bash
//! cargo build --bin snug_preview
//! cargo run --bin stamp_preview_icon
//! ```
//!
//! See `crates/snug-launcher/src/bin/stamp_preview_icon.rs` for the
//! helper. Cargo doesn't expose a post-link hook, and
//! `compile_for_everything` can't apply per-bin overrides without
//! a linker-resource conflict, so a manual step is the cleanest
//! path. The first build also needs `cargo build --bin
//! stamp_preview_icon` once to compile the helper.

use std::env;
use std::fs;
use std::io::Write;
use std::path::Path;

fn main() {
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR set by cargo");
    let out_dir = Path::new(&out_dir);
    let ico_path = out_dir.join("snug-icon.ico");
    let rc_path = out_dir.join("icon.rc");

    write_multi_resolution_ico("assets/snug-icon.png", &ico_path);

    let mut rc = fs::File::create(&rc_path).expect("create icon.rc");
    writeln!(rc, "MAINICON ICON \"snug-icon.ico\"").expect("write icon.rc");

    // `compile_for_everything` invokes the active toolchain's
    // resource compiler (`rc.exe` on MSVC, `windres` on MinGW /
    // GNU, LLVM's `RC` driver on `cargo zigbuild` cross-compiles)
    // and emits `cargo:rustc-link-arg=<out_dir>/icon.lib` — the
    // *un-binned* form. That single emission is what every rustc
    // invocation in this package picks up: the primary
    // `snug-launcher.exe` bin, every example, every test. Using
    // the un-binned form here (instead of `embed_resource::compile`
    // which emits the `cargo:rustc-link-arg-bins=` plural form)
    // is the difference between MSVC's `cvtres` accepting the
    // produced `icon.lib` and rejecting it with
    // `CVT1100: duplicate resource` + `LNK1123`.
    //
    // For cross-compilation via `cargo zigbuild` the user is
    // responsible for putting the appropriate resource compiler
    // on PATH; if it's missing the build fails with a clear
    // error from `embed-resource`.
    let _ = embed_resource::compile_for_everything(&rc_path, embed_resource::NONE);
}

/// Standard Windows icon sizes. Picked to cover File Explorer
/// (16/32), taskbar (32/48), shortcut icons (32/48), high-DPI
/// tiles (64/128), and the modern "extra large" view (256).
/// Adding 96/192 wouldn't hurt; the ICO format limits each entry
/// to 0..=255 px (0 means "256 or larger"), so we cap at 256.
const ICON_SIZES: &[u32] = &[16, 32, 48, 64, 128, 256];

/// Build a multi-resolution ICO from `png_src`. Each standard size
/// is encoded as its own PNG (Lanczos3-resampled from the source)
/// and packed behind an `ICONDIR` with one `ICONDIRENTRY` per size.
///
/// Layout (standard Windows ICO, *not* the 14-byte editpe variant
/// `find_best_icon_hicon` has a workaround for):
///
/// ```text
/// ICONDIR (6 B)            — reserved(2) + type(2) + count(2)
/// ICONDIRENTRY × count (16 B each)
///   — width(1) + height(1) + colours(1) + reserved(1)
///   + planes(2) + bit_count(2) + size(4) + offset(4)
/// PNG bytes for entry 0
/// PNG bytes for entry 1
/// ...
/// ```
fn write_multi_resolution_ico(png_src: &str, ico_dst: &Path) {
    let src_img = image::open(png_src)
        .unwrap_or_else(|e| panic!("snug-launcher build.rs: open {png_src}: {e}"));

    // Encode each size as PNG, keeping the (width, height, bytes).
    let mut entries: Vec<(u32, u32, Vec<u8>)> = Vec::with_capacity(ICON_SIZES.len());
    for &size in ICON_SIZES {
        let resized = src_img.resize_exact(
            size,
            size,
            image::imageops::FilterType::Lanczos3,
        );
        // `image::write_to` requires `Write + Seek` — `Vec<u8>` only
        // implements `Write`, so we wrap in a `Cursor` and pull the
        // inner `Vec` back out afterwards.
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut cursor = std::io::Cursor::new(&mut buf);
            resized
                .write_to(&mut cursor, image::ImageFormat::Png)
                .unwrap_or_else(|e| {
                    panic!("snug-launcher build.rs: encode {size}x{size} PNG: {e}")
                });
        }
        entries.push((size, size, buf));
    }

    // ICONDIR (6 bytes).
    let mut ico: Vec<u8> = Vec::new();
    ico.extend_from_slice(&[0, 0]); // reserved
    ico.extend_from_slice(&1u16.to_le_bytes()); // type = 1 (ICO)
    ico.extend_from_slice(&(entries.len() as u16).to_le_bytes());

    // Walk ICONDIRENTRY records twice — once to compute each
    // entry's data offset, then to emit the entries — so the
    // `offset` field on the Nth entry points past the 6-byte
    // header, the N-1 entry headers, and the N-1 PNGs.
    let header_bytes = 6u32;
    let entry_bytes = (entries.len() as u32) * 16;
    let mut data_offset = header_bytes + entry_bytes;
    let mut offsets = Vec::with_capacity(entries.len());
    for (_, _, png) in &entries {
        offsets.push(data_offset);
        data_offset += png.len() as u32;
    }

    // ICONDIRENTRY × count (16 bytes each).
    for ((w, h, png), offset) in entries.iter().zip(offsets.iter()) {
        // 0xFF would mean "256", but `width` / `height` are single
        // bytes — 256 doesn't fit. The ICO convention is to write 0
        // when the actual dimension is 256 (or larger).
        let dim_byte = |d: u32| if d >= 256 { 0u8 } else { d as u8 };
        ico.push(dim_byte(*w)); // width
        ico.push(dim_byte(*h)); // height
        ico.push(0); // colour count (0 = no palette)
        ico.push(0); // reserved
        ico.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        ico.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        ico.extend_from_slice(&(png.len() as u32).to_le_bytes()); // image size
        ico.extend_from_slice(&offset.to_le_bytes()); // image offset
    }

    // Image data — one PNG per entry, in the order the entries
    // were declared.
    for (_, _, png) in &entries {
        ico.extend_from_slice(png);
    }

    fs::write(ico_dst, &ico)
        .unwrap_or_else(|e| panic!("snug-launcher build.rs: write ICO: {e}"));

    // Re-run this build script whenever the PNG changes.
    println!("cargo:rerun-if-changed={png_src}");
}