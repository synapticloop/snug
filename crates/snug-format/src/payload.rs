//! The semantic payload embedded in a snug executable.

use serde::{Deserialize, Serialize};

use crate::{LauncherConfig, Localization};

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
    /// One or more JARs to add to the JVM classpath at runtime.
    ///
    /// A single-JAR build produces a `vec![single_jar]`; a
    /// `--input <directory>` build produces a vector with one entry
    /// per `.jar` file found in the directory. The launcher extracts
    /// every entry to its per-user cache and concatenates them into
    /// the `-classpath` argument.
    ///
    /// `Main-Class` is read from the JAR's manifest. When more than
    /// one JAR is present, the builder reads `META-INF/MANIFEST.MF`
    /// from the **first** entry; the CLI's `--main-class` flag
    /// always overrides this.
    pub jars: Vec<EmbeddedFile>,
    // NOTE: this struct used to carry `icon: Option<EmbeddedFile>` -- the
    // bytes of `--icon`, read off disk at build time. Nothing ever read
    // them: the builder re-read `cli.icon` from disk to stamp the PE, and
    // the launcher reads Explorer's resource, not the payload. So every
    // built EXE carried the icon twice, uncompressed, once to stamp with
    // and once for nothing.
    //
    // Removed rather than made live. Stamping from `payload.icon` reads
    // well but only works for ICO: `editpe`'s `ToIcon for &[u8]` parses an
    // ICO directory and rejects a PNG outright, where the path-based route
    // went through `image::ImageReader` and took either. Dropping the field
    // actually removes the bytes, which is the cost that was reported.
    //
    // `#[serde(default)]` on every remaining field, so an older payload
    // blob still decodes (extra trailing fields are skipped by postcard),
    // and this is a wire-layout change per AGENTS.md's versioning rules.

    /// Localization bundles for the launcher's runtime strings
    /// (error dialogs, splash errors, JDK-install errors, bare-stub
    /// fallback, etc.).
    ///
    /// Bundles are merged in priority order at runtime: full BCP 47
    /// tag → primary subtag → built-in English baseline. Missing
    /// keys fall through. An empty vector is valid — the launcher
    /// then uses only its compiled-in English baseline.
    ///
    /// Every well-formed build embeds at least the built-in English
    /// bundle (`snug-cli` always injects one), so the vector is
    /// non-empty in practice.
    #[serde(default)]
    pub localizations: Vec<Localization>,
}
