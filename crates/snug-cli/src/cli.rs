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

/// Reject a relative `--cache-dir`.
///
/// The value is baked into the payload at build time and used by the
/// launcher at *runtime*, on a machine the builder never sees, launched
/// by a user with an arbitrary working directory. A relative path would
/// silently resolve against whatever CWD that happens to be, so the
/// cache would scatter — or fail to be found at all.
///
/// Deliberately does **not** require the directory to exist: the
/// launcher creates the tree on first launch, and building on a
/// machine that has never run the app is the normal case.
fn validate_cache_dir(s: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(s);
    if s.trim().is_empty() {
        return Err("cache directory path is empty".to_string());
    }
    if !path.is_absolute() {
        return Err(format!(
            "cache directory must be an absolute path, got `{s}` \
             (it is resolved at runtime against the end user's working \
             directory, which snug cannot know)"
        ));
    }
    Ok(path)
}

#[derive(Debug, Clone, Parser)]
#[command(
    name = "snug",
    about = "Wrap a Java fat JAR into a native Windows .exe launcher",
    long_about = "\n\
                  \n\
                  #       .-----.-----.--.--.-----.\n\
                  #       |__ --|     |  |  |  _  |\n\
                  #       |_____|__|__|_____|___  |\n\
                  #             ... .-..    |_____|\n\
                  \n\
                  #            ~ ~ ~ * ~ ~ ~\n\
                  \n\
                  ~ ~ Wrap a Java JARs into a native Windows .exe launcher. ~ ~\n\
                  \n    \
                  The produced EXE loads `jvm.dll` directly via JNI so it appears as `<App>.exe` \n    \
                  (not `javaw.exe`) in Task Manager, and the launcher locates a compatible JDK on \n    \
                  the target machine before falling back to an optional JDK download.\n\
                  \n\
                  # Minimum required for a normal build:\n\n  \
                  Input:\n    \
                  --input <JAR|DIR>\n  \
                  or\n    \
                  [JAR]\n\n  \
                  Application identity:\n    \
                  --name <NAME>\n    \
                  --company <COMPANY>\n\
                  # Recommended before distribution:\n\n    \
                  --version <VERSION>\n    \
                  --description <TEXT>\n    \
                  --icon <PNG/ICO>\n    \
                  ",
    // Footer printed after the options list. Keeps the common
    // workflows in front of the user without re-listing every flag;
    // long-about covers the overview, after-help covers the recipes.
    after_help = "Examples:\n\n  \
                  snug App.jar -o App.exe --name \"My App\" --company \"My Company\" --version 1.2.3 --min-java 25 --icon app.png\n\
                  \n  \
                  snug App.jar --dry-run                      # validate, don't build\n  \
                  snug App.jar --emit-payload > payload.bin   # write encoded payload\n  \
                  snug --init-options                         # write a starter snug.options\n  \n                  snug --init-localizations --init-localizations-tag de  # starter localisations/\n\
                  \n\
                  Each `--localization your-locale.txt` you pass is embedded in the\n\
                  launcher alongside the built-in English baseline; the user's Windows\n\
                  UI language picks the right bundle at runtime. Bundles must match the\n\
                  baseline in both keys and `{placeholders}`; snug warns at build time\n\
                  when they disagree. Run `snug --list-tokens` for the full placeholder\n\
                  contract, or scaffold a starter bundle with `snug --init-localizations`.\n",
    // The brief uses `--version <VER>` to set the application version,
    // which clashes with clap's auto-generated `--version` flag. We
    // disable the auto-version and treat our `--version` as the app
    // version. Snug's own version is exposed via `--snug-version` and
    // embedded in the no-args help output.
    disable_version_flag = true,
    version = env!("CARGO_PKG_VERSION"),
)]
pub struct Cli {
    /// Input fat JAR file or directory of JARs. Optional only when
    /// `--input` — or an options file — supplies one; snug errors out
    /// rather than printing help when no input source is present.
    ///
    /// Equivalent to `--input <path>`; the positional form is kept for
    /// shell convenience. `--input` and the positional are mutually
    /// exclusive — supply one or the other.
    #[arg(value_name = "JAR|DIR", help_heading = "Input / output")]
    pub jar: Option<PathBuf>,

