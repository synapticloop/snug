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
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
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

    /// Optional URL the launcher's error dialog surfaces as a clickable
    /// "Check for a newer version" link. When `None` or empty, the
    /// link row is hidden. Useful for "this build is broken, grab the
    /// latest" messages without forcing users to read the error text.
    #[serde(default)]
    pub update_check_url: Option<String>,
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

    /// How the launcher should behave when it can't find a compatible
    /// JVM on the host. See [`DownloadJdkMode`] for the three states.
    ///
    /// Default: [`DownloadJdkMode::Auto`]. Set with the
    /// `--download-jdk <off|auto|force>` builder flag, whose value is
    /// required — a bare `--download-jdk` is an error.
    #[serde(default)]
    pub download_jdk: DownloadJdkMode,

    /// Permit more than one running instance of this application.
    ///
    /// Default: `false`, which is single-instance. Launching an
    /// already-running application then does **not** start a second
    /// JVM: it brings the running instance's window to the front and
    /// exits `0`, leaving the first instance — and its arguments —
    /// untouched.
    ///
    /// Set to `true` with `--allow-multiple-instances` for a server, a
    /// command-line tool, or anything that genuinely wants a second
    /// copy. Note that this is a *build-time* choice stamped into the
    /// payload; the end user cannot change it at runtime.
    ///
    /// Appended last on purpose. Postcard encodes structs as a
    /// positional sequence with no field names, so a new field changes
    /// the layout wherever it lands — but keeping it at the end means
    /// the bytes of every pre-existing field are unchanged, which is
    /// the only thing that makes a wire bump reviewable.
    #[serde(default)]
    pub allow_multiple_instances: bool,
}

impl Default for LauncherBehavior {
    fn default() -> Self {
        Self {
            forward_args: true,
            cache_dir: None,
            jvm_discovery: JvmDiscovery::default(),
            download_jdk: DownloadJdkMode::default(),
            // Single-instance is the default on purpose: a second
            // launch of a running desktop app almost never means "give
            // me another copy", it means the user clicked the icon
            // again because the window went behind something. Opting
            // *out* is a deliberate build decision.
            allow_multiple_instances: false,
        }
    }
}

/// How the launcher reacts to "no compatible JVM on this machine".
///
/// Three states, in order of decreasing deference to the user's
/// environment:
///
/// - [`Off`](Self::Off) — no download flow at all.
/// - [`Auto`](Self::Auto) — only when `discover_jvm()` returns `None`.
///   The launcher pops up a `TaskDialog` offering Download / Open in
///   browser / Cancel. **The default**, on both this type and the
///   `--download-jdk` CLI flag.
/// - [`Force`](Self::Force) — skip `discover_jvm()` entirely and go
///   straight to the TaskDialog. The user always gets to choose
///   whether to download Temurin from `api.adoptium.net`, even if
///   another Java install would technically satisfy the minimum
///   version. Useful when the end user explicitly wants the bundled
///   Temurin regardless of what's already on PATH / JAVA_HOME.
///
/// On either `Auto` or `Force`, the launcher first checks
/// `%LOCALAPPDATA%\snug\jdk\<version>\` for a cached Temurin matching
/// `min_java`. A cache hit is reused silently (no GUI).
///
/// Wire format: postcard serialises a 3-variant unit-only enum as a
/// 1-byte discriminator (`0` = Off, `1` = Auto, `2` = Force). New
/// variants must be appended at the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DownloadJdkMode {
    /// No download flow at all.
    Off,
    /// Pop the TaskDialog only when JVM discovery fails.
    #[default]
    Auto,
    /// Always pop the TaskDialog, bypassing JVM discovery.
    Force,
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
