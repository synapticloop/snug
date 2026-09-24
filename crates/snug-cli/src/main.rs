//! `snug` — wrap a Java fat JAR into a native Windows .exe launcher.
//!
//! Module declarations live in `lib.rs` so the integration tests can
//! import them. This binary is just the CLI entry point.

use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser};

use snug_cli::build::{build_exe, build_payload, output_path};
use snug_cli::cli::Cli;
use snug_cli::init_options;
use snug_cli::options_file;
use snug_format::SnugEmbedded;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    if let Err(err) = run() {
        eprintln!("snug: {err:?}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn run() -> Result<()> {
    let raw_args: Vec<String> = std::env::args().collect();

    // `--init-options` short-circuits everything: write the embedded
    // example, exit. We also skip options-file loading in this case —
    // there's no point reading a file we're about to write (or that
    // the user is about to overwrite). Use a dedicated mini-parser
    // here so clap handles every form (`--init-options`,
    // `--init-options=PATH`, `--init-options PATH`) consistently and
    // we don't depend on the full `Cli` validation passing.
    if raw_args
        .iter()
        .any(|a| a == "--init-options" || a.starts_with("--init-options="))
    {
        let parsed = InitOptionsCli::parse_from(&raw_args);
        let target = parsed.init_options.unwrap_or_default();
        return init_options::run(&target, parsed.init_options_force, parsed.init_options_stdout);
    }

    // Resolve the options file (explicit `--options <path>` or
    // CWD-relative `snug.options`) before clap sees anything. Tokens
    // from the file are prepended to the real CLI args so command-line
    // values win on conflict (clap's "last wins" semantics).
    let cwd = std::env::current_dir().context("reading current working directory")?;
    let options_path = options_file::resolve(&raw_args, &cwd);
    let file_tokens = match &options_path {
        Some(p) => options_file::load(p)
            .with_context(|| format!("loading options file {}", p.display()))?,
        None => Vec::new(),
    };

    let merged = options_file::merge(&raw_args, file_tokens);
    let cli = Cli::parse_from(merged);

    if let Some(p) = &options_path {
        eprintln!("snug: loaded options from {}", p.display());
    }

    // No input source supplied (positional `[JAR]` or `--input`):
    // print help + snug's own version, exit 0.
    // (clap's own `--help` is handled automatically by ArgAction::Help.)
    if cli.jar.is_none() && cli.input.is_none() {
        print_help_with_version();
        return Ok(());
    }

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
    println!("  output exE:  {}", output.display());
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
    println!("  resources:   editpe (icon + version + manifest if set)");
    println!("  stub bytes:  {}", snug_cli::stub::STUB_BYTES.len());
    println!(
        "  total exE:   {} bytes (approx)",
        snug_cli::stub::STUB_BYTES.len() + (embedded.payload_len as usize)
    );
    Ok(())
}

/// Render clap's help text followed by snug's version. Used when no
/// positional `<JAR>` was supplied so the user gets a useful intro
/// instead of a `MissingRequiredArgument` error.
fn print_help_with_version() {
    let mut cmd = Cli::command();
    let name = cmd.get_name().to_string();
    let help = cmd.render_help();
    print!("{help}");
    println!();
    println!("{name} {VERSION}");
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

/// Tiny `clap`-derived parser used to extract just the
/// `--init-options*` flags before the full `Cli` parses.
///
/// We keep it minimal because the full `Cli` struct enforces
/// `conflicts_with = "jar"` / `conflicts_with = "input"` on
/// `--init-options`, and we want the flag to work without a JAR
/// (the whole point is bootstrapping a `snug.options` from scratch).
/// Validating the rest of the surface in this early-exit branch
/// would reject legitimate invocations.
///
/// ## Strictness
///
/// By design, `--init-options` is mutually exclusive with **every**
/// other CLI flag — the whole point of the mode is "write the
/// example, exit, do nothing else". The enforcement is delegated to
/// clap's default "no unknown arguments" behaviour: `InitOptionsCli`
/// declares only the three `--init-options*` flags, so any other
/// argument fails parsing. The `tests` module below locks this down
/// — if anyone later adds a fourth flag here without thinking
/// through the implications, the strict-mode test catches the
/// regression.
#[derive(Debug, clap::Parser)]
struct InitOptionsCli {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Pull the three init-options flags out of an arg vector,
    /// returning `None` if the arg vector contains anything other
    /// than the recognised flags. Used by the strict-mode tests
    /// below — they're really asserting on `Err(_)`, but spelling
    /// that out as a helper makes the intent obvious.
    fn parse_or_reject(args: &[&str]) -> Result<InitOptionsCli, clap::Error> {
        // `parse_from` prepends argv[0]; we use a fixed binary
        // name to keep error messages stable across hosts.
        let mut argv = vec!["snug"];
        argv.extend_from_slice(args);
        InitOptionsCli::try_parse_from(argv)
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
