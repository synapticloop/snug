//! The semantic payload embedded in a snug executable.

use serde::{Deserialize, Serialize};

use crate::LauncherConfig;

/// An embedded binary artefact (fat JAR, icon, splash image) plus its
/// SHA-256 digest.
///
/// The digest is the cache key at runtime: the launcher extracts the artefact
/// to a per-user cache directory keyed by hash, so updating the application
/// produces a new cache entry automatically.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmbeddedFile {
    /// SHA-256 of `bytes`.
    pub sha256: [u8; 32],
    /// Raw file contents.
    pub bytes: Vec<u8>,
}

/// The semantic payload embedded in a snug executable.
///
/// Holds the launcher configuration together with the binary artefacts the
/// launcher needs to start the JVM and present a splash screen.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnugPayload {
    /// Behaviour and metadata for the launcher.
    pub config: LauncherConfig,
    /// The fat JAR to run.
    pub jar: EmbeddedFile,
    /// Optional `.ico` to use as the Windows Explorer icon.
    ///
    /// This is consumed by the **builder** when stamping the version
    /// resource into the EXE; the launcher itself does not need to read it.
    /// Storing it in the payload keeps the builder self-contained and
    /// avoids needing the original `.ico` at build time when the stub
    /// mode is used.
    #[serde(default)]
    pub icon: Option<EmbeddedFile>,
}
