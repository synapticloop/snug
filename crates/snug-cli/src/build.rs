//! Assemble a [`SnugPayload`] from parsed CLI args + loaded inputs, and
//! turn it into the final Windows `.exe`.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::cli::Cli;
use crate::manifest::read_main_class;
use crate::resources::ResourcePlan;
use crate::stub::STUB_BYTES;
use snug_format::{
    embedded_file, encode, AppMetadata, EmbeddedFile, LauncherBehavior, LauncherConfig,
    SnugEmbedded, SnugPayload, SplashConfig,
};

/// Build the full [`SnugPayload`] that the launcher will consume.
///
/// Reads the JAR bytes, hashes them, optionally loads icon / splash files,
/// reads `Main-Class` from the manifest (unless overridden), and assembles
/// a complete [`SnugPayload`].
pub fn build_payload(cli: &Cli) -> Result<SnugPayload> {
    let jar_path = cli
        .jar
        .as_ref()
        .context("a JAR path is required (pass it as the first positional argument)")?;

    // --- Validate the JAR ------------------------------------------------
    let jar_meta = std::fs::metadata(jar_path)
        .with_context(|| format!("stat-ing JAR {}", jar_path.display()))?;
    if !jar_meta.is_file() {
        bail!("{} is not a regular file", jar_path.display());
    }
    let jar_bytes = std::fs::read(jar_path)
        .with_context(|| format!("reading JAR {}", jar_path.display()))?;
    let jar = embedded_file(jar_bytes);

    // --- Resolve main class ---------------------------------------------
    let main_class = match &cli.main_class {
        Some(cls) => Some(cls.clone()),
        None => read_main_class(jar_path).context("reading Main-Class from JAR manifest")?,
    };

    // --- Optional icon ---------------------------------------------------
    let icon = match &cli.icon {
        Some(path) => Some(load_embedded_file(path, "icon")?),
        None => None,
    };

    // --- Optional splash -------------------------------------------------
    let splash = match &cli.splash {
        Some(path) => {
            let image = load_embedded_file(path, "splash")?;
            Some(SplashConfig {
                duration_ms: cli.splash_ms,
                image,
            })
        }
        None => None,
    };

    // --- App metadata defaults ------------------------------------------
    let name = cli
        .name
        .clone()
        .or_else(|| default_name_from_jar(jar_path))
        .context("--name is required when it cannot be inferred from the JAR filename")?;
    let company = cli.company.clone().unwrap_or_else(|| "Unknown".to_string());
    let version = cli.version.clone().unwrap_or_else(|| "0.0.0".to_string());

    let app = AppMetadata {
        name,
        company,
        version,
        description: cli.description.clone(),
        copyright: cli.copyright.clone(),
    };

    let config = LauncherConfig {
        app,
        main_class,
        min_java: cli.min_java,
        jvm_args: cli.jvm_args.clone(),
        splash,
        behavior: LauncherBehavior::default(),
    };

    Ok(SnugPayload {
        config,
        jar,
        icon,
    })
}

fn load_embedded_file(path: &std::path::Path, label: &str) -> Result<EmbeddedFile> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("reading {} file {}", label, path.display()))?;
    if bytes.is_empty() {
        bail!("{} file {} is empty", label, path.display());
    }
    Ok(embedded_file(bytes))
}

fn default_name_from_jar(jar: &std::path::Path) -> Option<String> {
    let stem = jar.file_stem()?.to_str()?;
    if stem.is_empty() {
        return None;
    }
    Some(stem.to_owned())
}

/// Resolve the final `.exe` output path from the CLI args.
///
/// Default: `<jar-stem>.exe` next to the input JAR.
pub fn output_path(cli: &Cli) -> PathBuf {
    match &cli.output {
        Some(p) => p.clone(),
        None => match &cli.jar {
            Some(j) => j.with_extension("exe"),
            None => PathBuf::from("App.exe"),
        },
    }
}

/// Build the final Windows `.exe` by concatenating the precompiled
/// stub launcher with the encoded snug payload, then stamping icon /
/// version-resource / manifest metadata via the in-process [`editpe`]
/// library.
///
/// Returns the path that was actually written.
pub fn build_exe(cli: &Cli, payload: &SnugPayload) -> Result<PathBuf> {
    let output = output_path(cli);

    let embedded = SnugEmbedded::new(payload.clone());
    let encoded = encode(&embedded).context("encoding snug payload")?;

    let total_size = STUB_BYTES.len() + encoded.len();
    let mut bytes = Vec::with_capacity(total_size);
    bytes.extend_from_slice(STUB_BYTES);
    bytes.extend_from_slice(&encoded);

    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating output directory {}", parent.display()))?;
        }
    }

    std::fs::write(&output, &bytes)
        .with_context(|| format!("writing EXE to {}", output.display()))?;

    eprintln!(
        "snug: wrote {} ({} bytes; stub {} + payload {})",
        output.display(),
        bytes.len(),
        STUB_BYTES.len(),
        encoded.len()
    );

    // In-process resource stamping (editpe). Version info is always
    // stamped from app metadata; icon and manifest are optional.
    let plan = ResourcePlan::from_cli(cli);
    if plan.should_run() {
        plan.run(&output, &embedded)
            .with_context(|| format!("stamping PE resources on {}", output.display()))?;
    }

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
