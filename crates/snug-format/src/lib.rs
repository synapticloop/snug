//! Embedded-payload format shared between the `snug` CLI/builder and the
//! Windows launcher runtime.
//!
//! The format is designed to be **stub-friendly**: the same encoded blob
//! can be embedded via `include_bytes!()` in a generated Rust launcher
//! (template-per-build mode) or appended to a precompiled `launcher-stub.exe`
//! (stub mode). The launcher locates its payload by scanning for the magic
//! header, then validates version + CRC32 before decoding.
//!
//! Layers, outside in:
//!
//! 1. [`SnugEmbedded`] — outer wrapper with magic, format version, payload
//!    length, CRC32, and the postcard-encoded payload bytes.
//! 2. [`SnugPayload`] — the semantic payload: launcher configuration plus
//!    embedded binary artefacts (fat JAR, icon, splash).
//! 3. [`LauncherConfig`] — typed launcher behaviour (main class, JVM args,
//!    minimum Java version, splash duration, JVM discovery strategy).
//!
//! Use [`encode`] / [`decode`] for round-tripping the outer wrapper, or
//! [`encode_payload`] / [`decode_payload`] to work with just the inner payload.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod codec;
pub mod config;
pub mod embedded;
pub mod error;
pub mod payload;

pub use codec::{decode, decode_payload, embedded_file, encode, encode_payload, sha256};
pub use config::{
    AppMetadata, JvmDiscovery, LauncherBehavior, LauncherConfig, SplashConfig, SplashImage,
};
pub use embedded::{SnugEmbedded, FORMAT_VERSION, MAGIC};
pub use error::FormatError;
pub use payload::{EmbeddedFile, SnugPayload};
