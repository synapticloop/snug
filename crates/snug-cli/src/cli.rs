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
    /// No download flow - bail if no JVM is found.
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
/// cache would scatter - or fail to be found at all.
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

// Help layout:
// - `wrap_help` is enabled in the workspace Cargo.toml.
// - Raw strings preserve explicit newlines and indentation in the overview.
// - Keep prose paragraphs on ONE source line and examples on separate lines.
// - `verbatim_doc_comment` preserves indentation within individual arg help.
// - clap generates the Usage line; don't duplicate it in long_about.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "snug",
    about = "Wrap a Java JAR into a native Windows .exe launcher",
    max_term_width = 80,
    long_about = r#"
        .-----.-----.--.--.-----.
        |__ --|     |  |  |  _  |
        |_____|__|__|_____|___  |
              ... .-..    |_____|

             ~ ~ ~ * ~ ~ ~

Wrap Java applications into native Windows executables.

Snug turns a Java JAR, or a directory of JARs, into a Windows .exe file that you can share with others.

The launcher runs your application directly through the Java Virtual Machine (JVM), appearing under your application's name in Windows Task Manager instead of javaw.exe.

If a compatible Java runtime is not installed, Snug can offer to download one automatically.

GETTING STARTED

The simplest way to build an application is:

    snug.exe MyApp.jar

This creates MyApp.exe alongside your JAR file.

You can also specify an input directory containing JARs:

    snug.exe --input my-jars

Snug automatically determines the application name from the input file or directory.

For an application you intend to distribute, consider adding:

    --name         Your application's display name
    --company      Your company or organisation
    --version      Your application's version number
    --description  A short description of your application
    --icon         An application icon (PNG or ICO)

USAGE:


  snug.exe [OPTIONS] [JAR|DIR]

    snug.exe MyApp.jar

  Run
    snug.exe --help
  or
    snug.exe -h
  for command-line help.

"#,
    after_long_help = r#"EXAMPLES

    snug.exe MyApp.jar
    snug.exe MyApp.jar -o MyProgram.exe
    snug.exe MyApp.jar --name "My App" --company "My Company"
    snug.exe MyApp.jar --version 1.2.3 --min-java 25 --icon app.png
    snug.exe MyApp.jar --dry-run
    snug.exe MyApp.jar --emit-payload > payload.bin
    snug.exe --init-options
    snug.exe --init-localisations --init-localisations-tag de
    snug.exe --list-tokens

Each `--localisation` file is embedded alongside the built-in English baseline. At runtime, the user's Windows UI language selects the appropriate bundle, falling back to English.

