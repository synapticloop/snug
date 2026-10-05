//! Wrap a Mach-O in a double-clickable macOS `.app`.
//!
//! Two snug artefacts are things a user *clicks* rather than types:
//! `snug_preview` and the `Build with Snug` dropper. Both ship as
//! bundles, because that is what macOS gives an icon to — a bare
//! executable gets its icon from an `__TEXT,__icns` section inside the
//! Mach-O, which works but is invisible to Finder's document plumbing and
//! cannot declare that the app accepts `.jar` drops. The one artefact
//! that is genuinely a terminal command, `snug`, deliberately does not
//! use this: it stays a bare binary and carries no icon at all.
//!
//! ```text
//! snug_preview.app/
//! └── Contents/
//!     ├── Info.plist
//!     ├── MacOS/
//!     │   └── snug_preview       <- the Mach-O, mode 0755
//!     └── Resources/
//!         └── App.icns           <- from snug-preview.png
//! ```
//!
//! ## Why this is a crate and not shell in the build script
//!
//! `macos_bundle.rs` already writes this layout for the *product* app, but
//! it is `#![cfg(target_os = "macos")]` inside `snug-cli` and is welded to
//! a launcher-plus-payload. These two artefacts are neither: they ship a
//! plain Mach-O and an icon and nothing else. Reusing the code rather
//! than re-shelling it also means the icon comes from `snug-icns` —
//! pure Rust — instead of the `iconutil` subprocess, so a missing icon is
//! a loud failure here rather than a bundle that quietly ships generic.
//!
//! ## Document types
//!
//! [`DocumentType`] is what makes drag-and-drop possible at all. Finder
//! will not hand a bundle a dropped file unless `CFBundleDocumentTypes`
//! claims it, so a dropper without this block is a bundle a user can
//! double-click and nothing more.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Conventional modes for a bundle. Each one is load-bearing.
///
/// | Path                          | Mode   | Why not something else |
/// |-------------------------------|--------|------------------------|
/// | bundle directories            | `0755` | a directory needs `x` to be **traversable**; `0644` makes the bundle invisible to `execve` |
/// | `Contents/MacOS/…`            | `0755` | needs `x` to execute |
/// | `Info.plist`, `Resources/…`   | `0644` | data, not code |
///
/// The executable is `0755` rather than a hardened `0555`. Both run, but
/// `0555` is not writable, which breaks `codesign` — and an ad-hoc
/// signature is exactly what we emit, so `0555` would make the artefact
/// un-signable. `0755` plus an explicit signature is the normal Xcode
/// arrangement.
const BUNDLE_DIR_MODE: u32 = 0o755;
const EXECUTABLE_MODE: u32 = 0o755;
const DATA_FILE_MODE: u32 = 0o644;

/// Deployment floor stamped into `LSMinimumSystemVersion`.
///
/// Tracked deliberately rather than left to the OS: it is the floor the
/// embedded Mach-O was compiled against, and the same value
/// `scripts/build-macos.sh` pins. Keep the two in step.
pub const MIN_SYSTEM_VERSION: &str = "12.0";

/// A file type Finder may drop onto the bundle.
#[derive(Debug, Clone)]
pub struct DocumentType {
    /// Human label, shown if the app appears in Open With.
    pub name: String,
    /// Lowercase extension without the dot — `jar`.
    pub extension: String,
    /// UTType identifier. `public.jar` is the modern spelling; the
    /// legacy `dyn.*` form is still honoured by older systems.
    pub uti: String,
}

impl DocumentType {
    /// A `.jar` claim, which is what both artefacts accept.
    pub fn jar() -> Self {
        Self {
            name: "Java Archive".to_string(),
            extension: "jar".to_string(),
            uti: "public.jar".to_string(),
        }
    }
}

