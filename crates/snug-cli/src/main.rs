//! `snug` — wrap a Java fat JAR into a native Windows .exe launcher.
//!
//! Module declarations live in `lib.rs` so the integration tests can
//! import them. This binary is just the CLI entry point.

use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;

use snug_cli::build::{
    build_exe, build_payload, guard_output_collision, output_path, wants_app_bundle,
};
use snug_cli::cli::Cli;
use snug_cli::{init_localizations, init_options};
use snug_cli::options_file;
use snug_format::SnugEmbedded;

fn main() -> ExitCode {
    if let Err(err) = run() {
        eprintln!("snug: {err:?}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn run() -> Result<()> {
    let raw_args: Vec<String> = std::env::args().collect();

    // The `--init-*` family short-circuits everything: write the
    // starter files, exit. We also skip options-file loading in this
    // case — there's no point reading a file we're about to write (or
    // that the user is about to overwrite). Use a dedicated
    // mini-parser here so clap handles every form (`--init-options`,
    // `--init-options=PATH`, `--init-options PATH`) consistently and
    // we don't depend on the full `Cli` validation passing.
    //
    // Both init modes can be combined: `snug --init-options
    // --init-localizations` lays down a `snug.options` *and* a starter
    // `localisations/` directory in one call, which is the whole
    // "start a new project from nothing" gesture.
    if raw_args.iter().any(|a| is_init_flag(a)) {
        let parsed = InitCli::parse_from(&raw_args);
        if let Some(target) = parsed.init_options {
            init_options::run(
                &target,
                parsed.init_options_force,
                parsed.init_options_stdout,
            )?;
        }
        if let Some(dir) = parsed.init_localizations {
            init_localizations::run(
                &dir,
                &parsed.init_localizations_tag,
                parsed.init_localizations_force,
                parsed.init_localizations_stdout,
            )?;
        }
        return Ok(());
    }

    // Resolve the options files before clap sees anything: an explicit
    // `--options <path>` (which stands alone), else `snug.options` and
    // the host's `snug.<os>.options`, lowest priority first. Tokens are
    // layered into the real CLI args so that a value on the command line
    // wins over an OS-specific file, which in turn wins over the generic
    // one. A missing file is not an error and not a warning — the OS
    // file is opt-in, so looking for one and finding nothing must stay
    // silent.
    let cwd = std::env::current_dir().context("reading current working directory")?;
    let exe_dir = options_file::current_exe_dir();
    let options_paths =
        options_file::resolve_all(&raw_args, &cwd, exe_dir.as_deref(), std::env::consts::OS);
    let mut file_layers = Vec::with_capacity(options_paths.len());
    for p in &options_paths {
        file_layers.push(
            options_file::load(p)
                .with_context(|| format!("loading options file {}", p.display()))?,
        );
    }

    let merged = options_file::merge(&raw_args, file_layers);
    let cli = Cli::parse_from(merged);

    for p in &options_paths {
        eprintln!("snug: loaded options from {}", p.display());
    }

    // `--find-main` is a standalone diagnostic: scan, print, exit. It
    // runs before the no-input help branch so `--find-main` with no
    // JAR gets a specific error rather than the full help text, and
    // well before `build_payload` so it doesn't read, hash, or embed
    // the JAR — on a 200 MB fat JAR that would cost far more than the
    // scan itself.
    if cli.find_main {
        return snug_cli::find_main::run(&cli);
    }

    // `--init-localizations-tag` carries a value, so unlike the
    // boolean modifiers it can't quietly do nothing: reaching here
    // means the user passed a locale without asking to scaffold one,
    // and a build would swallow the value without a word. Say so.
    if !cli.init_localizations_tag.is_empty() && cli.init_localizations.is_none() {
        anyhow::bail!(
            "--init-localizations-tag has no effect without --init-localizations\n\
             hint: snug --init-localizations --init-localizations-tag {}",
            cli.init_localizations_tag.join(" --init-localizations-tag ")
        );
    }

    // No input source supplied (positional `[JAR]` or `--input`):
    // say so, and exit non-zero. This used to print the help text and
    // exit 0, which is the wrong answer for a user whose `snug.options`
    // already sets every *other* value: they have made a build request,
    // and a help dump replies "here are some flags" instead of "you
    // left out the one value a file cannot know for me" — which reads
    // as the file having been ignored. `--help` remains one flag away
    // for anyone who does want the full usage.
    if cli.jar.is_none() && cli.input.is_none() {
        anyhow::bail!("no input JAR{}", missing_input_detail(&options_paths));
    }

    // Before the (potentially slow) payload build, refuse an output that
    // aliases an input. `--dry-run` and `--emit-payload` never reach the
    // write, but they should still report the mistake rather than let the
    // user believe the command line is sound.
    guard_output_collision(&cli)?;

    let payload = build_payload(&cli).context("building snug payload")?;
    let embedded = SnugEmbedded::new(payload);

    if cli.emit_payload {
        use std::io::Write;
        let bytes = encode_for_emit(&embedded);
        std::io::stdout()
            .write_all(&bytes)
            .context("writing payload to stdout")?;
        return Ok(());
    }

    if cli.dry_run {
        return dry_run(&cli, &embedded);
    }

    // `snug app.jar -o MyApp.app` builds a macOS bundle; `-o MyApp.exe`
    // builds the Windows one. Branching on the output extension is what
    // keeps the documented command identical on both platforms — the docs
    // never have to name a flag that exists on only one of them.
    if wants_app_bundle(&cli) {
        #[cfg(target_os = "macos")]
        {
            let output = snug_cli::macos_bundle::build_app(&cli, &embedded.payload)
                .context("building the macOS .app bundle")?;
            eprintln!("snug: built {}", output.display());
            return Ok(());
        }

        // Refused rather than quietly written. A `.app` is a *directory*
        // holding a Mach-O launcher plus its payload; a Windows snug has
        // no macOS launcher embedded, so the request cannot be honoured,
        // and the alternative — a flat PE named `Foo.app` — is precisely
        // the trap this check exists to prevent.
        #[cfg(not(target_os = "macos"))]
        anyhow::bail!(
            "-o {} names a macOS application, which is a *directory* \
             containing a macOS launcher.\n\
             snug on this platform embeds a Windows launcher, so a .app \
             cannot be built here.\n\
             hint: pass -o MyApp.exe for a Windows executable, or use the \
             macOS snug.",
            output_path(&cli).display()
        );
    }

    let output = build_exe(&cli, &embedded.payload)
        .context("building the Windows EXE")?;
    eprintln!("snug: built {}", output.display());
    Ok(())
}

fn encode_for_emit(embedded: &SnugEmbedded) -> Vec<u8> {
    snug_format::encode(embedded).expect("encoding never fails for in-memory payload")
}

fn dry_run(cli: &Cli, embedded: &SnugEmbedded) -> Result<()> {
    let output = output_path(cli);
    let payload = &embedded.payload;
    println!("snug: dry-run — no files written");
    println!(
        "  jar:         {}",
        cli.jar
            .as_ref()
            .or(cli.input.as_ref())
            .map_or("(none)".into(), |p| p.display().to_string())
    );
    println!("  output:      {}", output.display());
    println!(
        "  main-class:  {}",
        payload.config.main_class.as_deref().unwrap_or("(from manifest)")
    );
    println!("  min-java:    {}", payload.config.min_java);
    println!("  jvm args:    {}", payload.config.jvm_args.len());
    println!(
        "  icon:        {}",
        cli.icon
            .as_ref()
            .map_or("(none)".into(), |p| p.display().to_string())
    );
    println!(
        "  manifest:    {}",
        cli.manifest
            .as_ref()
            .map_or("(none)".into(), |p| p.display().to_string())
    );
    println!(
        "  splash:      {}",
        if payload.config.splash.is_some() { "yes" } else { "no" }
    );
    println!(
        "  jars:        {} (first sha256: {})",
        payload.jars.len(),
        hex_lower(&payload.jars[0].sha256)
    );
    println!(
        "  localizations: {} bundle(s) [{}]",
        payload.localizations.len(),
        payload
            .localizations
            .iter()
            .map(|b| b.tag.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    // Spelled per platform because it is: the `.exe` path stamps an icon,
    // a version resource and a manifest with `editpe`, while the `.app`
    // path converts the icon with `iconutil` and ad-hoc signs the bundle.
    // Printing "editpe" on a macOS dry-run would be a lie about what is
    // about to happen.
    #[cfg(target_os = "macos")]
    println!("  resources:   iconutil (icon as App.icns) + codesign --sign -");
    #[cfg(not(target_os = "macos"))]
    println!("  resources:   editpe (icon + version + manifest if set)");
    println!("  stub bytes:  {}", snug_cli::stub::STUB_BYTES.len());
    println!(
        "  total exE:   {} bytes (approx)",
        snug_cli::stub::STUB_BYTES.len() + (embedded.payload_len as usize)
    );
    Ok(())
}

/// The tail of the missing-input error, which depends on whether any
/// options file was actually read. Naming those files is the useful
/// part: it turns "snug ignored my `snug.options`" into a one-line
/// fix, and the paths are exactly the ones already announced on the
/// `loaded options from` lines above.
fn missing_input_detail(loaded: &[std::path::PathBuf]) -> String {
    const HINT: &str = "hint: snug <jar> -o <exe>, or set --input in snug.options \
                        (--help for the full list of options)";
    if loaded.is_empty() {
        return format!(" given\n{HINT}");
    }
    let names: Vec<String> = loaded.iter().map(|p| p.display().to_string()).collect();
    format!(
        ": {} loaded, but no --input in any of them\n{HINT}",
        names.join(", ")
    )
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Does this raw argv token select the early-exit init branch?
///
/// Matches the mode flags only, not their `--force` / `--stdout`
/// modifiers: `snug --init-options-force` alone is not an init
/// invocation, it's a build-mode flag that happens to be inert, and
/// it should fall through to the normal parse exactly as it did
/// before this branch existed.
fn is_init_flag(arg: &str) -> bool {
    arg == "--init-options"
        || arg.starts_with("--init-options=")
        || arg == "--init-localizations"
        || arg.starts_with("--init-localizations=")
}

/// Tiny `clap`-derived parser used to extract just the `--init-*` flags
/// before the full `Cli` parses.
///
/// We keep it minimal because the full `Cli` struct enforces
/// `conflicts_with = "jar"` / `conflicts_with = "input"` on
/// `--init-options`, and we want the flags to work without a JAR
/// (the whole point is bootstrapping a project from scratch).
/// Validating the rest of the surface in this early-exit branch
/// would reject legitimate invocations.
///
/// ## Strictness
///
/// By design, the `--init-*` family is mutually exclusive with **every**
/// other CLI flag — the point of the mode is "write the starter
/// files, exit, do nothing else". The only thing you may combine is
/// the init modes themselves. The enforcement is delegated to clap's
/// default "no unknown arguments" behaviour: [`InitCli`] declares only
/// the `--init-*` flags, so any other argument fails parsing. The
/// `tests` module below locks this down — if anyone later adds an
/// unrelated flag here without thinking through the implications, the
/// strict-mode tests catch the regression.
#[derive(Debug, clap::Parser)]
struct InitCli {
    /// Target path. Optional — defaults to `./snug.options` (the
    /// same path `options_file::resolve` reads on a normal
    /// invocation, so writing here means the next `snug` run will
    /// pick it up).
    #[arg(
        long = "init-options",
        value_name = "PATH",
        num_args = 0..=1,
        default_missing_value = init_options::DEFAULT_PATH,
    )]
    init_options: Option<String>,

    /// Overwrite the target if it already exists.
    #[arg(long = "init-options-force")]
    init_options_force: bool,

    /// Print to stdout instead of writing to disk.
    #[arg(long = "init-options-stdout")]
    init_options_stdout: bool,

    /// Target directory. Optional — defaults to `./localisations`
    /// relative to the *current working directory* (where snug was
    /// invoked from), not the executable's own directory. Created if
    /// missing.
    #[arg(
        long = "init-localizations",
        value_name = "DIR",
        num_args = 0..=1,
        default_missing_value = init_localizations::DEFAULT_DIR,
    )]
    init_localizations: Option<String>,

    /// Scaffold an extra `snug-localisations.<tag>.txt` translation
    /// template in the target directory. Repeatable; the English
    /// baseline is always written regardless.
    #[arg(long = "init-localizations-tag", value_name = "TAG")]
    init_localizations_tag: Vec<String>,

    /// Overwrite existing bundles in the `--init-localizations`
    /// target directory.
    #[arg(long = "init-localizations-force")]
    init_localizations_force: bool,

    /// Print the `--init-localizations` templates to stdout instead
    /// of writing them to disk.
    #[arg(long = "init-localizations-stdout")]
    init_localizations_stdout: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Pull the init flags out of an arg vector, returning `None` if
    /// the arg vector contains anything other than the recognised
    /// flags. Used by the strict-mode tests below — they're really
    /// asserting on `Err(_)`, but spelling that out as a helper makes
    /// the intent obvious.
    fn parse_or_reject(args: &[&str]) -> Result<InitCli, clap::Error> {
        // `parse_from` prepends argv[0]; we use a fixed binary
        // name to keep error messages stable across hosts.
        let mut argv = vec!["snug"];
        argv.extend_from_slice(args);
        InitCli::try_parse_from(argv)
    }

    #[test]
    fn is_init_flag_matches_modes_but_not_modifiers() {
        assert!(is_init_flag("--init-options"));
        assert!(is_init_flag("--init-options=cfg/build.options"));
        assert!(is_init_flag("--init-localizations"));
        assert!(is_init_flag("--init-localizations=i18n"));
        // Modifiers alone don't select the branch — they fall through
        // to the normal parse.
        assert!(!is_init_flag("--init-options-force"));
        assert!(!is_init_flag("--init-localizations-force"));
        assert!(!is_init_flag("--init-localizations-stdout"));
        assert!(!is_init_flag("--init-localizations-tag"));
        assert!(!is_init_flag("--init-options-extra"));
        assert!(!is_init_flag("MyApp.jar"));
    }

    #[test]
    fn accepts_bare_init_options() {
        let p = parse_or_reject(&["--init-options"]).unwrap();
        assert_eq!(p.init_options.as_deref(), Some(init_options::DEFAULT_PATH));
        assert!(!p.init_options_force);
        assert!(!p.init_options_stdout);
    }

    #[test]
    fn accepts_init_options_equals_path() {
        let p = parse_or_reject(&["--init-options=cfg/build.options"]).unwrap();
        assert_eq!(p.init_options.as_deref(), Some("cfg/build.options"));
    }

    #[test]
    fn accepts_init_options_space_path() {
        let p = parse_or_reject(&["--init-options", "cfg/build.options"]).unwrap();
        assert_eq!(p.init_options.as_deref(), Some("cfg/build.options"));
    }

    #[test]
    fn accepts_init_options_with_force_and_stdout() {
        let p = parse_or_reject(&[
            "--init-options=foo.options",
            "--init-options-force",
            "--init-options-stdout",
        ])
        .unwrap();
        assert_eq!(p.init_options.as_deref(), Some("foo.options"));
        assert!(p.init_options_force);
        assert!(p.init_options_stdout);
    }

    // ---- --init-localizations ----

    #[test]
    fn accepts_bare_init_localizations() {
        let p = parse_or_reject(&["--init-localizations"]).unwrap();
        assert_eq!(
            p.init_localizations.as_deref(),
            Some(init_localizations::DEFAULT_DIR)
        );
        assert!(p.init_localizations_tag.is_empty());
        assert!(!p.init_localizations_force);
        assert!(!p.init_localizations_stdout);
    }

    #[test]
    fn accepts_init_localizations_with_equals_and_space_forms() {
        let p = parse_or_reject(&["--init-localizations=i18n"]).unwrap();
        assert_eq!(p.init_localizations.as_deref(), Some("i18n"));
        let p = parse_or_reject(&["--init-localizations", "cfg/i18n"]).unwrap();
        assert_eq!(p.init_localizations.as_deref(), Some("cfg/i18n"));
    }

    #[test]
    fn accepts_repeated_init_localizations_tags() {
        let p = parse_or_reject(&[
            "--init-localizations",
            "--init-localizations-tag",
            "de",
            "--init-localizations-tag",
            "pt-BR",
        ])
        .unwrap();
        assert_eq!(p.init_localizations_tag, vec!["de", "pt-BR"]);
    }

    #[test]
    fn accepts_init_localizations_with_force_and_stdout() {
        let p = parse_or_reject(&[
            "--init-localizations",
            "--init-localizations-force",
            "--init-localizations-stdout",
        ])
        .unwrap();
        assert!(p.init_localizations_force);
        assert!(p.init_localizations_stdout);
    }

    #[test]
    fn accepts_both_init_modes_together() {
        // The "bootstrap a project in one call" gesture: options file
        // plus translation scaffolds, each with its own modifiers.
        let p = parse_or_reject(&[
            "--init-options",
            "--init-localizations",
            "--init-localizations-tag",
            "de",
        ])
        .unwrap();
        assert_eq!(p.init_options.as_deref(), Some(init_options::DEFAULT_PATH));
        assert_eq!(
            p.init_localizations.as_deref(),
            Some(init_localizations::DEFAULT_DIR)
        );
        assert_eq!(p.init_localizations_tag, vec!["de"]);
    }

    #[test]
    fn init_localizations_tag_without_mode_is_rejected_loudly() {
        // The early-exit branch is not entered (no mode flag), so this
        // reaches the full `Cli` — where the tag is declared for help
        // purposes. A build must not silently swallow the value, so
        // `main` bails with a hint instead. Parsing alone still
        // succeeds; the check is a runtime guard, not a clap rule.
        let p = parse_or_reject(&["--init-localizations-tag", "de"]);
        // The mini-parser accepts it (it declares the flag), so this
        // assertion documents that the *branch* is what excludes it.
        assert_eq!(p.unwrap().init_localizations_tag, vec!["de"]);
        assert!(!is_init_flag("--init-localizations-tag"));
    }

    // ---- strict-mode: any non-init-options flag is rejected ----

    /// Helper that asserts a particular argument vector is rejected
    /// by `InitOptionsCli`. The matching error message should also
    /// mention the offending arg so the user can see what they did
    /// wrong — that's the part that's easy to lose in a future
    /// refactor (e.g. turning off clap's strict mode).
    fn assert_rejected(args: &[&str], must_contain: &str) {
        let err = parse_or_reject(args).expect_err(&format!(
            "expected clap to reject {args:?} but it parsed cleanly"
        ));
        let msg = err.to_string();
        assert!(
            msg.contains(must_contain),
            "expected error to mention `{must_contain}` for {args:?}, got: {msg}"
        );
    }

    #[test]
    fn rejects_build_flag_name() {
        assert_rejected(&["--init-options", "--name", "My App"], "--name");
    }

    #[test]
    fn rejects_build_flag_company() {
        assert_rejected(&["--init-options", "--company", "Acme"], "--company");
    }

    #[test]
    fn rejects_build_flag_output() {
        assert_rejected(&["--init-options", "--output", "App.exe"], "--output");
    }

    #[test]
    fn rejects_build_flag_min_java() {
        assert_rejected(&["--init-options", "--min-java", "21"], "--min-java");
    }

    #[test]
    fn rejects_build_flag_main_class() {
        assert_rejected(
            &["--init-options", "--main-class", "com.example.Main"],
            "--main-class",
        );
    }

    #[test]
    fn rejects_build_flag_jvm_arg() {
        assert_rejected(
            &["--init-options", "--jvm-arg", "-Xmx2g"],
            "--jvm-arg",
        );
    }

    #[test]
    fn rejects_build_flag_localization() {
        assert_rejected(
            &["--init-options", "--localization", "de.txt"],
            "--localization",
        );
    }

    #[test]
    fn rejects_build_flag_download_jdk() {
        assert_rejected(
            &["--init-options", "--download-jdk=auto"],
            "--download-jdk",
        );
    }

    #[test]
    fn rejects_build_flag_dry_run() {
        assert_rejected(&["--init-options", "--dry-run"], "--dry-run");
    }

    #[test]
    fn rejects_build_flag_emit_payload() {
        assert_rejected(&["--init-options", "--emit-payload"], "--emit-payload");
    }

    #[test]
    fn rejects_positional_jar_argument() {
        // `snug MyApp.jar --init-options` — positional is fine on
        // its own, but combined with --init-options the user is
        // trying to do two things at once. Reject.
        assert_rejected(&["MyApp.jar", "--init-options"], "MyApp.jar");
    }

    #[test]
    fn rejects_positional_input_argument() {
        assert_rejected(&["--input", "build/", "--init-options"], "--input");
    }

    #[test]
    fn rejects_positional_argument_after_init_options() {
        // `snug --init-options MyApp.jar` looks like "set target to
        // MyApp.jar", which IS legal under our strict mode (it's
        // the documented target-path form). But `snug
        // --init-options foo -- bar` would be ambiguous, and
        // `snug --init-options App.jar extra` would have a stray
        // positional. Cover the stray-positional case explicitly.
        assert_rejected(
            &["--init-options", "App.jar", "extra-positional"],
            "extra-positional",
        );
    }
}
