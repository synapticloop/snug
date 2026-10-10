use thiserror::Error;

/// Errors produced by encoding, decoding, or validating a `SnugEmbedded` blob.
#[derive(Debug, Error)]
pub enum FormatError {
    /// The 8-byte magic prefix did not match.
    #[error("magic header mismatch: expected SNUGEMBD, found {found:?}")]
    BadMagic { found: [u8; 8] },

    /// The format version is newer than this build supports.
    #[error("format version {found} is newer than supported maximum {max}")]
    UnsupportedVersion { found: u16, max: u16 },

    /// The declared payload length did not match the actual remaining bytes.
    #[error("truncated payload: declared {declared} bytes, found {found}")]
    Truncated { declared: u32, found: usize },

    /// The CRC32 of the payload did not match the declared value.
    #[error("payload CRC32 mismatch: declared {declared:#010x}, computed {computed:#010x}")]
    BadCrc32 { declared: u32, computed: u32 },

    /// The encoded payload is larger than the header can describe.
    ///
    /// `payload_len` is a u32 in the wire header, so a payload past 4 GiB
    /// has no representation. Reachable only by a multi-gigabyte fat JAR; it
    /// used to be reached by `payload_bytes.len() as u32`, which *wrapped*
    /// and wrote a header disagreeing with the bytes actually present --
    /// producing an artefact that failed validation at launch with a
    /// truncated/CRC error pointing at the launcher rather than at the build.
    #[error(
        "encoded payload is {len} bytes, which exceeds the 4 GiB a u32 payload_len can describe"
    )]
    PayloadTooLarge { len: u64 },
    /// Underlying I/O error reading or writing a snug blob.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Postcard failed to (de)serialise the payload.
    #[error("postcard codec error: {0}")]
    Postcard(#[from] postcard::Error),
}
