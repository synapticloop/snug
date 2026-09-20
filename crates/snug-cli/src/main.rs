//! `snug` — wrap a Java fat JAR into a native Windows .exe launcher.
//!
//! Module declarations live in `lib.rs` so the integration tests can
//! import them. This binary is just the CLI entry point.

use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser};

use snug_cli::build::{build_exe, build_payload, output_path};
use snug_cli::cli::Cli;
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

    // No JAR supplied: print help + snug's own version, exit 0.
    // (clap's own `--help` is handled automatically by ArgAction::Help.)
    if cli.jar.is_none() {
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
        "  splash:      {}",
        if payload.config.splash.is_some() { "yes" } else { "no" }
    );
    println!("  jar sha256:  {}", hex_lower(&payload.jar.sha256));
    println!(
        "  rcedit:      {}",
        if cli.no_rcedit {
            "disabled".to_string()
        } else {
            cli.rcedit
                .as_ref()
                .map_or("(default rcedit on PATH)".into(), |p| p.display().to_string())
        }
    );
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
