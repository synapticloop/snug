//! Windows implementation: JVM discovery, jvm.dll load, JNI invocation.

use std::fs;
use std::io::Read;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use jni::objects::{JObjectArray, JString, JValue};
use jni::signature::RuntimeMethodSignature;
use jni::strings::JNIString;
use jni::{InitArgsBuilder, JNIVersion, JavaVM};

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
/// jvm.dll, invoke Java `main`, return its exit code.
///
/// On success the JVM is destroyed before returning. The Windows
/// launcher is the JVM's sole host for this process; we never spawn
/// `javaw.exe`.
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

    // 3. Locate a compatible JVM and the `jvm.dll` we'll load.
    let jvm_dir = discover_jvm(&config.behavior.jvm_discovery, config.min_java)?
        .ok_or(LauncherError::JvmNotFound { min_java: config.min_java })?;
    let jvm_dll = locate_jvm_dll(&jvm_dir).ok_or_else(|| LauncherError::LibraryLoad {
        library: format!("jvm.dll under {}", jvm_dir.display()),
        message: "no jvm.dll found under bin/server or bin/client".into(),
    })?;

    // 4. Forward EXE command-line arguments to Java `main`, if configured.
    let argv_strings: Vec<String> = if config.behavior.forward_args {
        collect_argv()
    } else {
        Vec::new()
    };

    // 5. Build the JNI InitArgs. The cached JAR is on the classpath so
    //    `find_class` resolves Main-Class through the system loader.
    let mut builder = InitArgsBuilder::new().version(JNIVersion::V21);
    for arg in &config.jvm_args {
        builder = builder.option(arg.clone());
    }
    builder = builder.option(format!("-Djava.class.path={}", jar_dest.display()));
    let init_args = builder
        .build()
        .map_err(|e| LauncherError::JniInit(e.to_string()))?;

    // 6. Create the JVM by loading jvm.dll directly.
    let jvm_dll_path = jvm_dll.clone();
    let vm = JavaVM::with_libjvm(init_args, || Ok::<_, jni::errors::StartJvmError>(jvm_dll_path.as_os_str()))
        .map_err(|e| LauncherError::JniCreate(e.to_string()))?;

    // 7. Attach the current thread, resolve Main-Class, build the
    //    String[] args, and invoke `main(String[])` synchronously.
    let main_class_jni = main_class_name.replace('.', "/");
    let main_class_name_for_err = main_class_name.clone();
    let argv_for_main = argv_strings;

    let exit_code: i32 = vm
        .attach_current_thread(|env| -> Result<i32, LauncherError> {
            let class = env.find_class(JNIString::new(&main_class_jni)).map_err(|e| {
                LauncherError::MainClassNotFound(format!("{main_class_name_for_err} ({e})"))
            })?;

            let method_sig = RuntimeMethodSignature::from_str("([Ljava/lang/String;)V")
                .map_err(|e| LauncherError::JniInvoke(format!("parse main signature: {e}")))?;

            // Build args[] as a String[].
            let arr: JObjectArray<JString> = if argv_for_main.is_empty() {
                let placeholder = env
                    .new_string("")
                    .map_err(|e| LauncherError::JniInvoke(format!("new_string placeholder: {e}")))?;
                JObjectArray::<JString>::new(env, 0, &placeholder)
                    .map_err(|e| LauncherError::JniInvoke(format!("new String[0]: {e}")))?
            } else {
                let first = env
                    .new_string(&argv_for_main[0])
                    .map_err(|e| LauncherError::JniInvoke(format!("new_string[0]: {e}")))?;
                let arr = JObjectArray::<JString>::new(env, argv_for_main.len(), &first)
                    .map_err(|e| LauncherError::JniInvoke(format!("new String[]: {e}")))?;
                for (i, s) in argv_for_main.iter().enumerate().skip(1) {
                    let jstr = env
                        .new_string(s)
                        .map_err(|e| LauncherError::JniInvoke(format!("new_string[{i}]: {e}")))?;
                    let jstr_ref: &JString = jstr.as_ref();
                    arr.set_element(env, i, jstr_ref).map_err(|e| {
                        LauncherError::JniInvoke(format!("set_element[{i}]: {e}"))
                    })?;
                }
                arr
            };

            // main returns void; treat any successful return as exit code 0.
            let args_value: JValue = (&arr).into();
            let result = env.call_static_method(
                &class,
                JNIString::new("main"),
                method_sig.method_signature(),
                &[args_value],
            );
            match result {
                Ok(_) => Ok(0),
                Err(jni::errors::Error::MethodNotFound { .. }) => {
                    Err(LauncherError::NoMainMethod(main_class_name_for_err.clone()))
                }
                Err(e) => {
                    if env.exception_check() {
                        env.exception_describe();
                        env.exception_clear();
                    }
                    Err(LauncherError::JavaException(format!(
                        "{main_class_name_for_err}: {e}"
                    )))
                }
            }
        })?;

    // 8. Best-effort JVM teardown. The JNI spec says DestroyJavaVM
    //    waits for non-daemon threads; on a clean main() return there
    //    shouldn't be any.
    let _ = unsafe { vm.destroy() };

    Ok(exit_code as u32)
}

/// Resolve the `jvm.dll` path inside a Java home. We prefer the
/// server VM when both server and client directories exist; the JVM
/// itself picks a tier automatically, but the server tier is the
/// modern default on x86_64.
fn locate_jvm_dll(java_home: &Path) -> Option<PathBuf> {
    let server = java_home.join("bin").join("server").join("jvm.dll");
    if server.is_file() {
        return Some(server);
    }
    let client = java_home.join("bin").join("client").join("jvm.dll");
    if client.is_file() {
        return Some(client);
    }
    // Some slim JREs drop jvm.dll straight under bin/.
    let flat = java_home.join("bin").join("jvm.dll");
    if flat.is_file() {
        return Some(flat);
    }
    None
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
    // JavaSoft stores versions like `21.0.5+9` or `21.0.1+12-LTS`. We
    // want a total order so the registry scan picks the highest
    // available patch level — pack major/minor/patch into a single
    // integer as `major*1_000_000 + minor*1_000 + patch`.
    let mut parts = name.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts
        .next()
        .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    let patch = parts
        .next()
        .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    Some(major * 1_000_000 + minor * 1_000 + patch)
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
        assert!(parse_version_subkey("21.0.5").unwrap() > parse_version_subkey("21.0.1").unwrap());
        assert_eq!(parse_version_subkey("21").unwrap(), 21_000_000);
        assert_eq!(parse_version_subkey("21.0").unwrap(), 21_000_000);
        assert_eq!(parse_version_subkey("21.0.5").unwrap(), 21_000_500);
        // Build suffix is ignored — major.minor.patch is enough to
        // disambiguate patch levels for registry ordering.
        assert_eq!(
            parse_version_subkey("21.0.5+9").unwrap(),
            parse_version_subkey("21.0.5").unwrap()
        );
        assert!(parse_version_subkey("not").is_none());
    }
}
