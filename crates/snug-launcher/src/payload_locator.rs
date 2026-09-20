//! Locate the snug embedded payload inside the launcher binary itself.
//!
//! The launcher binary (whether a `include_bytes!()`-embedded template or
//! a stub with the payload appended) is scanned backwards from end of
//! file looking for the 8-byte [`MAGIC`] prefix. This works in either
//! the template-per-build mode or the stub-append mode without any
//! configuration.
//!
//! For stub-append mode we additionally verify that the magic is within
//! the last `MAX_TAIL_SEARCH` bytes — payloads that ended up embedded
//! inside the file by accident should not be matched. In practice the
//! stub-append always places the payload at the tail, so this check
//! rejects only the template-mode case where `include_bytes!()` placed
//! the payload earlier in the file. We accept both.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use snug_format::{decode, FormatError, SnugEmbedded};

/// Re-export the embedded-payload magic from `snug-format` for callers
/// who want to scan for it without depending on `snug-format` directly.
pub use snug_format::MAGIC;

/// Maximum number of trailing bytes to scan for the magic prefix.
///
/// Set comfortably larger than any plausible payload (a few hundred MB)
/// so we never miss a real tail, but small enough to make the search
/// fast on large binaries.
pub const MAX_TAIL_SEARCH: u64 = 64 * 1024 * 1024;

/// Header length (8 magic + 2 version + 4 len + 4 crc) — used to
/// distinguish "this is the magic, payload follows" from "these 8 bytes
/// happen to spell SNUGEMBD but the next 10 bytes are nonsense".
pub const HEADER_LEN: u64 = 18;

/// Scan `path` for a snug payload. Returns `Ok(None)` if no valid
/// payload is found.
///
/// The scan walks backwards from end of file in 4 KiB chunks, looking
/// for the 8-byte magic. For each magic occurrence we attempt to decode
/// the following header and full payload; the first one that validates
/// is returned.
pub fn find_in_file(path: &Path) -> Result<Option<SnugEmbedded>, FormatError> {
    let mut file = fs::File::open(path)?;
    let file_len = file.metadata()?.len();
    if file_len < HEADER_LEN {
        return Ok(None);
    }

    let scan_limit = file_len.saturating_sub(HEADER_LEN);
    let tail_start = file_len.saturating_sub(MAX_TAIL_SEARCH);
    let search_from = tail_start.min(scan_limit);

    // Walk backwards in 4 KiB steps, reading enough bytes for the header
    // to straddle any candidate position.
    const STEP: u64 = 4096;
    let mut pos = file_len;
    while pos > search_from + HEADER_LEN {
        pos = pos.saturating_sub(STEP);
        let read_at = pos;
        let read_len = (file_len - read_at).min(STEP + HEADER_LEN) as usize;
        file.seek(SeekFrom::Start(read_at))?;
        let mut buf = vec![0u8; read_len];
        file.read_exact(&mut buf)?;

        // Look for the magic inside this chunk.
        if let Some(off) = find_magic_in_chunk(&buf) {
            let absolute = read_at + off as u64;
            let available = file_len - absolute;
            if available < HEADER_LEN {
                continue;
            }
            // Read the full header + payload starting at absolute.
            file.seek(SeekFrom::Start(absolute))?;
            let mut full = vec![0u8; available as usize];
            file.read_exact(&mut full)?;
            if let Ok(embedded) = decode(&full) {
                return Ok(Some(embedded));
            }
            // Not a valid payload at this position; keep scanning.
        }
    }

    // Also check the very tail (small files where the above loop never
    // entered) one more time.
    file.seek(SeekFrom::Start(0))?;
    let mut all = vec![0u8; file_len as usize];
    file.read_exact(&mut all)?;
    if let Some(off) = find_magic_in_chunk(&all) {
        if let Ok(embedded) = decode(&all[off..]) {
            return Ok(Some(embedded));
        }
    }

    Ok(None)
}

/// Find the offset of [`MAGIC`] inside `buf`, if present.
fn find_magic_in_chunk(buf: &[u8]) -> Option<usize> {
    if buf.len() < MAGIC.len() {
        return None;
    }
    buf.windows(MAGIC.len())
        .position(|w| w == MAGIC.as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use snug_format::{embedded_file, AppMetadata, LauncherBehavior, LauncherConfig, SnugEmbedded};

    fn write_payload_to(path: &Path) {
        let payload = snug_format::SnugPayload {
            config: LauncherConfig {
                app: AppMetadata {
                    name: "Stub Test".into(),
                    company: "Test Co".into(),
                    version: "0.1.0".into(),
                    description: None,
                    copyright: None,
                },
                main_class: Some("com.example.Main".into()),
                min_java: 25,
                jvm_args: vec![],
                splash: None,
                behavior: LauncherBehavior::default(),
            },
            jar: embedded_file(b"jar-bytes".to_vec()),
            icon: None,
        };
        let embedded = SnugEmbedded::new(payload);
        let encoded = snug_format::encode(&embedded).unwrap();

        // Write a "fake EXE" with 1 KiB of prefix bytes followed by the
        // payload — simulates a stub with an appended payload.
        let mut file = fs::File::create(path).unwrap();
        let prefix = vec![0xCCu8; 1024];
        file.write_all(&prefix).unwrap();
        file.write_all(&encoded).unwrap();
    }

    #[test]
    fn finds_payload_at_tail() {
        let dir = tempdir();
        let path = dir.join("launcher-stub.exe");
        write_payload_to(&path);

        let found = find_in_file(&path).unwrap();
        let embedded = found.expect("payload should be located");
        assert_eq!(embedded.payload.config.app.name, "Stub Test");
        assert_eq!(embedded.payload.jar.bytes, b"jar-bytes");
    }

    #[test]
    fn returns_none_when_no_payload() {
        let dir = tempdir();
        let path = dir.join("empty.exe");
        std::fs::write(&path, vec![0u8; 4096]).unwrap();
        assert!(find_in_file(&path).unwrap().is_none());
    }

    #[test]
    fn returns_none_when_magic_present_but_payload_corrupt() {
        let dir = tempdir();
        let path = dir.join("truncated.exe");
        // Real magic followed by garbage — should not be confused for a
        // valid payload.
        let mut bytes = vec![0u8; 4096];
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&[0xFFu8; 20]);
        std::fs::write(&path, bytes).unwrap();
        assert!(find_in_file(&path).unwrap().is_none());
    }

    fn tempdir() -> std::path::PathBuf {
        let base = std::env::temp_dir();
        let unique = format!(
            "snug-locator-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = base.join(unique);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
