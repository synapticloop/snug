//! `snug_preview` for macOS — the same idea, a much smaller job.
//!
//! # Why this file is not a port of `windows_impl.rs`
//!
//! The Windows preview is ~2000 lines because the Windows dialogs are
//! hand-painted Win32 windows: there is no way to ask AppKit-for-render
//! or even to drive the real one without an app around it, so the
//! previewer has to *reimplement* each window to judge the copy.
//!
//! The macOS dialogs have no such problem. They are real `NSWindow`s
//! behind ordinary functions (`appkit::consent`, `appkit::progress`, …),
//! so the previewer does not reimplement anything — it calls them. That
//! makes a macOS preview both much shorter and, more importantly,
//! *honest*: what you see is the window the launcher actually shows,
//! mascot included, rather than a copy that can drift from it.
//!
//! # Why the mascot override is mandatory rather than a nicety
//!
//! `cargo run --bin snug_preview` is not inside a `.app`, so
//! `NSImageNameApplicationIcon` resolves to nothing and every dialog
//! would preview with no mascot at all. The one tool whose entire job is
//! judging dialog appearance by eye could not show the appearance. Hence
//! `--mascot <FILE>`, mirroring the Windows preview's `--icon`.

use std::path::{Path, PathBuf};

use snug_launcher::appkit;
use snug_launcher::localize;
use snug_format::Localization;

fn log(msg: &str) {
    eprintln!("snug_preview: {msg}");
}

fn help_text() -> String {
    let mut h = String::from(
        "snug_preview - preview the snug launcher dialogs\n\n\
         USAGE:\n    \
         snug_preview [--localisation <FILE|DIR> | --localization <FILE|DIR>] \
         [--mascot <FILE>]\n\n\
         Renders every macOS launcher dialog (JDK install prompt, Adoptium \
         download progress,\nretry, terminal failure, metadata failure) so \
         localisation copy and layout can be\nreviewed without rebuilding \
         an application.\n\n\
         --mascot <FILE>  Image shown in each dialog's mascot slot. Required \
         in practice: run\n                  from a terminal, snug has no bundle here, \
         so the app icon resolves to\n                  nothing and the mascot would silently be absent.\n\n\
         Run it:\n    \
         cargo run --bin snug_preview -- --mascot assets/snug-icon.png",
    );
    if cfg!(windows) {
        h.push_str("\n\n(on Windows this same tool previews the Win32 dialogs)");
    }
    h
}

/// Build the bundle chain from the paths given to `--localisation`.
///
/// A path may be a single `snug-localisations.<tag>.txt` or a directory
/// of them; expansion goes through the same
/// `snug_format::discover_localization_files` the `snug` CLI uses, so
/// both resolve a given path identically.
///
/// Only the requested tag is handed to `localize::set_bundles` — the
/// built-in English baseline is appended behind it automatically, so
/// untranslated keys fall back exactly as they would in a shipped build.
fn build_localisations(entries: &[PathBuf]) -> Result<Vec<Localization>, String> {
    let files = snug_format::discover_localization_files(entries).map_err(|e| e.to_string())?;
    let mut out: Vec<Localization> = Vec::new();
    for path in &files {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        let tag = snug_format::tag_from_path(path).ok_or_else(|| {
            format!(
                "extracting locale tag from {} (expected `snug-localisations.<tag>.txt`)",
                path.display()
            )
        })?;
        if out.iter().any(|b| b.tag == tag) {
            return Err(format!(
                "duplicate localisation tag `{tag}` — pass each tag exactly once"
            ));
        }
        out.push(
            Localization::parse(&tag, &text)
                .map_err(|e| format!("parsing {}: {e}", path.display()))?,
        );
        log(&format!("loaded localisation `{tag}` from {}", path.display()));
    }
    Ok(out)
}

