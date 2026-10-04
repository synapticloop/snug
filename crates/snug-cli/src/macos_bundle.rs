//! Emit a macOS `.app` bundle.
//!
//! `snug app.jar -o MyApp.app` produces a double-clickable macOS
//! application; `snug app.jar -o MyApp.exe` produces a Windows one. Same
//! flag, same docs, and the platform is chosen by the extension — which
//! is what lets the documentation say `snug` everywhere without ever
//! naming a platform.
//!
//! ## Why a bundle is a directory
//!
//! A `.app` is a *directory* with a fixed internal layout, and it has to
//! be one: `Info.plist` is a text file that Launch Services and the Dock
//! read, `MacOS/<name>` is a Mach-O executable, and `Resources/` holds
//! everything else. Finder presents the whole directory as one file.
//!
//! ```text
//! MyApp.app/
//! └── Contents/
//!     ├── Info.plist
//!     ├── MacOS/
//!     │   └── MyApp          <- the launcher, mode 0755
//!     └── Resources/
//!         ├── MyApp.snugpayload
//!         └── App.icns
//! ```
//!
//! ## Permissions
//!
//! These are the conventional modes, and each is load-bearing:
//!
//! | Path                | Mode    | Why not something else |
//! |---------------------|---------|------------------------|
//! | bundle directories  | `0755`  | a directory needs `x` to be **traversable** — `0644` would make the bundle invisible to `execve` |
//! | `Contents/MacOS/…`  | `0755`  | needs `x` to execute |
//! | `Contents/Info.plist`, `Resources/…` | `0644` | data, not code |
//!
//! The executable is `0755` rather than the `0555` (`r-x`) a hardened
//! deployment might use. Both run. `0555` is deliberately *not* writable,
//! which is the point of a sealed bundle — but it also breaks
//! `codesign`, and an ad-hoc-signed bundle is exactly what we emit, so
//! `0555` would make the artefact un-signable and un-updatable. `0755`
//! plus an explicit signature is the normal Xcode arrangement.
//!
//! ## Signing
//!
//! arm64 refuses to execute a binary with *no* code signature at all, so
//! the bundle is ad-hoc signed (`codesign -s -`). That is enough to
//! *run*. Satisfying Gatekeeper for a browser-downloaded bundle needs a
//! Developer ID plus notarisation, which is out of scope here.

#![cfg(target_os = "macos")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use snug_format::SnugPayload;

use crate::build::output_path;
use crate::cli::Cli;

/// The precompiled launcher for the architecture `snug` itself was built
/// for.
///
/// Selected by `#[cfg(target_arch)]` so each shipped `snug` binary can
/// only build a `.app` for its own architecture — one launcher per
/// artefact, never two. `snug` in `release/macos-arm64/` embeds the arm64
/// launcher; `release/macos-x86_64/snug` embeds the Intel one.
#[cfg(target_arch = "aarch64")]
pub const LAUNCHER: &[u8] = include_bytes!("../../../bin/launcher-stub-macos-arm64");

#[cfg(target_arch = "x86_64")]
pub const LAUNCHER: &[u8] = include_bytes!("../../../bin/launcher-stub-macos-x86_64");

/// `LSMinimumSystemVersion` for the emitted bundle.
///
/// Tracked deliberately rather than left to the OS: it is the floor the
/// launcher was compiled against, and the same value
/// `scripts/build-macos.sh` pins for the `snug` binary. If you raise
/// `MACOSX_DEPLOYMENT_TARGET`, raise this too.
pub const MIN_SYSTEM_VERSION: &str = "12.0";

const BUNDLE_DIR_MODE: u32 = 0o755;
const EXECUTABLE_MODE: u32 = 0o755;
const DATA_FILE_MODE: u32 = 0o644;

/// Should this invocation produce a `.app` rather than a Windows `.exe`?
///
/// The predicate itself lives in [`crate::build::wants_app_bundle`] so
/// that *every* platform can ask it — the interesting half is the answer
/// on the platforms that cannot honour it. Re-exported here because this
/// is the module people look in for it.
pub use crate::build::wants_app_bundle;

