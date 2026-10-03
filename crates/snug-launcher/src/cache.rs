//! Per-user cache for extracted JARs.
//!
//! Default layout on Windows:
//!
//! ```text
//! %LOCALAPPDATA%\snug\<company>\<app>\<jar-sha256>\app.jar
//! ```
//!
//! All snug-managed data lives under a top-level `snug\` namespace so
//! it's easy to find (`Remove-Item -Recurse %LOCALAPPDATA%\snug\<co>\<app>`)
//! and doesn't pollute the wrapped app's own AppData directory. We use
//! `%LOCALAPPDATA%` (not `%APPDATA%`) because caches are by definition
//! regenerable from the source-of-truth EXE and shouldn't roam with
//! the user profile. Each JAR gets its own SHA-256-keyed subdirectory
//! so multi-JAR builds can have collision-free filenames and unchanged
//! JARs stay cached across rebuilds.
//!
//! # Retention
//!
//! Entries expire. [`touch`] stamps a cached JAR's mtime on every
//! launch that uses it, and [`sweep`] periodically deletes the ones no
//! longer wanted, so the cache cannot grow without bound across
//! rebuilds. The policy keeps the most recently used [`KEEP_RECENT`]
//! entries, up to a per-application byte budget, and evicts anything
//! untouched for [`MAX_UNUSED_AGE`] regardless of rank.

use std::collections::HashSet;
use std::fs::{File, FileTimes};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use snug_format::AppMetadata;

/// Default top-level subdirectory under the per-user data root.
pub const SNUG_SUBDIR: &str = "snug";

/// Default JAR filename inside a cache entry.
pub const CACHED_JAR_NAME: &str = "app.jar";

/// Compute the per-user cache root for the given app metadata.
///
/// Resolution order:
/// 1. Explicit override (passed via `behavior.cache_dir` or `app.cache_dir`).
/// 2. The platform's own per-user cache root, via [`platform_cache_base`]:
///    `%LOCALAPPDATA%` on Windows, `_CS_DARWIN_USER_CACHE_DIR` on macOS,
///    `$XDG_CACHE_HOME` / `~/.cache` elsewhere.
/// 3. `$HOME/.cache`, then the temp dir, as a last resort.
///
/// `snug/<company>/<app>/` is appended to whichever base wins.
pub fn cache_root(app: &AppMetadata, override_root: Option<&Path>) -> PathBuf {
    if let Some(root) = override_root {
        return root.to_path_buf();
    }

    let company = sanitize_component(&app.company);
    let name = sanitize_component(&app.name);

    let base = platform_cache_base().unwrap_or_else(fallback_cache_base);
    base.join(SNUG_SUBDIR).join(&company).join(&name)
}

/// Compute the full path to the cached JAR for a given SHA-256 digest.
pub fn cached_jar_path(root: &Path, sha256: &[u8; 32]) -> PathBuf {
    let hex = hex_lower(sha256);
    root.join(hex).join(CACHED_JAR_NAME)
}

/// Compute the path to the per-launch log file. Sits next to the
/// cached JAR so a user reviewing an EXE's behaviour has one place
/// to look.
pub fn cached_log_path(root: &Path, sha256: &[u8; 32]) -> PathBuf {
    let hex = hex_lower(sha256);
    root.join(hex).join("snug.log")
}

/// Lowercase hex encoding of a SHA-256 digest (64 chars). Public so
/// the log module can render the same string without re-implementing
/// the encoding.
pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    hex_lower_impl(bytes)
}

