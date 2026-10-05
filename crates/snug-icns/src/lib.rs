//! Build a single-file Apple `.icns` from a source PNG.
//!
//! Extracted from `snug-launcher`'s build script so `snug-cli` can
//! stamp its own icon without depending on the launcher crate. Same
//! reasoning as `snug-payload`: two crates need the same pure
//! function, and build scripts cannot depend on each other, so the
//! alternative is a copy that silently drifts.
//!
//! This is a **build-support** crate. It is a dependency of build
//! scripts only and never appears in a shipped binary's dependency
//! graph.
//!
//! # Why this is written by hand
//!
//! The first version shelled out to `/usr/bin/iconutil`, the only
//! other icns writer on macOS. That was wrong for two reasons: it
//! needed a temporary directory, so a build that could not be given
//! one left behind a binary with *no icon* behind a `cargo:warning`
//! everyone learns to scroll past — a cosmetic default must not be a
//! silent failure mode — and a subprocess is a real dependency for a
//! container this small. Pure Rust means a failure here is a genuine
//! bug, so it panics like the rest of the build script.
//!
//! # Format
//!
//! The container is trivial:
//!
//! ```text
//! 'icns'                    -- magic
//! u32 be  total length      -- including these 8 header bytes
//! then, repeated:
//!   4 bytes  chunk type     -- 'ic07', 'ic10', ...
//!   u32 be  chunk length    -- including these 8 chunk bytes
//!   bytes   payload         -- a whole PNG
//! ```
//!
//! Getting the total length wrong is the classic way to produce an
//! ICNS that parses cleanly and renders as nothing, so it is written
//! as a placeholder and patched once the real total is known.

use std::path::Path;

use anyhow::{Context, Result};
use image::imageops::FilterType;

/// ICNS chunk types, as (pixel size, four-character type).
///
/// Chunk ids and the sizes they carry are Apple's, from the Icon
/// Services documentation. `icp4`/`icp5` are the plain 1x 16 and 32
/// pixel entries; `ic11`..`ic14` and `ic10` are the `@2x` tiers, which
/// is why several of them share a pixel dimension with the entry above.
///
/// **Derived, not remembered**: read a real `App.icns` back out of a
/// built `.app` and dump its chunk list. That showed `iconutil` stores
/// 16/32px as raw `ARGB` (`ic04`, `ic05`) and appends an optional
/// `bpli` chunk. The PNG forms below are equivalent as far as Icon
/// Services is concerned and `bpli` is optional, so neither difference
/// earns a subprocess.
///
/// The size cost is real and deliberate: the 1024px chunk is ~1.2 MB of
/// the ~3 MB total. Dropping it is the lever if it ever matters, at
/// the cost of a softer icon on a Retina tile. The Windows `MAINICON`
/// is tens of KB and unaffected.
pub const ICNS_CHUNKS: &[(u32, &str)] = &[
    (16, "icp4"),
    (32, "icp5"),
    (32, "ic11"),
    (64, "ic12"),
    (128, "ic07"),
    (256, "ic08"),
    (256, "ic13"),
    (512, "ic09"),
    (512, "ic14"),
    (1024, "ic10"),
];

/// Build a single-file ICNS at `icns_dst` from the PNG at `png_src`,
/// one PNG-encoded chunk per entry in [`ICNS_CHUNKS`].
///
/// Panics on any failure. This runs inside a build script, where a
/// missing icon should be a loud build error rather than an artefact
/// that ships without one.
pub fn write_icns(png_src: &str, icns_dst: &Path) {
    try_write_icns(png_src, icns_dst).unwrap_or_else(|e| panic!("{e:#}"));
}

/// Fallible form of [`write_icns`], for callers that can report a
/// failure rather than abort.
///
/// A caller that has a sensible way to surface a missing icon should use
/// this. It is what the `.app` bundler needs: a bundle whose `Info.plist`
/// points `CFBundleIconFile` at an `App.icns` that was never written
/// launches to a generic icon, which looks like the bundler silently
/// forgot it.
pub fn try_write_icns(png_src: &str, icns_dst: &Path) -> Result<()> {
    use std::io::Cursor;

    let bytes =
        std::fs::read(png_src).with_context(|| format!("reading icon artwork {png_src}"))?;
    let img = image::load_from_memory(&bytes)
        .with_context(|| format!("decoding icon artwork {png_src}"))?;

    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(b"icns");
    // Placeholder, patched below.
    out.extend_from_slice(&0u32.to_be_bytes());

    for (size, ty) in ICNS_CHUNKS {
        let resized = img.resize_exact(*size, *size, FilterType::Lanczos3);
        let mut png: Vec<u8> = Vec::new();
        {
            // `write_to` needs `Write + Seek`; `Vec<u8>` is only
            // `Write`, so borrow it through a `Cursor` and take the
            // bytes after.
            let mut cursor = Cursor::new(&mut png);
            resized
                .write_to(&mut cursor, image::ImageFormat::Png)
                .with_context(|| format!("encoding {size}x{size} PNG"))?;
        }
        out.extend_from_slice(ty.as_bytes());
        out.extend_from_slice(&((png.len() + 8) as u32).to_be_bytes());
        out.extend_from_slice(&png);
    }

    let total = (out.len() as u32).to_be_bytes();
    out[4..8].copy_from_slice(&total);

    std::fs::write(icns_dst, &out)
        .with_context(|| format!("writing {}", icns_dst.display()))?;
    Ok(())
}
