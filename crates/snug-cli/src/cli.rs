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
    long_about = "Wrap a Java fat JAR into a native Windows .exe launcher.\n\
                  \n\
                  Flags below are grouped by function: input/output paths, the\n\
                  Windows metadata that lands in the EXE's version resource,\n\
                  Java-runtime controls, Windows-resource files (icon / manifest\n\
                  / splash), build behaviour (JDK download, localization, dry\n\
                  run), and finally tool-of-the-CLI flags (options-file path,\n\
                  version, init-options).",
    // Demo `help_template` — overrides clap's default rendering. Each
    // `{placeholder}` is substituted at render time. Available tokens:
    //   {name}            binary / command name
    //   {version}         the version string set above
    //   {about}           short about for `-h`, long about for `--help`
    //                     (clap auto-picks)
    //   {usage-heading}   the literal "Usage:" (or your override)
    //   {usage}           the usage synopsis line
    //   {all-args}        every arg + positional, grouped by help_heading
    //   {positionals}     positionals only
    //   {options}         option flags only
    //   {subcommands}     subcommands (n/a — we don't have any)
    //   {before-help}     the `before_help` text
    //   {after-help}      the `after_help` text
    //   {tab}             indentation helper (renders as 4 spaces)
    //
    // This template is identical to clap's default EXCEPT for two
    // cosmetic tweaks that demonstrate the feature:
    //   1. "Usage:" is replaced with "USAGE:" (uppercase).
    //   2. `{all-args}` is used instead of separate `{positionals}` +
    //      `{options}` blocks — this lets clap's per-arg `help_heading`
    //      do all the work (and keeps our 6-section layout intact).
    help_template = "\
{name} {version}\n\
{about}\n\
\n\
USAGE:\n  \
{usage}\n\
\n\
{all-args}\n\
\n\
{after-help}",
    // Footer printed after the options list. Keeps the common
    // workflows in front of the user without re-listing every flag;
    // long-about covers the overview, after-help covers the recipes.
    after_help = "Examples:\n  \
                  snug App.jar -o App.exe --name \"My App\" --company \"Acme\" \\\n  \
                        --version 1.2.3 --min-java 25 --icon app.png\n\
                  \n  \
                  snug App.jar --dry-run                       # validate, don't build\n  \
                  snug App.jar --emit-payload > payload.bin   # write encoded payload\n  \
                  snug --init-options                          # write a starter snug.options\n\
                  \n\
                  Each `--localization your-locale.txt` you pass is embedded in the\n\
                  launcher alongside the built-in English baseline; the user's Windows\n\
                  UI language picks the right bundle at runtime. See README and the\n\
                  `snug-localisations.en.txt` baseline for the full key inventory.",
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
    #[arg(
        long = "input",
        value_name = "JAR|DIR",
        conflicts_with = "jar",
        help_heading = "Input / output"
    )]
    pub input: Option<PathBuf>,

    /// Output Windows executable path.
    ///
    /// Defaults to `<jar-stem>.exe` in the current directory.
    #[arg(short = 'o', long = "output", value_name = "EXE", help_heading = "Input / output")]
    pub output: Option<PathBuf>,

    /// Application display name (e.g. `"My App"`).
    ///
    /// Maps to the `ProductName` / `FileDescription` Windows version
    /// resource fields and to the per-user cache directory.
    #[arg(long = "name", value_name = "NAME", help_heading = "Application metadata")]
    pub name: Option<String>,

    /// Company / vendor name (e.g. `"SynapticLoop"`).
    ///
    /// Maps to the `CompanyName` Windows version resource field.
    #[arg(long = "company", value_name = "COMPANY", help_heading = "Application metadata")]
    pub company: Option<String>,

    /// Application version (e.g. `1.2.3`).
    ///
    /// Maps to `ProductVersion` / `FileVersion` in the Windows version
    /// resource; use a dotted quad (`1.2.3.0`) if you need exact bits,
    /// otherwise the missing build/revision default to zero.
    #[arg(long = "version", value_name = "VERSION", help_heading = "Application metadata")]
    pub version: Option<String>,

    /// Short description (Windows `FileDescription`).
    ///
    /// Shown by File Explorer on the EXE's "Details" tab.
    #[arg(long = "description", value_name = "TEXT", help_heading = "Application metadata")]
    pub description: Option<String>,

    /// Copyright string (Windows `LegalCopyright`).
    ///
    /// Shown by File Explorer on the EXE's "Details" tab alongside
    /// `--version` and `--description`.
    #[arg(long = "copyright", value_name = "TEXT", help_heading = "Application metadata")]
    pub copyright: Option<String>,

    /// URL surfaced as a clickable "Check for a newer version" link
    /// on the launcher's error dialog (e.g. the project's GitHub
    /// releases page). When `None` or empty, the link row is hidden.
    ///
    /// Clicking the link invokes the user's default browser via
    /// `ShellExecuteW(..., "open", url, ...)`. Whitelisted only in
    /// that it's your own EXE pointing at your own URL — no URL
    /// validation is performed at build time, so be careful what you
    /// bake in.
    #[arg(long = "update-url", value_name = "URL", help_heading = "Application metadata")]
    pub update_url: Option<String>,

    /// Minimum required Java major version.
    ///
    /// Defaults to `25` (the project's development target).
    #[arg(long = "min-java", value_name = "N", default_value_t = 25, help_heading = "Java runtime")]
    pub min_java: u16,

    /// Override the `Main-Class` read from the JAR manifest.
    #[arg(long = "main-class", value_name = "CLASS", help_heading = "Java runtime")]
    pub main_class: Option<String>,

    /// Extra JVM option forwarded to `JNI_CreateJavaVM`.
    ///
    /// Repeatable. Examples:
    /// `--jvm-arg=-Xms256m --jvm-arg=-Xmx2g --jvm-arg=-Dfile.encoding=UTF-8`
    #[arg(long = "jvm-arg", value_name = "ARG", allow_hyphen_values = true, help_heading = "Java runtime")]
    pub jvm_args: Vec<String>,

    /// `.ico` or `.png` file used as the Windows Explorer icon for the EXE.
    #[arg(long = "icon", value_name = "PNG/ICO", help_heading = "Windows resources (icon / manifest / splash)")]
    pub icon: Option<PathBuf>,

    /// Optional Windows application manifest (XML) embedded as
    /// `RT_MANIFEST`. Use this to declare DPI-awareness, side-by-side
    /// assembly identity, or `requestedExecutionLevel` for UAC.
    #[arg(long = "manifest", value_name = "XML", help_heading = "Windows resources (icon / manifest / splash)")]
    pub manifest: Option<PathBuf>,

    /// PNG splash image shown by the native launcher before the JVM starts.
    ///
    /// The PNG is embedded verbatim into the EXE and rendered at its
    /// native pixel size, centred on the primary monitor. Recommended
    /// for a branded splash: somewhere between `480x270` and
    /// `640x360`. Anything bigger triggers a build-time warning
    /// (see `--splash-max`); anything smaller renders fine but may
    /// look lost on high-DPI displays.
    #[arg(long = "splash", value_name = "PNG", help_heading = "Windows resources (icon / manifest / splash)")]
    pub splash: Option<PathBuf>,

    /// Minimum splash duration in milliseconds.
    ///
    /// The splash is dismissed once the JVM signals readiness *or* this
    /// duration elapses, whichever is later. Defaults to `1500`.
    #[arg(long = "splash-ms", value_name = "MS", default_value_t = 1_500, help_heading = "Windows resources (icon / manifest / splash)")]
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
        default_value = "640x360",
        help_heading = "Windows resources (icon / manifest / splash)"
    )]
    pub splash_max: String,

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
        help_heading = "Behaviour (JDK download / localization / dry run)"
    )]
    pub download_jdk: CliDownloadJdkMode,

    /// Localization bundle to embed in the launcher, formatted as a
    /// flat `key = value` text file (Java-`.properties`-style).
    ///
    /// Filename convention: `snug-localisations.<tag>.txt` where
    /// `<tag>` is a BCP 47 locale (`en`, `en-US`, `pt-BR`, `de`,
    /// `ja`, ...). The tag is taken from the filename — the CLI does
    /// not inspect its contents to guess.
    ///
    /// Repeatable. Each bundle is embedded verbatim into the payload;
    /// at runtime the launcher builds an ordered lookup list of
    /// bundles (full BCP 47 tag → primary subtag → built-in English
    /// baseline) and walks them on every lookup. The built-in English
    /// baseline is always embedded by the CLI, so a build with no
    /// `--localization` flag still produces a working launcher.
    ///
    /// Pass a single `--localization your-locale.txt` for a
    /// full-translation build, or many (one per locale) to ship a
    /// multilingual launcher. Use the bare-stub
    /// `snug-localisations.en.txt` shipped in this repo as a template
    /// for the keys.
    ///
    /// The CLI warns at build time about any keys the bundle is
    /// missing relative to the built-in English baseline — keep the
    /// keys in sync so end users never see raw key text instead of a
    /// localized message.
    #[arg(
        long = "localization",
        value_name = "TXT",
        value_parser = crate::localization::validate_localization_path,
        help_heading = "Behaviour (JDK download / localization / dry run)"
    )]
    pub localizations: Vec<std::path::PathBuf>,

    /// Write the encoded embedded payload to stdout instead of writing a
    /// file or building an EXE. Useful for piping into other tools or for
    /// inspecting the format.
    #[arg(long = "emit-payload", help_heading = "Behaviour (JDK download / localization / dry run)")]
    pub emit_payload: bool,

    /// Validate inputs and print what would be built, but write nothing.
    #[arg(long = "dry-run", help_heading = "Behaviour (JDK download / localization / dry run)")]
    pub dry_run: bool,

    /// Print snug's own version (from `Cargo.toml`) and exit.
    ///
    /// Distinct from `--version <APP-VERSION>`, which sets the
    /// wrapped application's version. Snug's version is otherwise
    /// shown in the no-args help output.
    #[arg(long = "snug-version", action = clap::ArgAction::Version, help_heading = "Tooling (options file / version / init-options)")]
    pub snug_version: (),

    /// Path to a snug options file. Default: `snug.options` in the
    /// current working directory, if present.
    ///
    /// Format: one option per line, parsed as if it were supplied on
    /// the command line (so `--name "My App"` works, quoting and
    /// escaping included). Lines starting with `#` are comments.
    /// Command-line options override file options.
    #[arg(long = "options", value_name = "PATH", help_heading = "Tooling (options file / version / init-options)")]
    pub options: Option<PathBuf>,

    /// Write the embedded `snug.options` example file to disk, then
    /// exit without performing a build.
    ///
    /// The example covers every CLI flag with a commented-out
    /// demonstration, suitable for dropping into a project root and
    /// editing. The path is optional: `--init-options` writes
    /// `./snug.options` (the default lookup path), and
    /// `--init-options=cfg/build.options` writes a custom path.
    ///
    /// By default refuses to overwrite an existing file; pass
    /// `--init-options-force` to overwrite silently. Pass
    /// `--init-options-stdout` to print to stdout instead of writing
    /// (useful for piping, version control, or previewing).
    ///
    /// When this flag is set, the CLI bypasses options-file loading
    /// (no point reading a file we're about to write) and the
    /// JAR-required check — you don't need a fat JAR to bootstrap a
    /// config.
    #[arg(
        long = "init-options",
        value_name = "PATH",
        num_args = 0..=1,
        default_missing_value = "snug.options",
        conflicts_with = "jar",
        conflicts_with = "input",
        help_heading = "Tooling (options file / version / init-options)"
    )]
    pub init_options: Option<String>,

    /// Overwrite an existing file at the `--init-options` target
    /// instead of refusing. Has no effect without `--init-options`.
    #[arg(long = "init-options-force", help_heading = "Tooling (options file / version / init-options)")]
    pub init_options_force: bool,

    /// Print the `--init-options` example to stdout instead of
    /// writing it to disk. Has no effect without `--init-options`.
    #[arg(long = "init-options-stdout", help_heading = "Tooling (options file / version / init-options)")]
    pub init_options_stdout: bool,
}
