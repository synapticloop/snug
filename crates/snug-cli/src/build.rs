//! Assemble a [`SnugPayload`] from parsed CLI args + loaded inputs, and
//! turn it into the final Windows `.exe`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use editpe::Image;
use image::GenericImageView;

use crate::cli::Cli;
use crate::localization;
use crate::manifest::{read_main_class_from_bytes, survey_main_classes};
use crate::resources::ResourcePlan;
use editpe::constants::IMAGE_SUBSYSTEM_WINDOWS_GUI;
use crate::stub::STUB_BYTES;
use snug_format::{
    AppMetadata, EmbeddedFile, LauncherBehavior, LauncherConfig, SnugEmbedded, SnugPayload,
    SplashConfig, SplashImage, embedded_file, encode,
};
use snug_payload::PAYLOAD_RESOURCE_NAME;

/// Build the full [`SnugPayload`] that the launcher will consume.
///
/// Resolves the input source from either the positional `[JAR]`
/// argument or `--input <JAR|DIR>`:
/// - If the resolved path is a file, it is wrapped as a single-JAR
///   launcher.
/// - If the resolved path is a directory, every `*.jar` directly
///   inside it is scanned (sorted by name) and embedded as a
///   multi-JAR classpath.
///
/// Reads the JAR bytes, hashes them, optionally loads icon / splash
/// files, reads `Main-Class` from the **first** JAR's manifest
/// (unless `--main-class` overrides), and assembles a complete
/// [`SnugPayload`].
pub fn build_payload(cli: &Cli) -> Result<SnugPayload> {
    let (input_path, jars, labels) = resolve_input(cli)?;

    // --- Resolve main class ---------------------------------------------
    // For multi-JAR builds, Main-Class is read from the first JAR's
    // manifest. CLI `--main-class` always overrides.
    //
    // "First" is the first JAR *sorted by filename* — see
    // `resolve_input`. For a multi-JAR input that is an ordering
    // accident, not a decision, so the ambiguity is surfaced rather
    // than resolved silently.
    let main_class = match &cli.main_class {
        Some(cls) => Some(cls.clone()),
        None => {
            let first = jars.first().ok_or_else(|| {
                anyhow::anyhow!("at least one JAR is required to read Main-Class")
            })?;
            read_main_class_from_bytes(&first.bytes)
                .context("reading Main-Class from JAR manifest")?
        }
    };

    warn_on_ambiguous_manifests(&jars, &labels, &main_class, cli.main_class.is_some());

    // The icon is *not* read here. It used to be: `build_payload` loaded it
    // into `SnugPayload::icon` for the builder to stamp, while
    // `ResourcePlan` re-read the same file from disk -- so every built EXE
    // carried the icon twice, uncompressed, and only the second copy was
    // ever used. The payload field is gone (see `snug-format::payload`);
    // `ResourcePlan` still stamps from `cli.icon` at the PE-writing stage,
    // which is the only read that ever mattered.

    // --- Optional splash -------------------------------------------------
    // At build time we decode the user's PNG, validate its size, and
    // pre-convert it into the BGRA-premultiplied form the launcher's
    // `UpdateLayeredWindow` path expects. The launcher therefore
    // doesn't need an image decoder.
    let splash = match &cli.splash {
        Some(path) => {
            let png_bytes = std::fs::read(path)
                .with_context(|| format!("reading splash PNG {}", path.display()))?;
            if png_bytes.is_empty() {
                bail!("{} is empty", path.display());
            }
            let img = image::load_from_memory_with_format(&png_bytes, image::ImageFormat::Png)
                .map_err(|e| anyhow::anyhow!("decode splash PNG {}: {e}", path.display()))?;
            let (width, height) = img.dimensions();
            check_splash_dimensions(width, height, &cli.splash_max);
            let rgba = img.to_rgba8();
            let mut pixels: Vec<u8> = rgba.into_raw();
            // Convert RGBA straight → BGRA pre-multiplied. `UpdateLayeredWindow`
            // with `BLENDFUNCTION { ..., AC_SRC_ALPHA }` requires a
            // premultiplied 32-bit BGRA DIB. For fully-opaque pixels
            // (alpha = 255) the multiply is a no-op on the channels.
            for chunk in pixels.chunks_exact_mut(4) {
                let r = chunk[0] as u32;
                let g = chunk[1] as u32;
                let b = chunk[2] as u32;
                let a = chunk[3] as u32;
                chunk[0] = ((b * a + 127) / 255) as u8; // B ← pre(B)
                chunk[1] = ((g * a + 127) / 255) as u8; // G ← pre(G)
                chunk[2] = ((r * a + 127) / 255) as u8; // R ← pre(R)
                chunk[3] = a as u8; // A unchanged
            }
            Some(SplashConfig {
                duration_ms: cli.splash_ms,
                image: SplashImage {
                    width,
                    height,
                    bytes: pixels,
                },
            })
        }
        None => None,
    };

    // --- App metadata defaults ------------------------------------------
    let name = cli
        .name
        .clone()
        .or_else(|| default_name_from_input(&input_path))
        .context("--name is required when it cannot be inferred from the input path")?;
    let company = cli.company.clone().unwrap_or_else(|| "Unknown".to_string());
    let version = cli.version.clone().unwrap_or_else(|| "0.0.0".to_string());

    let app = AppMetadata {
        name,
        company,
        version,
        description: cli.description.clone(),
        copyright: cli.copyright.clone(),
        update_check_url: cli.update_url.clone(),
    };

    let config = LauncherConfig {
        app,
        main_class,
        min_java: cli.min_java,
        jvm_args: cli.jvm_args.clone(),
        splash,
        behavior: LauncherBehavior {
            download_jdk: cli.download_jdk.into(),
            cache_dir: cli.cache_dir.clone(),
            ..LauncherBehavior::default()
        },
    };

    Ok(SnugPayload {
        config,
        jars,
        localizations: collect_localizations(cli)?,
    })
}

