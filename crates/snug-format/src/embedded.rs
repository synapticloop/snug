//! Outer wrapper for an embedded snug payload.
//!
//! See [`crate::codec`] for the wire format.

use serde::{Deserialize, Serialize};

use crate::SnugPayload;

/// 8-byte magic prefix that identifies a snug embedded payload.
///
/// Picked to be ASCII-grep-able (`strings` / `grep`) and unlikely to collide
/// with arbitrary executable content.
pub const MAGIC: &[u8; 8] = b"SNUGEMBD";

/// Maximum format version this build understands. Bump when the wire format
/// changes incompatibly.
pub const FORMAT_VERSION: u16 = 1;

/// A snug payload wrapped with the metadata needed to locate, validate, and
/// version-check it at runtime.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnugEmbedded {
    /// Always equal to [`MAGIC`]; populated for symmetry with the wire format.
    pub magic: [u8; 8],
    /// Wire-format version; must be `<= FORMAT_VERSION` for this build.
    pub format_version: u16,
    /// Length in bytes of the postcard-encoded payload that follows.
    pub payload_len: u32,
    /// CRC32 of the postcard-encoded payload.
    pub payload_crc32: u32,
    /// Decoded payload.
    pub payload: SnugPayload,
}

impl SnugEmbedded {
    /// The fixed header length: 8 (magic) + 2 (version) + 4 (length) + 4 (CRC).
    pub const HEADER_LEN: usize = 18;

    /// Wrap a payload with a freshly computed length and CRC32.
    pub fn new(payload: SnugPayload) -> Self {
        let payload_bytes = postcard::to_allocvec(&payload).expect("postcard alloc never fails");
        let payload_len = payload_bytes.len() as u32;
        let payload_crc32 = crc32fast::hash(&payload_bytes);
        Self {
            magic: *MAGIC,
            format_version: FORMAT_VERSION,
            payload_len,
            payload_crc32,
            payload,
        }
    }
}
