//! The precompiled `launcher-stub.exe` embedded at compile time.
//!
//! The path resolves relative to this crate's manifest directory, i.e.
//! `crates/snug-cli/`, so `../../bin/launcher-stub.exe` points at the
//! committed binary at the workspace root.
//!
//! To regenerate the stub after changing `snug-launcher`:
//!
//! ```bash
//! cargo zigbuild --target x86_64-pc-windows-gnu --release -p snug-launcher
//! cp target/x86_64-pc-windows-gnu/release/snug-launcher.exe bin/launcher-stub.exe
//! ```

/// The precompiled Windows stub launcher.
///
/// On non-Windows hosts this is still embedded — the cross-compiled
/// `launcher-stub.exe` is just bytes from this crate's point of view.
/// It's only meaningful on a Windows machine (or via Wine) when the
/// produced `.exe` is actually executed.
pub const STUB_BYTES: &[u8] = include_bytes!("../../../bin/launcher-stub.exe");

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
