//! `snug` — wrap a Java fat JAR into a native Windows .exe launcher.
//!
//! Module declarations live in `lib.rs` so the integration tests can
//! import them. This binary is just the CLI entry point.
//!
//! Everything a build has to say goes to **stderr** and to the build log.
//! stdout carries only the `--emit-payload` binary, so a payload can be
//! piped without a diagnostic byte contaminating it.
//!
//! The two exceptions are `--find-main` and `--list-tokens`, which are
//! diagnostics rather than builds and whose documented contract is to
//! pipe to a file.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{CommandFactory, FromArgMatches, Parser};

use snug_cli::build::{
    build_exe, build_payload_with, guard_output_collision, output_path, wants_app_bundle,
};
use snug_cli::build_log::BuildLog;
use snug_cli::cli::Cli;
use snug_cli::effective;
use snug_cli::options_file;
use snug_cli::provenance::{Merged, Origin};
use snug_cli::report::{Reporter, codes};
use snug_cli::{init_localizations, init_options};
use snug_format::SnugEmbedded;

/// How a run ended.
enum Outcome {
    /// The build succeeded.
    Success,
    /// The build failed, and the reason has already been reported.
    Reported,
}

fn main() -> ExitCode {
    match run() {
        Ok(Outcome::Success) => ExitCode::SUCCESS,
        Ok(Outcome::Reported) => ExitCode::FAILURE,
        Err(err) => {
            // A failure that never reached the reporter: something the
            // build could not even describe. Print it and stop.
            eprintln!("snug: {err:?}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<Outcome> {
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
    // --init-localisations` lays down a `snug.options` *and* a starter
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
        return Ok(Outcome::Success);
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
    let options_paths = options_file::resolve_all(&raw_args, &cwd, std::env::consts::OS)
        .context("resolving which options files apply")?;
    // A file that does not parse is a build failure like any other: it
    // needs a code, and it needs to reach the log. The error is kept
    // rather than propagated so the log can be opened first and carry it —
    // otherwise a typo in `snug.options` loses the one record of what went
    // wrong, which is the whole point of writing one.
    let mut layers = Vec::with_capacity(options_paths.len());
    let mut layer_error: Option<anyhow::Error> = None;
    for p in &options_paths {
        match options_file::load_layer(p)
            .with_context(|| format!("loading options file {}", p.display()))
        {
            Ok(layer) => layers.push(layer),
            Err(err) => {
                // Keep the first failure: it is the one that stops the build,
                // and a later one would only bury it.
                if layer_error.is_none() {
                    layer_error = Some(err);
                }
            }
        }
    }

    // The merged argv comes from the files that parsed plus the command line.
    // The broken file contributes nothing: its tokens are unknown, so a
    // `--build-log` line in it cannot be honoured, and honouring one could
    // point the log at the broken file itself.
    let merged = options_file::merge(&raw_args, layers);

    // `--find-main` and `--list-tokens` are diagnostics, not builds, so
    // they write no build log and report no log location. The check has
    // to happen on the merged argv rather than on the parsed `Cli`,
    // because the log is opened *before* the full parse — that is what
    // lets a parse error land in the log instead of only on screen.
    let advisory = options_file::flags_present(&merged.argv, &["find-main", "list-tokens"]);

    // Parse once, before opening a file that may be truncated. We keep
    // the result until after the logger opens so clap errors still reach it.
    let parsed_matches = Cli::command().try_get_matches_from(&merged.argv);
    let early_output = parsed_matches
        .as_ref()
        .ok()
        .and_then(|matches| Cli::from_arg_matches(matches).ok())
        .map(|cli| output_path(&cli))
        .or_else(|| options_file::flag_value(&merged.argv, "output").map(PathBuf::from));

    let mut reporter = if advisory {
        Reporter::new(BuildLog::none())
    } else {
        open_build_log(&merged, &cwd, &options_paths, early_output.as_deref())?
    };

    // Parse fallibly, so an ordinary parse error is reported rather than
    // exiting with clap's own handler before anything was recorded.
    // Help and version are informational exits, not failures, and clap
    // renders them to stdout.
    let matches = match parsed_matches {
        Ok(matches) => matches,
        Err(err) => {
            report_parse_error(&mut reporter, &err);
            err.exit();
        }
    };
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(err) => {
            report_parse_error(&mut reporter, &err);
            err.exit();
        }
    };

    // A file that did not parse is reported like any other failure: with a
    // code, and in the log. The build never starts, so there is no effective
    // configuration to show — but the log's location still has to be
    // reported, or the record of the failure is a file the user was never
    // told about.
    if let Some(err) = layer_error {
        reporter.set_stage("options");
        reporter
            .error(codes::CFG_PARSE, "options", format!("{err:?}"))
            .hint("fix the syntax error in the options file and run snug again")
            .emit();
        finish(&mut reporter, &cli, None, false);
        return Ok(Outcome::Reported);
    }

    for p in &options_paths {
        reporter.write_line(&format!("snug: loaded options from {}", p.display()));
    }

    // `--find-main` is a standalone diagnostic: scan, print, exit. It
    // runs before the no-input help branch so `--find-main` with no
    // JAR gets a specific error rather than the full help text, and
    // well before `build_payload` so it doesn't read, hash, or embed
    // the JAR — on a 200 MB fat JAR that would cost far more than the
    // scan itself.
    if cli.find_main {
        return snug_cli::find_main::run(&cli).map(|()| Outcome::Success);
    }

    // `--list-tokens` is the same shape of diagnostic and is handled in
    // the same place: print, exit. It reads only the compiled-in
    // baseline, so it needs no JAR — which is why it has to sit above
    // the no-input bail rather than after it, or `snug --list-tokens`
    // would answer "no input JAR" to a question that needed none.
    if cli.list_tokens {
        return snug_cli::list_tokens::run().map(|()| Outcome::Success);
    }

    // `--init-localisations-tag` carries a value, so unlike the
    // boolean modifiers it can't quietly do nothing: reaching here
    // means the user passed a locale without asking to scaffold one,
    // and a build would swallow the value without a word. Say so.
    if !cli.init_localizations_tag.is_empty() && cli.init_localizations.is_none() {
        reporter
            .error(
                codes::CFG_INVALID,
                "options",
                "--init-localisations-tag has no effect without --init-localisations",
            )
            .hint(format!(
                "snug --init-localisations --init-localisations-tag {}",
                cli.init_localizations_tag
                    .join(" --init-localisations-tag ")
            ))
            .emit();
        return Ok(Outcome::Reported);
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
        reporter.set_stage("input");
        reporter
            .error(
                codes::CFG_INVALID,
                "input",
                format!("no input JAR{}", missing_input_detail(&options_paths)),
            )
            .hint(MISSING_INPUT_HINT)
            .emit();
        finish(&mut reporter, &cli, None, false);
        return Ok(Outcome::Reported);
    }

    // The effective options, and where each one came from. This is the
    // answer to "why did snug do *that*?", and it is printed before the
    // build so it is on screen even when the build fails.
    write_effective_options(&mut reporter, &cli, &merged, &matches, &cwd);

    // Before the (potentially slow) payload build, refuse an output that
    // aliases an input. `--dry-run` and `--emit-payload` never reach the
    // write, but they should still report the mistake rather than let the
    // user believe the command line is sound.
    let mut protected: Vec<PathBuf> = Vec::new();
    protected.extend(options_paths.iter().cloned());
    protected.extend(cli.localizations.iter().cloned());
    if let Some(log) = reporter.log_path() {
        protected.push(log.to_path_buf());
    }
    if let Err(err) = guard_output_collision(&cli, &protected) {
        reporter.set_stage("output");
        reporter
            .error(codes::OUTPUT_COLLISION, "output", format!("{err}"))
            .emit();
        finish(&mut reporter, &cli, None, false);
        return Ok(Outcome::Reported);
    }

    reporter.set_stage("payload");
    let payload = match build_payload_with(&cli, &mut reporter) {
        Ok(payload) => payload,
        Err(err) => {
            // A failure the user can act on is not a snug bug — and
            // `SNUG-INTERNAL-001` tells the user to report a bug. The
            // problem carries its own code and stage: `SNUG-INPUT-001` for
            // a path that cannot be used, `SNUG-MANIFEST-001` for a manifest
            // that cannot be read, and so on.
            match err.downcast::<snug_cli::build::UserProblem>() {
                Ok(problem) => {
                    let mut diagnostic =
                        reporter.error(problem.code, problem.stage, problem.message);
                    if let Some(hint) = problem.hint {
                        diagnostic = diagnostic.hint(hint);
                    }
                    diagnostic.emit();
                }
                Err(err) => {
                    reporter
                        .error(codes::INTERNAL, "payload", format!("{err:?}"))
                        .emit();
                }
            }
            finish(&mut reporter, &cli, None, false);
            return Ok(Outcome::Reported);
        }
    };
    let embedded = SnugEmbedded::new(payload);

    // Errors stop the build. The payload may have been assembled, but an
    // executable built from it would carry the problem straight to the
    // user's machine — which is the whole reason the checks raise errors
    // rather than warnings.
    let build_failed = reporter.has_errors();

    if cli.emit_payload {
        if !build_failed {
            use std::io::Write;
            let bytes = encode_for_emit(&embedded);
            std::io::stdout()
                .write_all(&bytes)
                .context("writing payload to stdout")?;
        }
        finish(&mut reporter, &cli, None, !build_failed);
        return if build_failed {
            Ok(Outcome::Reported)
        } else {
            Ok(Outcome::Success)
        };
    }

    if cli.dry_run {
        // Run the same PE resource checks as a normal Windows build,
        // including icon decoding and manifest XML validation. Nothing
        // is written to the output path.
        if !build_failed && !wants_app_bundle(&cli) {
            reporter.set_stage("resources");
            if let Err(err) = snug_cli::build::validate_resources(&cli, &embedded) {
                reporter
                    .error(codes::RESOURCE_INVALID, "resources", format!("{err:?}"))
                    .emit();
            }
        }
        let succeeded = !reporter.has_errors();
        dry_run(&cli, &embedded, &mut reporter);
        finish(&mut reporter, &cli, None, succeeded);
        return if succeeded {
            Ok(Outcome::Success)
        } else {
            Ok(Outcome::Reported)
        };
    }

    if build_failed {
        finish(&mut reporter, &cli, None, false);
        return Ok(Outcome::Reported);
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
            reporter.write_line(&format!("snug: built {}", output.display()));
            finish(&mut reporter, &cli, Some(output.as_path()), true);
            return Ok(Outcome::Success);
        }

        // Refused rather than quietly written. A `.app` is a *directory*
        // holding a Mach-O launcher plus its payload; a Windows snug has
        // no macOS launcher embedded, so the request cannot be honoured,
        // and the alternative — a flat PE named `Foo.app` — is precisely
        // the trap this check exists to prevent.
        #[cfg(not(target_os = "macos"))]
        {
            reporter
                .error(
                    codes::CFG_INVALID,
                    "output",
                    format!(
                        "-o {} names a macOS application, which is a *directory* \
                         containing a macOS launcher.\n\
                         snug on this platform embeds a Windows launcher, so a .app \
                         cannot be built here.",
                        output_path(&cli).display()
                    ),
                )
                .hint(
                    "pass -o MyApp.exe for a Windows executable, or use the \
                     macOS snug.",
                )
                .emit();
            finish(&mut reporter, &cli, None, false);
            return Ok(Outcome::Reported);
        }
    }

    reporter.set_stage("executable");
    let output = match build_exe(&cli, &embedded) {
        Ok(output) => output,
        Err(err) => {
            reporter
                .error(codes::OUTPUT_WRITE, "executable", format!("{err:?}"))
                .emit();
            finish(&mut reporter, &cli, None, false);
            return Ok(Outcome::Reported);
        }
    };
    let final_size = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
    reporter.write_line(&format!(
        "snug: wrote {} ({} bytes; payload {} as RT_RCDATA)",
        output.display(),
        final_size,
        embedded.payload_len
    ));
    reporter.write_line(&format!("snug: built {}", output.display()));
    finish(&mut reporter, &cli, Some(output.as_path()), true);
    Ok(Outcome::Success)
}

/// Open the build log, or explain why it could not be opened.
///
/// The log opens *before* the full clap parse so that a parse error
/// lands in it. When the destination cannot be opened, the build stops
/// before it writes an executable: a build whose report nobody can read
/// is not a build worth shipping. A temporary log still captures the
/// reason, so the failure is never silent.
fn open_build_log(
    merged: &Merged,
    cwd: &Path,
    options_paths: &[PathBuf],
    output: Option<&Path>,
) -> Result<Reporter> {
    let requested = options_file::flag_value(&merged.argv, "build-log");
    let requested = requested.map(PathBuf::from);
    let destination = BuildLog::resolve(requested.as_deref(), cwd);

    // The log is truncated when it opens, so it must not be one of the
    // files the build reads. Checking this here matters: the log opens
    // *before* the full parse, so the collision guard downstream is too
    // late to protect the input.
    // The output must be protected *before* the log is opened, not
    // by the later output guard (which would be too late).
    if output.is_some_and(|path| same_file(path, &destination)) {
        eprintln!(
            "snug: error {}: the build log {} would overwrite the executable",
            codes::OUTPUT_COLLISION,
            destination.display()
        );
        anyhow::bail!(
            "the build log and executable output refer to the same file: {}",
            destination.display()
        );
    }

    for input in protected_paths(merged, options_paths)? {
        if same_file(&input, &destination) {
            eprintln!(
                "snug: error {}: the build log {} is also one of this build's input files",
                codes::LOG_UNAVAILABLE,
                destination.display()
            );
            return Err(anyhow::anyhow!(
                "the build log would overwrite an input file: {}",
                destination.display()
            ));
        }
    }

    match BuildLog::open(&destination) {
        Ok(log) => Ok(Reporter::new(log)),
        Err(err) => {
            let fallback = write_fallback_log(&destination, &err);
            eprintln!(
                "snug: error {}: cannot write the build log to {}: {}",
                codes::LOG_UNAVAILABLE,
                destination.display(),
                err
            );
            if let Some(path) = &fallback {
                eprintln!("snug:   the reason was written to {}", path.display());
            }
            Err(anyhow::anyhow!(
                "the build log could not be opened at {}",
                destination.display()
            ))
        }
    }
}

/// The files the build reads, discovered from the merged argv.
///
/// The log opens before the full parse, so this is how the log
/// destination is checked against the inputs it must not alias.
fn protected_paths(merged: &Merged, options_paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut paths: Vec<PathBuf> = options_paths.to_vec();
    let mut inputs: Vec<PathBuf> = Vec::new();
    for value in options_file::flag_values(&merged.argv, "input") {
        inputs.push(PathBuf::from(value));
    }
    // The positional JAR, if there is one.
    if let Some(index) = merged.positional_at.first()
        && let Some(token) = merged.argv.get(*index)
    {
        inputs.push(PathBuf::from(token));
    }
    for name in ["icon", "splash", "manifest", "localisation"] {
        for value in options_file::flag_values(&merged.argv, name) {
            paths.push(PathBuf::from(value));
        }
    }

    for input in inputs {
        if input.is_dir() {
            // Reuse the exact JAR enumeration and ordering of the
            // packaging path. If enumeration is incomplete, never
            // risk truncating an undiscovered input JAR.
            let mut sink = Reporter::new(BuildLog::none());
            let jars = snug_cli::build::jar_paths_in(&input, &mut sink)
                .with_context(|| format!("checking JARs in {}", input.display()))?;
            if sink.warning_count() != 0 {
                anyhow::bail!(
                    "cannot safely inspect every JAR in input directory {}",
                    input.display()
                );
            }
            paths.extend(jars);
        }
        paths.push(input);
    }
    Ok(paths)
}

/// Whether two paths name the same file.
fn same_file(a: &Path, b: &Path) -> bool {
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    if canonical(a) == canonical(b) {
        return true;
    }
    // One of them may not exist yet, in which case canonicalisation
    // could not normalise the comparison. Fall back to a
    // case-insensitive textual match — right on Windows, harmless
    // elsewhere.
    a.to_string_lossy()
        .eq_ignore_ascii_case(&b.to_string_lossy())
}

/// Capture the reason a build log could not be opened, somewhere that is
/// likely to be writable.
fn write_fallback_log(destination: &Path, error: &std::io::Error) -> Option<PathBuf> {
    let path = std::env::temp_dir().join(format!("snug-build-{}.log", std::process::id()));
    let mut log = BuildLog::open(&path).ok()?;
    log.write_line(&format!(
        "snug: error {}: cannot write the build log to {}: {}",
        codes::LOG_UNAVAILABLE,
        destination.display(),
        error
    ));
    Some(path)
}

/// Record a parse failure in the log before clap exits.
///
/// clap renders the error and chooses the exit code itself, so the
/// message is not reworded here — the log gets clap's own text, which
/// is the text the user is about to read on screen.
fn report_parse_error(reporter: &mut Reporter, err: &clap::Error) {
    use clap::error::ErrorKind;
    if matches!(
        err.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) {
        return;
    }
    reporter
        .error(codes::CFG_PARSE, "arguments", err.render().to_string())
        .emit();
}

/// Write the effective-options table to the console and the log.
fn write_effective_options(
    reporter: &mut Reporter,
    cli: &Cli,
    merged: &Merged,
    matches: &clap::ArgMatches,
    cwd: &Path,
) {
    let main_class_source = if cli.main_class.is_none() {
        main_class_source_jar(cli).map(|jar| Origin::Manifest { jar })
    } else {
        None
    };
    let rows = effective::rows(cli, merged, matches, main_class_source);
    let rendered = effective::render(&rows, cwd);
    for line in rendered.lines() {
        reporter.write_line(line);
    }
}

/// The JAR that would supply the `Main-Class` when `--main-class` was
/// not given.
///
/// Best effort: the input may be a directory whose first JAR is not
/// knowable without reading it, and a name that is wrong is worse than
/// one that is absent.
fn main_class_source_jar(cli: &Cli) -> Option<String> {
    let path = cli.jar.as_ref().or(cli.input.as_ref())?;
    if path.is_dir() {
        let mut names: Vec<String> = std::fs::read_dir(path)
            .ok()?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("jar"))
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        names.sort();
        names.into_iter().next()
    } else {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
    }
}

