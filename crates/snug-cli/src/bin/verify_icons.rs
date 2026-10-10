//! Prove a shipped EXE actually carries the icon it was stamped with.
//!
//! `stamp_preview_icon` and `stamp_dropper_icon` both exit non-zero when
//! a stamp *errors*, so the pipeline already catches "the resource
//! directory would not parse" and "the PNG would not load". What nothing
//! catches is the quieter class of failure:
//!
//!   - the stamp silently did not apply, leaving whatever icon was there
//!     before — correct-looking, wrong artwork;
//!   - the right helper ran against the wrong EXE, so every binary in the
//!     release folder wears the same face;
//!   - a partial stamp, leaving some of the icon resolutions missing;
//!   - orphan `RT_ICON` entries from an earlier run, which is exactly the
//!     accumulation both stamp helpers work to prevent, and which shows
//!     up only as a file mysteriously 300 KB fatter than the last one.
//!
//! So this checks *provenance* rather than presence: it re-derives the
//! icon the stamper should have written and compares it to what is
//! actually in the file.
//!
//! ## What is actually stored
//!
//! `editpe`'s `ToIcon for &DynamicImage` resizes the source to
//! `RESOLUTIONS = [256, 128, 48, 32, 24, 16]` with `Lanczos3`, encodes
//! each as a single-image ICO, and then keeps only the middle of it: 12
//! bytes of `ICONDIRENTRY` metadata, a 2-byte dummy icon id, and the
//! image. `set_main_icon` reads that 14-byte prefix to build the
//! group's `GRPICONDIRENTRY` and stores **only what follows it** —
//! `icon[14..]`.
//!
//! And what follows it is a **PNG**, because that is how `image`'s ICO
//! encoder represents icon entries. So `get_main_icon` hands back a
//! plain PNG, which is a genuinely pleasant thing to verify: decode it
//! with the `png` decoder snug-cli already depends on, and compare
//! pixels. No new feature, no hand-rolled DIB walker, and comparing
//! decoded pixels rather than encoded bytes means the check survives an
//! `image` version bump instead of turning red over an encoder
//! difference.
//!
//! The 256x256 entry is the one compared: it is `RESOLUTIONS`' first, so
//! it is what `get_main_icon` returns.
//!
//! Usage:
//!
//! ```bash
//! cargo run --release --bin verify_icons -- <exe> <png> [<exe> <png> ...]
//! ```
//!
//! Pairs are `<exe> <source png>`. Exit code 0 when every pair matches,
//! 1 when any pair fails, 2 on a usage error.

use std::path::Path;
use std::process::ExitCode;

use image::imageops::FilterType::Lanczos3;

/// The largest resolution `editpe` writes, and therefore the entry
/// `get_main_icon` hands back. Mirrors `RESOLUTIONS` in
/// `vendor/editpe/src/resource.rs`; nothing else depends on the other
/// five, only on the group declaring this set (see `expected_widths`).
const ICON_SIZE: u32 = 256;

const RT_ICON: u32 = 3;
const RT_GROUP_ICON: u32 = 14;

/// `GRPICONDIR` = 6-byte header + one `GRPICONDIRENTRY` per size.
///
/// 14, not 16: a `GRPICONDIRENTRY` is bWidth, bHeight, bColorCount,
/// bReserved, wPlanes, wBitCount, dwBytesInRes and a **WORD** `nID` —
/// 1+1+1+1+2+2+4+2. Reading it as 16 silently overruns the last entry,
/// and since `bWidth` of entry *n* lands at a different offset under each
/// assumption, the sizes come out as plausible-looking garbage rather
/// than as an obvious out-of-range read. A 6-entry group is 90 bytes, and
/// the `len` guard below is what catches the wrong stride.
const GRPICONDIR_HEADER: usize = 6;
const GRPICONDIRENTRY: usize = 14;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() || args.len() % 2 != 0 {
        eprintln!("usage: verify_icons <exe> <png> [<exe> <png> ...]");
        eprintln!("  checks that each EXE's MAINICON is the given source PNG,");
        eprintln!("  that every icon resolution is present, and that RT_ICON");
        eprintln!("  holds no orphans from an earlier stamp.");
        return ExitCode::from(2);
    }

    let mut failures = 0;
    for pair in args.chunks(2) {
        if !verify(Path::new(&pair[0]), Path::new(&pair[1])) {
            failures += 1;
        }
    }

    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "verify_icons: {failures} of {} icon(s) did not match",
            args.len() / 2
        );
        ExitCode::FAILURE
    }
}

/// Check one `<exe> <png>` pair. Returns `true` on success, having
/// printed the reason on failure.
fn verify(exe: &Path, png: &Path) -> bool {
    match check(exe, png) {
        Ok(summary) => {
            println!("verify_icons: ok   {}  <- {}", exe.display(), png.display());
            println!("              {summary}");
            true
        }
        Err(e) => {
            eprintln!("verify_icons: FAIL {}  <- {}", exe.display(), png.display());
            eprintln!("              {e}");
            false
        }
    }
}