    /// Input source — either a single fat-JAR file or a directory
    /// containing one or more JARs.
    ///
    /// When the path is a file, it is wrapped as a single-JAR launcher
    /// (identical to the positional `[JAR|DIR]` argument). When the path is
    /// a directory, every `*.jar` directly inside it is scanned, sorted
    /// by name, and embedded into the launcher as a multi-JAR classpath;
    /// the runtime launcher extracts them all to its per-user cache and
    /// concatenates them into `-classpath`. Use `--main-class` to
    /// override the manifest's `Main-Class` for multi-JAR builds.
    ///
    /// `--input` and the positional `[JAR|DIR]` argument are mutually
    /// exclusive *on one command line* — supplying both is an error.
    /// Across sources it is a precedence question instead, and it
    /// resolves like every other option: a JAR given on the command line
    /// overrides an `--input` from an options file. So an options file
    /// can carry a default input for a bare `snug`, while
    /// `snug some-other.jar` still builds that one.
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
    #[arg(
        short = 'o',
        long = "output",
        value_name = "EXE",
        help_heading = "Input / output"
    )]
    pub output: Option<PathBuf>,

    /// Application display name (e.g. `"My App"`).
    ///
    /// Maps to the `ProductName` / `FileDescription` Windows version
    /// resource fields and to the per-user cache directory.
    #[arg(
        long = "name",
        value_name = "NAME",
        help_heading = "Application metadata"
    )]
    pub name: Option<String>,

    /// Company / vendor name (e.g. `"SynapticLoop"`).
    ///
    /// Maps to the `CompanyName` Windows version resource field.
    ///
    /// Optional. When omitted it falls back to the resolved application
    /// name, so a build without it shows `CompanyName` and `ProductName`
    /// alike rather than a placeholder that reads as an error. It has
    /// to be non-empty either way: the runtime cache is keyed
    /// `snug/<company>/<app>`, and an empty company would collapse that
    /// to `snug/<app>`, putting two companies' identically named apps
    /// in one directory.
    #[arg(
        long = "company",
        value_name = "COMPANY",
        help_heading = "Application metadata"
    )]
    pub company: Option<String>,

    /// Application version (e.g. `1.2.3`).
    ///
    /// Maps to `ProductVersion` / `FileVersion` in the Windows version
    /// resource. The Windows quad holds four 16-bit components, so pass
    /// 1–4 dot-separated numbers with no quotes — a bare `1.2.3.4`.
    /// Trailing components may be omitted and default to zero
    /// (`1.2.3` → `1.2.3.0`); each component must be 0–65535.
    ///
    /// Anything else is rejected before the build starts rather than
    /// being silently zero-filled into the version resource.
    #[arg(
        long = "version",
        value_name = "VERSION",
        value_parser = crate::resources::validate_app_version,
        help_heading = "Application metadata"
    )]
    pub version: Option<String>,

    /// Short description (Windows `FileDescription`).
    ///
    /// Shown by File Explorer on the EXE's "Details" tab.
    #[arg(
        long = "description",
        value_name = "TEXT",
        help_heading = "Application metadata"
    )]
    pub description: Option<String>,

    /// Copyright string (Windows `LegalCopyright`).
    ///
    /// Shown by File Explorer on the EXE's "Details" tab alongside
    /// `--version` and `--description`.
    #[arg(
        long = "copyright",
        value_name = "TEXT",
        help_heading = "Application metadata"
    )]
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
    #[arg(
        long = "update-url",
        value_name = "URL",
        help_heading = "Application metadata"
    )]
    pub update_url: Option<String>,

    /// Minimum required Java major version.
    ///
    /// Defaults to `25` (the project's development target).
    #[arg(
        long = "min-java",
        value_name = "N",
        default_value_t = 25,
        help_heading = "Java runtime"
    )]
    pub min_java: u16,

    /// Override the `Main-Class` read from the JAR manifest.
    #[arg(
        long = "main-class",
        value_name = "CLASS",
        help_heading = "Java runtime"
    )]
    pub main_class: Option<String>,

    /// Extra JVM option forwarded to `JNI_CreateJavaVM`.
    ///
    /// Repeatable. Examples:
    /// `--jvm-arg=-Xms256m --jvm-arg=-Xmx2g --jvm-arg=-Dfile.encoding=UTF-8`
    #[arg(
        long = "jvm-arg",
        value_name = "OPTION",
        allow_hyphen_values = true,
        help_heading = "Java runtime"
    )]
    pub jvm_args: Vec<String>,

    /// `.ico` or `.png` file used as the Windows Explorer icon for the EXE.
    #[arg(
        long = "icon",
        value_name = "PNG/ICO",
        help_heading = "Windows resources"
    )]
    pub icon: Option<PathBuf>,

    /// Optional Windows application manifest (XML) embedded as
    /// `RT_MANIFEST`. Use this to declare DPI-awareness, side-by-side
    /// assembly identity, or `requestedExecutionLevel` for UAC.
    #[arg(
        long = "manifest",
        value_name = "XML",
        help_heading = "Windows resources"
    )]
    pub manifest: Option<PathBuf>,

    /// PNG splash image shown by the native launcher before the JVM starts.
    ///
    /// The PNG is embedded verbatim into the EXE and rendered at its
    /// native pixel size, centred on the primary monitor. Recommended
    /// for a branded splash: somewhere between `480x270` and
    /// `640x360`. Anything bigger triggers a build-time warning
    /// (see `--splash-max`); anything smaller renders fine but may
    /// look lost on high-DPI displays.
    #[arg(
        long = "splash",
        value_name = "PNG",
        help_heading = "Windows resources"
    )]
    pub splash: Option<PathBuf>,

    /// Minimum splash duration in milliseconds.
    ///
    /// The splash is dismissed once the JVM signals readiness *or* this
    /// duration elapses, whichever is later. Defaults to `1500`.
    #[arg(
        long = "splash-ms",
        value_name = "MS",
        default_value_t = 1_500,
        help_heading = "Windows resources"
    )]
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
        help_heading = "Windows resources"
    )]
    pub splash_max: String,

    /// If the launcher can't find a compatible JDK at runtime, pop a
    /// `TaskDialog` and offer to download Eclipse Temurin from
    /// `api.adoptium.net`, verify SHA-256, extract to
    /// `%LOCALAPPDATA%\snug\jdk\<version>\`, and retry.
    ///
    /// Modes:
    ///
    /// - omitted — `auto`: pop the dialog only if JVM discovery fails.
    ///
    /// - `--download-jdk=auto` — same as above, explicit.
    ///
    /// - `--download-jdk=force` — always pop the dialog, bypassing
    ///   JVM discovery. Useful when the end user wants to install
    ///   Temurin regardless of what's on `JAVA_HOME` / `PATH`.
    ///
    /// - `--download-jdk=off` — no download flow at all.
    ///
    /// **The value is required.** `--download-jdk` on its own is an
    /// error, not a synonym for `auto`.
    ///
    /// That is a deliberate change from the earlier shape, where a bare
    /// `--download-jdk` meant `auto` and the value had to be attached
    /// with `=` (`require_equals = true`). The cost of that arrangement
    /// was that `--download-jdk force` did not mean what it looked
    /// like: `require_equals` stops clap consuming the next token, so
    /// the flag silently became `auto` and `force` fell through to the
    /// positional `[JAR|DIR]` slot. Depending on what else was on the
    /// line the user got `input path does not exist: force`, or
    /// `unexpected argument '<jar>' found`, or — the worst, because
    /// nobody sees a command line — `the argument '--input <JAR|DIR>'
    /// cannot be used with '[JAR|DIR]'` out of a shared options file.
    /// Every one of those names something the user never typed.
    ///
    /// Requiring the value trades that for an error that points at the
    /// flag you actually wrote. `--download-jdk force` now works, and
    /// `--download-jdk app.jar` (mode forgotten) becomes `invalid
    /// value 'app.jar' for '--download-jdk <DOWNLOAD_JDK>'` — still a
    /// mistake, but one the message names the cause of.
    ///
    /// `auto` by default, because that is the portable choice: an EXE
    /// that finds a compatible JVM runs silently, and one that doesn't
    /// offers to install Temurin rather than dying with an error
    /// dialog. Pass `--download-jdk=off` to cut a developer-facing
    /// build down to "Java must already be installed".
    #[arg(
        long = "download-jdk",
        value_enum,
        default_value_t = CliDownloadJdkMode::Auto,
        num_args = 1,
        help_heading = "Build behaviour"
    )]
    pub download_jdk: CliDownloadJdkMode,

    /// Allow more than one instance of the application to run at once.
    ///
    /// By default snug runs **one** instance per application. Launching
    /// the produced EXE (or `.app`) again while it is already running
    /// brings the running instance's window to the front and exits,
    /// rather than starting a second JVM — which is what almost every
    /// desktop application means by a second click on its icon, and
    /// what stops the user ending up with two copies writing to the
    /// same files.
    ///
    /// Pass this flag when a second copy is genuinely wanted: a server,
    /// a command-line tool, or an app that is happy to be launched
    /// twice.
    ///
    /// The choice is stamped into the payload at build time and applies
    /// to every launch of that EXE — the end user cannot override it.
    /// Instances are identified by `--company` and `--name`, so two
    /// builds with different metadata are independent of each other and
    /// can run side by side.
    #[arg(long = "allow-multiple-instances", help_heading = "Build behaviour")]
    pub allow_multiple_instances: bool,

    /// Override the per-user cache root the launcher extracts JARs into.
    ///
    /// By default the launcher uses
    /// `%LOCALAPPDATA%\snug\<company>\<app>\` on Windows (and
    /// `$HOME/.cache/snug/<company>/<app>/` elsewhere). Point this at a
    /// different volume when the system drive is small or slow — a
    /// large fat JAR is copied there in full on first launch, and
    /// snug's retention policy keeps several generations.
    ///
    /// The path is stamped into the payload at build time, so it must
    /// be **absolute**: a relative path would resolve against whatever
    /// working directory the end user happens to launch the EXE from.
    /// Must exist or be creatable at runtime; snug creates the
    /// directory tree on first launch.
    #[arg(
        long = "cache-dir",
        value_name = "DIR",
        value_parser = validate_cache_dir,
        help_heading = "Build behaviour"
    )]
    pub cache_dir: Option<PathBuf>,

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
    /// full-translation build, many (one per locale) to ship a
    /// multilingual launcher, or a directory containing
    /// `snug-localisations.<tag>.txt` files. A directory entry is
    /// scanned top-level only (subdirectories are not searched); every
    /// file in the directory must match the pattern (a stray `.bak`,
    /// README, or typo'd name fails the build with a clear error),
    /// and the expanded list combines freely with explicit-file flags.
    /// Use the bare-stub `snug-localisations.en.txt` shipped in this
    /// repo as a template for the keys.
    ///
    /// The CLI warns at build time about any keys the bundle is
    /// missing relative to the built-in English baseline — keep the
    /// keys in sync so end users never see raw key text instead of a
    /// localized message.
    #[arg(
        long = "localization",
        value_name = "TXT|DIR",
        value_parser = crate::localization::validate_localization_path,
        help_heading = "Build behaviour"
    )]
    pub localizations: Vec<std::path::PathBuf>,

    /// Write the encoded embedded payload to stdout instead of writing a
    /// file or building an EXE. Useful for piping into other tools or for
    /// inspecting the format.
    #[arg(long = "emit-payload", help_heading = "Build behaviour")]
    pub emit_payload: bool,

    /// Validate inputs and print what would be built, but write nothing.
    #[arg(long = "dry-run", help_heading = "Build behaviour")]
    pub dry_run: bool,

    /// List every class in the input JAR(s) that declares a
    /// `public static void main(String[])`, then report whether the
    /// main class snug would use is one of them.
    ///
    /// Use it to pick a value for `--main-class` on a fat JAR that
    /// ships several entry points, or to confirm the one already
    /// configured. The matching class is marked inline with `<--`.
    ///
    /// Purely diagnostic: it writes no files, builds no EXE, and
    /// never fails a build — including when your main class is absent
    /// from the list. That case is genuinely ambiguous, because a
    /// JavaFX `Application` subclass has no `main` method and snug's
    /// launcher supports those directly.
    ///
    /// Reads only the front of each `.class` entry, so it costs about
    /// a second on a 5,000-class fat JAR and nothing at all on builds
    /// that don't pass this flag.
    #[arg(long = "find-main", help_heading = "Build behaviour")]
    pub find_main: bool,

    /// Print the placeholder (`{token}`) contract for the built-in
    /// English baseline: every key, which tokens its value uses, and
    /// every distinct token in one list.
    ///
    /// A bundle's keys are only half of what has to match; the other
    /// half is the `{version}` / `{size_mb}` / `{attempt}` slots inside
    /// each value. Nothing documented that outside the English file, so
    /// authoring a translation meant reading 107 lines and spotting the
    /// braces by eye.
    ///
    /// Diagnostic, like `--find-main`: reads only the compiled-in
    /// baseline, touches no JAR and no options file, writes nothing, and
    /// cannot fail a build. Runs in microseconds with no build context
    /// at all — `snug --list-tokens` is the whole invocation.
    ///
    /// The build-time counterpart is the placeholder warning snug emits
    /// for `--localization` bundles that disagree with the baseline.
    #[arg(long = "list-tokens", help_heading = "CLI tooling")]
    pub list_tokens: bool,

    /// Print snug's own version (from `Cargo.toml`) and exit.
    ///
    /// Distinct from `--version <APP-VERSION>`, which sets the
    /// wrapped application's version. Snug's version is otherwise
    /// shown in the no-args help output.
    #[arg(long = "snug-version", action = clap::ArgAction::Version, help_heading = "CLI tooling")]
    pub snug_version: (),

    /// Path to a snug options file, used **instead of** the default
    /// lookup. Naming a file means that file only: the OS-specific
    /// `snug.<os>.options` is not consulted as well, so a build's
    /// result never depends on which machine ran it.
    ///
    /// With no flag, snug reads up to two files — `snug.options` for
    /// every platform, then `snug.<os>.options` for this host
    /// (`snug.macos.options`, `snug.windows.options`,
    /// `snug.linux.options`) — looking in the snug executable's own
    /// directory first, then the current working directory, for each
    /// name in turn. A missing file is not an error.
    ///
    /// Precedence, highest first: command line, then
    /// `snug.<os>.options`, then `snug.options`. An OS file is a partial
    /// override — it carries only the values that differ, and the rest
    /// still come from `snug.options`.
    ///
    /// Format: one option per line, parsed as if it were supplied on
    /// the command line (so `--name "My App"` works, quoting and
    /// escaping included). Lines starting with `#` are comments.
    #[arg(long = "options", value_name = "PATH", help_heading = "CLI tooling")]
    pub options: Option<PathBuf>,

    /// Write the embedded `snug.options` example file to disk, then
    /// exit without performing a build.
    ///
    /// The example covers every CLI flag with a commented-out
    /// demonstration, suitable for dropping into a project root and
    /// editing. The path is optional: `--init-options` writes
    /// `./snug.options` (the CWD default lookup path), and
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
        help_heading = "CLI tooling"
    )]
    pub init_options: Option<String>,

    /// Overwrite an existing file at the `--init-options` target
    /// instead of refusing. Has no effect without `--init-options`.
    #[arg(long = "init-options-force", help_heading = "CLI tooling")]
    pub init_options_force: bool,

    /// Print the `--init-options` example to stdout instead of
    /// writing it to disk. Has no effect without `--init-options`.
    #[arg(long = "init-options-stdout", help_heading = "CLI tooling")]
    pub init_options_stdout: bool,

    /// Write a starter `localisations/` directory of
    /// `snug-localisations.<tag>.txt` bundles, then exit without
    /// performing a build.
    ///
    /// The English baseline is always written; add more languages with
    /// `--init-localizations-tag de` (repeatable). The directory
    /// contains only files matching `snug-localisations.<tag>.txt`,
    /// so it can be handed straight to `--localization
    /// localisations` on a later build.
    ///
    /// The directory defaults to `./localisations` — the directory
    /// `snug` was invoked from.
    /// Pass a path to write elsewhere.
    ///
    /// By default refuses to overwrite existing bundles; pass
    /// `--init-localizations-force` to overwrite silently. Pass
    /// `--init-localizations-stdout` to print to stdout instead of
    /// writing (useful for piping or version control).
    ///
    /// Can be combined with `--init-options` to lay down a whole
    /// project skeleton in one call.
    #[arg(
        long = "init-localizations",
        value_name = "DIR",
        num_args = 0..=1,
        default_missing_value = "localisations",
        conflicts_with = "jar",
        conflicts_with = "input",
        help_heading = "CLI tooling"
    )]
    pub init_localizations: Option<String>,

    /// Scaffold an extra `snug-localisations.<tag>.txt` translation
    /// template in the `--init-localizations` directory. Repeatable.
    /// The template carries every baseline key with the English value
    /// in place, ready to translate. Has no effect without
    /// `--init-localizations`.
    #[arg(
        long = "init-localizations-tag",
        value_name = "TAG",
        help_heading = "CLI tooling"
    )]
    pub init_localizations_tag: Vec<String>,

    /// Overwrite existing bundles in the `--init-localizations`
    /// directory instead of refusing. Has no effect without
    /// `--init-localizations`.
    #[arg(long = "init-localizations-force", help_heading = "CLI tooling")]
    pub init_localizations_force: bool,

    /// Print the `--init-localizations` templates to stdout instead
    /// of writing them to disk. Has no effect without
    /// `--init-localizations`.
    #[arg(long = "init-localizations-stdout", help_heading = "CLI tooling")]
    pub init_localizations_stdout: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Parses `snug <args...>`. The missing-input check lives in
    /// `main.rs`, not here — these tests are about clap's handling of
    /// `--download-jdk` on its own.
    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        let mut argv = vec!["snug"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv)
    }

    #[test]
    fn omitting_download_jdk_defaults_to_auto() {
        assert_eq!(
            parse(&["App.jar"]).unwrap().download_jdk,
            CliDownloadJdkMode::Auto,
            "auto is the portable default: run silently when a compatible \
             JVM is present, offer Temurin when one is not"
        );
    }

    #[test]
    fn download_jdk_accepts_a_space_separated_value() {
        // The point of dropping `require_equals`. This used to parse as
        // `auto`, with `force` falling through to the positional
        // `[JAR|DIR]` slot.
        assert_eq!(
            parse(&["App.jar", "--download-jdk", "force"])
                .unwrap()
                .download_jdk,
            CliDownloadJdkMode::Force
        );
    }

    #[test]
    fn download_jdk_accepts_an_equals_value() {
        assert_eq!(
            parse(&["App.jar", "--download-jdk=force"])
                .unwrap()
                .download_jdk,
            CliDownloadJdkMode::Force
        );
    }

    #[test]
    fn download_jdk_parses_every_mode() {
        for (text, expected) in [
            ("off", CliDownloadJdkMode::Off),
            ("auto", CliDownloadJdkMode::Auto),
            ("force", CliDownloadJdkMode::Force),
        ] {
            let arg = format!("--download-jdk={text}");
            assert_eq!(
                parse(&["App.jar", &arg]).unwrap().download_jdk,
                expected,
                "{arg}"
            );
        }
    }

    #[test]
    fn a_bare_download_jdk_is_rejected() {
        let err = parse(&["App.jar", "--download-jdk"]).unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("--download-jdk"),
            "the error should name the flag: {text}"
        );
    }

    #[test]
    fn an_unknown_download_jdk_mode_is_rejected() {
        let err = parse(&["App.jar", "--download-jdk", "sometimes"]).unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("--download-jdk"),
            "the error should name the flag: {text}"
        );
        assert!(
            text.contains("off") && text.contains("auto") && text.contains("force"),
            "the error should list the accepted modes: {text}"
        );
    }

    /// The forgotten-mode case: `--download-jdk App.jar`. `App.jar` is
    /// read as the mode, and the error names the flag — which is the
    /// whole improvement over the old shape, where this instead
    /// reported `unexpected argument`.
    #[test]
    fn a_forgotten_mode_is_reported_against_the_flag() {
        let err = parse(&["--download-jdk", "App.jar"]).unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("--download-jdk"),
            "the error should name the flag rather than the JAR: {text}"
        );
    }
}
