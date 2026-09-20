//! Windows implementation: JVM discovery, jvm.dll load, JNI invocation.
//!
//! **Slice 2 status**: JVM discovery, cache extraction, payload
//! self-scan, and Main-Class lookup are fully implemented and unit
//! testable. The actual `JNI_CreateJavaVM` launch path is currently
//! stubbed — it compiles, links, and produces a runnable stub binary,
//! but on Windows the launcher will report a "not yet implemented"
//! error and exit cleanly. The jni crate's full invocation API needs
//! Windows-machine validation before we wire it up here.

use std::fs;
use std::io::Read;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Environment::GetEnvironmentVariableW;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE,
    KEY_READ, REG_SZ,
};
use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

use snug_format::{JvmDiscovery, SnugEmbedded};

use crate::cache;
use crate::manifest;
use crate::LauncherError;

/// Orchestrate the launch: extract JAR to cache, locate JVM, load
/// jvm.dll, invoke Java main, return the exit code.
///
/// **Currently stubbed at the JNI launch step**: every step up to and
/// including "found a usable jvm.dll path" runs, but the actual JNI
/// invocation is gated behind a TODO that returns
/// [`LauncherError::JniCreateVm`] with code `-99`. This is intentional
/// for slice 2 — the JNI launch path needs Windows-machine validation
/// before we ship it. The crate compiles for `x86_64-pc-windows-gnu`
/// and the resulting `launcher-stub.exe` runs as a bare stub (or, with
/// a payload appended, gets all the way to JNI launch and then exits
/// with a clear error).
pub fn run(_self_path: &Path, embedded: &SnugEmbedded) -> Result<u32, LauncherError> {
    let config = &embedded.payload.config;

    // 1. Extract the JAR to the per-user cache.
    let cache_root = cache::cache_root(&config.app, config.behavior.cache_dir.as_deref());
    let jar_dest = cache::cached_jar_path(&cache_root, &embedded.payload.jar.sha256);
    ensure_cached(&jar_dest, &embedded.payload.jar.bytes)?;

    // 2. Resolve the Main-Class.
    let main_class_name = match &config.main_class {
        Some(cls) => cls.clone(),
        None => manifest::read_main_class_required(&jar_dest)?,
    };
    let _ = main_class_name;

    // 3. Locate a compatible JVM.
    let jvm_dir = discover_jvm(&config.behavior.jvm_discovery, config.min_java)?
        .ok_or(LauncherError::JvmNotFound { min_java: config.min_java })?;

    // 4. JNI launch — TODO for slice 3.
    //
    // The next slice will use the `jni` crate's `InitArgsBuilder` and
    // `JavaVM::new` / `JavaVM::with_libjvm` to spawn the JVM, attach
    // the main thread, find `Main-Class`, build a `String[]` from
    // `CommandLineToArgvW(GetCommandLineW())`, invoke `main`, and
    // return the JVM's exit code. That code needs validation on a
    // real Windows machine before it ships — the jni 0.22 API is
    // mostly safe but the wrapper around `AttachCurrentThread` plus
    // exception-describe handling want eyeballs on them.
    let _ = jvm_dir;
    Err(LauncherError::JniStub)
}

