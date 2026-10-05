//! `snug-bundle` — wrap a Mach-O in a `.app`, from the shell.
//!
//! A thin driver over [`snug_app_bundle`], so `scripts/build-macos.sh`
//! can package the two clickable artefacts without re-implementing the
//! bundle layout in bash. The bundling logic lives in the library where
//! it can be tested; this is only argument parsing.
//!
//! ```text
//! snug-bundle --binary <mach-o> --out <Name.app> --name <Name> \
//!             --id <reverse.dns.id> [--icon <png>] [--accepts-jar]
//! ```
//!
//! `--accepts-jar` is what makes the bundle a drop target: without a
//! `CFBundleDocumentTypes` claim Finder will not hand it a dropped file.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use snug_app_bundle::{AppSpec, DocumentType, write_app};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("snug-bundle: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut binary: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut name: Option<String> = None;
    let mut identifier: Option<String> = None;
    let mut icon: Option<PathBuf> = None;
    let mut version = String::from(env!("CARGO_PKG_VERSION"));
    let mut accepts_jar = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .with_context(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--binary" => binary = Some(PathBuf::from(value()?)),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--name" => name = Some(value()?),
            "--id" => identifier = Some(value()?),
            "--icon" => icon = Some(PathBuf::from(value()?)),
            "--version" => version = value()?,
            "--accepts-jar" => accepts_jar = true,
            "-h" | "--help" => {
                println!(
                    "snug-bundle — wrap a Mach-O in a double-clickable macOS .app\n\n\
                     USAGE:\n    \
                     snug-bundle --binary <mach-o> --out <Name.app> --name <Name> \\\n\
                     \x20              --id <reverse.dns.id> [--icon <png>] \\\n\
                     \x20              [--version <v>] [--accepts-jar]\n\n\
                     --accepts-jar   declare a CFBundleDocumentTypes claim for .jar, which is\n\
                     \x20               what lets Finder drop a JAR onto the bundle\n"
                );
                return Ok(());
            }
            other => bail!("unknown argument '{other}' (try --help)"),
        }
    }

    let binary = binary.context("--binary is required")?;
    let out = out.context("--out is required")?;
    let name = name.context("--name is required")?;
    let identifier = identifier.context("--id is required")?;

    if !binary.is_file() {
        bail!("{} does not exist", binary.display());
    }
    // A `.app` is a directory. Catching the mismatch here beats letting
    // `fs::create_dir_all` fail deep inside the bundler.
    if out.extension().is_some_and(|e| !e.eq_ignore_ascii_case("app")) {
        bail!(
            "{} does not end in .app; a macOS bundle is a directory with a .app name",
            out.display()
        );
    }

    let spec = AppSpec {
        bundle: out.clone(),
        name,
        identifier,
        version,
        binary,
        icon_png: icon,
        document_types: if accepts_jar {
            vec![DocumentType::jar()]
        } else {
            Vec::new()
        },
    };

    write_app(&spec)?;
    println!("    bundled:  {} (icon: {})", out.display(), spec.icon_png.is_some());
    Ok(())
}
