//! Per-user cache for extracted JARs.
//!
//! Default layout on Windows:
//!
//! ```text
//! %LOCALAPPDATA%\snug\<company>\<app>\<jar-sha256>\app.jar
//! %LOCALAPPDATA%\snug\jars\<jar-sha256>\app.jar
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
//! # The shared store
//!
//! The second path is the **shared JAR store**: one copy of each
//! distinct JAR, under the same namespace as the per-application roots.
//! An application's entry is a *hard link* to the store's, so two
//! applications built from the same library stack hold the bytes once
//! between them. On a filesystem without hard links (FAT32, exFAT)
//! the link degrades to a copy, which is exactly the old behaviour.
//!
//! The store has to be on the same volume as the application's root for
//! a link to work at all, so it lives under the same cache base. It is
//! disabled outright when the application's cache root was explicitly
//! overridden — see [`store_root`].
//!
//! # Retention
//!
//! Entries expire. [`touch`] stamps a cached JAR's mtime on every
//! launch that uses it, and [`sweep`] deletes the ones the application
//! no longer needs, so the cache cannot grow without bound across
//! rebuilds.
//!
//! The application's own root keeps **one generation**: the running
//! build's JARs, and nothing else. A previous build's JARs are not
//! worth a second copy of the application on disk, and everything the
//! application might still want is recoverable from the store or from
//! the payload.
//!
//! The store keeps entries for [`MAX_UNUSED_AGE`] after they were last
//! used, which is what makes switching back to a previous build cheap:
//! its JARs are still in the store, so the launch re-links them rather
//! than copying a few hundred megabytes again.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{File, FileTimes};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use snug_format::AppMetadata;
use snug_format::cache::cache_root_segments;

/// Default top-level subdirectory under the per-user data root.
///
/// The namespace rules live in `snug-format` because the builder shows
/// this layout in the build log, and the two cannot drift apart.
pub use snug_format::cache::SNUG_SUBDIR;

/// The shared JAR store's name inside the `snug` namespace.
///
/// A reserved name: the launcher walks that namespace to find every
/// application's cache root, and the store is not one.
pub use snug_format::cache::STORE_SUBDIR;

/// Default JAR filename inside a cache entry.
pub const CACHED_JAR_NAME: &str = "app.jar";

/// Environment variable that redirects the per-user **cache base**.
///
/// A *base*, not a root: `snug/<company>/<app>/` is still appended, exactly
/// as for `$XDG_CACHE_HOME`. Treating it as the whole root would be the
/// surprising reading and would let two different apps collide in one
/// directory.
///
/// This exists because the platform answer can be unwritable. A sandboxed
/// home, a locked-down CI runner, or a container that denies writes to
/// `~/Library/Caches` otherwise makes the launcher fail at the very first
/// step — extracting the JAR — with `I/O error: Operation not permitted`
/// and nothing to suggest a way out. An escape hatch that is part of the
/// contract beats telling a user to relocate their home directory.
///
/// Only an **absolute** path is honoured. A relative value is ignored and
/// the platform answer wins, which is the same rule the XDG handling below
/// already uses; resolving a relative path against whatever the launcher's
/// working directory happens to be would scatter cache entries.
pub const ENV_CACHE_DIR: &str = "SNUG_CACHE_DIR";

/// Compute the per-user cache root for the given app metadata.
///
/// Resolution order:
/// 1. Explicit override (passed via `behavior.cache_dir` or `app.cache_dir`).
/// 2. `$SNUG_CACHE_DIR`, if set to an absolute path. See [`ENV_CACHE_DIR`].
/// 3. The platform's own per-user cache root, via [`platform_cache_base`]:
///    `%LOCALAPPDATA%` on Windows, `NSHomeDirectory()/Library/Caches` on
///    macOS, `$XDG_CACHE_HOME` / `~/.cache` elsewhere.
/// 4. `$HOME/.cache`, then the temp dir, as a last resort.
///
/// `snug/<company>/<app>/` is appended to whichever base wins.
pub fn cache_root(app: &AppMetadata, override_root: Option<&Path>) -> PathBuf {
    if let Some(root) = override_root {
        return root.to_path_buf();
    }

    let mut root = cache_base();
    for segment in cache_root_segments(&app.company, &app.name) {
        root = root.join(segment);
    }
    root
}

/// The per-user cache base, without the `snug` namespace.
///
/// Resolution is the same as for [`cache_root`] minus the application's
/// own segments: `$SNUG_CACHE_DIR` if it is an absolute path, else the
/// platform's own answer, else `$HOME/.cache` and the temp directory.
fn cache_base() -> PathBuf {
    pick_cache_base(std::env::var_os(ENV_CACHE_DIR), platform_cache_base())
}

/// The shared JAR store, or `None` when the store cannot be used.
///
/// `None` means the application's cache root was explicitly overridden
/// (`--cache-dir`). That override names the application's own directory,
/// wherever the user put it, and a hard link only works within one
/// volume — so a store on the platform's base could be on a different
/// volume from the application, and every "shared" copy would silently
/// be a second copy on disk. Better to keep the old behaviour for an
/// explicitly relocated cache than to double the space it uses.
///
/// The store shares the application's cache base, so the two are always
/// on the same volume and a link between them always works.
pub fn store_root(override_root: Option<&Path>) -> Option<PathBuf> {
    if override_root.is_some() {
        return None;
    }
    let mut root = cache_base();
    for segment in snug_format::cache::store_segments() {
        root = root.join(segment);
    }
    Some(root)
}