fn check(exe: &Path, png: &Path) -> Result<String, String> {
    let image = editpe::Image::parse_file(exe).map_err(|e| format!("parse_file: {e}"))?;
    let resources = image
        .resource_directory()
        .ok_or("no resource directory at all — nothing was ever stamped")?;

    // --- the MAINICON group must declare every resolution ----------------
    let widths = group_widths(resources)
        .ok_or("no RT_GROUP_ICON / MAINICON entry — the EXE has no icon")?;
    if widths.is_empty() {
        return Err("MAINICON group is empty".to_string());
    }
    // A zero bWidth is how a GRPICONDIRENTRY spells 256, since the field
    // is a single byte.
    let declared: Vec<u32> = widths.iter().map(|&w| if w == 0 { 256 } else { w as u32 }).collect();
    if let Some(missing) = expected_widths().iter().find(|w| !declared.contains(w)) {
        return Err(format!(
            "MAINICON declares {declared:?} but is missing the {missing}x{missing} \
             entry — a partial stamp"
        ));
    }

    // --- the 256x256 entry must be the source PNG, pixel for pixel -------
    let bytes = resources
        .get_main_icon()
        .map_err(|e| format!("get_main_icon: {e}"))?
        .ok_or("MAINICON resolves to no icon data")?;

    let embedded = decode_entry(bytes)?;
    let expected = image::open(png)
        .map_err(|e| format!("reading source PNG {}: {e}", png.display()))?
        .resize_exact(ICON_SIZE, ICON_SIZE, Lanczos3)
        .to_rgba8();

    if embedded.dimensions() != expected.dimensions() {
        return Err(format!(
            "MAINICON is {}x{} but {} is {}x{} — wrong artwork, or a stale stamp",
            embedded.width(),
            embedded.height(),
            png.display(),
            expected.width(),
            expected.height(),
        ));
    }
    compare_rgba(&embedded, &expected)?;

    // --- no orphan RT_ICON entries ----------------------------------------
    // Both stamp helpers clear RT_ICON wholesale before adding, precisely
    // because `remove_main_icon` cannot see icons that no group points at.
    // A count above the group's own is that bug reappearing.
    if let Some(total) = icon_entry_count(resources) {
        if total > widths.len() {
            return Err(format!(
                "{} orphan RT_ICON entr{} left from an earlier stamp \
                 ({} referenced by MAINICON) — this file is carrying dead icon bytes",
                total - widths.len(),
                if total - widths.len() == 1 { "y" } else { "ies" },
                widths.len(),
            ));
        }
    }

    Ok(format!(
        "MAINICON {declared:?}, largest {ICON_SIZE}x{ICON_SIZE} matches source pixels exactly",
    ))
}

/// The resolutions `editpe` writes, as a `GRPICONDIRENTRY` would spell
/// them. Kept next to the check that uses it so a change to
/// `RESOLUTIONS` on the `editpe` side is a one-line fix here rather than
/// a silent pass.
fn expected_widths() -> Vec<u32> {
    vec![256, 128, 48, 32, 24, 16]
}

/// Decode one `RT_ICON` payload back into pixels.
///
/// The payload is a PNG (see the module docs). A hand-rolled parse of
/// the 32bpp DIB an ICO *can* hold would be a second format to keep
/// correct for no gain: snug-cli's `image` dependency already has the
/// `png` feature, so this decodes with what is already linked in.
fn decode_entry(bytes: &[u8]) -> Result<image::RgbaImage, String> {
    image::load_from_memory(bytes).map(|i| i.to_rgba8()).map_err(|e| {
        format!(
            "MAINICON holds {len} bytes that are not a decodable image: {e}\n\
             first 16 bytes: {prefix}",
            len = bytes.len(),
            prefix = hex(&bytes[..bytes.len().min(16)]),
        )
    })
}

/// Compare two same-sized RGBA images, reporting the first pixel that
/// differs.
///
/// Pixel-exact rather than approximate on purpose. The expected value is
/// a deterministic `resize_exact` of the source PNG, so there is no
/// tolerance to tune and no threshold to argue about: either the file
/// holds the artwork that is on disk, or it does not. Reporting the
/// coordinate and both values is what makes a failure actionable — the
/// usual cause is "the wrong PNG was stamped", and the pixel is how you
/// confirm which one.
fn compare_rgba(actual: &image::RgbaImage, expected: &image::RgbaImage) -> Result<(), String> {
    if actual.dimensions() != expected.dimensions() {
        return Err(format!(
            "cannot compare {}x{} against {}x{}",
            actual.width(),
            actual.height(),
            expected.width(),
            expected.height(),
        ));
    }
    let (w, h) = (expected.width() as usize, expected.height() as usize);
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            let a = &actual.as_raw()[i..i + 4];
            let e = &expected.as_raw()[i..i + 4];
            if a != e {
                return Err(format!(
                    "pixels differ at ({x},{y}): EXE has RGBA({},{},{},{}), \
                     source has RGBA({},{},{},{})",
                    a[0], a[1], a[2], a[3], e[0], e[1], e[2], e[3],
                ));
            }
        }
    }
    Ok(())
}

