//! Command-line argument definitions for `snug`.
//!
//! Flag surface mirrors the brief:
//!
//! ```text
//! snug <jar> [-o EXE] [--name ...] [--company ...] [--version ...]
//!            [--description ...] [--copyright ...]
//!            [--min-java N] [--main-class CLASS]
//!            [--icon ICO] [--splash PNG] [--splash-ms MS]
//!            [--jvm-arg ARG]...
//! ```

use std::path::PathBuf;

use clap::Parser;

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
    pub jar: Option<PathBuf>,

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

    /// `.ico` file used as the Windows Explorer icon for the EXE.
    #[arg(long = "icon", value_name = "ICO")]
    pub icon: Option<PathBuf>,

    /// PNG splash image shown by the native launcher before the JVM starts.
    #[arg(long = "splash", value_name = "PNG")]
    pub splash: Option<PathBuf>,

    /// Minimum splash duration in milliseconds.
    ///
    /// The splash is dismissed once the JVM signals readiness *or* this
    /// duration elapses, whichever is later. Defaults to `1500`.
    #[arg(long = "splash-ms", value_name = "MS", default_value_t = 1_500)]
    pub splash_ms: u32,

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

    /// Path to `rcedit.exe` used for stamping icon and version-resource
    /// metadata into the produced EXE. Defaults to `rcedit` on `PATH`.
    ///
    /// Resource stamping only runs on Windows. On macOS / Linux this
    /// flag is ignored unless the explicit path points to a Wine-
    /// invokable rcedit binary.
    #[arg(long = "rcedit", value_name = "PATH")]
    pub rcedit: Option<PathBuf>,

    /// Skip the rcedit resource-stamping step even when icon /
    /// version metadata was supplied. Useful when building the EXE
    /// locally and stamping on a Windows machine as a separate step.
    #[arg(long = "no-rcedit")]
    pub no_rcedit: bool,

    /// Print snug's own version (from `Cargo.toml`) and exit.
    ///
    /// Distinct from `--version <APP-VERSION>`, which sets the
    /// wrapped application's version. Snug's version is otherwise
    /// shown in the no-args help output.
    #[arg(long = "snug-version", action = clap::ArgAction::Version)]
    pub snug_version: (),
}