/// The store's path for one JAR's bytes.
///
/// The same shape as [`cached_jar_path`], under the store instead of an
/// application root, so the digest names the entry in both.
pub fn store_entry_path(store: &Path, sha256: &[u8; 32]) -> PathBuf {
    store.join(hex_lower(sha256)).join(CACHED_JAR_NAME)
}

/// Choose the cache base from the two candidates, falling back when neither
/// is usable.
///
/// The `env` value is a raw `OsString` rather than an already-parsed
/// `PathBuf` so the absolute-path rule is applied *here*, next to the
/// fallback chain it feeds. That is also what makes this testable without
/// mutating the process environment: the environment is process-global, so
/// a test that called `std::env::set_var` to prove the override worked would
/// race every other test in the same binary, and whichever lost would fail
/// somewhere unrelated. Passing the value in keeps every assertion here
/// hermetic.
fn pick_cache_base(env: Option<OsString>, platform: Option<PathBuf>) -> PathBuf {
    let from_env = env.map(PathBuf::from).filter(|p| p.is_absolute());
    from_env.or(platform).unwrap_or_else(fallback_cache_base)
}

/// Compute the full path to the cached JAR for a given SHA-256 digest.
pub fn cached_jar_path(root: &Path, sha256: &[u8; 32]) -> PathBuf {
    let hex = hex_lower(sha256);
    root.join(hex).join(CACHED_JAR_NAME)
}

/// A sibling path to stage a write through, unique to this process.
///
/// Never the final name: a reader must see either the old file or the new
/// one, never a half-written mixture.
pub fn temp_sibling(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(format!(".tmp-{}", std::process::id()));
    path.with_file_name(name)
}

/// Write `bytes` to `dest`, atomically and only if needed.
///
/// Two properties, both learned from a cache that would not heal:
///
/// * **Atomic.** The previous shape was `dest.exists()` then `fs::write`,
///   which truncates in place. An interrupted write -- a full disk, a kill,
///   or simply two launches of the same app racing -- left a short
///   `app.jar`, and every later run *accepted* it, because `exists()` was
///   the only test. The app then failed with a `ZipException` (or "no
///   Main-Class found") until the user deleted the cache by hand.
///   Staging into a sibling and renaming means a reader sees either no
///   file or a complete one.
///
/// * **Verified.** `exists()` said nothing about *which* file. Comparing
///   the length turns a truncated entry from permanent breakage into a
///   repair the next launch performs on its own.
///
/// The temp name carries the pid so two launches never stage through the
/// same file; `rename` is atomic, so both succeed and the later one wins
/// with identical content. A temp left by a hard kill is not reclaimed
/// here -- `sweep` owns the cache's retention policy, and this is a single
/// bounded file rather than an entry.
pub fn ensure_cached_atomic(dest: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if is_correct_len(dest, bytes.len()) {
        return Ok(());
    }
    write_atomic(dest, bytes)
}

/// True when `path` is a file of exactly `len` bytes.
///
/// The length is the whole test, deliberately: `exists()` says nothing
/// about *which* file, and a truncated entry accepted as correct is
/// permanent breakage (see [`ensure_cached_atomic`]).
fn is_correct_len(path: &Path, len: usize) -> bool {
    std::fs::metadata(path)
        .map(|m| m.len() == len as u64)
        .unwrap_or(false)
}

/// Write `bytes` to `dest` atomically, replacing whatever was there.
fn write_atomic(dest: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = temp_sibling(dest);
    match std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, dest)) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Don't leave our own staging file behind on a failed write.
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Put `src`'s bytes at `dest`, as a hard link where the filesystem
/// allows one.
///
/// The link is made through a staging name and renamed into place, so a
/// reader at `dest` sees either the old file or the new one, never a
/// half-made mixture — the same atomicity [`write_atomic`] buys, with
/// the bytes shared rather than copied.
///
/// A hard link cannot cross a volume, and FAT32 and exFAT have none at
/// all. Both cases fall back to a copy, which is the old behaviour:
/// correct, and no worse than before. [`store_supports_links`] normally
/// keeps the store out of the way on such a filesystem.
fn link_over(src: &Path, dest: &Path) -> std::io::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = temp_sibling(dest);
    let _ = std::fs::remove_file(&tmp);
    if std::fs::hard_link(src, &tmp).is_err() {
        std::fs::copy(src, &tmp)?;
    }
    match std::fs::rename(&tmp, dest) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Whether the store's filesystem can hold hard links.
///
/// Probed once per process, by linking two files in the store itself.
///
/// FAT32 and exFAT have no hard links, and a store that fell back to
/// copying would be *worse* than no store: the JAR would be written
/// twice, and copied again on every launch, because the two copies
/// would never be the same file. So a filesystem that cannot link does
/// not get a store at all.
///
/// The answer is cached for the process. There is one store per launch,
/// so one answer per process is the right granularity.
fn store_supports_links(store: &Path) -> bool {
    static PROBED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PROBED.get_or_init(|| probe_hard_links(store))
}