Translation keys and placeholders must match the English baseline. Snug warns about mismatches when building. Use `snug.exe --list-tokens` to inspect the placeholder contract, or `snug.exe --init-localisations` to create starter files.
"#,
    // `--version` belongs to the packaged application, not the builder.
    disable_version_flag = true,
    version = env!("CARGO_PKG_VERSION"),
)]
pub struct Cli {
    /// Input JAR file or directory containing JAR files.
    ///
    /// A single JAR should be a runnable fat JAR containing the application and its required dependencies.
    ///
    /// For a directory, Snug includes all .jar files directly inside it, sorted by filename. Subdirectories are not searched.
    ///
    /// You can also supply the input as a positional argument:
    ///
    ///     snug.exe MyApp.jar
    ///
    /// Do not use --input and a positional input together on the same command line.
    #[arg(
        long = "input",
        value_name = "JAR|DIR",
        conflicts_with = "jar",
        help_heading = "INPUT / OUTPUT",
        verbatim_doc_comment
    )]
    pub input: Option<PathBuf>,

    /// Path and filename of the generated executable.
    ///
    /// By default, the output is created alongside the input, using the input filename or directory name.
    ///
    /// Example:
    ///
    ///     snug.exe MyApp.jar -o MyProgram.exe
    #[arg(
        short = 'o',
        long = "output",
        value_name = "EXE",
        help_heading = "INPUT / OUTPUT",
        verbatim_doc_comment
    )]
    pub output: Option<PathBuf>,

    /// Input JAR file or directory containing JAR files.
    ///
    /// A single JAR should be a runnable fat JAR containing the application and its required dependencies.
    ///
    /// For a directory, Snug includes all .jar files directly inside it, sorted by filename. Subdirectories are not searched.
    ///
    /// You can also supply the input as a positional argument:
    ///
    ///     snug.exe MyApp.jar
    ///
    /// Equivalent to --input <JAR|DIR>. Do not supply both forms on the same command line.
    #[arg(
        value_name = "JAR|DIR",
        help_heading = "INPUT / OUTPUT",
        verbatim_doc_comment
    )]
    pub jar: Option<PathBuf>,

    /// Display name of your application.
    ///
    /// Used in Windows application information and the runtime cache directory.
    ///
    /// Defaults to the input JAR filename without .jar, or the input directory name.
    ///
    /// Example:
    ///
    ///     --name "My Application"
    #[arg(
        long = "name",
        value_name = "NAME",
        help_heading = "APPLICATION DETAILS",
        verbatim_doc_comment
    )]
    pub name: Option<String>,

    /// Company or organisation responsible for the application.
    ///
    /// Appears in the Windows executable's Details information and helps keep application cache files separate.
    ///
    /// Defaults to the resolved application name.
    ///
    /// Example:
    ///
    ///     --company "My Company"
    #[arg(
        long = "company",
        value_name = "COMPANY",
        help_heading = "APPLICATION DETAILS",
        verbatim_doc_comment
    )]
    pub company: Option<String>,

    /// Version number of your application.
    ///
    /// Appears in Windows executable properties.
    ///
    /// Accepts one to four dot-separated numbers, with each number between 0 and 65535. Missing components default to zero.
    ///
    /// Examples:
    ///
    ///     --version 1
    ///     --version 1.2.3
    ///     --version 1.2.3.4
    #[arg(
        long = "version",
        value_name = "VERSION",
        value_parser = crate::resources::validate_app_version,
        help_heading = "APPLICATION DETAILS",
        verbatim_doc_comment
    )]
    pub version: Option<String>,

    /// Short description of your application.
    ///
    /// Appears in the Windows executable's Details information.
    ///
    /// Example:
    ///
    ///     --description "Customer management application"
    #[arg(
        long = "description",
        value_name = "TEXT",
        help_heading = "APPLICATION DETAILS",
        verbatim_doc_comment
    )]
    pub description: Option<String>,

    /// Copyright information embedded in the executable.
    ///
    /// Appears in the Windows executable's Details information.
    ///
    /// Example:
    ///
    ///     --copyright "Copyright 2026 My Company"
    #[arg(
        long = "copyright",
        value_name = "TEXT",
        help_heading = "APPLICATION DETAILS",
        verbatim_doc_comment
    )]
    pub copyright: Option<String>,

    /// Website where users can check for a newer version.
    ///
    /// When supplied, the launcher displays a clickable "Check for a newer version" link in its error dialog.
    ///
    /// The link opens in the user's default browser. Snug does not automatically check for updates.
    ///
    /// Example:
    ///
    ///     --update-url https://example.com/downloads
    ///
    /// [default: no update link]
    #[arg(
        long = "update-url",
        value_name = "URL",
        help_heading = "APPLICATION DETAILS",
        verbatim_doc_comment
    )]
    pub update_url: Option<String>,

    /// Minimum Java major version required by your application.
    ///
    /// The launcher searches for a compatible Java runtime when the application starts.
    ///
    /// Example:
    ///
    ///     --min-java 21
    #[arg(
        long = "min-java",
        value_name = "N",
        default_value_t = 25,
        help_heading = "JAVA RUNTIME",
        verbatim_doc_comment
    )]
    pub min_java: u16,

    /// Java class to start when the application launches.
    ///
    /// Overrides the Main-Class declared in the JAR manifest.
    ///
    /// Useful when a JAR contains several possible entry points, or when packaging multiple JARs.
    ///
    /// Example:
    ///
    ///     --main-class com.example.Main
    #[arg(
        long = "main-class",
        value_name = "CLASS",
        help_heading = "JAVA RUNTIME",
        verbatim_doc_comment
    )]
    pub main_class: Option<String>,

    /// Additional option passed to the Java Virtual Machine.
    ///
    /// Can be repeated to supply multiple options.
    ///
    /// Examples:
    ///
    ///     --jvm-arg=-Xms256m
    ///     --jvm-arg=-Xmx2g
    ///     --jvm-arg=-Dfile.encoding=UTF-8
    #[arg(
        long = "jvm-arg",
        value_name = "OPTION",
        allow_hyphen_values = true,
        help_heading = "JAVA RUNTIME",
        verbatim_doc_comment
    )]
    pub jvm_args: Vec<String>,

    /// Icon to embed in the Windows executable.
    ///
    /// Accepts PNG or ICO files.
    ///
    /// The icon appears in File Explorer and wherever Windows displays the application's executable icon.
    ///
    /// Example:
    ///
    ///     --icon app.png
    #[arg(
        long = "icon",
        value_name = "PNG/ICO",
        help_heading = "WINDOWS APPEARANCE",
        verbatim_doc_comment
    )]
    pub icon: Option<PathBuf>,

    /// Custom Windows application manifest.
    ///
    /// Use this for advanced Windows settings, including DPI awareness and administrator permissions.
    ///
    /// Example:
    ///
    ///     --manifest app.manifest
    #[arg(
        long = "manifest",
        value_name = "XML",
        help_heading = "WINDOWS APPEARANCE",
        verbatim_doc_comment
    )]
    pub manifest: Option<PathBuf>,

    /// PNG image displayed while the application starts.
    ///
    /// Displayed at its original size, centred on the primary monitor. The image is not resized.
    ///
    /// Recommended size: 480x270 to 640x360 pixels.
    ///
    /// Example:
    ///
    ///     --splash splash.png
    #[arg(
        long = "splash",
        value_name = "PNG",
        help_heading = "WINDOWS APPEARANCE",
        verbatim_doc_comment
    )]
    pub splash: Option<PathBuf>,

    /// Minimum time to display the splash screen, in milliseconds.
    ///
    /// The splash closes after both the minimum duration has elapsed and the JVM has signalled readiness.
    ///
    /// Example:
    ///
    ///     --splash-ms 2000
    #[arg(
        long = "splash-ms",
        value_name = "MS",
        default_value_t = 1_500,
        help_heading = "WINDOWS APPEARANCE",
        verbatim_doc_comment
    )]
    pub splash_ms: u32,

    /// Recommended maximum dimensions for the splash image.
    ///
    /// Snug warns during the build if the image exceeds these dimensions. The image is still embedded and displayed at its original size.
    ///
    /// Use off, none, or unlimited to disable the warning.
    ///
    /// Examples:
    ///
    ///     --splash-max 800x600
    ///     --splash-max off
    #[arg(
        long = "splash-max",
        value_name = "WxH|off",
        default_value = "640x360",
        help_heading = "WINDOWS APPEARANCE",
        verbatim_doc_comment
    )]
    pub splash_max: String,

    /// Controls downloading Java when the application starts.
    ///
    /// Modes:
    ///
    ///     auto   Offer to download if no compatible Java runtime is found.
    ///     force  Always offer a download if a cached Snug version is not available.
    ///     off    Disable downloading; Java must already be installed.
    ///
    /// The download is verified using SHA-256 and installed in a per-user directory. The mode is required when this flag is used.
    ///
    /// Examples:
    ///
    ///     --download-jdk auto
    ///     --download-jdk=off
    #[arg(
        long = "download-jdk",
        value_enum,
        default_value_t = CliDownloadJdkMode::Auto,
        num_args = 1,
        help_heading = "RUNTIME AND BUILD BEHAVIOUR",
        verbatim_doc_comment
    )]
    pub download_jdk: CliDownloadJdkMode,

    /// Allow more than one copy of the application to run.
    ///
    /// By default, opening the application a second time brings the existing window to the front instead of starting another JVM.
    ///
    /// Use this option for applications that need concurrent copies, such as servers or tools that run independently.
    ///
    /// [default: one instance per application]
    #[arg(
        long = "allow-multiple-instances",
        help_heading = "RUNTIME AND BUILD BEHAVIOUR",
        verbatim_doc_comment
    )]
    pub allow_multiple_instances: bool,

    /// Choose where the launcher stores extracted JAR files.
    ///
    /// By default, Windows uses:
    ///
    ///     %LOCALAPPDATA%\snug\<company>\<app>\
    ///
    /// Use an absolute path. The directory need not exist when building; the launcher creates it when the application runs.
    ///
    /// Useful for storing large application files on another drive.
    #[arg(
        long = "cache-dir",
        value_name = "DIR",
        value_parser = validate_cache_dir,
        help_heading = "RUNTIME AND BUILD BEHAVIOUR",
        verbatim_doc_comment
    )]
    pub cache_dir: Option<PathBuf>,

    /// Embed translated messages for the native launcher.
    ///
    /// Accepts a file or a directory and can be repeated. Files must be named snug-localisations.<tag>.txt (for example, de or pt-BR). Directories are searched at the top level only, and every file in them must follow this naming convention.
    ///
    /// The launcher uses the user's Windows language, falling back to the built-in English messages. English is always included. Snug warns about missing keys or mismatched placeholders.
    ///
    /// Examples:
    ///
    ///     --localisation snug-localisations.de.txt
    ///     --localisation localisations
    ///
    /// See --init-localisations and --list-tokens.
    #[arg(
        long = "localisation",
        value_name = "TXT|DIR",
        value_parser = crate::localization::validate_localization_path,
        help_heading = "RUNTIME AND BUILD BEHAVIOUR",
        verbatim_doc_comment
    )]
    pub localizations: Vec<std::path::PathBuf>,

    /// Write the encoded application payload to standard output instead of creating an executable.
    ///
    /// For advanced tooling or inspecting the payload format.
    ///
    /// Example:
    ///
    ///     snug.exe MyApp.jar --emit-payload > payload.bin
    #[arg(
        long = "emit-payload",
        conflicts_with = "dry_run",
        help_heading = "RUNTIME AND BUILD BEHAVIOUR",
        verbatim_doc_comment
    )]
    pub emit_payload: bool,

    /// Validate the inputs and show what would be built, without creating an executable or writing build output.
    ///
    /// The build log is still written, because the diagnostics are the point of a dry run.
    ///
    /// Example:
    ///
    ///     snug.exe MyApp.jar --dry-run
    #[arg(
        long = "dry-run",
        conflicts_with = "emit_payload",
        help_heading = "RUNTIME AND BUILD BEHAVIOUR",
        verbatim_doc_comment
    )]
    pub dry_run: bool,

    /// Write a build log to this path.
    ///
    /// Every build writes a log. By default it is snug.build.log in the current working directory, and each build replaces the log from the previous one.
    ///
    /// If the path names an existing directory, the log is written as snug.build.log inside it. Any other path is used as the file name.
    ///
    /// The log records the options the build actually used and where each one came from, every diagnostic raised along the way, and where the output was written.
    ///
    /// Example:
    ///
    /// ```text
    /// snug.exe MyApp.jar --build-log build.log
    /// ```
    #[arg(
        long = "build-log",
        value_name = "PATH",
        help_heading = "RUNTIME AND BUILD BEHAVIOUR",
        verbatim_doc_comment
    )]
    pub build_log: Option<PathBuf>,

    /// List classes in the input JARs that declare a Java main method.
    ///
    /// Marks the configured entry point when it is found. Useful for choosing or checking --main-class. JavaFX applications can have valid entry points without declaring a main method.
    ///
    /// Diagnostic only: does not create an executable.
    ///
    /// Example:
    ///
    ///     snug.exe MyApp.jar --find-main
    #[arg(
        long = "find-main",
        help_heading = "RUNTIME AND BUILD BEHAVIOUR",
        verbatim_doc_comment
    )]
    pub find_main: bool,

    /// List the built-in English launcher message keys and the replacement placeholders used by each message.
    ///
    /// When translating, preserve placeholders such as {version} and {size_mb} in the corresponding translated message.
    ///
    /// No input JAR is required and no files are created.
    ///
    /// Example:
    ///
    ///     snug.exe --list-tokens
    #[arg(
        long = "list-tokens",
        help_heading = "CLI TOOLING",
        verbatim_doc_comment
    )]
    pub list_tokens: bool,

    /// Display Snug's own version number and exit.
    ///
    /// Unlike --version, this does not set the version of the application being packaged.
    ///
    /// Example:
    ///
    ///     snug.exe --snug-version
    ///
    /// [default: 0.0.0.0]
    #[arg(
        long = "snug-version",
        action = clap::ArgAction::Version,
        help_heading = "CLI TOOLING",
        verbatim_doc_comment
    )]
    pub snug_version: (),

    /// Use a specific Snug options file instead of automatic lookup.
    ///
    /// Without this flag, Snug reads snug.options followed by the host-specific file (such as snug.windows.options). Both files are searched for in the current working directory only.
    ///
    /// Priority, highest first:
    ///
    ///     Command line > host-specific options > general options
    ///
    /// Each line contains one command-line option; lines beginning with # are comments. A named --options file is used alone.
    ///
    /// Example:
    ///
    ///     --options config/build.options
    #[arg(
        long = "options",
        value_name = "PATH",
        help_heading = "CLI TOOLING",
        verbatim_doc_comment
    )]
    pub options: Option<PathBuf>,

    /// Create a starter snug.options file containing examples of available build options, then exit without building.
    ///
    /// Defaults to ./snug.options. Existing files are not overwritten. Use --init-options-force to overwrite or --init-options-stdout to print the template instead of writing a file.
    ///
    /// Examples:
    ///
    ///     snug.exe --init-options
    ///     snug.exe --init-options=cfg/build.options
    ///
    /// No input JAR is required.
    #[arg(
        long = "init-options",
        value_name = "PATH",
        num_args = 0..=1,
        default_missing_value = "snug.options",
        conflicts_with = "jar",
        conflicts_with = "input",
        help_heading = "CLI TOOLING",
        verbatim_doc_comment
    )]
    pub init_options: Option<String>,

    /// Allow --init-options to overwrite an existing file.
    ///
    /// Has no effect without --init-options.
    #[arg(
        long = "init-options-force",
        help_heading = "CLI TOOLING",
        verbatim_doc_comment
    )]
    pub init_options_force: bool,

    /// Print the starter options template to standard output instead of writing it to disk.
    ///
    /// Has no effect without --init-options.
    #[arg(
        long = "init-options-stdout",
        help_heading = "CLI TOOLING",
        verbatim_doc_comment
    )]
    pub init_options_stdout: bool,

    /// Create starter launcher localisation files, then exit.
    ///
    /// Defaults to a ./localisations directory containing the English template. Add languages with --init-localisations-tag. The generated directory can be passed to --localisation.
    ///
    /// Existing files are not overwritten unless --init-localisations-force is supplied. Use --init-localisations-stdout to print templates instead.
    ///
    /// Example:
    ///
    ///     snug.exe --init-localisations
    ///
    /// Can be combined with --init-options.
    #[arg(
        long = "init-localisations",
        value_name = "DIR",
        num_args = 0..=1,
        default_missing_value = "localisations",
        conflicts_with = "jar",
        conflicts_with = "input",
        help_heading = "CLI TOOLING",
        verbatim_doc_comment
    )]
    pub init_localizations: Option<String>,

    /// Add another language to the starter localisation files.
    ///
    /// Repeatable. Each template initially contains English messages, ready to translate. Use language tags such as de, fr or pt-BR.
    ///
    /// Example:
    ///
    ///     --init-localisations-tag de
    ///
    /// Has no effect without --init-localisations.
    #[arg(
        long = "init-localisations-tag",
        value_name = "TAG",
        help_heading = "CLI TOOLING",
        verbatim_doc_comment
    )]
    pub init_localizations_tag: Vec<String>,

    /// Allow --init-localisations to overwrite existing localisation templates.
    ///
    /// Has no effect without --init-localisations.
    #[arg(
        long = "init-localisations-force",
        help_heading = "CLI TOOLING",
        verbatim_doc_comment
    )]
    pub init_localizations_force: bool,

    /// Print localisation templates to standard output instead of writing files.
    ///
    /// Has no effect without --init-localisations.
    #[arg(
        long = "init-localisations-stdout",
        help_heading = "CLI TOOLING",
        verbatim_doc_comment
    )]
    pub init_localizations_stdout: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Parses `snug <args...>`. The missing-input check lives in
    /// `main.rs`, not here - these tests are about clap's handling of
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
    fn american_flag_spelling_is_rejected() {
        // `--localization` is gone, not an alias, and `snug-preview` is
        // held to the same rule (`the_american_spelling_is_rejected` in
        // `snug_preview`). Renaming the flag is exactly the kind of change
        // that gets "helpfully" re-aliased later for compatibility, which
        // is the papercut the single British spelling exists to end. This
        // fails if an alias is ever added back.
        for flag in [
            "--localization",
            "--init-localizations",
            "--init-localizations-tag",
            "--init-localizations-force",
            "--init-localizations-stdout",
        ] {
            let err = parse(&["App.jar", flag]).expect_err(&format!(
                "`{flag}` was accepted, but only the British spelling exists"
            ));
            assert!(
                err.to_string().contains("unexpected argument"),
                "`{flag}` should be rejected as unknown, got: {err}"
            );
        }
    }

    #[test]
    fn british_flag_spelling_is_accepted() {
        // The other half of the pair above: the rename has to actually
        // work, not just reject the old name. `ValueParser` runs on the
        // value, so a real, correctly-named file has to exist for this to
        // get past argument parsing.
        let bundle = crate::localization::expected_filename("de");
        let dir = std::env::temp_dir().join("snug-flag-spelling-test");
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join(bundle);
        std::fs::write(&path, "err.io = io\n").expect("write bundle");
        let path = path.to_string_lossy().to_string();

        let cli = parse(&["App.jar", "--localisation", &path]).unwrap();
        assert_eq!(cli.localizations, vec![std::path::PathBuf::from(&path)]);

        let _ = std::fs::remove_file(&path);
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
    /// read as the mode, and the error names the flag - which is the
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
