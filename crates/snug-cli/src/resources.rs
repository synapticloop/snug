//! In-process PE resource stamping via the [`editpe`] crate.
//!
//! `snug` used to delegate icon / version stamping to an external
//! `rcedit.exe` (or a Wine-runnable copy on non-Windows hosts). That had
//! three structural pains: it required an external install, it made the
//! Mac/Linux dev loop unable to verify the stamp, and it spawned a child
//! process per build. Switching to [`editpe`] gives us a tiny pure-Rust
//! library that runs identically on every host and on every test bench.
//!
//! Stamping is unconditional — version info (product name, company,
//! version, optional description / copyright) is always written from the
//! `AppMetadata`. The icon and application manifest are optional
//! additions driven by `--icon` and `--manifest` respectively.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use editpe::constants::{
    IMAGE_SUBSYSTEM_WINDOWS_GUI, VFT_APP, VOS__WINDOWS32, VS_FIXEDFILEINFO_SIGNATURE,
    VS_FIXEDFILEINFO_VERSION,
};
use editpe::types::{FixedFileInfo, VersionU16, VersionU32};
use editpe::{Image, VersionInfo, VersionStringTable};

use snug_format::{AppMetadata, SnugEmbedded};

use crate::cli::Cli;

/// `LANG_EN_US` (0x0409) + `CP_WINUNICODE` (0x04B0) — the canonical
/// `040904B0` key for an English / Unicode string block.
const VERSION_STRING_TABLE_KEY: &str = "040904B0";

/// A plan describing which PE resources should be stamped into the
/// produced `.exe` after the payload is appended.
#[derive(Debug, Clone, Default)]
pub struct ResourcePlan {
    /// Optional path to an icon file (`.png`, `.ico`, ...). When set, the
    /// icon is read by `editpe` and installed as the main icon group.
    pub icon: Option<PathBuf>,
    /// Optional path to a Windows application manifest (XML). When set,
    /// the manifest's contents are read and embedded as `RT_MANIFEST`.
    pub manifest: Option<PathBuf>,
}

impl ResourcePlan {
    /// Build a `ResourcePlan` from a parsed CLI invocation.
    pub fn from_cli(cli: &Cli) -> Self {
        Self {
            icon: cli.icon.clone(),
            manifest: cli.manifest.clone(),
        }
    }

    /// Stamping is unconditional: the launcher stub carries no useful
    /// version metadata, and `AppMetadata` is always populated (the CLI
    /// has defaults for name / company / version).
    pub fn should_run(&self) -> bool {
        true
    }

    /// Parse `exe`, apply the planned stamp, and write the result back
    /// in-place. The launcher stub's existing subsystem (GUI) is
    /// preserved explicitly so a future stub swap can't silently change
    /// the resulting binary's launch behaviour.
    pub fn run(&self, exe: &Path, payload: &SnugEmbedded) -> Result<()> {
        let mut image = Image::parse_file(exe)
            .with_context(|| format!("parsing {} as a PE image", exe.display()))?;

        let mut resources = image
            .resource_directory()
            .cloned()
            .unwrap_or_default();

        if let Some(icon_path) = &self.icon {
            let icon_str = icon_path.to_str().ok_or_else(|| {
                anyhow::anyhow!(
                    "icon path {} is not valid UTF-8",
                    icon_path.display()
                )
            })?;
            resources
                .set_main_icon_file(icon_str)
                .with_context(|| {
                    format!(
                        "stamping icon from {} (PNG and ICO are supported via the `images` feature)",
                        icon_path.display()
                    )
                })?;
        }

        if let Some(manifest_path) = &self.manifest {
            let manifest_xml = std::fs::read_to_string(manifest_path).with_context(|| {
                format!("reading manifest XML at {}", manifest_path.display())
            })?;
            resources
                .set_manifest(&manifest_xml)
                .with_context(|| {
                    format!(
                        "embedding application manifest from {}",
                        manifest_path.display()
                    )
                })?;
        }

        let version_info = build_version_info(&payload.payload.config.app);
        resources
            .set_version_info(&version_info)
            .context("embedding VERSIONINFO from app metadata")?;

        // Defensive: ensure the produced binary stays a Windows GUI
        // app even if we ever swap stubs.
        image.set_subsystem(IMAGE_SUBSYSTEM_WINDOWS_GUI);

        image
            .set_resource_directory(resources)
            .with_context(|| format!("stamping resource directory into {}", exe.display()))?;

        // Flush the modified image back to disk. `set_resource_directory`
        // only mutates the in-memory representation — without this write
        // the disk file still has the empty stub resource directory.
        image
            .write_file(exe)
            .with_context(|| format!("writing stamped EXE to {}", exe.display()))?;

        Ok(())
    }
}

