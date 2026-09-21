//! Command-line argument definitions for `snug`.
//!
//! Flag surface mirrors the brief:
//!
//! ```text
//! snug <jar> [-o EXE] [--name ...] [--company ...] [--version ...]
//!            [--description ...] [--copyright ...]
//!            [--min-java N] [--main-class CLASS]
//!            [--icon PNG/ICO] [--manifest XML] [--splash PNG] [--splash-ms MS]
//!            [--jvm-arg ARG]...
//! ```

use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use snug_format::DownloadJdkMode;

/// CLI-side mirror of [`snug_format::DownloadJdkMode`].
///
/// Exists as a separate type so we can derive `clap::ValueEnum`
/// without pulling `clap` into `snug-format` (the orphan rule
/// forbids `impl ValueEnum for DownloadJdkMode` in this crate).
/// `build.rs` converts at the seam via `From`.
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum CliDownloadJdkMode {
    /// No download flow — bail if no JVM is found.
    Off,
    /// Ask the user to download Temurin only if no compatible JVM is found.
    Auto,
    /// Always show the download dialog, bypassing JVM discovery.
    Force,
}

impl From<CliDownloadJdkMode> for DownloadJdkMode {
    fn from(m: CliDownloadJdkMode) -> Self {
        match m {
            CliDownloadJdkMode::Off => DownloadJdkMode::Off,
            CliDownloadJdkMode::Auto => DownloadJdkMode::Auto,
            CliDownloadJdkMode::Force => DownloadJdkMode::Force,
        }
    }
}

#[derive(Debug, Clone, Parser)]
#[command(
    name = "snug",
    about = "Wrap a Java fat JAR into a native Windows .exe launcher",
    long_about = None,
    // The brief uses `--version <VER>` to set the application version,
    // which clashes with clap's auto-generated `--version` flag. We
    // disable the auto-version and treat our `--version` as the app
    // version. Snug's own version is exposed via `--snug-version` and
    // embedded in the no-args help output.
    disable_version_flag = true,
    version = env!("CARGO_PKG_VERSION"),
)]
pub struct Cli {
    /// Input fat JAR file. Omit to print help + version.
    ///
    /// Equivalent to `--input <jar>`; the positional form is kept for
    /// shell convenience. `--input` and the positional are mutually
    /// exclusive — supply one or the other.
    pub jar: Option<PathBuf>,

    /// Input source — either a single fat-JAR file or a directory
    /// containing one or more JARs.
    ///
    /// When the path is a file, it is wrapped as a single-JAR launcher
    /// (identical to the positional `[JAR]` argument). When the path is
    /// a directory, every `*.jar` directly inside it is scanned, sorted
    /// by name, and embedded into the launcher as a multi-JAR classpath;
    /// the runtime launcher extracts them all to its per-user cache and
    /// concatenates them into `-classpath`. Use `--main-class` to
    /// override the manifest's `Main-Class` for multi-JAR builds.
    ///
    /// `--input` and the positional `[JAR]` argument are mutually
    /// exclusive; supply one or the other. Suitable for `snug.options`
    /// so the JAR location doesn't need to live on the command line.
    #[arg(long = "input", value_name = "JAR|DIR", conflicts_with = "jar")]
    pub input: Option<PathBuf>,

    /// Output Windows executable path.
    ///
    /// Defaults to `<jar-stem>.exe` in the current directory.
    #[arg(short = 'o', long = "output", value_name = "EXE")]
    pub output: Option<PathBuf>,

    /// Application display name (e.g. `"My App"`).
    ///
    /// Maps to the `ProductName` / `FileDescription` Windows version
    /// resource fields and to the per-user cache directory.
    #[arg(long = "name", value_name = "NAME")]
    pub name: Option<String>,

    /// Company / vendor name (e.g. `"SynapticLoop"`).
    #[arg(long = "company", value_name = "COMPANY")]
    pub company: Option<String>,

    /// Application version (e.g. `1.2.3`).
    #[arg(long = "version", value_name = "VERSION")]
    pub version: Option<String>,

    /// Description (Windows `FileDescription`).
    #[arg(long = "description", value_name = "TEXT")]
    pub description: Option<String>,

    /// Copyright string (Windows `LegalCopyright`).
    #[arg(long = "copyright", value_name = "TEXT")]
    pub copyright: Option<String>,

    /// Minimum required Java major version.
    ///
    /// Defaults to `25` (the project's development target).
    #[arg(long = "min-java", value_name = "N", default_value_t = 25)]
    pub min_java: u16,

    /// Override the `Main-Class` read from the JAR manifest.
    #[arg(long = "main-class", value_name = "CLASS")]
    pub main_class: Option<String>,

    /// `.ico` or `.png` file used as the Windows Explorer icon for the EXE.
    #[arg(long = "icon", value_name = "PNG/ICO")]
    pub icon: Option<PathBuf>,