/// Replace path separators and other characters that are unsafe in a
/// directory name across the platforms we support.
fn sanitize_component(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

fn hex_lower_impl(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

// --- Retention policy ---------------------------------------------------

/// How many recently-used entries to retain per application.
const KEEP_RECENT: usize = 3;

/// Lower bound on one application's byte budget.
const MIN_CACHE_BUDGET: u64 = 512 * 1024 * 1024;

/// Upper bound on one application's byte budget.
const MAX_CACHE_BUDGET: u64 = 8 * 1024 * 1024 * 1024;

/// The byte budget for a build of `build_bytes`.
///
/// Scales with the build so a large application keeps a few generations
/// rather than collapsing to just the current one, then clamps at both
/// ends. The floor stops small applications being rationed — bounding
/// large applications is the budget's job, not rationing small ones. The
/// ceiling stops the largest being unbounded: without it a 4 GB fat JAR
/// would retain 12 GB indefinitely.
fn budget_for(build_bytes: u64) -> u64 {
    build_bytes
        .saturating_mul(KEEP_RECENT as u64)
        .clamp(MIN_CACHE_BUDGET, MAX_CACHE_BUDGET)
}

/// Evict an entry this long after it was last used, whatever its rank.
const MAX_UNUSED_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Minimum gap between two sweeps.
const SWEEP_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Rate-limit stamp file, relative to the app's cache root. Deliberately
/// not 64 hex characters, so [`is_hash_dir`] ignores it for free.
const SWEEP_STAMP: &str = ".sweep";

/// True when `name` is a cache directory snug created: exactly 64
/// lowercase hex characters.
///
/// A safety property, not just a format check — the sweep only ever
/// deletes directories it can positively identify as its own, so
/// anything else sharing the cache root (the `.sweep` stamp, a user's
/// own file, a future feature's directory) is left alone.
fn is_hash_dir(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Mark `path` as just-used by updating its modification time.
///
/// The cached JAR's mtime is the launcher's only record of when an
/// entry was last needed, so this runs for **every** JAR in the
/// current build rather than just the primary. Touching only the
/// primary would let a shared library JAR age out despite constant use,
/// and the next launch would re-copy it in full — the exact cost this
/// retention policy exists to avoid.
///
/// Requires write access; callers treat a failure as non-fatal. A
/// read-only entry simply ages out and is re-extracted on demand.
pub fn touch(path: &Path) -> std::io::Result<()> {
    let f = File::options().write(true).open(path)?;
    f.set_times(FileTimes::new().set_modified(SystemTime::now()))
}

/// What a [`sweep`] pass did.
///
/// Returned rather than logged directly so the caller controls the
/// message and the policy stays testable.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Directory names (hex digests) that were removed.
    pub removed: Vec<String>,
    /// Bytes reclaimed by `removed`.
    pub bytes_freed: u64,
    /// How many entries survived.
    pub kept: usize,
    /// Failures encountered. A sweep is best-effort: an entry it could
    /// not remove (a running app holding the file open on Windows) is
    /// left for a later pass.
    pub errors: Vec<String>,
}

/// Delete cache entries the application no longer needs.
///
/// `current` holds the hex digests of the JARs in the running build —
/// those are never evicted. `build_bytes` is their total size, which
/// scales the budget so a large application still retains a few
/// generations instead of collapsing to just the current one.
///
/// `now` is a parameter rather than read from the clock so the whole
/// policy is testable without waiting 30 days.
pub fn sweep(
    root: &Path,
    current: &HashSet<String>,
    build_bytes: u64,
    now: SystemTime,
) -> SweepReport {
    let mut report = SweepReport::default();
    let budget = budget_for(build_bytes);

    let mut entries: Vec<(String, SystemTime, u64)> = Vec::new();
    let mut partial: Vec<String> = Vec::new();

    // A missing root just means nothing has been extracted yet.
    let Ok(dir) = std::fs::read_dir(root) else {
        return report;
    };

    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_hash_dir(&name) {
            continue;
        }
        match std::fs::metadata(entry.path().join(CACHED_JAR_NAME)) {
            Ok(m) => entries.push((name, m.modified().unwrap_or(now), m.len())),
            // A directory with no `app.jar` is a half-finished
            // extraction from an interrupted run. Nothing can load it,
            // so it goes — unless it belongs to the current build, in
            // which case the extraction loop has already surfaced the
            // real failure and we must not mask it.
            Err(_) => partial.push(name),
        }
    }

    for name in partial {
        if current.contains(&name) {
            continue;
        }
        match std::fs::remove_dir_all(root.join(&name)) {
            Ok(()) => report.removed.push(name),
            Err(e) => report.errors.push(format!("{name}: {e}")),
        }
    }

    // Newest first, hash breaking ties so a filesystem with coarse
    // timestamp granularity (2s on FAT/exFAT) evicts the same entry on
    // every run instead of flipping between equals.
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let mut used: u64 = 0;
    for (name, mtime, size) in entries {
        // The current build is never evicted, and its size counts
        // against the budget. This is the one invariant that must hold
        // unconditionally — everything else is recoverable, because a
        // missing entry is re-extracted from the payload next launch.
        if current.contains(&name) {
            used = used.saturating_add(size);
            report.kept += 1;
            continue;
        }

        let stale = now
            .duration_since(mtime)
            .is_ok_and(|age| age > MAX_UNUSED_AGE);
        let fits = report.kept < KEEP_RECENT && used + size <= budget;
        if !stale && fits {
            used = used.saturating_add(size);
            report.kept += 1;
            continue;
        }

        match std::fs::remove_dir_all(root.join(&name)) {
            Ok(()) => {
                report.bytes_freed += size;
                report.removed.push(name);
            }
            Err(e) => report.errors.push(format!("{name}: {e}")),
        }
    }

    report
}

/// Rate-limit the sweep to one pass per [`SWEEP_INTERVAL`]. Returns
/// whether a sweep should run now.
///
/// The stamp is written *before* the sweep rather than after, so a
/// crash mid-pass cannot become a tight retry loop on every launch.
pub fn should_sweep(root: &Path, now: SystemTime) -> bool {
    let stamp = root.join(SWEEP_STAMP);
    let last = std::fs::read_to_string(&stamp)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs));

    if let Some(last) = last {
        if now
            .duration_since(last)
            .is_ok_and(|gap| gap < SWEEP_INTERVAL)
        {
            return false;
        }
    }

    let _ = std::fs::create_dir_all(root);
    let _ = std::fs::write(
        &stamp,
        now.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .to_string(),
    );
    true
}

