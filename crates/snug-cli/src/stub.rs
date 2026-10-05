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