/// Load, parse, and bundle every user-supplied localization file plus
/// the always-embedded built-in English baseline. Two CLI builds of
/// the same inputs must produce byte-identical `localizations`
/// vectors, so we sort by canonicalised path before insertion — this
/// matters because `snug.options` accumulates `--localization` lines
/// in arbitrary order, and a `--localization X` on the CLI vs. one
/// in the file shouldn't reorder the rest of the chain.
fn collect_localizations(cli: &Cli) -> Result<Vec<snug_format::Localization>> {
    let mut paths: Vec<std::path::PathBuf> = cli.localizations.clone();
    paths.sort();
    let bundles = localization::collect(&paths)?;
    localization::ensure_unique_tags(&bundles).context("validating localization bundle tags")?;
    Ok(bundles)
}

/// Resolve `[JAR]` vs `--input <JAR|DIR>` down to a concrete path,
/// returning it alongside whether it names a directory.
///
/// Deliberately does **no** work beyond a single `stat`: the
/// `--find-main` diagnostic uses this to get to the JARs without
/// reading, hashing, and embedding them, which on a 200 MB fat JAR
/// would cost far more than the scan it's there to enable.
pub fn resolve_input_path(cli: &Cli) -> Result<(PathBuf, bool)> {
    let input_path = match (&cli.jar, &cli.input) {
        (Some(p), None) => p.clone(),
        (None, Some(p)) => p.clone(),
        (None, None) => bail!(
            "an input source is required (pass a JAR as the positional argument, or use --input <JAR|DIR>)"
        ),
        (Some(_), Some(_)) => {
            bail!("the positional [JAR] argument and --input are mutually exclusive")
        }
    };

    let meta = std::fs::metadata(&input_path)
        .with_context(|| format!("stat-ing input {}", input_path.display()))?;

    let is_dir = if meta.is_dir() {
        true
    } else if meta.is_file() {
        false
    } else {
        bail!(
            "{} is neither a regular file nor a directory",
            input_path.display()
        );
    };

    Ok((input_path, is_dir))
}

/// Every `*.jar` directly inside `dir`, sorted by path for
/// determinism. Subdirectories are not searched.
pub fn jar_paths_in(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut jar_paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading directory {}", dir.display()))?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("jar") {
                Some(path)
            } else {
                None
            }
        })
        .collect();
    jar_paths.sort();

    if jar_paths.is_empty() {
        bail!(
            "no *.jar files found in {} (--input directory must contain at least one JAR)",
            dir.display()
        );
    }

    Ok(jar_paths)
}