/// Build the `.app` bundle and return the bundle directory path.
pub fn build_app(cli: &Cli, payload: &SnugPayload) -> Result<PathBuf> {
    let bundle = output_path(cli);
    let app_name = bundle
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("App")
        .to_string();

    prepare_bundle_dir(&bundle)?;

    let contents = bundle.join("Contents");
    let macos_dir = contents.join("MacOS");
    let resources_dir = contents.join("Resources");
    for dir in [&macos_dir, &resources_dir] {
        fs::create_dir_all(dir)
            .with_context(|| format!("creating {}", dir.display()))?;
        set_mode(dir, BUNDLE_DIR_MODE)?;
    }

    // 1. The launcher, copied byte-for-byte. Nothing is stamped into it:
    //    a Mach-O has no resource directory and `editpe` is PE-only, so
    //    unlike the Windows stub the launcher is *identical* in every
    //    bundle and only the payload beside it differs.
    let executable = macos_dir.join(&app_name);
    fs::write(&executable, LAUNCHER)
        .with_context(|| format!("writing launcher to {}", executable.display()))?;
    set_mode(&executable, EXECUTABLE_MODE)?;

    // 2. The payload, as a sibling file the launcher locates at runtime.
    //    The suffix is imported from the launcher crate rather than
    //    retyped: this is the producer, `platform::macos` is the
    //    consumer, and a mismatch surfaces only as "payload not found".
    let payload_name = format!("{app_name}.{}", snug_payload::PAYLOAD_SUFFIX);
    let payload_path = resources_dir.join(&payload_name);
    let embedded = snug_format::SnugEmbedded::new(payload.clone());
    let encoded = snug_format::encode(&embedded).context("encoding snug payload")?;
    fs::write(&payload_path, &encoded)
        .with_context(|| format!("writing payload to {}", payload_path.display()))?;
    set_mode(&payload_path, DATA_FILE_MODE)?;

    // 3. Icon. Best-effort — a missing iconutil is a cosmetic loss, and a
    //    bundle with no icon is far better than no bundle at all.
    let icon_written = match write_icns(cli, &resources_dir) {
        Ok(true) => true,
        Ok(false) => false,
        Err(e) => {
            eprintln!("snug: warning: could not build the icon: {e}");
            false
        }
    };

    // 4. Info.plist, written last so it already reflects whether there
    //    is an icon to point CFBundleIconFile at.
    let plist = info_plist(&app_name, payload, icon_written);
    let plist_path = contents.join("Info.plist");
    fs::write(&plist_path, plist)
        .with_context(|| format!("writing {}", plist_path.display()))?;
    set_mode(&plist_path, DATA_FILE_MODE)?;

    // 5. Sign. arm64 will not run an unsigned binary, so this is not
    //    optional polish.
    codesign(&bundle)?;

    Ok(bundle)
}

/// Tell Launch Services to forget a bundle we are about to delete.
///
/// Deleting a registered `.app` leaves a **stale** record pointing at the
/// old path. Rebuild the same bundle id somewhere else and Finder can keep
/// resolving the id to the dead path, refusing to open anything with
/// `_LSOpenURLsWithCompletionHandler() failed with error -1712` — which
/// gives no hint that the new copy is perfectly fine, and is maddening
/// to debug because the bundle validates, lints and verifies.
///
/// Unregistering first is cheap and advisory, so the exit status is
/// ignored: a bundle LS never saw is not a reason to fail a build.
fn unregister_bundle(path: &Path) {
    const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/\
        Frameworks/LaunchServices.framework/Support/lsregister";
    let _ = std::process::Command::new(LSREGISTER)
        .arg("-u")
        .arg(path)
        .output();
}