/// Values that stand in for a real Adoptium answer, so every `{...}`
/// placeholder in the copy is actually exercised. A preview that shows
/// unfilled placeholders is a preview that cannot catch a typo in one.
const DEMO_VERSION: &str = "25.0.4.1+1";
const DEMO_SIZE_MB: u32 = 190;
const DEMO_URL: &str = "https://api.adoptium.net/v3/binary/version/jdk-25.0.4.1%2B1/mac/x64/jdk/hotspot/normal/eclipse";
const DEMO_SHA: &str = "3f1a9c7e2b8d4056af19c2e7b0d4a8135ce6f9021b7da4e58c0a3f19b6d72e45c";
const DEMO_ERROR: &str =
    "the connection was reset while reading the archive (curl error 56)";

/// Show every dialog once, in the order the install flow reaches them.
fn show_all(mascot: Option<&Path>) {
    let Some(mtm) = objc2::MainThreadMarker::new() else {
        log("must be run on the main thread");
        std::process::exit(1);
    };

    if let Some(path) = mascot {
        match appkit::mascot_image_from_file(mtm, path) {
            Some(image) => {
                // Read the dimensions before handing the image over, since
                // `set_mascot_image` takes ownership and a `Retained` is
                // cheap to clone but not to borrow after a move.
                let size = image.size();
                log(&format!(
                    "mascot: loaded {} ({} x {})",
                    path.display(),
                    size.width,
                    size.height
                ));
                appkit::set_mascot_image(Some(image));
            }
            None => {
                // Not fatal: the dialogs are still worth looking at, and a
                // wrong icon should not hide the copy.
                log(&format!(
                    "mascot: could not load {} — windows will draw none",
                    path.display()
                ));
            }
        }
    } else {
        log("mascot: none given, and there is no bundle here, so the mascot slot is empty");
    }

    // Metadata failure: Adoptium could not be reached at all.
    log("— JDK metadata failure —");
    let choice = appkit::metadata_failed(25, DEMO_ERROR);
    log(&format!("  button index {choice} (0 = open in browser, 1 = cancel)"));

    // Retry: one download attempt failed, another is available.
    log("— download attempt failed, retrying —");
    let retry = appkit::retry(2, 3, DEMO_VERSION, DEMO_ERROR);
    log(&format!("  retry = {retry}"));

    // Terminal failure: out of attempts.
    log("— all attempts exhausted —");
    let d = snug_launcher::dialogs::dialogs();
    let content = snug_launcher::dialogs::fill(
        d.jdk_install.failure.content.as_str(),
        &[("version", DEMO_VERSION), ("error", DEMO_ERROR)],
    );
    appkit::failure(&d.jdk_install.failure.title, &content);
    log("  dismissed");

    // Consent: the actual ask.
    log("— install consent —");
    let consent = appkit::consent(DEMO_VERSION, DEMO_SIZE_MB, DEMO_URL, DEMO_SHA);
    log(&format!("  consent = {consent}"));

    // Progress: a synthetic 0-100% walk so every phase label is seen.
    log("— download progress (synthetic, no network) —");
    let ok = appkit::progress_demo();
    log(&format!("  completed = {ok}"));

    log("the launcher error window has no macOS implementation yet, so there is nothing to show");
}

pub fn run() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{}", help_text());
        return;
    }

    let mut localisations: Vec<PathBuf> = Vec::new();
    let mut mascot: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--localisation" | "--localization" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    log("--localisation needs a FILE or DIR");
                    std::process::exit(2);
                };
                localisations.push(PathBuf::from(v));
            }
            "--mascot" | "--icon" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    log("--mascot needs a FILE");
                    std::process::exit(2);
                };
                mascot = Some(PathBuf::from(v));
            }
            other => {
                log(&format!("unrecognised argument `{other}` (try --help)"));
                std::process::exit(2);
            }
        }
        i += 1;
    }

    match build_localisations(&localisations) {
        Ok(bundles) => {
            // An empty chain leaves the built-in baseline in place, which
            // is exactly what an unflagged preview should show.
            localize::set_bundles(&bundles);
            if bundles.is_empty() {
                log("localisation: built-in English baseline");
            }
        }
        Err(e) => {
            log(&format!("localisation: {e}"));
            std::process::exit(2);
        }
    }

    show_all(mascot.as_deref());
    log("done");
}