/// The `bWidth` of every `GRPICONDIRENTRY` in the `MAINICON` group, in
/// declaration order. `None` when the group is absent or malformed.
fn group_widths(resources: &editpe::ResourceDirectory) -> Option<Vec<u8>> {
    let editpe::ResourceEntry::Table(group_table) =
        resources.root().get(editpe::ResourceEntryName::ID(RT_GROUP_ICON))?
    else {
        return None;
    };
    let editpe::ResourceEntry::Table(dir_table) =
        group_table.get(editpe::ResourceEntryName::from_string("MAINICON"))?
    else {
        return None;
    };
    let name = *dir_table.entries().first()?;
    let editpe::ResourceEntry::Data(data) = dir_table.get(name)? else {
        return None;
    };
    let bytes = data.data();
    if bytes.len() < GRPICONDIR_HEADER {
        return None;
    }
    let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    if bytes.len() < GRPICONDIR_HEADER + count * GRPICONDIRENTRY {
        return None;
    }
    Some(
        (0..count)
            .map(|i| bytes[GRPICONDIR_HEADER + i * GRPICONDIRENTRY])
            .collect(),
    )
}

/// How many `RT_ICON` entries the file holds, referenced or not.
fn icon_entry_count(resources: &editpe::ResourceDirectory) -> Option<usize> {
    let editpe::ResourceEntry::Table(table) =
        resources.root().get(editpe::ResourceEntryName::ID(RT_ICON))?
    else {
        return None;
    };
    Some(table.entries().len())
}

/// `xx xx xx` hex, for diagnostics. A build tool that says "it didn't
/// match" without showing what it actually read is a build tool you end
/// up debugging with a hex editor instead.
fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageFormat;
    use std::io::Cursor;

    /// A gradient, so a pixel error is a *position* error and cannot be
    /// masked by a uniform region. Flat images make a wrong-pixel test
    /// pass for the wrong reason.
    fn gradient(w: u32, h: u32) -> image::RgbaImage {
        image::RgbaImage::from_fn(w, h, |x, y| {
            image::Rgba([
                (x * 7 % 256) as u8,
                (y * 11 % 256) as u8,
                ((x + y) * 3 % 256) as u8,
                255,
            ])
        })
    }

    fn encode_png(img: &image::RgbaImage) -> Vec<u8> {
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .expect("encode");
        out
    }

    #[test]
    fn decodes_a_png_entry() {
        let bytes = encode_png(&gradient(4, 4));
        let entry = decode_entry(&bytes).expect("decode");
        assert_eq!(entry.dimensions(), (4, 4));
    }

    /// The payload really is a PNG and nothing else — no 14-byte prefix,
    /// no DIB header. This is the assumption the whole check rests on, so
    /// it is worth pinning rather than inferring.
    #[test]
    fn the_payload_is_a_bare_png() {
        let bytes = encode_png(&gradient(2, 2));
        assert_eq!(&bytes[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        assert_eq!(&bytes[..8], &encode_png(&gradient(2, 2))[..8]);
    }

    #[test]
    fn identical_images_compare_equal() {
        compare_rgba(&gradient(8, 8), &gradient(8, 8)).expect("should match");
    }

    #[test]
    fn a_single_differing_pixel_fails_with_its_position() {
        let expected = gradient(8, 8);
        let mut actual = expected.clone();
        actual.put_pixel(3, 5, image::Rgba([1, 2, 3, 4]));
        let err = compare_rgba(&actual, &expected).expect_err("should differ");
        assert!(err.contains("(3,5)"), "expected the coordinate, got: {err}");
    }

    #[test]
    fn mismatched_dimensions_are_refused_rather_than_compared() {
        let err = compare_rgba(&gradient(8, 8), &gradient(8, 4)).expect_err("should differ");
        assert!(err.contains("cannot compare"), "unexpected message: {err}");
    }

    #[test]
    fn non_image_bytes_are_rejected_with_a_hex_prefix() {
        let err = decode_entry(&[0xde, 0xad, 0xbe, 0xef, 0x00, 0x11])
            .expect_err("garbage should not decode");
        assert!(err.contains("de ad be ef"), "expected a hex dump, got: {err}");
    }

    #[test]
    fn truncated_bytes_are_rejected() {
        let full = encode_png(&gradient(16, 16));
        let err = decode_entry(&full[..full.len() / 2]).expect_err("truncated");
        assert!(err.contains("not a decodable image"), "got: {err}");
    }

    /// A zero `bWidth` is how a `GRPICONDIRENTRY` spells 256, since the
    /// field is one byte. Getting that wrong would make every correctly
    /// stamped 256px entry look like a missing resolution.
    #[test]
    fn expected_widths_include_the_zero_spelled_256() {
        let widths = expected_widths();
        assert!(widths.contains(&256), "the largest entry is 256");
        assert_eq!(widths.len(), 6, "editpe writes six resolutions");
    }
}