/// Build the `VersionInfo` block the launcher stamps into every EXE.
///
/// `ProductName` / `CompanyName` always come from `AppMetadata`. The
/// `FileDescription` falls back to `ProductName` when no explicit
/// description is supplied. `LegalCopyright` is only present when an
/// explicit copyright is supplied.
///
/// `FileVersion` / `ProductVersion` are derived from the dotted
/// `app.version` string and packed into the Windows-specific
/// `(MS, LS)` quad form.
fn build_version_info(app: &AppMetadata) -> VersionInfo {
    let version_quad = parse_version_quad(&app.version);

    let fixed = FixedFileInfo {
        signature: VS_FIXEDFILEINFO_SIGNATURE,
        struct_version: VersionU16 {
            major: (VS_FIXEDFILEINFO_VERSION >> 16) as u16,
            minor: (VS_FIXEDFILEINFO_VERSION & 0xFFFF) as u16,
        },
        file_version: version_quad,
        product_version: version_quad,
        file_flags_mask: 0x3F,
        file_flags: 0,
        file_os: VOS__WINDOWS32,
        file_type: VFT_APP,
        file_subtype: 0,
        file_date: 0,
    };

    let mut string_table = VersionStringTable {
        key: VERSION_STRING_TABLE_KEY.to_string(),
        ..VersionStringTable::default()
    };
    string_table.strings.insert("ProductName".to_string(), app.name.clone());
    string_table
        .strings
        .insert("CompanyName".to_string(), app.company.clone());
    string_table.strings.insert(
        "FileDescription".to_string(),
        app.description.clone().unwrap_or_else(|| app.name.clone()),
    );
    if let Some(copyright) = &app.copyright {
        string_table
            .strings
            .insert("LegalCopyright".to_string(), copyright.clone());
    }

    VersionInfo {
        info: fixed,
        strings: vec![string_table],
        vars: vec![],
    }
}

/// Parse `"A.B.C.D"` (each component optional, defaults to 0) and pack
/// it into the Windows `VS_FIXEDFILEINFO` representation:
/// `major: (MAJOR << 16) | MINOR`, `minor: (PATCH << 16) | BUILD`.
fn parse_version_quad(s: &str) -> VersionU32 {
    let mut parts = s.split('.').filter_map(|p| p.parse::<u16>().ok());
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    let patch = parts.next().unwrap_or(0);
    let build = parts.next().unwrap_or(0);
    VersionU32 {
        major: ((major as u32) << 16) | (minor as u32),
        minor: ((patch as u32) << 16) | (build as u32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(version: &str) -> AppMetadata {
        AppMetadata {
            name: "Demo".into(),
            company: "SynapticLoop".into(),
            version: version.into(),
            description: None,
            copyright: None,
        }
    }

    #[test]
    fn parse_full_version_quad() {
        let v = parse_version_quad("1.2.3.4");
        // MS = (1<<16)|2  == 0x00010002
        // LS = (3<<16)|4  == 0x00030004
        assert_eq!(v.major, 0x0001_0002);
        assert_eq!(v.minor, 0x0003_0004);
    }

    #[test]
    fn parse_partial_version_quad_pads_with_zeros() {
        let v = parse_version_quad("25");
        assert_eq!(v.major, 0x0019_0000);
        assert_eq!(v.minor, 0x0000_0000);
    }

    #[test]
    fn parse_garbage_version_quad_falls_back_to_zero() {
        let v = parse_version_quad("not.a.version");
        assert_eq!(v.major, 0);
        assert_eq!(v.minor, 0);
    }

    #[test]
    fn build_version_info_uses_app_metadata() {
        let app = meta("2.4.6.8");
        let info = build_version_info(&app);
        assert_eq!(info.info.signature, VS_FIXEDFILEINFO_SIGNATURE);
        assert_eq!(info.info.file_type, VFT_APP);
        assert_eq!(info.info.file_version.major, 0x0002_0004);
        assert_eq!(info.info.file_version.minor, 0x0006_0008);
        assert_eq!(info.info.product_version.major, 0x0002_0004);
        assert_eq!(info.info.product_version.minor, 0x0006_0008);
        assert_eq!(info.strings.len(), 1);
        let table = &info.strings[0];
        assert_eq!(table.key, VERSION_STRING_TABLE_KEY);
        assert_eq!(table.strings.get("ProductName").map(String::as_str), Some("Demo"));
        assert_eq!(
            table.strings.get("CompanyName").map(String::as_str),
            Some("SynapticLoop")
        );
        // Description falls back to name when none is set.
        assert_eq!(
            table.strings.get("FileDescription").map(String::as_str),
            Some("Demo")
        );
    }

    #[test]
    fn build_version_info_prefers_explicit_description_and_copyright() {
        let app = AppMetadata {
            name: "Demo".into(),
            company: "Co".into(),
            version: "1.0.0".into(),
            description: Some("Does things".into()),
            copyright: Some("© 2026 Co".into()),
        };
        let info = build_version_info(&app);
        let table = &info.strings[0];
        assert_eq!(
            table.strings.get("FileDescription").map(String::as_str),
            Some("Does things")
        );
        assert_eq!(
            table.strings.get("LegalCopyright").map(String::as_str),
            Some("© 2026 Co")
        );
    }
}