/// Copy `bytes` to `dest` (and create parents) unless it already exists.
fn ensure_cached(dest: &Path, bytes: &[u8]) -> Result<(), LauncherError> {
    if dest.exists() {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(dest, bytes)?;
    Ok(())
}

/// Discover a JVM satisfying the configured `min_java`.
///
/// Walks the [`JvmDiscovery`] strategy in order and returns the first
/// directory that exposes a sufficiently new `java`.
pub fn discover_jvm(
    strategy: &JvmDiscovery,
    min_java: u16,
) -> Result<Option<PathBuf>, LauncherError> {
    if let Some(p) = &strategy.explicit {
        if let Some(ok) = check_candidate(p, min_java)? {
            return Ok(Some(ok));
        }
    }
    if strategy.try_java_home {
        if let Some(p) = read_env_var("JAVA_HOME") {
            if let Some(ok) = check_candidate(&p, min_java)? {
                return Ok(Some(ok));
            }
        }
    }
    if strategy.try_jdk_home {
        if let Some(p) = read_env_var("JDK_HOME") {
            if let Some(ok) = check_candidate(&p, min_java)? {
                return Ok(Some(ok));
            }
        }
    }
    if strategy.try_path {
        if let Some(p) = read_env_var("PATH") {
            let p_str = p.to_string_lossy();
            for dir in p_str.split(';') {
                let dir = dir.trim();
                if dir.is_empty() {
                    continue;
                }
                let dir_path = PathBuf::from(dir);
                let candidate = dir_path.parent().unwrap_or(&dir_path);
                if let Some(ok) = check_candidate(candidate, min_java)? {
                    return Ok(Some(ok));
                }
            }
        }
    }
    if strategy.try_registry {
        for root in [
            "SOFTWARE\\JavaSoft\\JDK",
            "SOFTWARE\\JavaSoft\\JRE",
            "SOFTWARE\\Eclipse Adoptium\\JDK",
            "SOFTWARE\\Eclipse Foundation\\JDK",
            "SOFTWARE\\Microsoft\\JDK",
        ] {
            if let Ok(Some(path)) = read_registry_jdk_path(HKEY_LOCAL_MACHINE, root) {
                if let Some(ok) = check_candidate(&path, min_java)? {
                    return Ok(Some(ok));
                }
            }
        }
    }
    if strategy.try_common {
        for c in common_install_paths() {
            if let Some(ok) = check_candidate(&c, min_java)? {
                return Ok(Some(ok));
            }
        }
    }
    Ok(None)
}

fn check_candidate(dir: &Path, min_java: u16) -> Result<Option<PathBuf>, LauncherError> {
    if !dir.is_dir() {
        return Ok(None);
    }
    let has_java = dir.join("bin").join("java.exe").exists()
        || dir.join("bin").join("javaw.exe").exists();
    if !has_java {
        return Ok(None);
    }
    match read_java_major(dir)? {
        Some(major) if major >= min_java => Ok(Some(dir.to_path_buf())),
        Some(found) => Err(LauncherError::JvmTooOld {
            path: dir.to_path_buf(),
            found,
            min_java,
        }),
        None => Ok(None),
    }
}

fn read_java_major(java_home: &Path) -> Result<Option<u16>, LauncherError> {
    let release_path = java_home.join("release");
    if release_path.is_file() {
        if let Ok(mut file) = fs::File::open(&release_path) {
            let mut buf = String::new();
            file.read_to_string(&mut buf)?;
            for line in buf.lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("JAVA_VERSION=") {
                    let v = rest.trim().trim_matches('"');
                    if let Some(major) = parse_major_version(v) {
                        return Ok(Some(major));
                    }
                }
            }
        }
    }
    let java_exe = java_home.join("bin").join("java.exe");
    if !java_exe.is_file() {
        return Ok(None);
    }
    let output = match std::process::Command::new(&java_exe).arg("-version").output() {
        Ok(o) => o,
        Err(_) => return Ok(None),
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(parse_major_version(&stderr))
}

fn parse_major_version(s: &str) -> Option<u16> {
    let needle = s.trim().trim_matches('"');
    let mut parts = needle.split('.');
    let first = parts.next()?;
    if first == "1" {
        return parts.next()?.parse::<u16>().ok();
    }
    first.parse::<u16>().ok()
}

fn read_env_var(name: &str) -> Option<PathBuf> {
    let wide_name: Vec<u16> = std::ffi::OsStr::new(name)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let needed = unsafe { GetEnvironmentVariableW(wide_name.as_ptr(), std::ptr::null_mut(), 0) };
    if needed == 0 {
        return None;
    }
    let mut buf = vec![0u16; needed as usize];
    let written =
        unsafe { GetEnvironmentVariableW(wide_name.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if written == 0 {
        return None;
    }
    buf.truncate(written as usize);
    Some(PathBuf::from(std::ffi::OsString::from_wide(&buf)))
}

fn read_registry_jdk_path(root: HKEY, subkey: &str) -> Result<Option<PathBuf>, LauncherError> {
    let wide_subkey: Vec<u16> = std::ffi::OsStr::new(subkey)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let mut hkey: HKEY = std::ptr::null_mut();
    let result = unsafe { RegOpenKeyExW(root, wide_subkey.as_ptr(), 0, KEY_READ, &mut hkey) };
    if result != ERROR_SUCCESS {
        return Ok(None);
    }

    let mut max_version: Option<(u32, PathBuf)> = None;
    let mut index = 0;
    loop {
        let mut name_buf = vec![0u16; 256];
        let mut name_len = name_buf.len() as u32;
        let result = unsafe {
            RegEnumKeyExW(
                hkey,
                index,
                name_buf.as_mut_ptr(),
                &mut name_len,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if result != ERROR_SUCCESS {
            break;
        }
        name_buf.truncate(name_len as usize);
        let subkey_name = String::from_utf16_lossy(&name_buf);
        if let Some(version_num) = parse_version_subkey(&subkey_name) {
            let wide_sub: Vec<u16> = std::ffi::OsStr::new(&subkey_name)
                .encode_wide()
                .chain(Some(0))
                .collect();
            let mut sub_hkey: HKEY = std::ptr::null_mut();
            let open_result =
                unsafe { RegOpenKeyExW(hkey, wide_sub.as_ptr(), 0, KEY_READ, &mut sub_hkey) };
            if open_result == ERROR_SUCCESS {
                let value_name: Vec<u16> = std::ffi::OsStr::new("JavaHome")
                    .encode_wide()
                    .chain(Some(0))
                    .collect();
                let mut data = vec![0u16; 1024];
                let mut data_len = (data.len() * 2) as u32;
                let mut data_type: u32 = 0;
                let q = unsafe {
                    RegQueryValueExW(
                        sub_hkey,
                        value_name.as_ptr(),
                        std::ptr::null_mut(),
                        &mut data_type,
                        data.as_mut_ptr().cast(),
                        &mut data_len,
                    )
                };
                if q == ERROR_SUCCESS && data_type == REG_SZ {
                    let chars = data_len as usize / 2;
                    data.truncate(chars);
                    let path_str = String::from_utf16_lossy(&data)
                        .trim_end_matches('\0')
                        .to_string();
                    let path = PathBuf::from(path_str);
                    if path.is_dir() {
                        let replace = match &max_version {
                            Some((existing, _)) => version_num > *existing,
                            None => true,
                        };
                        if replace {
                            max_version = Some((version_num, path));
                        }
                    }
                }
                unsafe { RegCloseKey(sub_hkey) };
            }
        }
        index += 1;
    }

    unsafe { RegCloseKey(hkey) };
    Ok(max_version.map(|(_, p)| p))
}

fn parse_version_subkey(name: &str) -> Option<u32> {
    let mut parts = name.split('.');
    let first = parts.next()?.parse::<u32>().ok()?;
    let second = parts
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    Some(first * 1000 + second)
}

fn common_install_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(pf) = read_env_var("ProgramFiles") {
        out.push(pf.join("Java"));
        out.push(pf.join("Eclipse Adoptium"));
    }
    if let Some(pf86) = read_env_var("ProgramFiles(x86)") {
        out.push(pf86.join("Java"));
    }
    out
}

/// Walk `GetCommandLineW()` and return argv-style strings, skipping
/// argv[0]. Uses [`CommandLineToArgvW`] for correctness.
///
/// Exposed for slice 3 — once the JNI launch path is wired up, this
/// will populate the Java `String[] args` passed to `main`.
#[allow(dead_code)]
fn collect_argv() -> Vec<String> {
    unsafe extern "system" {
        fn GetCommandLineW() -> *const u16;
        fn LocalFree(handle: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    }

    unsafe {
        let cmd = GetCommandLineW();
        let mut argc: i32 = 0;
        let argv_w = CommandLineToArgvW(cmd, &mut argc);
        if argv_w.is_null() || argc <= 1 {
            return Vec::new();
        }
        let mut out = Vec::with_capacity((argc - 1) as usize);
        for &p in std::slice::from_raw_parts(argv_w.add(1), (argc - 1) as usize) {
            let mut len = 0;
            while *p.add(len) != 0 {
                len += 1;
            }
            let slice = std::slice::from_raw_parts(p, len as usize);
            out.push(String::from_utf16_lossy(slice));
        }
        LocalFree(argv_w as *mut _);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_major_handles_modern_and_legacy() {
        assert_eq!(parse_major_version("\"25\""), Some(25));
        assert_eq!(parse_major_version("\"25.0.1\""), Some(25));
        assert_eq!(parse_major_version("\"1.8.0_421\""), Some(8));
        assert_eq!(parse_major_version("not a version"), None);
    }

    #[test]
    fn parse_version_subkey_orders_correctly() {
        assert!(parse_version_subkey("25").unwrap() > parse_version_subkey("21").unwrap());
        assert!(parse_version_subkey("21.0.1").unwrap() > parse_version_subkey("21").unwrap());
        assert!(parse_version_subkey("not").is_none());
    }
}