/// Resolve `[JAR]` vs `--input <JAR|DIR>` and return the resolved
/// path, the embedded JAR(s), and a display label per JAR (indexes
/// align with the embedded list).
fn resolve_input(cli: &Cli) -> Result<(PathBuf, Vec<EmbeddedFile>, Vec<String>)> {
    let (input_path, is_dir) = resolve_input_path(cli)?;

    if !is_dir {
        // Single-JAR input.
        let bytes = std::fs::read(&input_path)
            .with_context(|| format!("reading JAR {}", input_path.display()))?;
        if bytes.is_empty() {
            bail!("{} is empty", input_path.display());
        }
        let label = file_name_of(&input_path);
        Ok((input_path, vec![embedded_file(bytes)], vec![label]))
    } else {
        // Multi-JAR input: scan the directory for `*.jar`, sort by
        // filename for determinism.
        let jar_paths = jar_paths_in(&input_path)?;

        let mut jars = Vec::with_capacity(jar_paths.len());
        let mut labels = Vec::with_capacity(jar_paths.len());
        for jar_path in &jar_paths {
            let bytes = std::fs::read(jar_path)
                .with_context(|| format!("reading JAR {}", jar_path.display()))?;
            if bytes.is_empty() {
                bail!("{} is empty", jar_path.display());
            }
            jars.push(embedded_file(bytes));
            labels.push(file_name_of(jar_path));
        }
        eprintln!(
            "snug: indexed {} JAR(s) from {}",
            jars.len(),
            input_path.display()
        );
        Ok((input_path, jars, labels))
    }
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Flag an ambiguous `Main-Class` source on a multi-JAR input.
///
/// Snug resolves `Main-Class` from the **first** JAR, which is the
/// first entry sorted by filename. That is a defensible tie-break and
/// an indefensible *policy*: a directory of JARs routinely contains
/// more than one `Main-Class` (a fat JAR plus a bundled runnable tool,
/// or a dependency that is itself executable), and which one wins then
/// depends on filenames rather than anything the user decided.
///
/// A warning is the right severity — the build genuinely works, and
/// failing instead would break the legitimate case of a directory
/// where the alphabetical winner is correct on purpose. Silent would
/// be the failure mode to avoid, so name what was chosen and how to
/// change it.
///
/// Two shapes get flagged, both of which otherwise produce a launcher
/// pointing at a class the user didn't choose (or at nothing):
///
/// - the manifests disagree, and filename order silently picked one;
/// - the *first* JAR declares nothing while a later one does, so snug
///   embeds no main class at all despite one being available.
///
/// No-op when `--main-class` was passed, because then the user has
/// already answered the question this warning asks.
fn warn_on_ambiguous_manifests(
    jars: &[EmbeddedFile],
    labels: &[String],
    main_class: &Option<String>,
    explicit: bool,
) {
    if explicit || jars.len() < 2 {
        return;
    }

    let declarations = survey_main_classes(
        jars.iter()
            .zip(labels)
            .map(|(file, label)| (label.clone(), file.bytes.clone())),
    );
    if declarations.is_empty() {
        return;
    }

    // The first JAR is what snug actually reads. If it declares
    // nothing, we embedded no main class even though one was on offer.
    if main_class.is_none() {
        eprintln!(
            "snug: warning: {} (the first JAR by filename) declares no Main-Class, \
             so the built launcher will have none.",
            labels[0]
        );
        eprintln!("snug: note:    other JAR(s) in this directory do declare one:");
        for declaration in &declarations {
            eprintln!(
                "snug:            {} -> {}",
                declaration.jar, declaration.main_class
            );
        }
        eprintln!(
            "snug: note:    pass --main-class <CLASS> to choose one, e.g. \
             --main-class {}",
            declarations[0].main_class
        );
        return;
    }

    // Several JARs declaring the *same* class is not a conflict — the
    // answer doesn't depend on ordering, so there's nothing to warn
    // about.
    let distinct: std::collections::BTreeSet<&str> =
        declarations.iter().map(|d| d.main_class.as_str()).collect();
    if distinct.len() < 2 {
        return;
    }

    eprintln!(
        "snug: warning: {} JARs declare different Main-Class values:",
        distinct.len()
    );
    for declaration in &declarations {
        eprintln!(
            "snug:          {} -> {}",
            declaration.jar, declaration.main_class
        );
    }

    // `declarations` is in JAR order and snug reads the first JAR, so
    // index 0 is the one in use. Match on position rather than on the
    // class value — two JARs can declare the same class, and a
    // value-based match would credit the wrong one.
    eprintln!(
        "snug: note:    using {} (from {}, the first JAR by filename)",
        declarations[0].main_class, declarations[0].jar
    );
    eprintln!(
        "snug: note:    pass --main-class <CLASS> to choose explicitly, e.g. \
         --main-class {}",
        declarations[0].main_class
    );
}

fn default_name_from_input(path: &std::path::Path) -> Option<String> {
    let stem = if path.is_dir() {
        path.file_name()?.to_str()?
    } else {
        path.file_stem()?.to_str()?
    };
    if stem.is_empty() {
        return None;
    }
    Some(stem.to_owned())
}

/// Parse a `--splash-max` value.
///
/// Accepts `<W>x<H>` (e.g. `640x360`) or `off`/`none`/`unlimited`
/// (case-insensitive) to disable the warning. `None` returned by this
/// helper means "no max, never warn" (the `off` path); `Some((w, h))`
/// means the warning fires when the source PNG exceeds either bound.
fn parse_splash_max(value: &str) -> Option<(u32, u32)> {
    let v = value.trim();
    if v.is_empty() {
        return Some((640, 360));
    }
    let low = v.to_ascii_lowercase();
    if matches!(
        low.as_str(),
        "off" | "none" | "unlimited" | "disable" | "no" | "false"
    ) {
        return None;
    }
    let (w_str, h_str) = v
        .split_once('x')
        .or_else(|| v.split_once('X'))
        .or_else(|| v.split_once('×'))
        .or_else(|| v.split_once(' '))?;
    let w: u32 = w_str.trim().parse().ok()?;
    let h: u32 = h_str.trim().parse().ok()?;
    if w == 0 || h == 0 {
        return None;
    }
    Some((w, h))
}

/// Emit a build-time warning if the decoded splash dimensions exceed
/// the `--splash-max` bounds. No-op when the value parses to
/// `off`/`none` or anything that disables the cap.
fn check_splash_dimensions(width: u32, height: u32, max_value: &str) {
    let max = match parse_splash_max(max_value) {
        Some(m) => m,
        None => return,
    };
    if width > max.0 || height > max.1 {
        eprintln!(
            "snug: warning: splash PNG is {width}x{height}, exceeds recommended max {}x{}",
            max.0, max.1
        );
        eprintln!(
            "snug: note:    the launcher renders at native pixel size (often looks oversized at >{}x{})",
            max.0, max.1
        );
        eprintln!(
            "snug: note:    silence with --splash-max off, or customise with --splash-max <WxH>"
        );
    }
}

/// The extension the *default* output name gets on this host.
///
/// snug embeds exactly one launcher, selected for the machine it was
/// built on: a PE stub on Windows, a Mach-O for the host's architecture
/// on macOS. So the default output should be the artefact this snug is
/// actually equipped to produce — a macOS `snug` defaults to a `.app`
/// bundle it can really make, not to a Windows `.exe` it only carries as
/// a cross-platform convenience.
///
/// A Windows `snug` has no macOS launcher at all, so `Foo.app` there is
/// rejected rather than quietly written as a flat PE — see
/// [`wants_app_bundle`].
///
/// The convenience is still available either way, just explicitly:
/// `-o App.exe` on a Mac builds a Windows EXE, because the PE stub is
/// embedded unconditionally. That is how Windows artefacts get built from
/// a Mac, and it should be something you ask for rather than something
/// that happens.
#[cfg(target_os = "macos")]
const DEFAULT_OUTPUT_EXT: &str = "app";

#[cfg(not(target_os = "macos"))]
const DEFAULT_OUTPUT_EXT: &str = "exe";

/// Resolve the final output path from the CLI args.
///
/// Default: `<input-stem>.<DEFAULT_OUTPUT_EXT>` next to the input JAR /
/// directory — `App.app` on macOS, `App.exe` elsewhere.
pub fn output_path(cli: &Cli) -> PathBuf {
    match &cli.output {
        Some(p) => p.clone(),
        None => {
            let default_path = match (&cli.jar, &cli.input) {
                (Some(j), None) => Some(j.with_extension(DEFAULT_OUTPUT_EXT)),
                (None, Some(i)) => Some(i.with_extension(DEFAULT_OUTPUT_EXT)),
                _ => None,
            };
            default_path.unwrap_or_else(|| PathBuf::from(format!("App.{DEFAULT_OUTPUT_EXT}")))
        }
    }
}

/// Refuse to write the launcher over one of the build's own inputs.
///
/// Every input is read into memory before anything touches the disk, so
/// an output path that aliases a JAR, icon, splash or manifest is not a
/// transient failure — the write *succeeds*, destroying the file, and the
/// build reports success. `snug App.jar -o App.jar` exits 0 and leaves a
/// PE where the user's source archive was.
///
/// Comparison is canonical rather than textual, so `App.jar`, `.\App.jar`
/// and `App.JAR` are all recognised as the same file. A path that does not
/// resolve yet (an output being created) falls back to itself, which is
/// what makes a non-existent default output compare equal to itself and
/// correctly *not* collide.
pub fn guard_output_collision(cli: &Cli) -> Result<()> {
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let output = output_path(cli);
    let output_key = canonical(&output);

    // Path-valued inputs, in the order a reader would guess at them.
    let inputs = [
        cli.jar.as_ref(),
        cli.input.as_ref(),
        cli.icon.as_ref(),
        cli.splash.as_ref(),
        cli.manifest.as_ref(),
    ];

    for input in inputs.into_iter().flatten() {
        if canonical(input) == output_key {
            anyhow::bail!(
                "-o {} is also one of this build's input files.\n\
                 snug reads every input into memory before writing anything, so \
                 proceeding would destroy it with no backup.\n\
                 hint: pass a different output path, e.g. -o {}",
                // Echo the user's own spelling, not the canonicalised form —
                // on Windows that carries a `\\?\` prefix they never typed.
                output.display(),
                output.with_extension(DEFAULT_OUTPUT_EXT).display()
            );
        }
    }
    Ok(())
}

/// Did the user ask for a macOS application bundle?
///
/// Deliberately platform-neutral, and deliberately a question about the
/// *request* rather than about what this build can do. It has to be
/// answerable everywhere, because the interesting half is the answer on
/// platforms that cannot honour it: a Windows `snug` has no macOS
/// launcher embedded, so `-o Foo.app` there cannot be satisfied and must
/// be refused rather than turned into a flat file with a lying name.
///
/// Case-insensitive, so `-o Foo.APP` is caught too. The macOS emitter
/// also refuses to clobber a non-directory at a `.app` path, which is the
/// other half of the same trap.
pub fn wants_app_bundle(cli: &Cli) -> bool {
    output_path(cli)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("app"))
}