/// Try to link two files in `dir`, and say whether it worked.
fn probe_hard_links(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let source = temp_sibling(&dir.join("link-probe-a"));
    let target = temp_sibling(&dir.join("link-probe-b"));
    let _ = std::fs::remove_file(&source);
    let _ = std::fs::remove_file(&target);
    let ok =
        std::fs::write(&source, b"probe").is_ok() && std::fs::hard_link(&source, &target).is_ok();
    let _ = std::fs::remove_file(&source);
    let _ = std::fs::remove_file(&target);
    ok
}

/// True when two paths name the same file.
///
/// The difference between "the application's copy is already the shared
/// one" and "the application has a private copy" — which is the
/// difference between one copy of a JAR on the disk and two.
#[cfg(windows)]
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    /// (volume serial number, file index) — the pair Windows treats as a
    /// file's identity, and the only thing a hard link preserves.
    fn identity(path: &Path) -> Option<(u32, u64)> {
        let file = std::fs::File::open(path).ok()?;
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) };
        if ok == 0 {
            return None;
        }
        let index = ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64;
        Some((info.dwVolumeSerialNumber, index))
    }

    match (identity(a), identity(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// True when two paths name the same file.
#[cfg(unix)]
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
        _ => false,
    }
}

/// True when two paths name the same file.
///
/// Unknown platforms cannot answer, so the answer is "no" — which makes
/// the caller re-link rather than assume the copy is already shared.
/// Re-linking is wasteful, never wrong.
#[cfg(not(any(windows, unix)))]
fn same_file(_a: &Path, _b: &Path) -> bool {
    false
}