/// Refuse to clobber anything that is not obviously a snug bundle.
///
/// A `.app` is a directory, so "the output already exists" is ambiguous:
/// silently `rm -rf`-ing whatever is at a user-supplied path is exactly
/// the kind of thing that eats someone's work. Only a directory that
/// already looks like a bundle we made is removed.
fn prepare_bundle_dir(bundle: &Path) -> Result<()> {
    if !bundle.exists() {
        return Ok(());
    }
    if !bundle.is_dir() {
        bail!(
            "{} exists and is not a directory.\n\
             hint: a .app must be a directory. If you meant to build a Windows EXE, use a .exe name.",
            bundle.display()
        );
    }
    let looks_like_a_bundle = bundle.join("Contents").join("Info.plist").is_file();
    if !looks_like_a_bundle {
        bail!(
            "{} is a directory but not a snug bundle (no Contents/Info.plist).\n\
             hint: remove it yourself, or choose another output path.",
            bundle.display()
        );
    }
    unregister_bundle(bundle);
    fs::remove_dir_all(bundle)
        .with_context(|| format!("removing the previous bundle at {}", bundle.display()))?;
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)
        .with_context(|| format!("reading permissions of {}", path.display()))?
        .permissions();
    perms.set_mode(mode);
    fs::set_permissions(path, perms)
        .with_context(|| format!("setting mode {:o} on {}", mode, path.display()))
}

/// Render the bundle's `Info.plist`.
///
/// Hand-rolled rather than via a plist library because the shape is
/// fixed and the escaping is the only subtle part — app names and
/// companies routinely contain `&` and `<`, and an unescaped one produces
/// a plist that silently fails to parse, which Launch Services reports
/// only as "the app is damaged".
fn info_plist(app_name: &str, payload: &SnugPayload, has_icon: bool) -> String {
    let app = &payload.config.app;
    let version = short_version(&app.version);
    let build_version = build_version(&app.version);

    let mut s = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n",
    );

    entry(&mut s, "CFBundleDevelopmentRegion", "en");
    entry(&mut s, "CFBundleExecutable", app_name);
    entry(
        &mut s,
        "CFBundleIdentifier",
        &bundle_identifier(&app.company, &app.name),
    );
    entry(&mut s, "CFBundleInfoDictionaryVersion", "6.0");
    entry(&mut s, "CFBundleName", &bundle_name(&app.name));
    // The user-visible name may be longer or prettier than the
    // executable, so both are set.
    entry(&mut s, "CFBundleDisplayName", &app.name);
    entry(&mut s, "CFBundlePackageType", "APPL");
    entry(&mut s, "CFBundleShortVersionString", &version);
    entry(&mut s, "CFBundleVersion", &build_version);
    if has_icon {
        entry(&mut s, "CFBundleIconFile", "App");
    }
    if let Some(desc) = &app.description {
        s.push_str(&format!(
            "\t<key>NSApplicationDescription</key>\n\t<string>{}</string>\n",
            xml_escape(desc)
        ));
    }
    if let Some(copyright) = &app.copyright {
        entry(&mut s, "NSHumanReadableCopyright", copyright);
    }
    entry(&mut s, "LSMinimumSystemVersion", MIN_SYSTEM_VERSION);
    s.push_str("\t<key>NSHighResolutionCapable</key>\n\t<true/>\n");

    s.push_str("</dict>\n</plist>\n");
    s
}