/// Where to stage the EXE before renaming it onto the real output path.
///
/// Suffix rather than extension replacement, so `App.exe` becomes
/// `App.exe.tmp-<pid>` and keeps its own extension: anything watching the
/// output directory (an editor, a sync client, Explorer) recognises the
/// staging file as unfinished instead of as a competing build.
fn staged_output_path(output: &std::path::Path) -> std::path::PathBuf {
    let mut name = output
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(format!(".tmp-{}", std::process::id()));
    output.with_file_name(name)
}

/// Build the final Windows `.exe`.
///
/// v2 (slice 4): the encoded payload is embedded as an `RT_RCDATA`
/// resource entry (named `"SNUGEMBD"`) inside the precompiled stub's
/// resource directory, alongside the optional icon, application
/// manifest, and always-stamped version info. There is no overlay —
/// the file ends at the last PE section.
///
/// The launcher locates the payload at runtime via
/// [`crate::payload_locator::find_in_file`], which parses the running
/// EXE's resource directory and reads the `RT_RCDATA` entry.
pub fn build_exe(cli: &Cli, embedded: &SnugEmbedded) -> Result<PathBuf> {
    let output = output_path(cli);

    // One encode, not two, and no second copy of every JAR byte.
    //
    // This used to take `&SnugPayload` and do
    //     let embedded = SnugEmbedded::new(payload.clone());
    //     let encoded = encode(&embedded);
    // — but `main.rs` had *already* built exactly that `SnugEmbedded` a few
    // lines earlier, purely to satisfy this signature. So every build
    // cloned the whole payload (a second full copy of a 200 MB fat JAR,
    // live at the same time as the first) and serialised it a second time
    // to produce a value the caller was already holding. The caller now
    // passes it in.
    //
    // `SnugEmbedded::new` still encodes once, for `payload_len` /
    // `payload_crc32`. Caching those bytes on the struct would save that
    // too, but `payload` is a public field: a cached copy would go stale the
    // moment anyone mutated it, and a silently-wrong payload is worse than
    // one redundant pass. Left alone deliberately.
    let encoded = encode(embedded).context("encoding snug payload")?;
    // Taken now because `set_rcdata` consumes the vector below; the log line
    // at the end wants the length. Cheaper than cloning it back.
    let payload_len = encoded.len();

    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating output directory {}", parent.display()))?;
        }
    }

    // Open the stub as a PE image so we can mutate its resource
    // directory in place.
    let mut image = Image::parse(STUB_BYTES).context("parsing embedded stub as PE image")?;

    let mut resources = image.resource_directory().cloned().unwrap_or_default();

    // Embed the snug payload as an RT_RCDATA resource entry.
    resources
        .set_rcdata(PAYLOAD_RESOURCE_NAME, encoded)
        .context("embedding snug payload as RT_RCDATA resource")?;

    // Stamp icon / manifest / version from CLI args.
    let plan = ResourcePlan::from_cli(cli);
    plan.apply(&mut resources, embedded)
        .context("stamping icon / manifest / version resources")?;

    image
        .set_resource_directory(resources)
        .context("installing resource directory onto stub image")?;
    // Defensive, and now actually on the production path. This lived only
    // in `ResourcePlan::run`, which `build_exe` does not call -- so the one
    // place that ships an EXE never applied it, and the comment claimed a
    // guarantee nothing enforced. `stub::tests::stub_is_a_pe32_plus_gui_exe`
    // now checks the same three facts on the committed stub, so the guard
    // and the test agree about what "GUI" means.
    image.set_subsystem(IMAGE_SUBSYSTEM_WINDOWS_GUI);
    // Write through a sibling temp and rename into place.
    //
    // `Image::write_file` is `std::fs::write`, i.e. `File::create` + `write_all`
    // -- truncate in place. So a disk that fills, an AV scanner holding a
    // handle, or a Ctrl-C at the wrong moment left a *truncated* `App.exe`
    // where a working one had been a moment earlier, and the build reported
    // the error without the user ever getting their previous artefact back.
    // Everything up to here is in memory, so there is no other reason this
    // has to touch the real output path.
    let staged = staged_output_path(&output);
    if let Err(e) = image.write_file(&staged) {
        let _ = std::fs::remove_file(&staged);
        return Err(e).with_context(|| format!("writing EXE to {}", output.display()));
    }
    if let Err(e) = std::fs::rename(&staged, &output) {
        let _ = std::fs::remove_file(&staged);
        return Err(anyhow::Error::new(e))
            .with_context(|| format!("installing the EXE at {}", output.display()));
    }

    let final_size = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
    eprintln!(
        "snug: wrote {} ({} bytes; payload {} as RT_RCDATA)",
        output.display(),
        final_size,
        payload_len,
    );

    Ok(output)
}