/// Everything needed to lay out one bundle.
#[derive(Debug, Clone)]
pub struct AppSpec {
    /// Where the `.app` directory is written.
    pub bundle: PathBuf,
    /// Display name. Also the `Contents/MacOS/` executable name and the
    /// value Finder shows, so `Build with Snug.app` is built with a
    /// `Build with Snug` name.
    pub name: String,
    /// Reverse-DNS bundle identifier. Must be unique per bundle on the
    /// machine, or Launch Services conflates them.
    pub identifier: String,
    /// `CFBundleShortVersionString`.
    pub version: String,
    /// The Mach-O to install at `Contents/MacOS/<name>`.
    pub binary: PathBuf,
    /// Source artwork for `Contents/Resources/App.icns`. `None` ships a
    /// bundle with no icon — legitimate for a tool that is never
    /// double-clicked, and never silently substituted with something
    /// else.
    pub icon_png: Option<PathBuf>,
    /// File types Finder may drop onto the bundle. Empty means the
    /// bundle cannot receive drops.
    pub document_types: Vec<DocumentType>,
}

/// Build the bundle described by `spec`, replacing any existing one.
///
/// The icon is **not** best-effort. If artwork was supplied and the ICNS
/// cannot be written, that is an error: a `CFBundleIconFile` pointing at
/// a file that does not exist yields a generic icon, which is exactly
/// the cosmetic failure nobody notices until a user reports it.
pub fn write_app(spec: &AppSpec) -> Result<()> {
    prepare_bundle_dir(&spec.bundle)?;

    let contents = spec.bundle.join("Contents");
    let macos_dir = contents.join("MacOS");
    let resources_dir = contents.join("Resources");
    for dir in [&macos_dir, &resources_dir] {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        set_mode(dir, BUNDLE_DIR_MODE)?;
    }

    // 1. The Mach-O, copied byte-for-byte. Nothing is stamped into it:
    //    unlike a PE there is no resource directory to write, so the
    //    icon travels beside the binary as App.icns.
    let executable = macos_dir.join(&spec.name);
    fs::copy(&spec.binary, &executable).with_context(|| {
        format!(
            "installing {} into {}",
            spec.binary.display(),
            executable.display()
        )
    })?;
    set_mode(&executable, EXECUTABLE_MODE)?;

    // 2. The icon.
    let icon_written = match &spec.icon_png {
        Some(png) => {
            let icns = resources_dir.join("App.icns");
            snug_icns::try_write_icns(&png.to_string_lossy(), &icns)
                .with_context(|| format!("building App.icns from {}", png.display()))?;
            set_mode(&icns, DATA_FILE_MODE)?;
            true
        }
        None => false,
    };

    // 3. Info.plist last, so it already reflects whether there is an icon
    //    for CFBundleIconFile to point at.
    let plist = info_plist(spec, icon_written);
    let plist_path = contents.join("Info.plist");
    fs::write(&plist_path, plist)
        .with_context(|| format!("writing {}", plist_path.display()))?;
    set_mode(&plist_path, DATA_FILE_MODE)?;

    // 4. Sign. arm64 will not run an unsigned binary, so this is not
    //    optional polish.
    codesign(&spec.bundle)
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .with_context(|| format!("chmod {mode:o} {}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

/// Render the bundle's `Info.plist`.
///
/// Hand-rolled rather than plist-serialised: the shape is fixed and
/// small, and a build step that cannot fail on a missing dependency is
/// worth more here than a library.
fn info_plist(spec: &AppSpec, icon_written: bool) -> String {
    let mut s = String::new();
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    s.push_str(
        "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
    );
    s.push_str("<plist version=\"1.0\">\n<dict>\n");

    let entry = |s: &mut String, key: &str, value: &str| {
        s.push_str(&format!("\t<key>{key}</key>\n\t<string>{}</string>\n", escape(value)));
    };

    entry(&mut s, "CFBundleName", &spec.name);
    entry(&mut s, "CFBundleDisplayName", &spec.name);
    entry(&mut s, "CFBundleIdentifier", &spec.identifier);
    entry(&mut s, "CFBundlePackageType", "APPL");
    entry(&mut s, "CFBundleVersion", &spec.version);
    entry(&mut s, "CFBundleShortVersionString", &spec.version);
    entry(&mut s, "CFBundleExecutable", &spec.name);
    entry(&mut s, "LSMinimumSystemVersion", MIN_SYSTEM_VERSION);
    if icon_written {
        entry(&mut s, "CFBundleIconFile", "App");
    }

    if !spec.document_types.is_empty() {
        s.push_str("\t<key>CFBundleDocumentTypes</key>\n\t<array>\n");
        for doc in &spec.document_types {
            s.push_str("\t\t<dict>\n");
            s.push_str("\t\t\t<key>CFBundleTypeName</key>\n");
            s.push_str(&format!("\t\t\t<string>{}</string>\n", escape(&doc.name)));
            s.push_str("\t\t\t<key>CFBundleTypeRole</key>\n\t\t\t<string>Editor</string>\n");
            s.push_str("\t\t\t<key>LSItemContentTypes</key>\n\t\t\t<array>\n");
            s.push_str(&format!("\t\t\t\t<string>{}</string>\n", escape(&doc.uti)));
            s.push_str("\t\t\t</array>\n");
            // The legacy extension list. Still consulted by macOS 12,
            // which is this bundle's deployment floor, so both are
            // written rather than relying on UTType alone.
            s.push_str("\t\t\t<key>CFBundleTypeExtensions</key>\n\t\t\t<array>\n");
            s.push_str(&format!(
                "\t\t\t\t<string>{}</string>\n",
                escape(&doc.extension)
            ));
            s.push_str("\t\t\t</array>\n");
            s.push_str("\t\t</dict>\n");
        }
        s.push_str("\t</array>\n");
    }

    s.push_str("</dict>\n</plist>\n");
    s
}

/// Escape the five XML predefined entities. A name like
/// `Build with Snug` needs none of them, but a path-derived name could,
/// and an unescaped `&` produces a plist Launch Services refuses.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Tell Launch Services to forget a bundle we are about to delete.
///
/// Deleting a registered `.app` leaves a **stale** record pointing at the
/// old path, and rebuilding the same bundle id elsewhere makes Finder
/// keep resolving the id to the dead path — surfacing as
/// `_LSOpenURLsWithCompletionHandler() failed with error -1712` with no
/// hint that the new copy is fine. Unregistering first is cheap and
/// advisory, so the exit status is ignored: a bundle LS never saw is no
/// reason to fail a build.
fn unregister_bundle(path: &Path) {
    const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/\
        Frameworks/LaunchServices.framework/Support/lsregister";
    let _ = Command::new(LSREGISTER).arg("-u").arg(path).output();
}

/// Refuse to clobber anything that is not obviously a snug bundle.
///
/// A `.app` is a directory, so "the output already exists" is ambiguous:
/// silently `rm -rf`-ing whatever sits at a user-supplied path is
/// exactly the kind of thing that eats someone's work. Only a directory
/// that already looks like a bundle is removed.
fn prepare_bundle_dir(bundle: &Path) -> Result<()> {
    if !bundle.exists() {
        return Ok(());
    }
    if !bundle.is_dir() {
        bail!(
            "{} exists and is not a directory.\n\
             hint: a .app must be a directory.",
            bundle.display()
        );
    }
    if !bundle.join("Contents").join("Info.plist").is_file() {
        bail!(
            "{} is a directory but does not look like an app bundle \
             (no Contents/Info.plist).\n\
             refusing to delete it.",
            bundle.display()
        );
    }
    unregister_bundle(bundle);
    fs::remove_dir_all(bundle)
        .with_context(|| format!("removing the previous bundle at {}", bundle.display()))?;
    Ok(())
}

/// Ad-hoc sign the bundle. arm64 refuses to execute an unsigned binary.
fn codesign(bundle: &Path) -> Result<()> {
    let status = Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(bundle)
        .status()
        .with_context(|| "running codesign")?;
    if !status.success() {
        bail!("codesign failed for {}", bundle.display());
    }
    Ok(())
}