/// Append one `<key>`/`<string>` pair to a plist dict.
///
/// A free function rather than a closure: a closure would hold a mutable
/// borrow of the buffer, and the description key below needs to write to
/// the same `String`.
fn entry(s: &mut String, key: &str, value: &str) {
    s.push_str(&format!(
        "\t<key>{}</key>\n\t<string>{}</string>\n",
        xml_escape(key),
        xml_escape(value)
    ));
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// The value written to `CFBundleName`, which is the title macOS gives the
/// **application menu** — the first, always-present menu in the menu bar.
///
/// This is a different job from `CFBundleExecutable`, which has to name the
/// file inside `Contents/MacOS` and therefore must stay the slug. Writing
/// the slug into both used to make the demo's application menu read
/// "snug-javafx-demo" while the window and the Finder read "Snug JavaFX
/// Demo". It is the macOS counterpart of the `ProductName` the Windows
/// build stamps into `VS_VERSIONINFO`.
///
/// Apple caps `CFBundleName` at 16 characters, so a longer `--name` is
/// truncated rather than emitted whole. `CFBundleDisplayName` carries the
/// full name for the Finder and the Dock, so nothing is actually lost.
fn bundle_name(name: &str) -> String {
    const MAX_CFBUNDLE_NAME: usize = 16;
    if name.chars().count() <= MAX_CFBUNDLE_NAME {
        return name.to_string();
    }
    // Truncating can leave a trailing space mid-word ("A Very Long App" ->
    // "A Very Long App " -> "A Very Long App"), so drop it rather than pad.
    name.chars()
        .take(MAX_CFBUNDLE_NAME)
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// Reverse-DNS bundle id from the company and app names.
///
/// macOS treats the identifier as a uniqueness key, so it has to look
/// like a domain in reverse. The result is a best effort from whatever
/// metadata we were given — a real reverse-DNS prefix would want a new
/// `--bundle-id` flag, and inventing a domain we do not own would be
/// worse than a neutral one.
fn bundle_identifier(company: &str, name: &str) -> String {
    let part = |s: &str| -> String {
        let slug: String = s
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
            .collect();
        let slug = slug
            .split('-')
            .filter(|seg| !seg.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        if slug.is_empty() { "app".to_string() } else { slug }
    };
    format!("com.{}.{}", part(company), part(name))
}

/// `CFBundleShortVersionString` may only contain digits and dots.
///
/// A user-supplied `1.2.3-beta` is perfectly reasonable and perfectly
/// invalid here, where it produces a bundle the Dock refuses to launch.
/// The pre-release part moves to `CFBundleVersion`, which is more
/// permissive, rather than being dropped.
fn short_version(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_ascii_digit() || c == '.' { c } else { '.' })
        .collect();
    // Collapsing the dot runs is what makes `1.0.0-beta.1` usable: the
    // `-beta` becomes a separator, so the result stays a legal
    // digits-and-dots string *and* keeps the user's version. Bailing out
    // to a constant here would report every pre-release build as 1.0.
    let trimmed = collapse_dots(&cleaned);
    let valid = !trimmed.is_empty()
        && trimmed.chars().next().is_some_and(|c| c.is_ascii_digit());
    if valid {
        trimmed
    } else {
        "1.0".to_string()
    }
}

/// `CFBundleVersion` is the build number: up to three dot-separated
/// integers, which comfortably holds a pre-release suffix's digits.
fn build_version(raw: &str) -> String {
    let digits: String = raw
        .chars()
        .map(|c| if c.is_ascii_digit() || c == '.' { c } else { '.' })
        .collect();
    let collapsed = collapse_dots(&digits);
    if collapsed.is_empty() {
        "1".to_string()
    } else {
        collapsed
    }
}

fn collapse_dots(s: &str) -> String {
    let mut out = String::new();
    let mut last_dot = true;
    for c in s.chars() {
        if c == '.' {
            if last_dot {
                continue;
            }
            last_dot = true;
        } else {
            last_dot = false;
        }
        out.push(c);
    }
    out.trim_matches('.').to_string()
}

/// Build `Resources/App.icns` from `--icon`, if one was given.
///
/// Uses the system `iconutil`, which is the only icns writer on macOS.
/// The source PNG is resampled to each size `iconutil` expects; a
/// retina `@2x` entry is the same pixel size as the next tier, which is
/// why several names share a dimension.
fn write_icns(cli: &Cli, resources_dir: &Path) -> Result<bool> {
    let Some(src) = &cli.icon else {
        return Ok(false);
    };
    let bytes = fs::read(src)
        .with_context(|| format!("reading icon {}", src.display()))?;
    let image = image::load_from_memory(&bytes)
        .with_context(|| format!("decoding icon {}", src.display()))?;

    let iconset = resources_dir.join("App.iconset");
    fs::create_dir_all(&iconset).with_context(|| {
        format!("creating the temporary iconset {}", iconset.display())
    })?;

    // (file name, pixel size)
    const ENTRIES: &[(&str, u32)] = &[
        ("icon_16x16.png", 16),
        ("icon_16x16@2x.png", 32),
        ("icon_32x32.png", 32),
        ("icon_32x32@2x.png", 64),
        ("icon_128x128.png", 128),
        ("icon_128x128@2x.png", 256),
        ("icon_256x256.png", 256),
        ("icon_256x256@2x.png", 512),
        ("icon_512x512.png", 512),
        ("icon_512x512@2x.png", 1024),
    ];

    for (name, size) in ENTRIES {
        let resized = image.resize_exact(
            *size,
            *size,
            image::imageops::FilterType::Lanczos3,
        );
        let path = iconset.join(name);
        resized
            .save(&path)
            .with_context(|| format!("writing {}", path.display()))?;
    }

    let icns = resources_dir.join("App.icns");
    let out = Command::new("/usr/bin/iconutil")
        .arg("-c")
        .arg("icns")
        .arg(&iconset)
        .arg("-o")
        .arg(&icns)
        .output()
        .context("running /usr/bin/iconutil")?;
    let _ = fs::remove_dir_all(&iconset);

    if !out.status.success() {
        bail!(
            "iconutil failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    set_mode(&icns, DATA_FILE_MODE)?;
    Ok(true)
}

/// Ad-hoc sign the bundle.
///
/// Mandatory for arm64, which will not execute a binary with no code
/// signature at all. Enough to *run*; Gatekeeper still applies to a
/// bundle downloaded through a browser, which needs a Developer ID and
/// notarisation (see AGENTS.md, "Build host").
fn codesign(bundle: &Path) -> Result<()> {
    let out = Command::new("/usr/bin/codesign")
        .arg("--force")
        .arg("--sign")
        .arg("-")
        // No secure timestamp for an ad-hoc signature: there is no
        // identity to timestamp against, and asking for one is an error.
        .arg("--timestamp=none")
        .arg(bundle)
        .output()
        .context("running /usr/bin/codesign")?;
    if !out.status.success() {
        bail!(
            "codesign failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use snug_format::AppMetadata;

    fn payload(company: &str, name: &str, version: &str) -> SnugPayload {
        SnugPayload {
            config: snug_format::LauncherConfig {
                app: AppMetadata {
                    name: name.into(),
                    company: company.into(),
                    version: version.into(),
                    update_check_url: None,
                    description: None,
                    copyright: None,
                },
                main_class: None,
                min_java: 21,
                jvm_args: vec![],
                splash: None,
                behavior: Default::default(),
            },
            jars: vec![],
            icon: None,
            localizations: Vec::new(),
        }
    }

    #[test]
    fn plist_is_wellformed_and_names_the_executable() {
        let p = payload("SynapticLoop", "Demo", "1.0.0");
        let xml = info_plist("Demo", &p, true);
        assert!(xml.starts_with("<?xml"));
        assert!(xml.contains("<key>CFBundleExecutable</key>\n\t<string>Demo</string>"));
        assert!(xml.contains("<key>CFBundleIconFile</key>\n\t<string>App</string>"));
        assert!(xml.trim_end().ends_with("</plist>"));
    }

    #[test]
    fn plist_omits_the_icon_key_when_there_is_no_icon() {
        let p = payload("SynapticLoop", "Demo", "1.0.0");
        let xml = info_plist("Demo", &p, false);
        assert!(!xml.contains("CFBundleIconFile"));
    }

    #[test]
    fn plist_escapes_metadata_that_would_otherwise_break_it() {
        // An unescaped `&` makes the plist fail to parse, and Launch
        // Services reports that only as "the app is damaged" — a genuinely
        // baffling error unless you know to look here.
        let p = payload("A & B <Ltd>", "Tom & \"Jerry\"", "1.0.0");
        let xml = info_plist("App", &p, false);
        assert!(xml.contains("Tom &amp; &quot;Jerry&quot;"));
        // The company is not emitted raw at all: it only reaches the
        // plist through the slugged reverse-DNS identifier.
        assert!(xml.contains("com.a-b-ltd.tom-jerry"));
        assert!(!xml.contains("A & B"));
        // Well-formedness itself is covered properly by
        // `plist_round_trips_through_a_real_parser`, which parses the
        // output. Scraping the string for a bare `&` is not a
        // substitute — it trips over the legitimate `&quot;` and
        // `&apos;` entities.
    }

    #[test]
    fn plist_round_trips_through_a_real_parser() {
        // Hand-rolled plist is exactly the kind of thing that looks fine
        // and is subtly malformed, so parse it back rather than trusting
        // the string assertions above.
        let p = payload("SynapticLoop", "Snug Demo", "2.3.4");
        let xml = info_plist("Snug Demo", &p, true);
        let value = plist::Value::from_reader_xml(xml.as_bytes())
            .expect("generated Info.plist must be well-formed");
        let dict = value.as_dictionary().expect("root must be a dict");
        let get = |k: &str| dict.get(k).and_then(|v| v.as_string()).map(String::from);
        assert_eq!(get("CFBundleExecutable").as_deref(), Some("Snug Demo"));
        assert_eq!(get("CFBundleIdentifier").as_deref(), Some("com.synapticloop.snug-demo"));
        assert_eq!(get("CFBundlePackageType").as_deref(), Some("APPL"));
        assert_eq!(get("CFBundleShortVersionString").as_deref(), Some("2.3.4"));
        assert_eq!(get("CFBundleIconFile").as_deref(), Some("App"));
        assert_eq!(get("LSMinimumSystemVersion").as_deref(), Some("12.0"));
    }

    #[test]
    fn short_version_rejects_pre_release_suffixes() {
        assert_eq!(short_version("1.0.0"), "1.0.0");
        assert_eq!(short_version("2.1"), "2.1");
        // CFBundleShortVersionString allows only digits and dots.
        assert_eq!(short_version("1.0.0-beta.1"), "1.0.0.1");
        assert_eq!(short_version("v3"), "3");
        // Nonsense must still yield something Launch Services accepts.
        assert_eq!(short_version(""), "1.0");
        assert_eq!(short_version("beta"), "1.0");
    }

    #[test]
    fn bundle_name_is_the_app_menu_title_not_the_slug() {
        // The whole point: the application menu shows this, so it has to be
        // the pretty name even though the executable is still the slug.
        assert_eq!(bundle_name("Snug JavaFX Demo"), "Snug JavaFX Demo");
        // Exactly at Apple's cap, untouched.
        assert_eq!(bundle_name("Sixteen Char Nam"), "Sixteen Char Nam");
        // Over the cap it is cut, and a word broken by the cut does not
        // leave a trailing space behind.
        assert_eq!(bundle_name("A Very Long Application Name"), "A Very Long Appl");
        assert_eq!(bundle_name("Seventeen Char Name"), "Seventeen Char N");
        // Counted in characters, not bytes, so a multi-byte name is not
        // cut in the middle of a code point.
        assert_eq!(bundle_name("Ünïcödé Süper Lóng Näme"), "Ünïcödé Süper Ló");
    }

    #[test]
    fn info_plist_names_the_application_menu_with_the_app_name() {
        let p = payload("SynapticLoop", "Snug JavaFX Demo", "1.0.0");
        let xml = info_plist("snug-javafx-demo", &p, true);
        // The executable stays the slug so it matches the file in
        // Contents/MacOS...
        assert!(xml.contains(
            "<key>CFBundleExecutable</key>\n\t<string>snug-javafx-demo</string>"
        ));
        // ...while the application menu gets the name a user recognises.
        assert!(xml.contains(
            "<key>CFBundleName</key>\n\t<string>Snug JavaFX Demo</string>"
        ));
    }

    #[test]
    fn bundle_identifier_is_reverse_dns_and_slugged() {
        assert_eq!(
            bundle_identifier("SynapticLoop", "Snug Demo"),
            "com.synapticloop.snug-demo"
        );
        // Punctuation and spaces become separators, not literal characters.
        assert_eq!(bundle_identifier("A & B", "My App!"), "com.a-b.my-app");
        // An empty component must not produce `com..` which is invalid.
        assert_eq!(bundle_identifier("", ""), "com.app.app");
    }

    #[test]
    fn collapse_dots_removes_runs_and_edges() {
        assert_eq!(collapse_dots("1...2"), "1.2");
        assert_eq!(collapse_dots(".1."), "1");
        assert_eq!(collapse_dots("abc"), "abc");
    }
}
