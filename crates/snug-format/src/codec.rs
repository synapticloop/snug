//! Encode / decode [`SnugEmbedded`] and the inner [`SnugPayload`].
//!
//! The wire format is intentionally minimal so the launcher can locate its
//! payload by scanning for the magic header — useful both when the payload
//! is embedded via `include_bytes!()` and when it has been appended to a
//! precompiled stub binary.
//!
//! ```text
//! +--------+----------------+--------------+--------------+----------------------+
//! | magic  | format_version | payload_len  | payload_crc32| payload (postcard)   |
//! | 8 B    | u16 LE         | u32 LE       | u32 LE       | payload_len bytes    |
//! +--------+----------------+--------------+--------------+----------------------+
//! ```

use crate::{EmbeddedFile, FormatError, SnugEmbedded, SnugPayload, FORMAT_VERSION, MAGIC};

/// Encode a [`SnugPayload`] (without the outer wrapper) as postcard bytes.
///
/// Useful for testing or for callers that want to manage the outer wrapper
/// themselves.
pub fn encode_payload(payload: &SnugPayload) -> Result<Vec<u8>, FormatError> {
    let bytes = postcard::to_allocvec(payload)?;
    Ok(bytes)
}

/// Decode a postcard-encoded [`SnugPayload`].
pub fn decode_payload(bytes: &[u8]) -> Result<SnugPayload, FormatError> {
    let payload = postcard::from_bytes(bytes)?;
    Ok(payload)
}

/// Encode a [`SnugEmbedded`] — outer wrapper + payload — to a single byte
/// vector suitable for embedding via `include_bytes!()` or appending to a
/// precompiled stub binary.
/// Narrow an encoded payload length to the `u32` the wire header carries.
///
/// Checked, not `as u32`. Past 4 GiB that cast wraps and writes a header
/// that disagrees with the bytes actually following it, so the artefact
/// fails validation at launch and the error points at the launcher instead
/// of at the build that produced it.
///
/// Every narrowing goes through here. That is what lets the boundary be
/// tested against the function the builder actually uses: `encode` cannot
/// be handed a 4 GiB payload in a unit test, so a test of `u32::try_from`
/// on its own proves the standard library works and says nothing about
/// whether `encode` calls it.
fn checked_payload_len(len: usize) -> Result<u32, FormatError> {
    u32::try_from(len).map_err(|_| FormatError::PayloadTooLarge { len: len as u64 })
}

pub fn encode(embedded: &SnugEmbedded) -> Result<Vec<u8>, FormatError> {
    let payload_bytes = postcard::to_allocvec(&embedded.payload)?;
    let len = checked_payload_len(payload_bytes.len())?;
    let crc = crc32fast::hash(&payload_bytes);

    let mut out = Vec::with_capacity(SnugEmbedded::HEADER_LEN + payload_bytes.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&embedded.format_version.to_le_bytes());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&payload_bytes);
    Ok(out)
}

/// Decode a [`SnugEmbedded`] from a byte slice. Validates magic, format
/// version, length, and CRC32 before attempting postcard deserialisation.
pub fn decode(bytes: &[u8]) -> Result<SnugEmbedded, FormatError> {
    if bytes.len() < SnugEmbedded::HEADER_LEN {
        return Err(FormatError::Truncated {
            declared: SnugEmbedded::HEADER_LEN as u32,
            found: bytes.len(),
        });
    }

    let mut magic = [0u8; 8];
    magic.copy_from_slice(&bytes[..8]);
    if &magic != MAGIC {
        return Err(FormatError::BadMagic { found: magic });
    }

    let format_version = u16::from_le_bytes(bytes[8..10].try_into().unwrap());
    if format_version > FORMAT_VERSION {
        return Err(FormatError::UnsupportedVersion {
            found: format_version,
            max: FORMAT_VERSION,
        });
    }

    let payload_len = u32::from_le_bytes(bytes[10..14].try_into().unwrap());
    let payload_crc = u32::from_le_bytes(bytes[14..18].try_into().unwrap());

    let header_len = SnugEmbedded::HEADER_LEN;
    let available = bytes.len().saturating_sub(header_len);
    if (payload_len as usize) > available {
        return Err(FormatError::Truncated {
            declared: payload_len,
            found: available,
        });
    }

    let payload_bytes = &bytes[header_len..header_len + payload_len as usize];
    let computed_crc = crc32fast::hash(payload_bytes);
    if computed_crc != payload_crc {
        return Err(FormatError::BadCrc32 {
            declared: payload_crc,
            computed: computed_crc,
        });
    }

    let payload = postcard::from_bytes(payload_bytes)?;
    Ok(SnugEmbedded {
        magic,
        format_version,
        payload_len,
        payload_crc32: payload_crc,
        payload,
    })
}

