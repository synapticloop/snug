//! The precompiled Windows launcher stub embedded at compile time.
//!
//! The path resolves relative to this crate's manifest directory, i.e.
//! `crates/snug-cli/`, so `../../bin/launcher-stub-windows-x86_64.exe`
//! points at the committed binary at the workspace root.
//!
//! The name carries `<os>-<arch>` because `bin/` holds one stub per
//! target — `launcher-stub-windows-x86_64.exe` alongside
//! `launcher-stub-macos-arm64` and `launcher-stub-macos-x86_64` — and a
//! bare `launcher-stub.exe` could not be told apart from the others at a
//! glance. The suffix matches the `release/<os>-<arch>/` directory the
//! artefact is shipped into, so the whole set is spelled one way.
//!
//! To regenerate the stub after changing `snug-launcher`, on a native
//! Windows host:
//!
//! ```text
//! cargo build --release -p snug-launcher
//! copy /Y target\release\snug-launcher.exe bin\launcher-stub-windows-x86_64.exe
//! ```
//!
//! `scripts\build-windows.cmd` does both steps in that order, which is
//! load-bearing: the embed has to exist *before* `snug-cli` compiles.

/// The precompiled Windows stub launcher.
pub const STUB_BYTES: &[u8] =
    include_bytes!("../../../bin/launcher-stub-windows-x86_64.exe");

/// The name of this stub on disk, relative to the workspace root.
///
/// Kept as a constant rather than repeating the literal in the tests
/// below, so the `include_bytes!` path and the assertions that talk
/// about the file cannot drift apart silently. A rename that updated
/// one and not the other would still compile.
pub const STUB_FILE_NAME: &str = "launcher-stub-windows-x86_64.exe";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_is_a_pe32_plus_gui_exe() {
        // DOS header magic.
        assert_eq!(&STUB_BYTES[..2], b"MZ", "stub must start with MZ");
        // PE header offset lives at 0x3C as a u32 LE.
        let pe_offset = u32::from_le_bytes([
            STUB_BYTES[0x3C],
            STUB_BYTES[0x3D],
            STUB_BYTES[0x3E],
            STUB_BYTES[0x3F],
        ]) as usize;
        assert_eq!(
            &STUB_BYTES[pe_offset..pe_offset + 4],
            b"PE\0\0",
            "stub must contain a PE\\0\\0 signature at the header offset"
        );

        // The rest of what the name promises. This test used to assert only
        // `MZ` and `PE\0\0`, so "PE32+" and "GUI" were decoration -- a stub
        // that was 32-bit or console-subsystem would have passed it, and
        // `Image::parse` in `build_exe` would then fail (or produce a
        // console app that flashes a window on every launch).
        let coff = pe_offset + 4;
        // IMAGE_FILE_HEADER: Machine(2) NumberOfSections(2) ...
        assert_eq!(
            u16::from_le_bytes([STUB_BYTES[coff], STUB_BYTES[coff + 1]]),
            0x8664,
            "stub must be IMAGE_FILE_MACHINE_AMD64 (PE32+)"
        );

        let opt = coff + 20; // start of IMAGE_OPTIONAL_HEADER
        let opt_magic = u16::from_le_bytes([STUB_BYTES[opt], STUB_BYTES[opt + 1]]);
        assert_eq!(
            opt_magic, 0x20B,
            "stub's optional header must be PE32+ (0x20B), not PE32 (0x10B)"
        );

        // Subsystem sits at offset 68 in IMAGE_OPTIONAL_HEADER, for both
        // PE32 and PE32+ -- the data directories differ in offset, but the
        // fixed fields up to and including Subsystem do not.
        let subsystem = u16::from_le_bytes([STUB_BYTES[opt + 68], STUB_BYTES[opt + 69]]);
        assert_eq!(
            subsystem, 2,
            "stub must be IMAGE_SUBSYSTEM_WINDOWS_GUI (2), not CONSOLE (3) -- \
             a console subsystem flashes a window on every launch"
        );
    }

    #[test]
    fn stub_contains_magic_constant_in_data_section() {
        // The compiled binary embeds the SNUGEMBD literal in its data
        // section. Sanity-check that the constant was actually present
        // at compile time — otherwise the locator's "skip false
        // positives inside the stub" path won't be exercised by real
        // builds.
        assert!(
            STUB_BYTES.windows(snug_format::MAGIC.len()).any(|w| w == snug_format::MAGIC),
            "stub should contain SNUGEMBD literal at least once"
        );
    }
}
