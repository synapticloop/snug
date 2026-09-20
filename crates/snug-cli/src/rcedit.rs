//! Optional Windows resource stamping via `rcedit.exe`.
//!
//! After the stub launcher + payload are concatenated into the final
//! `.exe`, this module invokes `rcedit` (from
//! <https://github.com/electron/rcedit>) to stamp:
//!
//! - the application `.ico` as the Windows Explorer icon,
//! - the version-resource strings (`ProductName`, `CompanyName`,
//!   `FileDescription`, `LegalCopyright`, `FileVersion`,
//!   `ProductVersion`).
//!
//! **Platform behaviour:**
//!
//! - **Windows:** rcedit runs if available. If unavailable and any
//!   resource fields were supplied, we fail loudly so the user knows
//!   the produced EXE has stub defaults.
//! - **macOS / Linux:** skipped by default. Users who want resource
//!   stamping on those hosts must run `rcedit` via Wine and pass the
//!   path with `--rcedit`.
//!
//! The `rcedit` binary is **not** bundled with snug — users install it
//! themselves (e.g. `winget install rcedit`, `choco install rcedit`, or
//! download from GitHub releases).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use snug_format::SnugEmbedded;

use crate::cli::Cli;

/// Resolve the effective rcedit configuration from CLI flags.
#[derive(Debug, Clone)]
pub struct RceditPlan {
    /// Whether to run rcedit at all. `false` when `--no-rcedit` was set
    /// or when nothing would actually be stamped.
    enabled: bool,
    /// Explicit rcedit path from `--rcedit`, if any.
    explicit: Option<PathBuf>,
    /// .ico file to stamp as the application icon (if supplied).
    icon: Option<PathBuf>,
}

impl RceditPlan {
    pub fn from_cli(cli: &Cli) -> Self {
        let mut plan = Self {
            enabled: !cli.no_rcedit,
            explicit: cli.rcedit.clone(),
            icon: cli.icon.clone(),
        };
        if plan.icon.is_none() && !plan.has_version_fields(cli) {
            // Nothing to stamp — disable automatically.
            plan.enabled = false;
        }
        plan
    }

    fn has_version_fields(&self, cli: &Cli) -> bool {
        cli.name.is_some()
            || cli.company.is_some()
            || cli.version.is_some()
            || cli.description.is_some()
            || cli.copyright.is_some()
    }

    /// Whether this plan actually wants to run rcedit.
    pub fn should_run(&self) -> bool {
        self.enabled && (self.icon.is_some() || cfg!(windows))
    }

    /// Run rcedit against `exe`, applying the supplied metadata.
    pub fn run(&self, exe: &Path, payload: &SnugEmbedded) -> Result<()> {
        // Default behaviour on non-Windows hosts: skip unless the user
        // explicitly asked for an rcedit binary.
        if !cfg!(windows) && self.explicit.is_none() {
            return Ok(());
        }

        let rcedit = match &self.explicit {
            Some(p) => p.clone(),
            None => PathBuf::from("rcedit"),
        };

        let mut cmd = Command::new(&rcedit);
        cmd.arg(exe);

        // --set-icon <ico>
        if let Some(icon) = &self.icon {
            cmd.arg("--set-icon").arg(icon);
        }

        // --set-version-string <key> <value>
        let app = &payload.payload.config.app;
        let set = |cmd: &mut Command, key: &str, value: &str| {
            cmd.arg("--set-version-string").arg(key).arg(value);
        };
        if let Some(v) = app.description.as_deref().or(Some(app.name.as_str())) {
            set(&mut cmd, "FileDescription", v);
        }
        set(&mut cmd, "ProductName", &app.name);
        set(&mut cmd, "CompanyName", &app.company);
        if let Some(v) = &app.copyright {
            set(&mut cmd, "LegalCopyright", v);
        }

        // --set-file-version / --set-product-version expect M.m.p.s.
        let version_str = normalize_version(&app.version);
        if !version_str.is_empty() {
            cmd.arg("--set-file-version").arg(&version_str);
            cmd.arg("--set-product-version").arg(&version_str);
        }

        if let Some(icon) = &self.icon {
            eprintln!(
                "snug: stamping icon {} into {} via {}",
                icon.display(),
                exe.display(),
                rcedit.display()
            );
        } else {
            eprintln!(
                "snug: stamping version-resource fields into {} via {}",
                exe.display(),
                rcedit.display()
            );
        }

        let status = cmd
            .status()
            .with_context(|| format!("spawning rcedit ({})", rcedit.display()))?;

        if !status.success() {
            bail!(
                "rcedit {} failed with exit status {:?}",
                rcedit.display(),
                status.code()
            );
        }

        // Validate that the icon path actually exists; rcedit is silent
        // when given a missing icon.
        if let Some(icon) = &self.icon {
            if !icon.is_file() {
                return Err(anyhow!(
                    "--icon {} does not exist or is not a regular file",
                    icon.display()
                ));
            }
        }

        Ok(())
    }
}

/// Normalise a user-supplied version string like `"1"`, `"1.2"`, or
/// `"1.2.3"` into the `M.m.p.s` (major.minor.patch.build) form that
/// rcedit / VERSIONINFO expects. An empty string means "don't stamp
/// version fields".
fn normalize_version(input: &str) -> String {
    let parts: Vec<u32> = input
        .split('.')
        .filter_map(|s| s.parse::<u32>().ok())
        .collect();
    if parts.is_empty() {
        return String::new();
    }
    let mut padded = [0u32; 4];
    for (i, p) in parts.iter().enumerate().take(4) {
        padded[i] = *p;
    }
    padded
        .iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(".")
}

#[allow(dead_code)]
fn _silence_osstr(_: &OsStr) {}