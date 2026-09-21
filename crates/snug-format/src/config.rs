//! Typed launcher configuration embedded in every snug executable.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Top-level launcher behaviour and metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LauncherConfig {
    /// Human-readable application identity used for the Windows version
    /// resource and the per-user cache directory.
    pub app: AppMetadata,

    /// Explicit `Main-Class` override. If `None`, the launcher reads
    /// `Main-Class` from the fat JAR's manifest.
    #[serde(default)]
    pub main_class: Option<String>,

    /// Minimum required Java major version (e.g. `25`).
    pub min_java: u16,

    /// Extra JVM options forwarded to `JNI_CreateJavaVM`. Forwarded in
    /// the order supplied; user-supplied args come last so they override
    /// any baked-in defaults.
    #[serde(default)]
    pub jvm_args: Vec<String>,

    /// Optional splash screen shown before the JVM starts.
    #[serde(default)]
    pub splash: Option<SplashConfig>,

    /// Runtime behaviour knobs (cache layout, JVM discovery strategy,
    /// whether to forward EXE arguments to `main(String[])`).
    #[serde(default)]
    pub behavior: LauncherBehavior,
}

/// Windows version-resource metadata + cache-key identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppMetadata {
    /// Display name (e.g. `"My App"`). Maps to `ProductName` and
    /// `FileDescription` in the version resource.
    pub name: String,

    /// Company / vendor name. Maps to `CompanyName` and forms part of the
    /// per-user cache directory.
    pub company: String,

    /// Application version as a dotted string (e.g. `"1.2.3"`). Maps to
    /// `FileVersion` and `ProductVersion`.
    pub version: String,

    /// Optional `FileDescription` override; defaults to `app.name`.
    #[serde(default)]
    pub description: Option<String>,

    /// Optional `LegalCopyright` string.
    #[serde(default)]
    pub copyright: Option<String>,
}

/// Optional native splash shown by the launcher before the JVM is loaded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SplashConfig {
    /// Minimum display duration in milliseconds. The splash is dismissed
    /// once the JVM signals readiness or this duration elapses, whichever
    /// is later.
    pub duration_ms: u32,

    /// Pre-processed splash pixels, ready for the Windows layered-window
    /// DIB. Carries explicit dimensions and a BGRA premultiplied buffer
    /// — no PNG decoding or colour-space conversion is needed at
    /// runtime.
    ///
    /// The `snug` CLI converts the user-supplied PNG into this form at
    /// build time; the launcher just memcpy's `bytes` into a DIB and
    /// calls `UpdateLayeredWindow`. This keeps the launcher free of an
    /// image-codec dependency and removes per-launch PNG decode cost.
    pub image: SplashImage,
}

/// Pre-processed splash pixels for the launcher's `UpdateLayeredWindow`
/// path.
///
/// Wire format note: `bytes` is row-major, **BGRA premultiplied by
/// alpha**. Windows' 32-bit DIB + `AC_SRC_ALPHA` blend expects this
/// layout. Generating it is the CLI's job; consumers (the launcher)
/// just memcpy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SplashImage {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Row-major BGRA bytes, premultiplied, `width * height * 4` long.
    pub bytes: Vec<u8>,
}

/// Runtime behaviour knobs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LauncherBehavior {
    /// Forward command-line arguments from the EXE to Java's
    /// `main(String[] args)`.
    ///
    /// Default: `true`. Set to `false` to consume EXE args (useful for
    /// wrapper-style launches).
    pub forward_args: bool,

    /// Override for the per-user cache directory.
    ///
    /// Default layout on Windows:
    /// ```text
    /// %LOCALAPPDATA%\snug\<company>\<name>\<jar-sha256>\app.jar
    /// ```
    /// Set this to inject a custom cache root (mostly useful for tests
    /// and for portable installs).
    #[serde(default)]
    pub cache_dir: Option<PathBuf>,

    /// Strategy the launcher uses to locate a compatible JVM.
    #[serde(default)]
    pub jvm_discovery: JvmDiscovery,

    /// When `true` and the JVM lookup fails, the launcher pops up a
    /// `TaskDialog` and offers to download a compatible Eclipse
    /// Temurin JDK from `api.adoptium.net`, verifies the SHA-256,
    /// extracts the zip under `%LOCALAPPDATA%\snug\jdk\<version>\`,
    /// and retries discovery with the new `JAVA_HOME`.
    ///
    /// Default: `false`. Recommended to set via the `--download-jdk`
    /// CLI flag on the builder so the user is never asked at runtime
    /// unless the EXE was deliberately built with the opt-in.
    #[serde(default)]
    pub auto_download_jdk: bool,
}

impl Default for LauncherBehavior {
    fn default() -> Self {
        Self {
            forward_args: true,
            cache_dir: None,
            jvm_discovery: JvmDiscovery::default(),
            auto_download_jdk: false,
        }
    }
}

/// JVM discovery strategy. The launcher walks these in order, returning the
/// first JVM that satisfies [`LauncherConfig::min_java`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct JvmDiscovery {
    /// Explicit `JAVA_HOME`-style path. Highest priority.
    #[serde(default)]
    pub explicit: Option<PathBuf>,

    /// Try the `JAVA_HOME` environment variable.
    pub try_java_home: bool,

    /// Try the `JDK_HOME` environment variable.
    pub try_jdk_home: bool,

    /// Try `PATH` (looking for `java.exe` / `javaw.exe`).
    pub try_path: bool,

    /// Try well-known Windows registry locations
    /// (`HKLM\SOFTWARE\JavaSoft\...`).
    pub try_registry: bool,

    /// Try common install locations
    /// (`C:\Program Files\Java\...`, `C:\Program Files\Eclipse Adoptium\...`, etc.).
    pub try_common: bool,
}

impl Default for JvmDiscovery {
    fn default() -> Self {
        Self {
            explicit: None,
            try_java_home: true,
            try_jdk_home: true,
            try_path: true,
            try_registry: true,
            try_common: true,
        }
    }
}