/// Ensure `dest` holds `bytes`, sharing them with the store when there
/// is one.
///
/// With a store, the bytes live in the store and `dest` is a hard link
/// to them; without one, this is [`ensure_cached_atomic`] and the
/// application's root holds the only copy.
///
/// The two paths are made to agree in both directions, because either
/// can be the one that already exists:
///
/// * The store entry missing but the application's copy good — the
///   application's copy is *adopted* as the shared one. A hard link
///   costs a directory entry, not the bytes, so this shares what is
///   already on disk instead of writing a second copy of it. It is also
///   how a cache written before the store existed is absorbed without
///   doubling the space it already uses.
/// * The store entry present but the application's copy missing or
///   truncated — the application re-links to the store's bytes.
/// * Both present and already the same file — nothing to do, which is
///   the case on every launch after the first.
///
/// The last case is the one that needs [`same_file`]: without it the
/// only way to be sure is to re-link every time, and a re-link that
/// fails on Windows (the file is open in another instance) would leave
/// the application with a private copy for no reason.
///
/// The store is an optimisation and the application's own copy is the
/// fallback, so a store that cannot be written never fails the launch:
/// the application keeps its own copy, exactly as it did before.
pub fn ensure_cached_shared(
    store: Option<&Path>,
    sha256: &[u8; 32],
    dest: &Path,
    bytes: &[u8],
) -> std::io::Result<()> {
    let Some(store) = store else {
        return ensure_cached_atomic(dest, bytes);
    };

    // A store that cannot link cannot share, and is worse than no store
    // at all. See [`store_supports_links`].
    if !store_supports_links(store) {
        return ensure_cached_atomic(dest, bytes);
    }

    let entry = store_entry_path(store, sha256);
    let dest_ok = is_correct_len(dest, bytes.len());

    if !dest_ok {
        // The application needs a copy. The store writes the bytes once
        // and the application links to them, so they are stored once.
        if is_correct_len(&entry, bytes.len()) {
            if link_over(&entry, dest).is_ok() {
                return Ok(());
            }
        } else if write_atomic(&entry, bytes).is_ok() {
            if link_over(&entry, dest).is_ok() {
                return Ok(());
            }
            // The store's copy is referenced by nothing, so it is dead
            // weight rather than a shared copy.
            if let Some(dir) = entry.parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
        // The store could not serve it after all. The application's own
        // copy is the fallback, and it is what always worked.
        return ensure_cached_atomic(dest, bytes);
    }

    // The application already has the right bytes. Share them with the
    // store, so that another application built from the same library
    // stack can use them too.
    if !is_correct_len(&entry, bytes.len()) {
        // Adopt the application's copy: a link costs a directory entry,
        // not the bytes, so nothing is written twice.
        let _ = link_over(dest, &entry);
    } else if !same_file(dest, &entry) {
        // A private copy and a store copy of the same JAR. Make the
        // application's entry the link, so the bytes are stored once.
        let _ = link_over(&entry, dest);
    }
    Ok(())
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

/// How many generations of an application to retain in its own root.
///
/// One. The application's root holds the running build and nothing else:
/// a second generation is a second copy of the application on disk, and
/// anything it might still be wanted for is recoverable from the store
/// or from the payload.
///
/// The running build's own entries count toward this depth, so a build
/// of three JARs fills it on its own.
const KEEP_RECENT: usize = 1;

/// Evict a store entry this long after it was last used.
///
/// The store's horizon, and the only time-based rule in the cache. The
/// per-application root has no age rule because it has no history to
/// age out: it holds one generation, and the store holds the previous
/// ones.
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
/// those are never evicted, and everything else is a previous generation
/// of this application.
///
/// The policy is one generation. The running build's entries fill the
/// depth on their own, so an older generation goes on the next sweep
/// whether it was used yesterday or a year ago. What it costs is a
/// re-extraction for anyone who launches the older build again — and the
/// store is what stops that being expensive, because the older build's
/// JARs are still in it.
///
/// `now` is a parameter rather than read from the clock so the whole
/// policy is testable without waiting 30 days.
pub fn sweep(root: &Path, current: &HashSet<String>, now: SystemTime) -> SweepReport {
    let mut report = SweepReport::default();

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

    // The running build's own entries fill the depth, and they are
    // counted *before* anything else is considered. Counting them as
    // the scan reached them would make the depth depend on which entry
    // happened to have the newest timestamp — and the running build is
    // not necessarily the newest, because a rebuild produces a JAR
    // whose digest sorts nowhere near the old one's.
    let mut kept = entries
        .iter()
        .filter(|(name, _, _)| current.contains(name))
        .count();

    for (name, _mtime, size) in entries {
        // The current build is never evicted. This is the one invariant
        // that must hold unconditionally — everything else is
        // recoverable, because a missing entry is re-extracted from the
        // payload next launch.
        if current.contains(&name) {
            continue;
        }

        // Depth is measured against the running build's own entries, so
        // with one generation kept there is no room for an older one.
        if kept < KEEP_RECENT {
            kept += 1;
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

    report.kept = kept;
    report
}

/// Delete store entries nothing needs any more.
///
/// An entry is kept while it was used within [`MAX_UNUSED_AGE`], and
/// kept for as long as any application's cache root still points at it.
///
/// That second rule is what makes a rollback cheap. An application that
/// has not run for a month still links its build's JARs, and those links
/// are the only other copy of the bytes — deleting the store's copy
/// would mean copying them again from the payload to get them back.
/// The age rule alone would have thrown them away.
///
/// The store has no byte ceiling. A ceiling measured over the store's
/// own entries would free nothing anyway, because a linked application's
/// copy keeps the bytes alive, and it would evict precisely the shared
/// libraries that make the store worth having.
///
/// `now` is a parameter rather than read from the clock so the whole
/// policy is testable without waiting 30 days.
pub fn sweep_store(store: &Path, now: SystemTime) -> SweepReport {
    let mut report = SweepReport::default();

    // The namespace holds the store and every application's root, so
    // its parent is where the references live.
    let Some(namespace) = store.parent() else {
        return report;
    };
    let referenced = referenced_digests(namespace, store);

    let Ok(dir) = std::fs::read_dir(store) else {
        return report;
    };

    let mut entries: Vec<(String, SystemTime, u64)> = Vec::new();
    let mut partial: Vec<String> = Vec::new();

    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_hash_dir(&name) {
            continue;
        }
        match std::fs::metadata(entry.path().join(CACHED_JAR_NAME)) {
            Ok(m) => entries.push((name, m.modified().unwrap_or(now), m.len())),
            Err(_) => partial.push(name),
        }
    }

    for name in partial {
        if referenced.contains(&name) {
            continue;
        }
        match std::fs::remove_dir_all(store.join(&name)) {
            Ok(()) => report.removed.push(name),
            Err(e) => report.errors.push(format!("{name}: {e}")),
        }
    }

    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    for (name, mtime, size) in entries {
        if referenced.contains(&name) {
            report.kept += 1;
            continue;
        }

        let stale = now
            .duration_since(mtime)
            .is_ok_and(|age| age > MAX_UNUSED_AGE);
        if !stale {
            report.kept += 1;
            continue;
        }

        match std::fs::remove_dir_all(store.join(&name)) {
            Ok(()) => {
                report.bytes_freed += size;
                report.removed.push(name);
            }
            Err(e) => report.errors.push(format!("{name}: {e}")),
        }
    }

    report
}

/// The digests that some application's cache root still points at.
///
/// Every application's root is `<namespace>\<company>\<app>\`, and a
/// hash-named directory inside one is a reference to that JAR's content.
/// The store itself is skipped by path rather than by name, so a
/// company that happens to be called `jars` is still counted.
fn referenced_digests(namespace: &Path, store: &Path) -> HashSet<String> {
    let mut referenced = HashSet::new();

    let Ok(companies) = std::fs::read_dir(namespace) else {
        return referenced;
    };
    for company in companies.flatten() {
        if company.path() == store {
            continue;
        }
        let Ok(apps) = std::fs::read_dir(company.path()) else {
            continue;
        };
        for app in apps.flatten() {
            let Ok(entries) = std::fs::read_dir(app.path()) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if is_hash_dir(&name) {
                    referenced.insert(name);
                }
            }
        }
    }
    referenced
}

/// Rate-limit the sweep to one pass per [`SWEEP_INTERVAL`]. Returns
/// whether a sweep should run now.
///
/// The stamp is written *before* the sweep rather than after, so a
/// crash mid-pass cannot become a tight retry loop on every launch.
pub fn should_sweep(root: &Path, now: SystemTime) -> bool {
    should_sweep_at(&root.join(SWEEP_STAMP), now)
}

/// Rate-limit the store sweep to one pass per [`SWEEP_INTERVAL`].
///
/// The store is shared by every application, so its stamp lives in the
/// store: one pass per interval for the whole machine, not one per
/// application that happens to launch.
pub fn should_sweep_store(store: &Path, now: SystemTime) -> bool {
    should_sweep_at(&store.join(SWEEP_STAMP), now)
}

/// The rate limit itself, against a stamp file.
fn should_sweep_at(stamp: &Path, now: SystemTime) -> bool {
    let last = std::fs::read_to_string(stamp)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs));

    if let Some(last) = last
        && now
            .duration_since(last)
            .is_ok_and(|gap| gap < SWEEP_INTERVAL)
    {
        return false;
    }

    if let Some(parent) = stamp.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        stamp,
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
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
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
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
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
    use snug_format::cache::sanitize_component;

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

        assert!(
            root.starts_with(&base),
            "{} not under {}",
            root.display(),
            base.display()
        );
        assert!(
            root.ends_with("snug/SynapticLoop/Demo"),
            "got {}",
            root.display()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn app_support_base_is_application_support_not_caches() {
        let support = platform_app_support_base().expect("Foundation should answer");
        assert!(
            support.is_absolute(),
            "{} should be absolute",
            support.display()
        );
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

    // Unix keeps application support and caches in separate directories so
    // the OS reclaims caches without touching data. Windows has no such
    // split, and `platform_app_support_base` returns the cache directory
    // there on purpose (see its own doc comment), so this invariant is
    // asserted only where it is meant to hold.
    //
    // It also could not have been made to pass on Windows by fixing the
    // fixture: both fallbacks branch on `HOME`, which a Windows shell does
    // not define. Under Git Bash - which *does* set HOME - the two resolve
    // to `$HOME/.cache` and `$HOME/.local/share` and this test passed. In
    // cmd or PowerShell they collapse to the same `temp_dir()` and it
    // failed. An assertion whose outcome depends on an unrelated
    // environment variable is the real bug, and gating removes it rather
    // than papering over one machine's environment.
    #[cfg(not(windows))]
    #[test]
    fn app_support_fallback_is_not_the_cache_fallback() {
        let support = fallback_app_support_base();
        let cache = fallback_cache_base();
        assert_ne!(support, cache);
        assert!(support.is_absolute());
    }

    /// The Windows half of the pair above: here the two bases are
    /// *deliberately* the same directory, so that the install path does
    /// not move. Without this, Windows simply had no coverage of the
    /// behaviour its own implementation documents.
    #[cfg(windows)]
    #[test]
    fn on_windows_app_support_is_deliberately_the_cache_directory() {
        assert_eq!(platform_app_support_base(), platform_cache_base());
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

    /// A directory that is genuinely absolute on *every* platform.
    ///
    /// `pick_cache_base` drops an env value that is not absolute, so a
    /// fixture has to be absolute or it silently exercises the
    /// relative-path branch instead of the one under test. `/var/empty`
    /// and `/Users/someone/...` read as absolute but are not on Windows,
    /// where only a drive prefix or a UNC path counts - which is how these
    /// two tests came to assert nothing about the override on Windows.
    ///
    /// `temp_dir()` is absolute on every platform cargo supports, and the
    /// tests below never touch the filesystem, so this names nothing real.
    fn an_absolute_dir() -> PathBuf {
        std::env::temp_dir().join("snug-cache-fixture")
    }

    #[test]
    fn env_cache_base_wins_over_the_platform_base() {
        // The whole point of the override: a home whose platform cache
        // directory is unwritable, pointed somewhere that is not.
        let env = an_absolute_dir();
        let platform = PathBuf::from("/Users/someone/Library/Caches");
        assert_eq!(
            pick_cache_base(Some(env.clone().into_os_string()), Some(platform)),
            env
        );
    }

    #[test]
    fn env_cache_base_keeps_the_snug_company_app_namespace() {
        // A *base*, not a root. If this ever stopped appending the
        // namespace, two apps sharing one SNUG_CACHE_DIR would overwrite
        // each other's cached JARs.
        //
        // Compared against `env` rather than a literal, so the assertion
        // holds whatever the platform's absolute-path form happens to be.
        let env = an_absolute_dir();
        let base = pick_cache_base(Some(env.clone().into_os_string()), None);
        let root = base
            .join(SNUG_SUBDIR)
            .join("Example Company Pty Ltd")
            .join("Demo");
        assert!(root.starts_with(&env), "got {}", root.display());
        assert!(
            root.ends_with("snug/Example Company Pty Ltd/Demo"),
            "got {}",
            root.display()
        );
    }

    #[test]
    fn a_relative_env_cache_base_is_ignored() {
        // A relative path resolved against the launcher's working
        // directory would scatter cache entries; the platform answer wins
        // instead, matching how XDG_CACHE_HOME is already treated.
        let platform = PathBuf::from("/Users/someone/Library/Caches");
        assert_eq!(
            pick_cache_base(
                Some(OsString::from("relative/cache")),
                Some(platform.clone())
            ),
            platform
        );
    }

    #[test]
    fn an_empty_env_cache_base_is_ignored() {
        // `SNUG_CACHE_DIR=` in a shell is an empty string, not an unset
        // variable. Treating it as a path would silently redirect the
        // cache to a relative directory named after the CWD.
        let platform = PathBuf::from("/Users/someone/Library/Caches");
        assert_eq!(
            pick_cache_base(Some(OsString::new()), Some(platform.clone())),
            platform
        );
    }

    #[test]
    fn an_unset_env_cache_base_leaves_the_platform_base_alone() {
        // The overwhelmingly common case must not move: no variable set
        // means the same path as before this override existed.
        let platform = PathBuf::from("/Users/someone/Library/Caches");
        assert_eq!(pick_cache_base(None, Some(platform.clone())), platform);
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
        let root = cache_root(
            &app("Example Company Pty Ltd", "Demo"),
            Some(&override_path),
        );
        assert_eq!(root, override_path);
    }

    #[test]
    fn cache_root_returns_override_directly() {
        let override_path = PathBuf::from("/tmp/x");
        let root = cache_root(
            &app("Example Company Pty Ltd", "Demo"),
            Some(&override_path),
        );
        assert_eq!(root, override_path);
    }

    #[test]
    fn cache_root_namespaces_under_snug_subdir() {
        // With an explicit override the helper should return the
        // override untouched; without one, the path must contain the
        // top-level `snug` segment before the company / app segments.
        let app = app("Example Company Pty Ltd", "Demo");
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
            .position(|p| *p == "Example Company Pty Ltd")
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
        assert_eq!(
            sanitize_component("Example Company Pty Ltd"),
            "Example Company Pty Ltd"
        );
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

    // --- Atomic extraction (F-10) ----------------------------------------

    #[test]
    fn a_correct_cached_jar_is_left_alone() {
        let dir = tempdir();
        let dest = dir.join(CACHED_JAR_NAME);
        let bytes = b"PK\x03\x04 a perfectly good jar".to_vec();
        ensure_cached_atomic(&dest, &bytes).unwrap();
        let first = std::fs::read(&dest).unwrap();

        // Second call must be a no-op, not a rewrite.
        ensure_cached_atomic(&dest, &bytes).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), first);
        assert!(
            staging_files(&dir).is_empty(),
            "nothing is staged once the write has landed: {:?}",
            staging_files(&dir)
        );
    }

    #[test]
    fn a_truncated_cached_jar_is_repaired() {
        // The bug this exists for. An interrupted `fs::write` left a short
        // `app.jar`, `exists()` returned true, and every later launch
        // accepted it -- so the app stayed broken with a ZipException until
        // the user deleted the cache by hand. A length check turns that into
        // a repair the next launch performs itself.
        let dir = tempdir();
        let dest = dir.join(CACHED_JAR_NAME);
        let bytes = b"PK\x03\x04 the whole fat jar, all of it".to_vec();

        // Exactly the state an interrupted write leaves behind.
        std::fs::write(&dest, &bytes[..10]).unwrap();
        assert_eq!(std::fs::metadata(&dest).unwrap().len(), 10);

        ensure_cached_atomic(&dest, &bytes).unwrap();
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            bytes,
            "the truncated entry must be rewritten in full"
        );
    }

    #[test]
    fn extraction_never_leaves_a_partial_file_at_the_canonical_path() {
        // The atomicity property, stated as a test: whatever happens to the
        // write, a reader looking at the canonical path sees a complete file
        // or nothing -- never a mixture.
        let dir = tempdir();
        let dest = dir.join(CACHED_JAR_NAME);
        let bytes = b"x".repeat(64 * 1024);

        ensure_cached_atomic(&dest, &bytes).unwrap();
        let seen = std::fs::read(&dest).unwrap();
        assert_eq!(seen.len(), bytes.len());
        assert_eq!(seen, bytes);

        // And the staging file is not left behind on the happy path.
        assert!(
            staging_files(&dir).is_empty(),
            "staging files must not survive a successful write: {:?}",
            staging_files(&dir)
        );
    }

    #[test]
    fn a_failed_write_does_not_leave_its_staging_file() {
        // A staging file left behind is dead weight the sweep has no policy
        // for, so the helper cleans up its own.
        let dir = tempdir();
        // A directory where the file should go makes the rename fail.
        let dest = dir.join("blocked");
        std::fs::create_dir_all(&dest).unwrap();
        assert!(ensure_cached_atomic(&dest, b"payload").is_err());
        assert!(
            staging_files(&dir).is_empty(),
            "staging leaked: {:?}",
            staging_files(&dir)
        );
    }

    /// Staging files this helper leaves in `dir`, if any.
    fn staging_files(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.contains(".tmp-"))
                    .collect()
            })
            .unwrap_or_default()
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
        assert!(
            before < SystemTime::now(),
            "fixture should start in the past"
        );

        touch(&jar).unwrap();

        let after = std::fs::metadata(&jar).unwrap().modified().unwrap();
        assert!(
            after > before,
            "touch must advance the mtime: {before:?} -> {after:?}"
        );
    }

    #[test]
    fn sweep_never_removes_the_current_build() {
        // The current build is deliberately the *oldest* entry. It has to
        // survive anyway: everything else is recoverable by re-extraction
        // from the payload, the running app's own classpath is not.
        let root = tempdir();
        let names: Vec<String> = (1..=5u8)
            .map(|n| entry(&root, &hash_name(n), 64, Duration::from_secs(5 - n as u64)))
            .collect();
        let oldest = names[0].clone();
        let current: HashSet<String> = [oldest.clone()].into_iter().collect();

        let report = sweep(&root, &current, NOW);

        assert!(
            root.join(&oldest).exists(),
            "the current build must survive even when it ranks last"
        );
        assert!(!report.removed.contains(&oldest));
        assert_eq!(
            sorted(report.removed),
            sorted(names[1..].to_vec()),
            "every previous generation goes, however recently it was used"
        );
    }

    #[test]
    fn the_application_root_keeps_one_generation() {
        // One. A second generation is a second copy of the application
        // on disk, and the store is where the previous build's JARs live
        // until they age out of it.
        let root = tempdir();
        let names: Vec<String> = (1..=4u8)
            .map(|n| entry(&root, &hash_name(n), 64, Duration::from_secs(4 - n as u64)))
            .collect();
        let current: HashSet<String> = [names[3].clone()].into_iter().collect();

        let report = sweep(&root, &current, NOW);

        assert_eq!(report.kept, KEEP_RECENT);
        assert!(root.join(&names[3]).exists());
        for gone in &names[..3] {
            assert!(!root.join(gone).exists(), "{gone} should be evicted");
        }
    }

    #[test]
    fn a_multi_jar_build_keeps_all_of_its_own_jars() {
        // The depth is counted in entries and the running build's own
        // entries fill it, so a build of three JARs keeps all three —
        // and still keeps no previous generation.
        let root = tempdir();
        let names: Vec<String> = (1..=5u8)
            .map(|n| entry(&root, &hash_name(n), 64, Duration::from_secs(5 - n as u64)))
            .collect();
        let current: HashSet<String> = names[..3].iter().cloned().collect();

        let report = sweep(&root, &current, NOW);

        assert_eq!(report.kept, 3);
        for kept in &names[..3] {
            assert!(root.join(kept).exists(), "{kept} should survive");
        }
        for gone in &names[3..] {
            assert!(!root.join(gone).exists(), "{gone} should be evicted");
        }
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

        sweep(&root, &HashSet::new(), NOW);

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

        let report = sweep(&root, &HashSet::new(), NOW);

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

        let report = sweep(&root, &current, NOW);

        assert!(root.join(&partial).exists());
        assert!(report.removed.is_empty());
    }

    #[test]
    fn sweep_tolerates_a_missing_root() {
        let missing = tempdir().join("does-not-exist");
        let report = sweep(&missing, &HashSet::new(), NOW);
        assert_eq!(report, SweepReport::default());
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

    // --- The shared store -------------------------------------------------

    /// A digest, and the hex name the cache stores it under.
    fn digest_and_name(n: u8) -> ([u8; 32], String) {
        let digest = [n; 32];
        let name = hex_lower(&digest);
        (digest, name)
    }

    /// An application's cache root inside a namespace.
    fn app_root(namespace: &Path, company: &str, app: &str) -> PathBuf {
        namespace.join(company).join(app)
    }

    /// A store entry of `size` bytes, last used `age` before [`NOW`].
    fn store_entry(store: &Path, name: &str, size: usize, age: Duration) -> PathBuf {
        let dir = store.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let jar = dir.join(CACHED_JAR_NAME);
        std::fs::write(&jar, vec![b'x'; size]).unwrap();
        touch_at(&jar, NOW - age);
        dir
    }

    #[test]
    fn two_applications_sharing_a_jar_hold_its_bytes_once() {
        // The whole point of the store. Two applications built from the
        // same library stack used to write the same JAR out twice; now
        // the second one links to what the first one already wrote.
        let dir = tempdir();
        let store = dir.join("jars");
        let (digest, _) = digest_and_name(7);
        let bytes = b"PK\x03\x04 the same fat jar in two places".to_vec();

        let first = cached_jar_path(&app_root(&dir, "Acme", "First"), &digest);
        let second = cached_jar_path(&app_root(&dir, "Acme", "Second"), &digest);

        ensure_cached_shared(Some(&store), &digest, &first, &bytes).unwrap();
        ensure_cached_shared(Some(&store), &digest, &second, &bytes).unwrap();

        assert_eq!(std::fs::read(&first).unwrap(), bytes);
        assert_eq!(std::fs::read(&second).unwrap(), bytes);
        assert!(
            same_file(&first, &second),
            "both applications must point at the same bytes"
        );
        assert_eq!(
            std::fs::read(store_entry_path(&store, &digest)).unwrap(),
            bytes,
            "the store holds the bytes"
        );
    }

    #[test]
    fn a_truncated_application_entry_is_repaired_from_the_store() {
        // The write that repairs a truncated entry replaces a directory
        // entry. If that touched the store's copy, the two would drift
        // apart and the bytes would be stored twice.
        let dir = tempdir();
        let store = dir.join("jars");
        let (digest, _) = digest_and_name(9);
        let bytes = b"PK\x03\x04 the whole fat jar, all of it".to_vec();

        let dest = cached_jar_path(&app_root(&dir, "Acme", "App"), &digest);
        ensure_cached_shared(Some(&store), &digest, &dest, &bytes).unwrap();

        // Exactly the state an interrupted write leaves behind.
        std::fs::write(&dest, &bytes[..10]).unwrap();

        ensure_cached_shared(Some(&store), &digest, &dest, &bytes).unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), bytes);
        assert!(
            same_file(&dest, &store_entry_path(&store, &digest)),
            "the repaired entry must share the store's bytes, not copy them"
        );
    }

    #[test]
    fn an_existing_private_copy_is_adopted_rather_than_duplicated() {
        // A cache written before the store existed holds a private copy
        // of the JAR. Adopting it as the shared one costs a directory
        // entry rather than the bytes, so the upgrade to the store does
        // not double the space the cache already uses.
        let dir = tempdir();
        let store = dir.join("jars");
        let (digest, _) = digest_and_name(11);
        let bytes = b"PK\x03\x04 an older cache's own copy".to_vec();

        let dest = cached_jar_path(&app_root(&dir, "Acme", "App"), &digest);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, &bytes).unwrap();

        ensure_cached_shared(Some(&store), &digest, &dest, &bytes).unwrap();

        assert!(
            same_file(&dest, &store_entry_path(&store, &digest)),
            "the existing copy must be adopted, not copied"
        );
    }

    #[test]
    fn without_a_store_the_application_keeps_its_own_copy() {
        // The fallback for a relocated cache root: no store, so no link,
        // and the application's own directory holds the only copy.
        let dir = tempdir();
        let (digest, _) = digest_and_name(17);
        let bytes = b"PK\x03\x04 on its own".to_vec();

        let dest = cached_jar_path(&app_root(&dir, "Acme", "App"), &digest);
        ensure_cached_shared(None, &digest, &dest, &bytes).unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), bytes);
        assert!(
            !dir.join("jars").exists(),
            "no store is created without one"
        );
    }

    #[test]
    fn the_store_keeps_an_entry_an_application_still_points_at() {
        // The rollback case. An application that has not run for a month
        // still links its build's JARs, and those links are the only
        // other copy of the bytes: deleting the store's copy would mean
        // copying them again from the payload to get them back.
        let dir = tempdir();
        let store = dir.join("jars");
        let (digest, _) = digest_and_name(13);

        let dest = cached_jar_path(&app_root(&dir, "Acme", "App"), &digest);
        ensure_cached_shared(Some(&store), &digest, &dest, b"payload").unwrap();

        // Age the shared file past the horizon, as a month without
        // running the application would.
        touch_at(&dest, NOW - Duration::from_secs(40 * 24 * 3600));

        let report = sweep_store(&store, NOW);

        assert!(
            store_entry_path(&store, &digest).exists(),
            "a referenced entry must survive the age limit"
        );
        assert!(report.removed.is_empty());
    }

    #[test]
    fn the_store_ages_out_an_entry_nothing_points_at() {
        let dir = tempdir();
        let store = dir.join("jars");
        let (_, name) = digest_and_name(15);
        let entry = store_entry(&store, &name, 32, Duration::from_secs(40 * 24 * 3600));

        let report = sweep_store(&store, NOW);

        assert!(!entry.exists(), "an unreferenced entry must be reclaimed");
        assert_eq!(report.bytes_freed, 32);
    }

    #[test]
    fn the_store_keeps_a_recently_used_entry() {
        let dir = tempdir();
        let store = dir.join("jars");
        let (_, name) = digest_and_name(19);
        let entry = store_entry(&store, &name, 32, Duration::from_secs(29 * 24 * 3600));

        let report = sweep_store(&store, NOW);

        assert!(entry.exists(), "a recent entry must survive");
        assert_eq!(report.kept, 1);
        assert!(report.removed.is_empty());
    }

    #[test]
    fn the_store_sweep_tolerates_a_missing_store() {
        let missing = tempdir().join("no-store");
        let report = sweep_store(&missing, NOW);
        assert_eq!(report, SweepReport::default());
    }

    #[test]
    fn should_sweep_store_rate_limits_repeat_calls() {
        // The store is shared by every application, so its rate limit is
        // a machine-wide one rather than one per application.
        let store = tempdir().join("jars");
        assert!(should_sweep_store(&store, NOW), "first call should run");
        assert!(
            !should_sweep_store(&store, NOW + HOUR),
            "a second call within the interval must be skipped"
        );
        assert!(
            should_sweep_store(&store, NOW + SWEEP_INTERVAL + HOUR),
            "a call past the interval should run"
        );
    }

    #[test]
    fn the_store_sits_in_the_snug_namespace() {
        let store = store_root(None).expect("the platform always has a base");
        assert_eq!(store.file_name().unwrap(), STORE_SUBDIR);
        assert_eq!(store.parent().unwrap().file_name().unwrap(), SNUG_SUBDIR);
    }

    #[test]
    fn an_overridden_cache_root_disables_the_store() {
        // A hard link only works within one volume, and an override may
        // well point at another one. The store is switched off rather
        // than silently costing a second copy on a different disk.
        assert!(store_root(Some(Path::new("/somewhere/else"))).is_none());
    }

    #[test]
    fn the_store_probes_for_hard_links() {
        // A filesystem that cannot link gets no store at all, rather
        // than a store that copies the JAR again on every launch
        // because the two copies would never be the same file.
        let dir = tempdir();
        assert!(
            probe_hard_links(&dir),
            "an ordinary filesystem has hard links"
        );
        assert!(
            probe_hard_links(&dir.join("not-created-yet")),
            "the probe creates the directory it needs"
        );
        assert!(
            staging_files(&dir).is_empty(),
            "the probe must not leave its files behind: {:?}",
            staging_files(&dir)
        );
    }
}