fn encode_for_emit(embedded: &SnugEmbedded) -> Vec<u8> {
    snug_format::encode(embedded).expect("encoding never fails for in-memory payload")
}

fn dry_run(_cli: &Cli, embedded: &SnugEmbedded, reporter: &mut Reporter) {
    let payload = &embedded.payload;
    reporter.write_line("snug: dry-run — no application output written");
    // The options themselves are already in the effective-options table
    // above, with their origins. Repeating them here would be noise, so
    // this block carries only what the table cannot say: what was
    // embedded, and how big the result would be.
    reporter.write_line(&format!(
        "  jars:          {} (first sha256: {})",
        payload.jars.len(),
        hex_lower(&payload.jars[0].sha256)
    ));
    reporter.write_line(&format!(
        "  localizations: {} bundle(s) [{}]",
        payload.localizations.len(),
        payload
            .localizations
            .iter()
            .map(|b| b.tag.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    // Spelled per platform because it is: the `.exe` path stamps an icon,
    // a version resource and a manifest with `editpe`, while the `.app`
    // path converts the icon with `iconutil` and ad-hoc signs the bundle.
    // Printing "editpe" on a macOS dry-run would be a lie about what is
    // about to happen.
    #[cfg(target_os = "macos")]
    reporter.write_line("  resources:     iconutil (icon as App.icns) + codesign --sign -");
    #[cfg(not(target_os = "macos"))]
    reporter.write_line("  resources:     editpe (icon + version + manifest if set)");
    reporter.write_line(&format!(
        "  stub bytes:    {}",
        snug_cli::stub::STUB_BYTES.len()
    ));
    reporter.write_line(&format!(
        "  total size:    {} bytes (approx)",
        snug_cli::stub::STUB_BYTES.len() + (embedded.payload_len as usize)
    ));
}

/// The closing summary: what happened, how many problems, and where
/// everything is.
fn finish(reporter: &mut Reporter, cli: &Cli, output: Option<&Path>, succeeded: bool) {
    // A log that could not be written completely is reported rather than left
    // for the user to discover. It carries a code, which is the point: the log
    // is what a bug report quotes, and "the log was incomplete" has to be one
    // of the things it says about itself.
    reporter.report_log_write_failure();

    let output_path = output_path(cli);
    let mut lines = Vec::new();
    if succeeded {
        lines.push("Build succeeded.".to_string());
    } else {
        lines.push(format!("Build failed during {}.", reporter.stage()));
    }
    lines.push(format!("  Errors:   {}", reporter.error_count()));
    lines.push(format!("  Warnings: {}", reporter.warning_count()));
    match output {
        Some(path) => lines.push(format!("  Output:   {}", path.display())),
        None => lines.push(format!(
            "  Output:   {} (not written)",
            output_path.display()
        )),
    }
    match reporter.log_path() {
        Some(path) => lines.push(format!("  Log:      {}", path.display())),
        None => lines.push("  Log:      (none)".to_string()),
    }
    for line in lines {
        reporter.write_line(&line);
    }
}

/// The tail of the missing-input error, which depends on whether any
/// options file was actually read. Naming those files is the useful
/// part: it turns "snug ignored my `snug.options`" into a one-line
/// fix, and the paths are exactly the ones already announced on the
/// `loaded options from` lines above.
fn missing_input_detail(loaded: &[std::path::PathBuf]) -> String {
    if loaded.is_empty() {
        return " given".to_string();
    }
    let names: Vec<String> = loaded.iter().map(|p| p.display().to_string()).collect();
    format!(
        ": {} loaded, but no --input in any of them",
        names.join(", ")
    )
}

/// What to do about a missing input.
const MISSING_INPUT_HINT: &str = "pass a JAR as an argument, or set --input in snug.options \
     (--help for the full list of options)";

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
        || arg == "--init-localisations"
        || arg.starts_with("--init-localisations=")
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
    /// Target path. Optional — defaults to `./snug.options`, which is
    /// exactly the path `options_file::resolve_all` reads on a normal
    /// invocation: both resolve against the directory snug was invoked
    /// from, so writing here means the next `snug` run picks it up.
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
    /// invoked from). Created if missing.
    #[arg(
        long = "init-localisations",
        value_name = "DIR",
        num_args = 0..=1,
        default_missing_value = init_localizations::DEFAULT_DIR,
    )]
    init_localizations: Option<String>,

    /// Scaffold an extra `snug-localisations.<tag>.txt` translation
    /// template in the target directory. Repeatable; the English
    /// baseline is always written regardless.
    #[arg(long = "init-localisations-tag", value_name = "TAG")]
    init_localizations_tag: Vec<String>,

    /// Overwrite existing bundles in the `--init-localisations`
    /// target directory.
    #[arg(long = "init-localisations-force")]
    init_localizations_force: bool,

    /// Print the `--init-localisations` templates to stdout instead
    /// of writing them to disk.
    #[arg(long = "init-localisations-stdout")]
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
        assert!(is_init_flag("--init-localisations"));
        assert!(is_init_flag("--init-localisations=i18n"));
        // Modifiers alone don't select the branch — they fall through
        // to the normal parse.
        assert!(!is_init_flag("--init-options-force"));
        assert!(!is_init_flag("--init-localisations-force"));
        assert!(!is_init_flag("--init-localisations-stdout"));
        assert!(!is_init_flag("--init-localisations-tag"));
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

    // ---- --init-localisations ----

    #[test]
    fn accepts_bare_init_localizations() {
        let p = parse_or_reject(&["--init-localisations"]).unwrap();
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
        let p = parse_or_reject(&["--init-localisations=i18n"]).unwrap();
        assert_eq!(p.init_localizations.as_deref(), Some("i18n"));
        let p = parse_or_reject(&["--init-localisations", "cfg/i18n"]).unwrap();
        assert_eq!(p.init_localizations.as_deref(), Some("cfg/i18n"));
    }

    #[test]
    fn accepts_repeated_init_localizations_tags() {
        let p = parse_or_reject(&[
            "--init-localisations",
            "--init-localisations-tag",
            "de",
            "--init-localisations-tag",
            "pt-BR",
        ])
        .unwrap();
        assert_eq!(p.init_localizations_tag, vec!["de", "pt-BR"]);
    }

    #[test]
    fn accepts_init_localizations_with_force_and_stdout() {
        let p = parse_or_reject(&[
            "--init-localisations",
            "--init-localisations-force",
            "--init-localisations-stdout",
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
            "--init-localisations",
            "--init-localisations-tag",
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
        let p = parse_or_reject(&["--init-localisations-tag", "de"]);
        // The mini-parser accepts it (it declares the flag), so this
        // assertion documents that the *branch* is what excludes it.
        assert_eq!(p.unwrap().init_localizations_tag, vec!["de"]);
        assert!(!is_init_flag("--init-localisations-tag"));
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
        assert_rejected(
            &["--init-options", "--company", "Example Company Pty Ltd"],
            "--company",
        );
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
        assert_rejected(&["--init-options", "--jvm-arg", "-Xmx2g"], "--jvm-arg");
    }

    #[test]
    fn rejects_build_flag_localization() {
        assert_rejected(
            &["--init-options", "--localisation", "de.txt"],
            "--localisation",
        );
    }

    #[test]
    fn rejects_build_flag_download_jdk() {
        assert_rejected(&["--init-options", "--download-jdk=auto"], "--download-jdk");
    }

    #[test]
    fn rejects_build_flag_allow_multiple_instances() {
        assert_rejected(
            &["--init-options", "--allow-multiple-instances"],
            "--allow-multiple-instances",
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
        // --init-options foo -- bar` would be ambiguous, and `snug
        // --init-options App.jar extra` would have a stray
        // positional. Cover the stray-positional case explicitly.
        assert_rejected(
            &["--init-options", "App.jar", "extra-positional"],
            "extra-positional",
        );
    }
}