/// The OS's own answer to "where do per-user cached files go".
///
/// This is deliberately *asked of the platform* rather than hardcoded, so
/// that if the convention moves we follow it instead of being wrong.
///
/// - **Windows** reads `%LOCALAPPDATA%` via the Win32 API. The variable
///   is the convention, so reading it is the whole story.
/// - **macOS** asks `confstr(_CS_DARWIN_USER_CACHE_DIR)`. That is the key
///   behind `NSSearchPathForDirectoriesInDomains(NSCachesDirectory, …)`,
///   i.e. the exact analogue of `%LOCALAPPDATA%`, and it is the documented
///   way to ask. It also honours `CFFIXED_USER_HOME` inside a sandboxed
///   app, which reading `$HOME` yourself does not.
///
/// `LOCALAPPDATA` is *not* a macOS variable. An earlier version read it
/// for every non-Windows target, so on macOS it always missed and the
/// path fell through to the `$HOME/.cache` fallback — a Linux convention
/// in a directory macOS never uses.
#[cfg(windows)]
pub fn platform_cache_base() -> Option<PathBuf> {
    // %LOCALAPPDATA% is the canonical per-user, per-machine data root
    // on Windows. We read it directly via the Win32 API for accuracy.
    use windows_sys::Win32::System::Environment::GetEnvironmentVariableW;

    let name: Vec<u16> = "LOCALAPPDATA\0".encode_utf16().collect();
    let needed = unsafe { GetEnvironmentVariableW(name.as_ptr(), std::ptr::null_mut(), 0) };
    if needed == 0 {
        return None;
    }
    let mut buf = vec![0u16; needed as usize];
    let written =
        unsafe { GetEnvironmentVariableW(name.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if written == 0 {
        return None;
    }
    buf.truncate(written as usize);
    Some(PathBuf::from(String::from_utf16_lossy(&buf)))
}

/// Ask the OS where the per-user cache directory is.
///
/// `NSHomeDirectory()` is the value Foundation itself derives
/// `NSCachesDirectory` from, and it honours `CFFIXED_USER_HOME` inside a
/// sandboxed `.app` — which reading `$HOME` yourself does not.
///
/// # The three APIs that look right and are not
///
/// Recorded because each cost a detour, and the first two would have
/// shipped a worse answer:
///
/// - **`confstr(_CS_DARWIN_USER_CACHE_DIR)`** is the obvious answer and it
///   is wrong. Its man page promises "a good location for user cache data
///   as it will not be automatically cleaned by the system", but on
///   macOS 15 it returns `/var/folders/<hash>/C/` — inside the per-boot
///   temporary tree, which the system *does* purge. Reproducible with the
///   environment stripped (`env -i`), so it is not an artefact of how snug
///   was launched. snug's JAR cache there could be deleted out from under
///   a running app.
/// - **Declaring `NSHomeDirectory()` by hand** as
///   `-> *const c_char` returns an `NSString *`, not a `char *`. Reading it
///   as a C string yields garbage bytes rather than failing loudly, which
///   is the kind of bug that is invisible until something tries to use the
///   path. Hence the binding crate rather than a hand-rolled `extern`.
/// - **The older `objc` 0.2 crate** does not compile on current rustc
///   (`cannot find macro sel`). `objc2-foundation` is the maintained one.
///
/// The one thing this *does* hardcode is the `Library/Caches` subpath.
/// That is a documented, stable part of the macOS layout and is the same
/// value Foundation reports; what can actually move — a sandboxed home, a
/// relocated user — comes from the OS.
#[cfg(target_os = "macos")]
pub fn platform_cache_base() -> Option<PathBuf> {
    use objc2_foundation::NSHomeDirectory;

    let home = NSHomeDirectory().to_string();
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home).join("Library").join("Caches"))
}