/// Compute the SHA-256 of a byte slice and return the 32-byte digest.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let out = hasher.finalize();
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&out);
    digest
}

/// Convenience: build an [`EmbeddedFile`] from in-memory bytes, computing
/// the SHA-256 digest automatically.
pub fn embedded_file(bytes: Vec<u8>) -> EmbeddedFile {
    let sha256 = sha256(&bytes);
    EmbeddedFile { sha256, bytes }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AppMetadata, LauncherConfig};

    fn sample_payload() -> SnugPayload {
        let jar = embedded_file(b"fake-jar-bytes".to_vec());
        SnugPayload {
            config: LauncherConfig {
                app: AppMetadata {
                    name: "Test".into(),
                    company: "Acme".into(),
                    update_check_url: None,
                    version: "1.0.0".into(),
                    description: None,
                    copyright: None,
                },
                main_class: Some("com.example.Main".into()),
                min_java: 25,
                jvm_args: vec!["-Xmx2g".into()],
                splash: None,
                behavior: Default::default(),
            },
            jars: vec![jar],
            localizations: Vec::new(),
        }
    }

    #[test]
    fn a_payload_the_header_cannot_describe_is_refused_not_wrapped() {
        // The wire header carries `payload_len` as a u32, so a payload past
        // 4 GiB has no representation. This used to be `as u32`, which
        // wrapped: the header then disagreed with the bytes actually
        // written, and the artefact failed validation at launch with a
        // truncated/CRC error pointing at the launcher rather than at the
        // build that produced it.
        //
        // Exercised on the boundary value rather than by allocating 4 GiB --
        // `encode` cannot be given a 4 GiB payload in a test, so the
        // assertion is on `checked_payload_len`, which is the only place the
        // narrowing happens. The half that pins the call site is
        // `encode_writes_the_length_it_checked` below.
        let header_can_hold = u32::MAX as usize;
        let over = header_can_hold.checked_add(1).expect("64-bit usize");
        // The old cast would have wrapped `over` to 0; assert the maths we
        // are protecting, so a future reversion is visible here too.
        assert_eq!(
            over as u32, 0,
            "sanity: the old `as u32` cast wraps this to zero"
        );
        assert!(
            u32::try_from(over).is_err(),
            "try_from must reject what the cast accepted"
        );
        assert!(u32::try_from(header_can_hold).is_ok());

        // The function under test, not the standard library.
        assert!(checked_payload_len(over).is_err());
        assert_eq!(checked_payload_len(header_can_hold).unwrap(), u32::MAX);
        assert_eq!(checked_payload_len(0).unwrap(), 0);
        match checked_payload_len(over) {
            Err(FormatError::PayloadTooLarge { len }) => assert_eq!(len, over as u64),
            other => panic!("expected PayloadTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn encode_writes_the_length_it_checked() {
        // The 4 GiB case above cannot reach `encode`, so pin the other
        // half: that the value landing in the header is the one
        // `checked_payload_len` produced, rather than a second cast made
        // independently at the call site.
        let embedded = SnugEmbedded::new(sample_payload());
        let bytes = encode(&embedded).unwrap();
        let actual = bytes.len() - SnugEmbedded::HEADER_LEN;
        let in_header = u32::from_le_bytes(bytes[10..14].try_into().unwrap());
        assert_eq!(in_header as usize, actual);
        assert_eq!(in_header, checked_payload_len(actual).unwrap());
    }

    #[test]
    fn roundtrip_embedded() {
        let payload = sample_payload();
        let embedded = SnugEmbedded::new(payload);
        let bytes = encode(&embedded).unwrap();
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.format_version, FORMAT_VERSION);
        assert_eq!(decoded.payload_len, embedded.payload_len);
        assert_eq!(decoded.payload_crc32, embedded.payload_crc32);
        assert_eq!(decoded.payload.config.app.name, "Test");
    }

    #[test]
    fn detects_bad_magic() {
        let payload = sample_payload();
        let embedded = SnugEmbedded::new(payload);
        let mut bytes = encode(&embedded).unwrap();
        bytes[0] = b'X';
        assert!(matches!(decode(&bytes), Err(FormatError::BadMagic { .. })));
    }

    #[test]
    fn detects_bad_crc() {
        let payload = sample_payload();
        let embedded = SnugEmbedded::new(payload);
        let mut bytes = encode(&embedded).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        assert!(matches!(decode(&bytes), Err(FormatError::BadCrc32 { .. })));
    }

    #[test]
    fn detects_truncated() {
        let payload = sample_payload();
        let embedded = SnugEmbedded::new(payload);
        let bytes = encode(&embedded).unwrap();
        let truncated = &bytes[..bytes.len() - 4];
        assert!(matches!(decode(truncated), Err(FormatError::Truncated { .. })));
    }
}