/// Write a standalone `.snug-blob` (encoded payload only) for
/// debugging or as an escape hatch when the user wants to combine the
/// payload with a non-default stub themselves.
#[allow(dead_code)]
pub fn write_blob(cli: &Cli, payload: &SnugPayload) -> Result<PathBuf> {
    let jar_path = cli
        .jar
        .as_ref()
        .context("a JAR path is required to determine the blob output path")?;
    let embedded = SnugEmbedded::new(payload.clone());
    let encoded = encode(&embedded).context("encoding snug payload")?;
    let target = jar_path.with_extension("snug-blob");
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(&target, &encoded)
        .with_context(|| format!("writing blob to {}", target.display()))?;
    Ok(target)
}

#[allow(dead_code)]
fn _silence_path(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::read_main_class;
    use clap::Parser;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    #[test]
    fn default_output_is_the_hosts_native_artefact() {
        // snug embeds exactly one launcher, chosen for the machine it was
        // built on, so the default output has to be the artefact this
        // build can actually produce. A macOS snug defaulting to `.exe`
        // would be handing out a cross-platform special case as the
        // common case.
        let cli = Cli::parse_from(["snug", "app.jar"]);
        let out = output_path(&cli);
        #[cfg(target_os = "macos")]
        assert_eq!(out, PathBuf::from("app.app"));
        #[cfg(not(target_os = "macos"))]
        assert_eq!(out, PathBuf::from("app.exe"));

        // Same for a directory input.
        let dir = Cli::parse_from(["snug", "libs"]);
        assert!(output_path(&dir)
            .to_string_lossy()
            .ends_with(DEFAULT_OUTPUT_EXT));
    }

    #[test]
    fn default_output_always_has_an_extension() {
        // Guards the "no input at all" branch, which builds the name with
        // `format!` rather than `with_extension` and is therefore the one
        // that could quietly regress to a bare "App".
        let cli = Cli::parse_from(["snug"]);
        let out = output_path(&cli);
        assert!(out.extension().is_some(), "got {out:?}");
    }

    #[test]
    fn app_bundle_request_is_recognised_on_every_platform() {
        // The predicate has to be answerable everywhere. The interesting
        // case is a Windows snug given `-o Foo.app`, which cannot be
        // honoured and must be refused rather than turned into a flat PE.
        for name in ["Foo.app", "Foo.APP", "Foo.App"] {
            let cli = Cli::parse_from(["snug", "app.jar", "-o", name]);
            assert!(
                wants_app_bundle(&cli),
                "{name} should read as a bundle request"
            );
        }
        for name in ["Foo.exe", "Foo.EXE", "App.applescript", "Foo"] {
            let cli = Cli::parse_from(["snug", "app.jar", "-o", name]);
            assert!(
                !wants_app_bundle(&cli),
                "{name} should not read as a bundle request"
            );
        }
    }

    #[test]
    fn explicit_output_always_wins_over_the_default() {
        // Including when it disagrees with the default: a macOS snug
        // asked for `.exe` means it, and must not be talked into a
        // bundle. That is how Windows artefacts get built from a Mac.
        let cli = Cli::parse_from(["snug", "app.jar", "-o", "Windows.exe"]);
        assert_eq!(output_path(&cli), PathBuf::from("Windows.exe"));
        assert!(!wants_app_bundle(&cli));
    }

    // --- output/input collision -----------------------------------------

    #[test]
    fn output_aliasing_the_input_jar_is_refused() {
        // The data-loss case: every input is already in memory by the time
        // the output is written, so this would succeed and eat the archive.
        let dir = tempdir();
        let jar = dir.join("demo.jar");
        write_fake_jar(&jar, Some("com.example.Main"));
        let cli = Cli::parse_from(["snug", jar.to_str().unwrap(), "-o", jar.to_str().unwrap()]);
        let err = guard_output_collision(&cli).unwrap_err().to_string();
        assert!(err.contains("input files"), "unhelpful error: {err}");
        assert!(err.contains("demo.jar"), "error omits the path: {err}");
    }

    #[test]
    fn output_aliasing_icon_splash_or_manifest_is_refused() {
        let dir = tempdir();
        let jar = dir.join("demo.jar");
        write_fake_jar(&jar, Some("com.example.Main"));
        let icon = dir.join("logo.png");
        std::fs::write(&icon, b"png").unwrap();
        let splash = dir.join("boot.png");
        std::fs::write(&splash, b"png").unwrap();
        let manifest = dir.join("app.manifest");
        std::fs::write(&manifest, b"<assembly/>").unwrap();
        let j = jar.to_str().unwrap();

        for extra in [
            vec!["--icon", icon.to_str().unwrap()],
            vec!["--splash", splash.to_str().unwrap()],
            vec!["--manifest", manifest.to_str().unwrap()],
        ] {
            let target = extra[1];
            let mut argv = vec!["snug", j, "-o", target];
            argv.extend(extra.clone());
            let cli = Cli::parse_from(argv);
            assert!(
                guard_output_collision(&cli).is_err(),
                "-o {target} should be refused as it is also {extra:?}"
            );
        }
    }

    #[test]
    fn collision_is_compared_canonically_not_textually() {
        // `App.jar`, `.\App.jar` and a differently-cased drive path are the
        // same file on Windows; a string compare would wave the last two
        // straight through.
        let dir = tempdir();
        let jar = dir.join("demo.jar");
        write_fake_jar(&jar, Some("com.example.Main"));
        let spelled = dir.join(".").join("demo.jar");
        let cli = Cli::parse_from(["snug", jar.to_str().unwrap(), "-o", spelled.to_str().unwrap()]);
        assert!(
            guard_output_collision(&cli).is_err(),
            "an equivalent spelling of the input must still be refused"
        );
    }

    #[test]
    fn an_ordinary_output_is_not_refused() {
        // The guard must not fire on the normal shapes, including the
        // default output (which is derived from the input and is `.exe`,
        // so it can never alias a `.jar`).
        let dir = tempdir();
        let jar = dir.join("demo.jar");
        write_fake_jar(&jar, Some("com.example.Main"));

        let plain = Cli::parse_from([
            "snug",
            jar.to_str().unwrap(),
            "-o",
            dir.join("demo.exe").to_str().unwrap(),
        ]);
        assert!(guard_output_collision(&plain).is_ok(), "distinct output refused");

        // Also the no-`-o` default, resolved in a different CWD.
        let implicit = Cli::parse_from(["snug", jar.to_str().unwrap()]);
        assert_eq!(output_path(&implicit), jar.with_extension("exe"));
        assert!(guard_output_collision(&implicit).is_ok(), "default output refused");
    }


    #[test]
    fn the_staging_path_sits_beside_the_output_and_keeps_its_extension() {
        // Suffix, not extension replacement: an editor or sync client
        // watching the directory should recognise `App.exe.tmp-1234` as
        // unfinished rather than as a competing `App.exe.tmp`.
        assert_eq!(
            staged_output_path(std::path::Path::new("C:/out/App.exe")),
            std::path::Path::new(&format!("C:/out/App.exe.tmp-{}", std::process::id()))
        );
        // No extension at all is still handled.
        let bare = staged_output_path(std::path::Path::new("App"));
        assert!(bare.to_string_lossy().starts_with("App.tmp-"));
        // A dotted release name keeps all of it.
        let dotted = staged_output_path(std::path::Path::new("My.App.v1.2.exe"));
        assert!(dotted.to_string_lossy().starts_with("My.App.v1.2.exe.tmp-"));
    }

    #[test]
    fn a_failed_rebuild_leaves_the_previous_build_intact() {
        // The regression this guards: `Image::write_file` is
        // `std::fs::write`, i.e. truncate-in-place, so a write that failed
        // part way through left a broken `App.exe` where a working one had
        // been. Blocking the staging path makes the write fail *after* the
        // payload is fully assembled, which is exactly when the old code
        // had already truncated the output.
        let dir = tempdir();
        let jar = dir.join("demo.jar");
        write_fake_jar(&jar, Some("com.example.Main"));
        let exe = dir.join("App.exe");
        std::fs::write(&exe, b"the previous, working build").unwrap();

        let cli = Cli::parse_from([
            "snug",
            jar.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ]);
        let payload = build_payload(&cli).unwrap();

        // Occupy the staging path with a directory so writing it must fail.
        std::fs::create_dir_all(staged_output_path(&exe)).unwrap();

        assert!(
            build_exe(&cli, &snug_format::SnugEmbedded::new(payload.clone())).is_err(),
            "the staged write should have failed"
        );
        assert_eq!(
            std::fs::read(&exe).unwrap(),
            b"the previous, working build",
            "a failed rebuild must not damage the artefact that already worked"
        );
    }

    #[test]
    fn build_exe_encodes_the_payload_it_is_given_rather_than_rebuilding_it() {
        // The regression this guards is a signature, not a value: `build_exe`
        // used to take `&SnugPayload` and do `SnugEmbedded::new(payload
        // .clone())` internally, while `main.rs` had already built exactly
        // that value one line earlier to satisfy the call. Every build
        // therefore cloned the entire payload -- a second full copy of a
        // 200 MB fat JAR, live alongside the first -- and serialised it a
        // second time, to hand back something the caller was holding.
        //
        // Taking `&SnugEmbedded` makes that structurally impossible: there
        // is no `SnugPayload` left inside to clone, and no second `encode`.
        // A regression would have to change this parameter back, which is
        // exactly what a compile error should catch.
        let dir = tempdir();
        let jar = dir.join("demo.jar");
        write_fake_jar(&jar, Some("com.example.Main"));
        let cli = Cli::parse_from(["snug", jar.to_str().unwrap(), "-o",
            dir.join("demo.exe").to_str().unwrap()]);

        let payload = build_payload(&cli).unwrap();
        let embedded = SnugEmbedded::new(payload);
        let exe = build_exe(&cli, &embedded).expect("build");

        // The embedded value is used verbatim, not re-derived: the written
        // EXE's payload must match the caller's `payload_len` exactly.
        let found = snug_payload::find_in_file(&exe)
            .expect("locate")
            .expect("payload present");
        assert_eq!(
            found.payload_len,
            embedded.payload_len,
            "the EXE must carry the caller's encoded payload, not a rebuilt one"
        );
        assert_eq!(found.payload_crc32, embedded.payload_crc32);
    }

    #[test]
    fn a_successful_build_stages_and_leaves_nothing_behind() {
        let dir = tempdir();
        let jar = dir.join("demo.jar");
        write_fake_jar(&jar, Some("com.example.Main"));
        let exe = dir.join("App.exe");
        let cli = Cli::parse_from([
            "snug",
            jar.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ]);
        let payload = build_payload(&cli).unwrap();

        let embedded = SnugEmbedded::new(payload);
        let written = build_exe(&cli, &embedded).expect("build");
        assert_eq!(written, exe);
        assert!(
            !staged_output_path(&exe).exists(),
            "the staging file must be renamed away, not left on disk"
        );
        assert!(
            std::fs::metadata(&exe).unwrap().len() > 0,
            "the renamed output is the real build"
        );
    }

    fn write_fake_jar(path: &std::path::Path, main_class: Option<&str>) {
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = SimpleFileOptions::default();
        if let Some(mc) = main_class {
            zip.start_file("META-INF/MANIFEST.MF", opts).unwrap();
            let mf = format!("Manifest-Version: 1.0\nMain-Class: {mc}\n");
            zip.write_all(mf.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn reads_main_class_from_jar() {
        let dir = tempdir();
        let jar = dir.join("demo.jar");
        write_fake_jar(&jar, Some("com.example.Main"));
        assert_eq!(
            read_main_class(&jar).unwrap().as_deref(),
            Some("com.example.Main")
        );
    }

    #[test]
    fn returns_none_when_no_manifest_main_class() {
        let dir = tempdir();
        let jar = dir.join("norc.jar");
        write_fake_jar(&jar, None);
        assert_eq!(read_main_class(&jar).unwrap(), None);
    }

    fn tempdir() -> std::path::PathBuf {
        let base = std::env::temp_dir();
        // The counter is load-bearing, not belt-and-braces — see the note
        // on the same helper in `options_file.rs`. pid + nanos collides
        // when parallel tests read the same (coarse) macOS clock tick.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = format!(
            "snug-test-{}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = base.join(unique);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // --- --cache-dir ---------------------------------------------------

    fn cli_for(cache_dir: Option<&str>) -> Cli {
        use clap::Parser;
        let jar = tempdir().join("demo.jar");
        write_fake_jar(&jar, Some("com.example.Main"));
        let mut argv = vec!["snug", jar.to_str().unwrap()];
        if let Some(d) = cache_dir {
            argv.push("--cache-dir");
            argv.push(d);
        }
        Cli::parse_from(argv)
    }

    #[test]
    fn cache_dir_defaults_to_none() {
        let payload = build_payload(&cli_for(None)).unwrap();
        assert_eq!(payload.config.behavior.cache_dir, None);
    }

    #[test]
    fn cache_dir_lands_in_the_payload() {
        let target = tempdir().join("custom-cache");
        let payload = build_payload(&cli_for(Some(target.to_str().unwrap()))).unwrap();
        assert_eq!(
            payload.config.behavior.cache_dir.as_deref(),
            Some(target.as_path())
        );
    }

    #[test]
    fn cache_dir_survives_a_payload_roundtrip() {
        use snug_format::{decode, encode, SnugEmbedded};
        let target = tempdir().join("roundtrip-cache");
        let embedded = SnugEmbedded::new(build_payload(&cli_for(Some(target.to_str().unwrap()))).unwrap());
        let decoded = decode(&encode(&embedded).unwrap()).unwrap();
        assert_eq!(
            decoded.payload.config.behavior.cache_dir.as_deref(),
            Some(target.as_path())
        );
    }

    #[test]
    fn relative_cache_dir_is_rejected_at_the_cli() {
        use clap::Parser;
        let err = Cli::try_parse_from(["snug", "app.jar", "--cache-dir", "relative/path"])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("absolute"),
            "expected an absolute-path complaint, got: {err}"
        );
    }
}