/// Everything that is neither Windows nor macOS: XDG, which is the Linux
/// convention. This is also the `~/.cache` fallback's target, so the
/// common case is unchanged.
#[cfg(all(not(windows), not(target_os = "macos")))]
pub fn platform_cache_base() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache"))
        })
}

pub fn fallback_cache_base() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".cache");
    }
    std::env::temp_dir()
}

/// The OS's own answer for "where do per-user **application data** go" —
/// as distinct from caches, which the system is free to reclaim.
///
/// Same shape and same reasoning as [`platform_cache_base`]: asked of the
/// platform rather than hardcoded, so a sandboxed home or a relocated user
/// is followed.
///
/// This exists because macOS treats those two as different things and the
/// difference matters. A `.app`'s own extracted JARs and logs are cache —
/// if the system reclaims them, snug silently re-extracts and nothing is
/// lost. An adopted JDK is a few hundred megabytes that took a real
/// download; the system purging it and making the next launch pay for it
/// again is a bug, not housekeeping. Hence `Library/Caches` for one and
/// `Library/Application Support` for the other.
#[cfg(windows)]
pub fn platform_app_support_base() -> Option<PathBuf> {
    // Deliberately the same answer as the cache base, so the existing
    // Windows install path does not move. `%LOCALAPPDATA%` is per-user
    // *and* per-machine, which is the property we want; the roaming
    // `%APPDATA%` is not.
    platform_cache_base()
}

#[cfg(target_os = "macos")]
pub fn platform_app_support_base() -> Option<PathBuf> {
    use objc2_foundation::NSHomeDirectory;

    let home = NSHomeDirectory().to_string();
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return None;
    }
    Some(
        PathBuf::from(home)
            .join("Library")
            .join("Application Support"),
    )
}

/// XDG's data home — `~/.local/share` — the Linux counterpart to
/// `~/.cache`. Unchanged in the common case where XDG_DATA_HOME is unset.
#[cfg(all(not(windows), not(target_os = "macos")))]
pub fn platform_app_support_base() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share"))
        })
}

