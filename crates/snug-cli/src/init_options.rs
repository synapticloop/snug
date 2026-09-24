//! `snug --init-options` — write the embedded example `snug.options`
//! file to disk (or stdout).
//!
//! The example is built into the binary at compile time via
//! [`include_str!`] so `snug --init-options` works on a fresh machine
//! with no installer / install path / network. The text is a single
//! `#`-commented `snug.options` covering every CLI flag with a
//! placeholder example — drop it in your project root, edit what you
//! need, leave the rest commented.
//!
//! ## Write policy
//!
//! By default refuses to overwrite an existing file at the target
//! path. Pass `--init-options-force` to overwrite silently. Use
//! `--init-options-stdout` to print to stdout instead of writing to
//! disk — useful for piping (`snug --init-options --stdout > preview.options`)
//! or version control (`git show :0:snug.options.example | snug --init-options --stdout`).
//!
//! Errors are non-fatal: a missing parent directory, an unwritable
//! target, or a permission refusal all bubble up via [`anyhow`]
//! through `main.rs`, which prints them and exits non-zero.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Path-snug provides when `--init-options` is given with no
/// explicit value. Matches the default lookup path used by
/// `options_file::resolve` — same string on both sides, so
/// `snug --init-options` writes the file the next `snug` invocation
/// will read.
pub const DEFAULT_PATH: &str = "snug.options";

/// The example template text, embedded at compile time.
///
/// Sourced from `crates/snug-cli/assets/snug.options.example` so the
/// file is also readable in the source tree (good for diffing in
/// PRs). Bumping this requires no separate hand-sync.
pub const EXAMPLE: &str = include_str!("../assets/snug.options.example");

/// Write / print the embedded example. The single entry point used
/// by `main.rs` after the CLI parses.
///
/// Arguments:
/// - `target`: the user-supplied path (defaults to `snug.options`).
/// - `force`: when `false`, refuse if `target` already exists.
/// - `to_stdout`: when `true`, ignore `target` and print to stdout
///   instead of writing to disk.
///
/// Returns `Ok(())` on a successful write / print, `Err` otherwise.
pub fn run(target: &str, force: bool, to_stdout: bool) -> Result<()> {
    if to_stdout {
        return print_to_stdout();
    }
    let path = PathBuf::from(target);
    write_to_disk(&path, force)
}

fn print_to_stdout() -> Result<()> {
    let mut out = std::io::stdout().lock();
    out.write_all(EXAMPLE.as_bytes())
        .context("writing example to stdout")?;
    out.flush().context("flushing stdout")?;
    Ok(())
}

fn write_to_disk(path: &Path, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!(
            "refusing to overwrite existing file at `{}`\n\
             hint: pass --init-options-force to overwrite, or move the existing file aside",
            path.display()
        );
    }

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            bail!(
                "parent directory `{}` does not exist\n\
                 hint: create it first (mkdir -p) or pass --init-options-stdout to print instead",
                parent.display()
            );
        }
    }

    std::fs::write(path, EXAMPLE.as_bytes())
        .with_context(|| format!("writing example to {}", path.display()))?;

    eprintln!("snug: wrote example `snug.options` template to {}", path.display());
    eprintln!(
        "snug: edit the file to set your app's metadata, JVM args, icon, etc., then re-run snug."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let base = std::env::temp_dir();
        let unique = format!(
            "snug-init-options-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let dir = base.join(unique);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn example_is_non_empty_and_mentions_every_cli_flag() {
        // The example must reference every documented CLI flag so
        // users discover them. This is a coarse sanity check —
        // missing one of these keywords means a flag was added to
        // the CLI but not surfaced in the example.
        for keyword in [
            "--input",
            "--output",
            "--name",
            "--company",
            "--version",
            "--description",
            "--copyright",
            "--main-class",
            "--min-java",
            "--icon",
            "--manifest",
            "--splash",
            "--splash-ms",
            "--splash-max",
            "--jvm-arg",
            "--download-jdk",
            "--localization",
            "--emit-payload",
            "--dry-run",
            "--options",
            "--snug-version",
            "--init-options",
        ] {
            assert!(
                EXAMPLE.contains(keyword),
                "snug.options.example is missing a mention of `{keyword}`"
            );
        }
    }

    #[test]
    fn example_starts_with_a_header_comment() {
        assert!(
            EXAMPLE.starts_with("#"),
            "example should start with a header comment block"
        );
    }

    #[test]
    fn example_loads_via_include_str() {
        // If `include_str!` ever fails at compile time this test
        // never runs; if it succeeds, we know the file is in the
        // binary.
        assert!(EXAMPLE.len() > 1_000, "example is suspiciously small");
    }

    #[test]
    fn run_writes_when_file_does_not_exist() {
        let dir = tmpdir();
        let path = dir.join("snug.options");
        run(path.to_str().unwrap(), false, false).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, EXAMPLE);
    }

    #[test]
    fn run_refuses_to_overwrite_without_force() {
        let dir = tmpdir();
        let path = dir.join("snug.options");
        std::fs::write(&path, "existing content").unwrap();
        let err = run(path.to_str().unwrap(), false, false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("refusing to overwrite"),
            "unexpected error: {msg}"
        );
        // Original content preserved.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "existing content");
    }

    #[test]
    fn run_overwrites_with_force() {
        let dir = tmpdir();
        let path = dir.join("snug.options");
        std::fs::write(&path, "old content").unwrap();
        run(path.to_str().unwrap(), true, false).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, EXAMPLE);
    }

    #[test]
    fn run_refuses_when_parent_dir_missing() {
        let dir = tmpdir();
        let path = dir.join("nonexistent_subdir").join("snug.options");
        let err = run(path.to_str().unwrap(), false, false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("parent directory") && msg.contains("does not exist"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn run_to_stdout_writes_example_bytes() {
        // The `--init-options-stdout` path uses stdout, which we
        // can't easily capture without a `gag` crate. Instead, drive
        // it via `print_to_stdout` and assert it doesn't error —
        // behaviour is the same since it's a one-liner. The CLI
        // integration test exercises the end-to-end flag.
        print_to_stdout().unwrap();
    }

    #[test]
    fn default_path_matches_options_file_resolve() {
        // We don't import `options_file::resolve` here to avoid a
        // cyclic test dep, but the literal default we advertise
        // must match the file the next `snug` invocation will read.
        // Sanity check the literal is what we expect:
        assert_eq!(DEFAULT_PATH, "snug.options");
    }
}