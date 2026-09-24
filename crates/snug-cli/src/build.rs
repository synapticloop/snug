//! Assemble a [`SnugPayload`] from parsed CLI args + loaded inputs, and
//! turn it into the final Windows `.exe`.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use editpe::Image;
use image::GenericImageView;

use crate::cli::Cli;
use crate::localization;
use crate::manifest::read_main_class;
use crate::resources::ResourcePlan;
use crate::stub::STUB_BYTES;
use snug_launcher::payload_locator::PAYLOAD_RESOURCE_NAME;
use snug_format::{
    embedded_file, encode, AppMetadata, EmbeddedFile, LauncherBehavior, LauncherConfig,
    SnugEmbedded, SnugPayload, SplashConfig, SplashImage,
};

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
    let (input_path, jars) = resolve_input(cli)?;

    // --- Resolve main class ---------------------------------------------
    // For multi-JAR builds, Main-Class is read from the first JAR's
    // manifest. CLI `--main-class` always overrides.
    let main_class_jar = jars.first().map(|ef| &ef.bytes).ok_or_else(|| {
        anyhow::anyhow!("at least one JAR is required to read Main-Class")
    })?;
    let main_class = match &cli.main_class {
        Some(cls) => Some(cls.clone()),
        None => {
            // We need a path for read_main_class; if the input was a
            // directory, write the first JAR to a temp file so the
            // existing manifest parser can read it. Cleaner: refactor
            // read_main_class to take bytes directly. For now, read
            // from a temp file.
            let tmp = std::env::temp_dir().join(format!(
                "snug-manifest-{}-{}.jar",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            std::fs::write(&tmp, main_class_jar)?;
            let result = read_main_class(&tmp);
            let _ = std::fs::remove_file(&tmp);
            result.context("reading Main-Class from JAR manifest")?
        }
    };

    // --- Optional icon ---------------------------------------------------
    let icon = match &cli.icon {
        Some(path) => Some(load_embedded_file(path, "icon")?),
        None => None,
    };

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
            ..LauncherBehavior::default()
        },
    };

    Ok(SnugPayload {
        config,
        jars,
        icon,
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
    paths.sort_by(|a, b| a.cmp(b));
    let bundles = localization::collect(&paths)?;
    localization::ensure_unique_tags(&bundles)
        .context("validating localization bundle tags")?;
    Ok(bundles)
}

/// Resolve `[JAR]` vs `--input <JAR|DIR>` and return the resolved
/// path plus the embedded JAR(s).
fn resolve_input(cli: &Cli) -> Result<(PathBuf, Vec<EmbeddedFile>)> {
    let input_path = match (&cli.jar, &cli.input) {
        (Some(p), None) => p.clone(),
        (None, Some(p)) => p.clone(),
        (None, None) => bail!(
            "an input source is required (pass a JAR as the positional argument, or use --input <JAR|DIR>)"
        ),
        (Some(_), Some(_)) => bail!(
            "the positional [JAR] argument and --input are mutually exclusive"
        ),
    };

    let meta = std::fs::metadata(&input_path)
        .with_context(|| format!("stat-ing input {}", input_path.display()))?;

    if meta.is_file() {
        // Single-JAR input.
        let bytes = std::fs::read(&input_path)
            .with_context(|| format!("reading JAR {}", input_path.display()))?;
        if bytes.is_empty() {
            bail!("{} is empty", input_path.display());
        }
        Ok((input_path, vec![embedded_file(bytes)]))
    } else if meta.is_dir() {
        // Multi-JAR input: scan the directory for `*.jar`, sort by
        // filename for determinism.
        let mut jar_paths: Vec<PathBuf> = std::fs::read_dir(&input_path)
            .with_context(|| format!("reading directory {}", input_path.display()))?
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let path = entry.path();
                if path.is_file()
                    && path.extension().and_then(|e| e.to_str()) == Some("jar")
                {
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
                input_path.display()
            );
        }

        let mut jars = Vec::with_capacity(jar_paths.len());
        for jar_path in &jar_paths {
            let bytes = std::fs::read(jar_path)
                .with_context(|| format!("reading JAR {}", jar_path.display()))?;
            if bytes.is_empty() {
                bail!("{} is empty", jar_path.display());
            }
            jars.push(embedded_file(bytes));
        }
        eprintln!(
            "snug: indexed {} JAR(s) from {}",
            jars.len(),
            input_path.display()
        );
        Ok((input_path, jars))
    } else {
        bail!(
            "{} is neither a regular file nor a directory",
            input_path.display()
        );
    }
}

fn load_embedded_file(path: &std::path::Path, label: &str) -> Result<EmbeddedFile> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("reading {} file {}", label, path.display()))?;
    if bytes.is_empty() {
        bail!("{} file {} is empty", label, path.display());
    }
    Ok(embedded_file(bytes))
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
    if matches!(low.as_str(), "off" | "none" | "unlimited" | "disable" | "no" | "false") {
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

/// Resolve the final `.exe` output path from the CLI args.
///
/// Default: `<input-stem>.exe` next to the input JAR / directory.
pub fn output_path(cli: &Cli) -> PathBuf {
    match &cli.output {
        Some(p) => p.clone(),
        None => {
            let default_path = match (&cli.jar, &cli.input) {
                (Some(j), None) => Some(j.with_extension("exe")),
                (None, Some(i)) => Some(i.with_extension("exe")),
                _ => None,
            };
            default_path.unwrap_or_else(|| PathBuf::from("App.exe"))
        }
    }
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
pub fn build_exe(cli: &Cli, payload: &SnugPayload) -> Result<PathBuf> {
    let output = output_path(cli);

    let embedded = SnugEmbedded::new(payload.clone());
    let encoded = encode(&embedded).context("encoding snug payload")?;

    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating output directory {}", parent.display()))?;
        }
    }

    // Open the stub as a PE image so we can mutate its resource
    // directory in place.
    let mut image = Image::parse(STUB_BYTES).context("parsing embedded stub as PE image")?;

    let mut resources = image
        .resource_directory()
        .cloned()
        .unwrap_or_default();

    // Embed the snug payload as an RT_RCDATA resource entry.
    resources
        .set_rcdata(PAYLOAD_RESOURCE_NAME, encoded.clone())
        .context("embedding snug payload as RT_RCDATA resource")?;

    // Stamp icon / manifest / version from CLI args.
    let plan = ResourcePlan::from_cli(cli);
    plan.apply(&mut resources, &embedded)
        .context("stamping icon / manifest / version resources")?;

    image
        .set_resource_directory(resources)
        .context("installing resource directory onto stub image")?;
    image
        .write_file(&output)
        .with_context(|| format!("writing EXE to {}", output.display()))?;

    let final_size = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
    eprintln!(
        "snug: wrote {} ({} bytes; payload {} as RT_RCDATA)",
        output.display(),
        final_size,
        encoded.len(),
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
    use std::io::Write;
    use zip::write::SimpleFileOptions;

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
        let unique = format!(
            "snug-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = base.join(unique);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