/// Last-resort root when the platform gives us nothing.
pub fn fallback_app_support_base() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".local").join("share");
    }
    std::env::temp_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn cache_base_is_the_darwin_caches_directory() {
        let base = platform_cache_base().expect("Foundation should answer on macOS");

        assert!(base.is_absolute(), "{} should be absolute", base.display());
        assert!(
            base.is_dir(),
            "{} should exist on a real machine",
            base.display()
        );
        // The whole point of asking the OS: this is the Darwin caches
        // directory, not the Linux `~/.cache` convention the old code
        // fell back to.
        assert!(
            base.ends_with("Library/Caches"),
            "expected .../Library/Caches, got {}",
            base.display()
        );
        assert!(
            !base.ends_with(".cache"),
            "must not be the Linux fallback, got {}",
            base.display()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn cache_root_sits_under_the_darwin_caches_directory() {
        let meta = app("SynapticLoop", "Demo");
        let root = cache_root(&meta, None);
        let base = platform_cache_base().unwrap();

        assert!(root.starts_with(&base), "{} not under {}", root.display(), base.display());
        assert!(root.ends_with("snug/SynapticLoop/Demo"), "got {}", root.display());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn app_support_base_is_application_support_not_caches() {
        let support = platform_app_support_base().expect("Foundation should answer");
        assert!(support.is_absolute(), "{} should be absolute", support.display());
        assert!(
            support.ends_with("Library/Application Support"),
            "expected .../Library/Application Support, got {}",
            support.display()
        );
        // The whole point of the split: the two roots must not be the
        // same directory, or the JDK would land back in a place macOS
        // may purge.
        let cache = platform_cache_base().expect("Foundation should answer");
        assert_ne!(
            support,
            cache,
            "application support and caches must differ; both resolved to {}",
            support.display()
        );
    }

    #[test]
    fn app_support_fallback_is_not_the_cache_fallback() {
        let support = fallback_app_support_base();
        let cache = fallback_cache_base();
        assert_ne!(support, cache);
        assert!(support.is_absolute());
    }

    #[test]
    fn cache_root_honours_an_explicit_override() {
        let meta = app("SynapticLoop", "Demo");
        let override_root = Path::new("/tmp/snug-explicit");
        assert_eq!(
            cache_root(&meta, Some(override_root)),
            override_root.to_path_buf()
        );
    }

    fn app(company: &str, name: &str) -> AppMetadata {
        AppMetadata {
            name: name.into(),
            company: company.into(),
            update_check_url: None,
            version: "0.0.0".into(),
            description: None,
            copyright: None,
        }
    }

    #[test]
    fn cache_root_uses_override_when_set() {
        let override_path = PathBuf::from("/tmp/snug-test-override");
        let root = cache_root(&app("Acme", "Demo"), Some(&override_path));
        assert_eq!(root, override_path);
    }

    #[test]
    fn cache_root_returns_override_directly() {
        let override_path = PathBuf::from("/tmp/x");
        let root = cache_root(&app("Acme", "Demo"), Some(&override_path));
        assert_eq!(root, override_path);
    }

    #[test]
    fn cache_root_namespaces_under_snug_subdir() {
        // With an explicit override the helper should return the
        // override untouched; without one, the path must contain the
        // top-level `snug` segment before the company / app segments.
        let app = app("Acme", "Demo");
        let root = cache_root(&app, None);
        let parts: Vec<_> = root
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .collect();
        let snug_idx = parts
            .iter()
            .position(|p| *p == SNUG_SUBDIR)
            .expect("snug namespace segment must be present");
        let company_idx = parts
            .iter()
            .position(|p| *p == "Acme")
            .expect("company segment must be present");
        let app_idx = parts
            .iter()
            .position(|p| *p == "Demo")
            .expect("app segment must be present");
        assert!(
            snug_idx < company_idx && company_idx < app_idx,
            "expected snug/<company>/<app> ordering, got {:?}",
            parts
        );
    }

    #[test]
    fn sanitize_strips_unsafe_chars() {
        assert_eq!(sanitize_component("Acme Co"), "Acme Co");
        assert_eq!(sanitize_component("a/b\\c:d*e"), "a_b_c_d_e");
        assert_eq!(sanitize_component("My\u{0001}App"), "My_App");
        assert_eq!(sanitize_component("   "), "");
    }

    #[test]
    fn cached_jar_path_uses_hex_sha256() {
        let sha = [0u8; 32];
        let root = PathBuf::from("/cache");
        let p = cached_jar_path(&root, &sha);
        // 64 hex chars + "/app.jar"
        let expected = root.join("0".repeat(64)).join(CACHED_JAR_NAME);
        assert_eq!(p, expected);
    }

    // --- Retention ------------------------------------------------------

    /// A fixed instant so age arithmetic in these tests is exact rather
    /// than dependent on how long the test takes to run. Anchored at the
    /// epoch because `SystemTime + Duration` is not a const expression;
    /// every fixture age below is relative to this, so the absolute base
    /// is arbitrary.
    const NOW: SystemTime = SystemTime::UNIX_EPOCH;

    fn tempdir() -> PathBuf {
        // A counter, not a clock: the test suite runs in parallel, so
        // every call must get a genuinely distinct path. Deriving the
        // name from a fixed instant would hand every test the same one.
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("snug-cache-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Create a cache entry whose `app.jar` is `size` bytes and was last
    /// used `age` before [`NOW`]. Returns the directory name.
    fn entry(root: &Path, name: &str, size: usize, age: Duration) -> String {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let jar = dir.join(CACHED_JAR_NAME);
        std::fs::write(&jar, vec![b'x'; size]).unwrap();
        let when = NOW - age;
        touch_at(&jar, when);
        name.to_string()
    }

    /// Set a file's mtime without opening it for write — used to stage
    /// fixture ages that [`touch`] would otherwise overwrite with "now".
    fn touch_at(path: &Path, when: SystemTime) {
        let f = std::fs::File::options().write(true).open(path).unwrap();
        f.set_times(FileTimes::new().set_modified(when)).unwrap();
    }

    /// A 64-char lowercase-hex name, varying in the last characters so
    /// fixtures are distinguishable.
    fn hash_name(n: u8) -> String {
        format!("{:0>64}", format!("{n:x}"))
    }

    const HOUR: Duration = Duration::from_secs(3600);

    /// `SweepReport::removed` is in walk order — the sweep scans
    /// newest-first, so the most recently evicted entry comes first.
    /// Tests care about *which* entries went, not when, so compare
    /// sorted.
    fn sorted(mut names: Vec<String>) -> Vec<String> {
        names.sort();
        names
    }

    #[test]
    fn touch_advances_the_mtime() {
        let dir = tempdir();
        let jar = dir.join("f.txt");
        std::fs::write(&jar, b"data").unwrap();
        let old = NOW - Duration::from_secs(30 * 24 * 3600);
        touch_at(&jar, old);
        let before = std::fs::metadata(&jar).unwrap().modified().unwrap();
        assert!(before < SystemTime::now(), "fixture should start in the past");

        touch(&jar).unwrap();

        let after = std::fs::metadata(&jar).unwrap().modified().unwrap();
        assert!(
            after > before,
            "touch must advance the mtime: {before:?} -> {after:?}"
        );
    }

    #[test]
    fn sweep_never_removes_the_current_build() {
        // The current build is deliberately the *oldest* entry, and
        // there are more entries than KEEP_RECENT, so the count filter
        // would evict it last. It has to survive anyway: everything else
        // is recoverable by re-extraction from the payload, the running
        // app's own classpath is not.
        let root = tempdir();
        let names: Vec<String> = (1..=5u8)
            .map(|n| entry(&root, &hash_name(n), 64, Duration::from_secs(5 - n as u64)))
            .collect();
        let oldest = names[0].clone();
        let current: HashSet<String> = [oldest.clone()].into_iter().collect();

        let report = sweep(&root, &current, 64, NOW);

        assert!(
            root.join(&oldest).exists(),
            "the current build must survive even when it ranks last"
        );
        assert!(!report.removed.contains(&oldest));
        // Four non-current entries, three slots: hash_name(2) is the
        // oldest of the four, so it is the one that goes.
        assert_eq!(sorted(report.removed), sorted(vec![names[1].clone()]));
    }

    #[test]
    fn current_build_counts_toward_the_retained_depth() {
        // `KEEP_RECENT` bounds the *total* retained generations, not the
        // history on top of the current one. That is what makes the
        // `build_bytes * KEEP_RECENT` budget line up: a single-JAR app
        // holds three copies of itself, not four.
        let root = tempdir();
        let names: Vec<String> = (1..=4u8)
            .map(|n| entry(&root, &hash_name(n), 64, Duration::from_secs(4 - n as u64)))
            .collect();
        let current: HashSet<String> = [names[3].clone()].into_iter().collect();

        let report = sweep(&root, &current, 64, NOW);

        assert_eq!(report.kept, KEEP_RECENT);
        assert_eq!(report.removed.len(), 1);
        assert_eq!(sorted(report.removed), sorted(vec![names[0].clone()]));
    }

    #[test]
    fn sweep_evicts_the_oldest_over_budget() {
        let root = tempdir();
        // Five fresh entries; the budget is clamped to MIN_CACHE_BUDGET
        // for a tiny build, so the *count* is what limits retention.
        let names: Vec<String> = (1..=5u8)
            .map(|n| entry(&root, &hash_name(n), 1024, Duration::from_secs(5 - n as u64)))
            .collect();
        // The current build is the newest, as it would be in practice.
        let current: HashSet<String> = [names[4].clone()].into_iter().collect();

        let report = sweep(&root, &current, 1024, NOW);

        assert_eq!(report.kept, KEEP_RECENT);
        assert_eq!(sorted(report.removed), sorted(names[..2].to_vec()));
        for gone in &names[..2] {
            assert!(!root.join(gone).exists(), "{gone} should be evicted");
        }
        for kept in &names[2..] {
            assert!(root.join(kept).exists(), "{kept} should survive");
        }
    }

    #[test]
    fn sweep_respects_count_even_with_a_huge_budget() {
        // 100 tiny entries under an enormous build size: the count must
        // still bound retention, so the directory count can't run away.
        let root = tempdir();
        for n in 1..=100u8 {
            entry(&root, &hash_name(n), 1, HOUR);
        }
        let current: HashSet<String> = HashSet::new();

        let report = sweep(&root, &current, MAX_CACHE_BUDGET, NOW);

        assert_eq!(report.kept, KEEP_RECENT);
        assert_eq!(report.removed.len(), 100 - KEEP_RECENT);
    }

    #[test]
    fn sweep_evicts_by_age_regardless_of_budget() {
        // Under budget and recently ranked, but untouched for 40 days:
        // the age filter is what catches the long tail the budget never
        // would.
        let root = tempdir();
        let old = entry(
            &root,
            &hash_name(1),
            10,
            Duration::from_secs(40 * 24 * 3600),
        );
        let current: HashSet<String> = HashSet::new();

        let report = sweep(&root, &current, 0, NOW);

        assert!(!root.join(&old).exists());
        assert_eq!(report.kept, 0);
        assert_eq!(report.bytes_freed, 10);
    }

    #[test]
    fn sweep_keeps_entries_younger_than_the_age_limit() {
        let root = tempdir();
        let recent = entry(&root, &hash_name(1), 10, Duration::from_secs(29 * 24 * 3600));
        let current: HashSet<String> = HashSet::new();

        let report = sweep(&root, &current, 0, NOW);

        assert!(root.join(&recent).exists());
        assert_eq!(report.kept, 1);
    }

    #[test]
    fn sweep_ignores_anything_that_is_not_a_hash_directory() {
        let root = tempdir();
        // The rate-limit stamp, a stray file, and a user-created
        // directory must all survive untouched.
        let stamp = root.join(SWEEP_STAMP);
        std::fs::write(&stamp, "0").unwrap();
        let stray = root.join("notes.txt");
        std::fs::write(&stray, b"hello").unwrap();
        let user_dir = root.join("my-backup");
        std::fs::create_dir_all(&user_dir).unwrap();
        std::fs::write(user_dir.join("data.bin"), b"mine").unwrap();
        // Uppercase hex is not ours either.
        let upper = root.join("A".repeat(64));
        std::fs::create_dir_all(&upper).unwrap();

        sweep(&root, &HashSet::new(), 0, NOW);

        assert!(stamp.exists());
        assert!(stray.exists());
        assert!(user_dir.join("data.bin").exists());
        assert!(upper.exists());
    }

    #[test]
    fn sweep_removes_an_unfinished_extraction() {
        // A hash directory with no `app.jar` is a half-finished write
        // from an interrupted run; nothing can load it.
        let root = tempdir();
        let partial = root.join(hash_name(1));
        std::fs::create_dir_all(&partial).unwrap();

        let report = sweep(&root, &HashSet::new(), 0, NOW);

        assert!(!partial.exists());
        assert_eq!(report.removed, vec![hash_name(1)]);
    }

    #[test]
    fn sweep_keeps_an_unfinished_current_extraction() {
        // Same shape, but it belongs to the running build: the
        // extraction loop has already reported the real failure and the
        // sweep must not mask it by deleting the directory.
        let root = tempdir();
        let partial = hash_name(1);
        std::fs::create_dir_all(root.join(&partial)).unwrap();
        let current: HashSet<String> = [partial.clone()].into_iter().collect();

        let report = sweep(&root, &current, 0, NOW);

        assert!(root.join(&partial).exists());
        assert!(report.removed.is_empty());
    }

    #[test]
    fn sweep_tolerates_a_missing_root() {
        let missing = tempdir().join("does-not-exist");
        let report = sweep(&missing, &HashSet::new(), 0, NOW);
        assert_eq!(report, SweepReport::default());
    }

    #[test]
    fn budget_clamps_at_both_ends() {
        // Floor: a tiny build still gets the minimum rather than being
        // rationed to a few bytes.
        assert_eq!(budget_for(0), MIN_CACHE_BUDGET);
        assert_eq!(budget_for(1), MIN_CACHE_BUDGET);
        // Mid-range: scales with the build.
        let mid = MIN_CACHE_BUDGET * 4;
        assert_eq!(budget_for(mid), mid * KEEP_RECENT as u64);
        // Ceiling: an enormous build is capped, and can't overflow.
        assert_eq!(budget_for(MAX_CACHE_BUDGET), MAX_CACHE_BUDGET);
        assert_eq!(budget_for(u64::MAX / 2), MAX_CACHE_BUDGET);
        assert_eq!(budget_for(u64::MAX), MAX_CACHE_BUDGET);
    }

    #[test]
    fn should_sweep_rate_limits_repeat_calls() {
        let root = tempdir();
        assert!(should_sweep(&root, NOW), "first call should run");
        assert!(
            !should_sweep(&root, NOW + HOUR),
            "a second call within the interval must be skipped"
        );
        assert!(
            should_sweep(&root, NOW + SWEEP_INTERVAL + HOUR),
            "a call past the interval should run"
        );
    }

    #[test]
    fn should_sweep_runs_when_the_stamp_is_unreadable() {
        let root = tempdir();
        std::fs::write(root.join(SWEEP_STAMP), b"not a number").unwrap();
        assert!(should_sweep(&root, NOW));
    }

    #[test]
    fn is_hash_dir_requires_exactly_64_lowercase_hex() {
        assert!(is_hash_dir(&hash_name(255)));
        assert!(is_hash_dir(&"a".repeat(64)));
        assert!(!is_hash_dir(&"a".repeat(63)));
        assert!(!is_hash_dir(&"a".repeat(65)));
        assert!(!is_hash_dir(&"A".repeat(64)));
        assert!(!is_hash_dir(&"g".repeat(64)));
        assert!(!is_hash_dir(SWEEP_STAMP));
        assert!(!is_hash_dir("app.jar"));
    }
}
