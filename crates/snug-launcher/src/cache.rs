//! Per-user cache directory for extracted fat JARs.
//!
//! Default layout on Windows:
//!
//! ```text
//! %LOCALAPPDATA%\<company>\<name>\snug\<jar-sha256>\app.jar
//! ```
//!
//! The SHA-256 of the JAR is the cache key: changing the application
//! automatically yields a new cache location, so old versions can be
//! cleaned up without affecting a running install.

use std::path::{Path, PathBuf};

use snug_format::AppMetadata;

/// Default subdirectory under the cache root.
pub const SNUG_SUBDIR: &str = "snug";

/// Default JAR filename inside a cache entry.
pub const CACHED_JAR_NAME: &str = "app.jar";

/// Compute the per-user cache root for the given app metadata.
///
/// Resolution order:
/// 1. `config.behavior.cache_dir` if set (explicit override).
/// 2. `config.app.cache_dir` if set (per-app override).
/// 3. `%LOCALAPPDATA%\<company>\<name>\snug\` on Windows.
/// 4. `$HOME/.cache/<company>/<name>/snug/` on other platforms (fallback).
pub fn cache_root(app: &AppMetadata, override_root: Option<&Path>) -> PathBuf {
    if let Some(root) = override_root {
        return root.to_path_buf();
    }

    let company = sanitize_component(&app.company);
    let name = sanitize_component(&app.name);

    let base = platform_local_app_data().unwrap_or_else(|| fallback_cache_base());
    base.join(&company).join(&name).join(SNUG_SUBDIR)
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