    /// Optional Windows application manifest (XML) embedded as
    /// `RT_MANIFEST`. Use this to declare DPI-awareness, side-by-side
    /// assembly identity, or `requestedExecutionLevel` for UAC.
    #[arg(long = "manifest", value_name = "XML")]
    pub manifest: Option<PathBuf>,

    /// PNG splash image shown by the native launcher before the JVM starts.
    ///
    /// The PNG is embedded verbatim into the EXE and rendered at its
    /// native pixel size, centred on the primary monitor. Recommended
    /// for a branded splash: somewhere between `480x270` and
    /// `640x360`. Anything bigger triggers a build-time warning
    /// (see `--splash-max`); anything smaller renders fine but may
    /// look lost on high-DPI displays.
    #[arg(long = "splash", value_name = "PNG")]
    pub splash: Option<PathBuf>,

    /// Minimum splash duration in milliseconds.
    ///
    /// The splash is dismissed once the JVM signals readiness *or* this
    /// duration elapses, whichever is later. Defaults to `1500`.
    #[arg(long = "splash-ms", value_name = "MS", default_value_t = 1_500)]
    pub splash_ms: u32,

    /// Maximum recommended splash dimensions, or `off` to silence the
    /// size warning at build time.
    ///
    /// Format: `<W>x<H>` (e.g. `640x360`, `800x600`). When the supplied
    /// `--splash` PNG is wider or taller than this, snug emits a
    /// build-time warning telling you the launcher will render at
    /// native size (which usually looks oversized). The launcher does
    /// not rescale the image — this is purely advisory.
    ///
    /// Set to `off`, `none`, or `unlimited` (case-insensitive) to
    /// disable the warning without changing dimensions. Use a custom
    /// `WxH` to raise the threshold; e.g. `--splash-max 1920x1080`
    /// for a full-screen splash.
    #[arg(
        long = "splash-max",
        value_name = "WxH|off",
        default_value = "640x360"
    )]
    pub splash_max: String,

    /// Extra JVM option forwarded to `JNI_CreateJavaVM`.
    ///
    /// Repeatable. Examples:
    /// `--jvm-arg=-Xms256m --jvm-arg=-Xmx2g --jvm-arg=-Dfile.encoding=UTF-8`
    #[arg(long = "jvm-arg", value_name = "ARG", allow_hyphen_values = true)]
    pub jvm_args: Vec<String>,

    /// Write the encoded embedded payload to stdout instead of writing a
    /// file or building an EXE. Useful for piping into other tools or for
    /// inspecting the format.
    #[arg(long = "emit-payload")]
    pub emit_payload: bool,

    /// Validate inputs and print what would be built, but write nothing.
    #[arg(long = "dry-run")]
    pub dry_run: bool,

    /// Print snug's own version (from `Cargo.toml`) and exit.
    ///
    /// Distinct from `--version <APP-VERSION>`, which sets the
    /// wrapped application's version. Snug's version is otherwise
    /// shown in the no-args help output.
    #[arg(long = "snug-version", action = clap::ArgAction::Version)]
    pub snug_version: (),

    /// Path to a snug options file. Default: `snug.options` in the
    /// current working directory, if present.
    ///
    /// Format: one option per line, parsed as if it were supplied on
    /// the command line (so `--name "My App"` works, quoting and
    /// escaping included). Lines starting with `#` are comments.
    /// Command-line options override file options.
    #[arg(long = "options", value_name = "PATH")]
    pub options: Option<PathBuf>,

    /// If the launcher can't find a compatible JDK at runtime, pop a
    /// `TaskDialog` and offer to download Eclipse Temurin from
    /// `api.adoptium.net`, verify SHA-256, extract to
    /// `%LOCALAPPDATA%\snug\jdk\<version>\`, and retry.
    ///
    /// Modes:
    ///
    /// - omitted — no download flow at all.
    /// - `--download-jdk` (no value) — `auto`: pop the dialog only if
    ///   JVM discovery fails. Equivalent to the legacy boolean flag.
    /// - `--download-jdk=auto` — same as above, explicit.
    /// - `--download-jdk=force` — always pop the dialog, bypassing
    ///   JVM discovery. Useful when the end user wants to install
    ///   Temurin regardless of what's on `JAVA_HOME` / `PATH`.
    ///
    /// Off by default — you'd typically build two flavours of your
    /// EXE: one with the flag (portable, end-user friendly) and one
    /// without (developer, requires Java pre-installed).
    #[arg(
        long = "download-jdk",
        value_enum,
        default_missing_value = "auto",
        default_value_t = CliDownloadJdkMode::Off,
        num_args = 0..=1,
        require_equals = true,
    )]
    pub download_jdk: CliDownloadJdkMode,
}
