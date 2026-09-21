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

use std::path::{Path, PathBuf};

use snug_format::AppMetadata;

/// Default top-level subdirectory under the per-user data root.
pub const SNUG_SUBDIR: &str = "snug";

/// Default JAR filename inside a cache entry.
pub const CACHED_JAR_NAME: &str = "app.jar";

/// Compute the per-user cache root for the given app metadata.
///
/// Resolution order:
/// 1. Explicit override (passed via `behavior.cache_dir` or `app.cache_dir`).
/// 2. `%LOCALAPPDATA%\snug\<company>\<app>\` on Windows.
/// 3. `$HOME/.cache/snug/<company>/<app>/` on other platforms (fallback).
pub fn cache_root(app: &AppMetadata, override_root: Option<&Path>) -> PathBuf {
    if let Some(root) = override_root {
        return root.to_path_buf();
    }

    let company = sanitize_component(&app.company);
    let name = sanitize_component(&app.name);

    let base = platform_local_app_data().unwrap_or_else(|| fallback_cache_base());
    base.join(SNUG_SUBDIR).join(&company).join(&name)
}

/// Compute the full path to the cached JAR for a given SHA-256 digest.
pub fn cached_jar_path(root: &Path, sha256: &[u8; 32]) -> PathBuf {
    let hex = hex_lower(sha256);
    root.join(hex).join(CACHED_JAR_NAME)
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

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(windows)]
fn platform_local_app_data() -> Option<PathBuf> {
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

#[cfg(not(windows))]
fn platform_local_app_data() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
}

fn fallback_cache_base() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".cache");
    }
    std::env::temp_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(company: &str, name: &str) -> AppMetadata {
        AppMetadata {
            name: name.into(),
            company: company.into(),
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
}
