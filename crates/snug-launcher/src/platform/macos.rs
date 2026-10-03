//! macOS implementation: JVM discovery, `libjvm.dylib` load, JNI invocation.
//!
//! This mirrors `windows.rs` step for step, and diverges only where macOS
//! actually differs:
//!
//!   - `libjvm.dylib` under `lib/server` / `lib/client`, not `jvm.dll`
//!     under `bin/server` / `bin/client`.
//!   - The classpath separator is `:`. **This is the single most
//!     dangerous difference** — a `;`-joined classpath silently degrades
//!     to "one enormous bogus path", so every class fails to resolve and
//!     the symptom is a confusing `NoClassDefFoundError` from the JVM
//!     rather than anything from snug. It is built with
//!     [`std::env::join_paths`] rather than `join(";")` so the separator
//!     comes from the platform instead of being remembered.
//!   - `PATH` is split with [`std::env::split_paths`] for the same reason
//!     (Windows splits on `;`, and hardcoding either is a portability bug
//!     waiting to happen).
//!   - Discovery leans on `/usr/libexec/java_home`, which is macOS's own
//!     version-ordered catalogue and the only mechanism that understands
//!     the Apple JDK, Homebrew openjdk, and versioned bundle layout at
//!     once. There is no registry, so `JvmDiscovery::try_registry` has no
//!     macOS analogue and is ignored.
//!   - No splash window and no Adoptium download dialog: both are Win32.
//!     `DownloadJdkMode::Auto` / `Force` therefore degrade to
//!     "discovery only", which is logged rather than silently ignored.
//!
//! ## Not yet implemented
//!
//! Everything Windows has and macOS does not: the splash, the JDK
//! download flow, and the error dialog. A macOS app bundle launched from
//! Finder has no console to print to, so errors currently go to stderr
//! (visible via `Console.app` or a terminal launch) and to the per-launch
//! log in the cache.

#![cfg(target_os = "macos")]

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use jni::objects::{JObject, JObjectArray, JString, JValue};
use jni::signature::RuntimeMethodSignature;
use jni::strings::JNIString;
use jni::{InitArgsBuilder, JNIVersion, JavaVM};

use snug_format::{decode, DownloadJdkMode, FormatError, JvmDiscovery, SnugEmbedded};

use crate::cache;
use crate::log;
use crate::manifest;
use crate::LauncherError;

/// Suffix of the payload file snug writes into a `.app` bundle.
///
/// Deliberately *not* defined here. The producer is `snug-cli`, which
/// writes the file; this module is the consumer that reads it. Both take
/// the constant from `snug-payload`, where it lives with
/// `PAYLOAD_RESOURCE_NAME`, so the two spellings cannot drift — which is
/// the failure that surfaces only as a bare "payload not found".
pub use snug_payload::PAYLOAD_SUFFIX;

/// Locate this launcher's embedded payload.
///
/// On Windows the payload is stamped into the EXE's `RT_RCDATA`, so the
/// lookup is a PE resource-tree walk. That is not an option here: a Mach-O
/// has no equivalent resource directory, and `editpe` is PE-only. A macOS
/// `.app` is a *directory*, so there is nothing to stamp — snug writes the
/// encoded payload to a sibling file instead.
///
/// Two layouts are probed, in order:
///
/// 1. **Bundled** — `<App>.app/Contents/MacOS/<App>` alongside
///    `../Resources/<App>.snugpayload`. This is what snug emits.
/// 2. **Bare** — `<dir>/<App>` alongside `<dir>/<App>.snugpayload`. Not
///    something a shipped app uses, but it lets the launcher be driven
///    directly from a test or a terminal without building a whole bundle.
///
/// Returns `Ok(None)` when no payload file is present, which is how
/// `main.rs` distinguishes a bare stub from a real launch.
pub fn locate_payload(self_path: &Path) -> Result<Option<SnugEmbedded>, FormatError> {
    let Some(stem) = self_path.file_stem().and_then(|s| s.to_str()) else {
        return Ok(None);
    };
    let file_name = format!("{stem}.{PAYLOAD_SUFFIX}");

    // Bundled: <App>.app/Contents/MacOS/<App> -> Contents/Resources/<App>.snugpayload
    if let Some(contents_dir) = self_path.parent().and_then(|macos| macos.parent()) {
        if let Some(found) = read_payload(&contents_dir.join("Resources").join(&file_name))? {
            return Ok(Some(found));
        }
    }

    // Bare: <dir>/<App> -> <dir>/<App>.snugpayload
    if let Some(dir) = self_path.parent() {
        if let Some(found) = read_payload(&dir.join(&file_name))? {
            return Ok(Some(found));
        }
    }

    Ok(None)
}

/// Read and decode one payload file. A missing file is `Ok(None)`; any
/// other I/O failure is a real error, and a malformed payload is left to
/// `decode` so the caller reports a CRC/format problem rather than a
/// confusing "no payload".
fn read_payload(path: &Path) -> Result<Option<SnugEmbedded>, FormatError> {
    match fs::read(path) {
        Ok(bytes) => decode(&bytes).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(FormatError::Io(e)),
    }
}

/// Orchestrate the launch: extract JARs to the cache, locate a JVM, load
/// `libjvm.dylib`, invoke Java `main`, return its exit code.
///
/// On success the JVM is destroyed before returning. As on Windows, the
/// launcher is the JVM's sole host for this process — we never spawn
/// `java` as a child, so the app appears as its own process rather than
/// as a Java tool.
pub fn run(_self_path: &Path, embedded: &SnugEmbedded) -> Result<u32, LauncherError> {
    let config = &embedded.payload.config;
    let jars = &embedded.payload.jars;

    if jars.is_empty() {
        return Err(LauncherError::NoMainClass);
    }

    // 1. Extract every JAR to the per-user cache. Each JAR gets its own
    //    sha256-keyed subdirectory, and every one is *touched* — the mtime
    //    is the launcher's only record of when an entry was last needed,
    //    and the periodic sweep evicts anything stale, so touching only
    //    `jars[0]` would let a shared library JAR age out under constant
    //    use and be re-copied in full every launch.
    let cache_root = cache::cache_root(&config.app, config.behavior.cache_dir.as_deref());
    let mut cached_paths: Vec<PathBuf> = Vec::with_capacity(jars.len());
    for jar in jars {
        let dest = cache::cached_jar_path(&cache_root, &jar.sha256);
        ensure_cached(&dest, &jar.bytes)?;
        if let Err(e) = cache::touch(&dest) {
            // Non-fatal: the JAR is on disk and usable. It just won't be
            // protected from the sweep, so it may be re-extracted later.
            log::log(&format!(
                "cache: could not touch {}: {e}",
                dest.display()
            ));
        }
        cached_paths.push(dest);
    }
    let primary_jar_dest = cached_paths[0].clone();

    // 1b. Cache housekeeping on a detached thread. Deliberately after the
    //     touches (the sweep reads those mtimes) and deliberately before
    //     JVM discovery (a launch that never reaches the JVM should still
    //     clean up — GC shouldn't depend on the app starting).
    {
        let root = cache_root.clone();
        let current: std::collections::HashSet<String> =
            jars.iter().map(|j| cache::hex_lower(&j.sha256)).collect();
        let build_bytes: u64 = jars.iter().map(|j| j.bytes.len() as u64).sum();
        std::thread::spawn(move || {
            let now = std::time::SystemTime::now();
            if !cache::should_sweep(&root, now) {
                return;
            }
            let report = cache::sweep(&root, &current, build_bytes, now);
            log::log(&format!(
                "cache sweep: kept {}, removed {} ({} MB), {} error(s)",
                report.kept,
                report.removed.len(),
                report.bytes_freed / (1024 * 1024),
                report.errors.len()
            ));
            for e in &report.errors {
                log::log(&format!("cache sweep error: {e}"));
            }
        });
    }

    // 1a. Per-launch log next to the primary cached JAR. Failures are
    //     non-fatal: we still launch, just without file logging.
    let log_path = cache::cached_log_path(&cache_root, &jars[0].sha256);
    match log::init(&log_path) {
        Ok(resolved) => log::log(&format!(
            "snug-launcher (macos) starting — log file: {}",
            resolved.display()
        )),
        Err(e) => eprintln!(
            "snug-launcher: warning: could not open log file at {}: {e}",
            log_path.display()
        ),
    }
    log::log(&format!("app: {} / {}", config.app.company, config.app.name));
    log::log(&format!("app version: {}", config.app.version));
    log::log(&format!("cache_root: {}", cache_root.display()));
    log::log(&format!(
        "primary JAR sha256: {}",
        cache::hex_lower(&jars[0].sha256)
    ));
    log::log(&format!("cached jars: {}", cached_paths.len()));
    log::log(&format!("min-java: {}", config.min_java));

    // 2. Resolve the Main-Class. For multi-JAR builds the first JAR is
    //    the one whose manifest carries `Main-Class`.
    let main_class_name = match &config.main_class {
        Some(cls) => cls.clone(),
        None => manifest::read_main_class_required(&primary_jar_dest)?,
    };
    log::log(&format!("main class: {main_class_name}"));

    // 3. Locate a compatible JVM. `Auto` and `Force` both degrade to
    //    discovery-only on macOS — see the module docs — and we say so
    //    rather than silently ignoring the build's intent.
    match config.behavior.download_jdk {
        DownloadJdkMode::Off => {}
        mode => log::log(&format!(
            "download-jdk mode is {mode:?}, but the Adoptium download flow is \
             not implemented on macOS; falling back to discovery only"
        )),
    }

    let jvm_dir = discover_jvm(&config.behavior.jvm_discovery, config.min_java)?;
    log::log(&format!(
        "JVM discovery: {}",
        jvm_dir
            .as_ref()
            .map(|p| format!("found at {}", p.display()))
            .unwrap_or_else(|| "no compatible JVM found".to_string())
    ));

    let jvm_dir = jvm_dir.ok_or_else(|| {
        log::log(&format!(
            "no compatible Java {}+ JVM found; aborting",
            config.min_java
        ));
        LauncherError::JvmNotFound {
            min_java: config.min_java,
        }
    })?;
    log::log(&format!("using JAVA_HOME: {}", jvm_dir.display()));

    let libjvm = locate_libjvm_dylib(&jvm_dir).ok_or_else(|| {
        log::log(&format!(
            "libjvm.dylib not found under {}/lib/server or lib/client",
            jvm_dir.display()
        ));
        LauncherError::LibraryLoad {
            library: format!("libjvm.dylib under {}", jvm_dir.display()),
            message: "no libjvm.dylib found under lib/server or lib/client".into(),
        }
    })?;
    log::log(&format!("loading libjvm.dylib from: {}", libjvm.display()));

    if config.splash.is_some() {
        log::log("splash configured but not implemented on macOS; ignoring");
    }

    // 4. Forward command-line arguments to Java `main`, if configured.
    let argv_strings: Vec<String> = if config.behavior.forward_args {
        std::env::args().skip(1).collect()
    } else {
        Vec::new()
    };

    // 5. Build the JNI InitArgs. All cached JARs go on the classpath so
    //    `find_class` resolves through the system loader. `join_paths`
    //    supplies the platform's separator — do NOT hand-roll this with
    //    `join(";")`, that is the Windows spelling and it fails here in a
    //    way that surfaces as a Java-side ClassNotFound rather than a
    //    snug-side error.
    let classpath = std::env::join_paths(&cached_paths)
        .map_err(|e| LauncherError::JniInit(format!("classpath: {e}")))?;
    let discovered_major = read_java_major(&jvm_dir)?;
    let (jni_label, jni_version) = jni_version_for(discovered_major);
    log::log(&format!(
        "requesting JNI {jni_label} from the discovered JVM (Java {discovered_major:?})"
    ));
    let mut builder = InitArgsBuilder::new().version(jni_version);
    for arg in &config.jvm_args {
        builder = builder.option(arg.clone());
    }
    builder = builder.option(format!("-Djava.class.path={}", classpath.display()));
    let init_args = builder
        .build()
        .map_err(|e| LauncherError::JniInit(e.to_string()))?;

    // 6. Create the JVM by loading libjvm.dylib directly.
    let libjvm_path = libjvm.clone();
    let vm = JavaVM::with_libjvm(init_args, || {
        Ok::<_, jni::errors::StartJvmError>(libjvm_path.as_os_str())
    })
    .map_err(|e| LauncherError::JniCreate(e.to_string()))?;

    // 7. Attach, resolve Main-Class, build the String[] args, and invoke
    //    Java's entry point synchronously.
    //
    //    Standard Java SE apps expose `public static void main(String[])`.
    //    JavaFX apps only override `start(Stage)` and must be launched via
    //    `Application.launch(Class, String[])`. If `main` is missing and the
    //    class extends `javafx.application.Application`, fall through to
    //    that path.
    let main_class_jni = main_class_name.replace('.', "/");
    let main_class_name_for_err = main_class_name.clone();
    let argv_for_main = argv_strings;

    let exit_code: i32 = vm
        .attach_current_thread(|env| -> Result<i32, LauncherError> {
            let class = env
                .find_class(JNIString::new(&main_class_jni))
                .map_err(|e| {
                    LauncherError::MainClassNotFound(format!("{main_class_name_for_err} ({e})"))
                })?;

            // args[] as a String[]. The empty-arg case still allocates a
            // zero-length array so we have a real JObject to hand to JNI.
            let args_arr: JObjectArray<JString> = if argv_for_main.is_empty() {
                let placeholder = env
                    .new_string("")
                    .map_err(|e| LauncherError::JniInvoke(format!("new_string placeholder: {e}")))?;
                JObjectArray::<JString>::new(env, 0, &placeholder)
                    .map_err(|e| LauncherError::JniInvoke(format!("new String[0]: {e}")))?
            } else {
                let first = env.new_string(&argv_for_main[0]).map_err(|e| {
                    LauncherError::JniInvoke(format!("new_string[0]: {e}"))
                })?;
                let arr = JObjectArray::<JString>::new(env, argv_for_main.len(), &first)
                    .map_err(|e| LauncherError::JniInvoke(format!("new String[]: {e}")))?;
                for (i, s) in argv_for_main.iter().enumerate().skip(1) {
                    let jstr = env.new_string(s).map_err(|e| {
                        LauncherError::JniInvoke(format!("new_string[{i}]: {e}"))
                    })?;
                    let jstr_ref: &JString = jstr.as_ref();
                    arr.set_element(env, i, jstr_ref).map_err(|e| {
                        LauncherError::JniInvoke(format!("set_element[{i}]: {e}"))
                    })?;
                }
                arr
            };

            let args_obj: &JObjectArray<JString> = &args_arr;
            let args_value: JValue = args_obj.into();

            let main_sig = RuntimeMethodSignature::from_str("([Ljava/lang/String;)V")
                .map_err(|e| LauncherError::JniInvoke(format!("parse main signature: {e}")))?;

            let main_method_result = env.get_static_method_id(
                &class,
                JNIString::new("main"),
                main_sig.method_signature(),
            );

            let result = match main_method_result {
                Ok(_) => env.call_static_method(
                    &class,
                    JNIString::new("main"),
                    main_sig.method_signature(),
                    &[args_value],
                ),
                Err(jni::errors::Error::MethodNotFound { .. }) => {
                    // JavaFX fallback: invoke Application.launch(userClass,
                    // args) iff the user's class extends
                    // javafx.application.Application.
                    let app_class = env
                        .find_class(JNIString::new("javafx/application/Application"))
                        .map_err(|e| {
                            LauncherError::JniInvoke(format!(
                                "find javafx.application.Application: {e}"
                            ))
                        })?;
                    let is_fx = env.is_assignable_from(&app_class, &class).map_err(|e| {
                        LauncherError::JniInvoke(format!(
                            "isAssignableFrom(Application): {e}"
                        ))
                    })?;
                    if !is_fx {
                        return Err(LauncherError::NoMainMethod(
                            main_class_name_for_err.clone(),
                        ));
                    }
                    let launch_sig =
                        RuntimeMethodSignature::from_str("(Ljava/lang/Class;[Ljava/lang/String;)V")
                            .map_err(|e| {
                                LauncherError::JniInvoke(format!("parse launch signature: {e}"))
                            })?;
                    let class_obj: &JObject = class.as_ref();
                    let class_value: JValue = JValue::Object(class_obj);
                    env.call_static_method(
                        &app_class,
                        JNIString::new("launch"),
                        launch_sig.method_signature(),
                        &[class_value, args_value],
                    )
                }
                Err(e) => {
                    return Err(LauncherError::JniInvoke(format!("get main method: {e}")));
                }
            };

            match result {
                Ok(_) => Ok(0),
                Err(jni::errors::Error::MethodNotFound { .. }) => {
                    Err(LauncherError::NoMainMethod(main_class_name_for_err.clone()))
                }
                Err(e) => {
                    // Full stacktrace to stderr before clearing — preserves
                    // detail for log-file debugging.
                    if env.exception_check() {
                        let _ = env.exception_describe();
                    }

                    // User-facing detail via Throwable.getMessage(). Falls
                    // back to "No error message" when it returns
                    // null/empty, and to the raw JNI error when no Java
                    // exception is pending.
                    let detail = take_exception_message(env).unwrap_or_else(|| {
                        format!("{main_class_name_for_err}: {e}")
                    });

                    Err(LauncherError::JavaException(detail))
                }
            }
        })?;

    // 8. Best-effort teardown. DestroyJavaVM waits for non-daemon threads;
    //    on a clean main() return there shouldn't be any.
    let _ = unsafe { vm.destroy() };

    Ok(exit_code as u32)
}

/// Copy `bytes` to `dest` (creating parents) unless it already exists.
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

/// The JNI spec version to request from the JVM we just discovered.
///
/// Asking for a version *newer* than the JVM understands is how
/// `JNI_CreateJavaVM` fails with a useless "JNI call failed". This used
/// to pin `JNIVersion::V21` unconditionally, so a user who set
/// `--min-java 17` got a perfectly good Java 17 discovered and loaded —
/// and then was asked for a JNI 21 entry point that does not exist in it.
/// Same failure shape as a `;`-joined classpath: snug reports nothing
/// useful and the error surfaces from inside the JVM.
///
/// Everything used here (`FindClass`, `GetStaticMethodID`,
/// `CallStaticMethod`, `NewString`, the array and exception calls) is
/// JNI 1.0-era, so requesting exactly what the JVM provides costs
/// nothing and is correct for every version. `None` — meaning we could
/// not read the JVM's version — falls back to the lowest spec
/// everything understands, which can only ever succeed.
fn jni_version_for(major: Option<u16>) -> (&'static str, JNIVersion) {
    match major {
        Some(m) if m >= 21 => ("21", JNIVersion::V21),
        Some(m) if m >= 20 => ("20", JNIVersion::V20),
        Some(m) if m >= 19 => ("19", JNIVersion::V19),
        Some(m) if m >= 10 => ("10", JNIVersion::V10),
        Some(m) if m >= 9 => ("9", JNIVersion::V9),
        _ => ("1.8", JNIVersion::V1_8),
    }
}

/// Locate the `libjvm.dylib` for a `JAVA_HOME`.
///
/// macOS layout is `lib/server/libjvm.dylib`; the `client` tier only
/// exists on the old Oracle 8 JREs, but probing it costs nothing and
/// keeps the door open for them.
fn locate_libjvm_dylib(java_home: &Path) -> Option<PathBuf> {
    let server = java_home.join("lib").join("server").join("libjvm.dylib");
    if server.is_file() {
        return Some(server);
    }
    let client = java_home.join("lib").join("client").join("libjvm.dylib");
    if client.is_file() {
        return Some(client);
    }
    // Some slim JREs drop it straight under lib/.
    let flat = java_home.join("lib").join("libjvm.dylib");
    if flat.is_file() {
        return Some(flat);
    }
    None
}

/// Extract a pending Java exception's `Throwable.getMessage()`.
///
/// Returns `None` when no exception is pending. The exception is always
/// cleared so subsequent JNI calls aren't poisoned.
fn take_exception_message(env: &mut jni::Env<'_>) -> Option<String> {
    // Snapshot the pending exception; `exception_occurred` returns it
    // without clearing, so we can extract its message first.
    let exception = match env.exception_occurred() {
        Some(e) if !e.is_null() => e,
        _ => return None,
    };

    // Throwable.getMessage() — `()Ljava/lang/String;`. Returns null for
    // exceptions built without a detail message, which is very common.
    let detail_msg: Option<String> = (|| -> Option<String> {
        let sig = RuntimeMethodSignature::from_str("()Ljava/lang/String;").ok()?;
        env.call_method(
            &exception,
            JNIString::new("getMessage"),
            sig.method_signature(),
            &[],
        )
        .ok()
        .and_then(|v| v.l().ok())
        .and_then(|obj| {
            if obj.is_null() {
                None
            } else {
                // Downcast JObject -> JString. `cast_local` does an
                // IsInstanceOf check; the signature guarantees a String
                // (or null) when the object isn't null.
                let jstring: JString = (&*env).cast_local::<JString>(obj).ok()?;
                Some(jstring.to_string())
            }
        })
    })();

    // Always clear so the attach_current_thread return path isn't poisoned.
    (&*env).exception_clear();

    let detail = detail_msg.unwrap_or_default();
    let trimmed = detail.trim();
    Some(if trimmed.is_empty() {
        "No error message".to_string()
    } else {
        trimmed.to_string()
    })
}

/// Discover a `JAVA_HOME` satisfying `min_java`.
///
/// Walk order, mirroring `windows.rs` with the registry step replaced by
/// macOS's own catalogue:
///
///   1. `explicit` — a `--jvm-home` style override, highest priority.
///   2. `JAVA_HOME`, if `try_java_home`.
///   3. `JDK_HOME`, if `try_jdk_home`.
///   4. `PATH`, if `try_path`. Each entry is the `bin` directory holding
///      `java`, so the candidate is its parent.
///   5. If `try_common`: `/usr/libexec/java_home -v <min_java>`, then the
///      well-known `JavaVirtualMachines` directories.
///
/// `JvmDiscovery::try_registry` has no macOS analogue and is ignored.
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
        if let Some(raw) = std::env::var_os("PATH") {
            // `split_paths` rather than a hand-rolled split: the separator
            // is platform-defined (`:` here, `;` on Windows) and only the
            // stdlib gets it right.
            for dir in std::env::split_paths(&raw) {
                // `java` lives in `<home>/bin`, so the home is the parent.
                let candidate = dir.parent().unwrap_or(&dir).to_path_buf();
                if let Some(ok) = check_candidate(&candidate, min_java)? {
                    return Ok(Some(ok));
                }
            }
        }
    }
    if strategy.try_registry {
        log::log("jvm-discovery: try_registry has no macOS analogue; ignoring");
    }
    if strategy.try_common {
        // `/usr/libexec/java_home` is the one mechanism that knows about
        // every install flavour at once — the Apple JDK, Homebrew's
        // openjdk, Zulu/Corretto/Adoptium bundles — and returns them
        // already sorted by version. Ask it for our minimum and take the
        // first answer.
        if let Some(home) = java_home_tool(min_java) {
            if let Some(ok) = check_candidate(&home, min_java)? {
                return Ok(Some(ok));
            }
        }
        for c in common_install_paths() {
            if let Some(ok) = check_candidate(&c, min_java)? {
                return Ok(Some(ok));
            }
        }
    }
    Ok(None)
}

/// Ask `/usr/libexec/java_home` for the newest JDK that is at least
/// `min_java`. Returns `None` if the tool is missing or finds nothing.
///
/// A non-zero exit is normal here ("no matching JDK"), so it is not an
/// error — it just means fall through to the directory scan.
fn java_home_tool(min_java: u16) -> Option<PathBuf> {
    const TOOL: &str = "/usr/libexec/java_home";
    if !Path::new(TOOL).is_file() {
        return None;
    }
    let out = std::process::Command::new(TOOL)
        .arg("-v")
        .arg(min_java.to_string())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

/// The well-known macOS JDK install locations.
///
/// `/Library/Java/JavaVirtualMachines` is the system-wide one;
/// `~/Library/Java/JavaVirtualMachines` is per-user and is where Homebrew
/// and manual `pkg` installs tend to land. Each entry is a
/// `*.jdk` bundle whose real home is `Contents/Home`.
///
/// Homebrew's openjdk is covered twice over — `/usr/libexec/java_home`
/// already reports it, and its Cellar symlink sits in one of these
/// directories — but it is cheap to also probe the `opt` symlinks
/// directly, which is the path `brew install openjdk` documents.
fn common_install_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let roots = [
        PathBuf::from("/Library/Java/JavaVirtualMachines"),
        PathBuf::from("/opt/homebrew/opt/openjdk/libexec/openjdk.jdk/Contents/Home"),
        PathBuf::from("/usr/local/opt/openjdk/libexec/openjdk.jdk/Contents/Home"),
    ];

    for root in roots {
        if root.is_file() {
            out.push(root);
            continue;
        }
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        // Sorted so the scan is deterministic across runs and machines;
        // `check_candidate` rejects anything under `min_java` anyway, so
        // ordering only affects which of several *valid* JDKs wins.
        let mut found: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path().join("Contents").join("Home"))
            .filter(|p| p.is_dir())
            .collect();
        found.sort();
        out.extend(found);
    }

    if let Some(home) = std::env::var_os("HOME") {
        let user_root = PathBuf::from(home).join("Library/Java/JavaVirtualMachines");
        if let Ok(entries) = fs::read_dir(&user_root) {
            let mut found: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path().join("Contents").join("Home"))
                .filter(|p| p.is_dir())
                .collect();
            found.sort();
            out.extend(found);
        }
    }

    out
}

/// Does `dir` look like a `JAVA_HOME` new enough to use?
fn check_candidate(dir: &Path, min_java: u16) -> Result<Option<PathBuf>, LauncherError> {
    if !dir.is_dir() {
        return Ok(None);
    }
    // macOS has no `java.exe` / `javaw.exe` split — one `bin/java`.
    if !dir.join("bin").join("java").exists() {
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

/// Read `JAVA_VERSION` out of a `JAVA_HOME`'s `release` file.
///
/// Every modern macOS JDK ships one at the JAVA_HOME root, and the format
/// is the same on both platforms, so this is the portable way to ask a
/// JVM its version without executing it. Returns `Ok(None)` when there is
/// no `release` file or no parseable version — a JRE too old to ship one
/// is not usable for our purposes anyway.
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
    Ok(None)
}

/// Parse a `JAVA_VERSION` string into its major number.
///
/// Handles the three spellings that appear in the wild:
///   - `21` / `23.0.2` (modern)  -> `21` / `23`
///   - `1.8.0_412` (legacy 8)   -> `8`
///   - `11.0.12` (Zulu style)    -> `11`
fn parse_major_version(s: &str) -> Option<u16> {
    let first = s.split(['.', '_', '-']).next()?;
    let n: u16 = first.parse().ok()?;
    if n == 1 {
        // `1.8.0_412` means Java 8, not Java 1.
        let second = s
            .split(['.', '_', '-'])
            .nth(1)
            .and_then(|seg| seg.parse::<u16>().ok())?;
        return Some(second);
    }
    Some(n)
}

fn read_env_var(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modern_java_versions() {
        assert_eq!(parse_major_version("21"), Some(21));
        assert_eq!(parse_major_version("23.0.2"), Some(23));
        assert_eq!(parse_major_version("25-ea"), Some(25));
    }

    #[test]
    fn parses_legacy_one_dot_eight_as_java_eight() {
        // The 1.x lineage is the one place a naive "first component"
        // parse gets it badly wrong: 1.8.0_412 is Java 8.
        assert_eq!(parse_major_version("1.8.0_412"), Some(8));
        assert_eq!(parse_major_version("1.8.0_302"), Some(8));
    }

    #[test]
    fn rejects_unparseable_versions() {
        assert_eq!(parse_major_version(""), None);
        assert_eq!(parse_major_version("open"), None);
    }

    #[test]
    fn libjvm_layout_is_lib_server_not_bin_server() {
        // The Windows path is bin/server/jvm.dll. Getting this wrong on
        // macOS yields a LibraryLoad error on every launch, so pin the
        // shape explicitly.
        let root = tempdir();
        let home = root.join("Home");
        let dylib = home.join("lib").join("server").join("libjvm.dylib");
        fs::create_dir_all(dylib.parent().unwrap()).unwrap();
        fs::write(&dylib, b"fake").unwrap();
        assert_eq!(locate_libjvm_dylib(&home).as_deref(), Some(dylib.as_path()));
    }

    #[test]
    fn libjvm_falls_back_to_client() {
        let root = tempdir();
        let home = root.join("Home");
        let client = home.join("lib").join("client").join("libjvm.dylib");
        fs::create_dir_all(client.parent().unwrap()).unwrap();
        fs::write(&client, b"fake").unwrap();
        assert_eq!(locate_libjvm_dylib(&home).as_deref(), Some(client.as_path()));
    }

    #[test]
    fn libjvm_falls_back_to_flat_under_lib() {
        // A fresh home: the `client` tier must not be present, or it
        // wins and this would silently assert the wrong precedence.
        let root = tempdir();
        let home = root.join("Home");
        let flat = home.join("lib").join("libjvm.dylib");
        fs::create_dir_all(flat.parent().unwrap()).unwrap();
        fs::write(&flat, b"fake").unwrap();
        assert_eq!(locate_libjvm_dylib(&home).as_deref(), Some(flat.as_path()));
    }

    #[test]
    fn libjvm_prefers_server_tier_over_client_and_flat() {
        let root = tempdir();
        let home = root.join("Home");
        for tier in ["server", "client"] {
            let p = home.join("lib").join(tier).join("libjvm.dylib");
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, b"fake").unwrap();
        }
        let flat = home.join("lib").join("libjvm.dylib");
        fs::write(&flat, b"fake").unwrap();

        let server = home.join("lib").join("server").join("libjvm.dylib");
        assert_eq!(locate_libjvm_dylib(&home).as_deref(), Some(server.as_path()));
    }

    #[test]
    fn libjvm_absent_returns_none() {
        let root = tempdir();
        assert!(locate_libjvm_dylib(&root.join("nope")).is_none());
    }

    #[test]
    fn read_java_major_parses_a_release_file() {
        let root = tempdir();
        fs::write(
            root.join("release"),
            "JAVA_VERSION=\"21.0.2\"\nJAVA_VERSION_DATE=\"2023-09-19\"\nOS_NAME=\"Mac OS X\"\n",
        )
        .unwrap();
        assert_eq!(read_java_major(&root).unwrap(), Some(21));
    }

    #[test]
    fn read_java_major_absent_release_is_none() {
        let root = tempdir();
        assert_eq!(read_java_major(&root).unwrap(), None);
    }

    #[test]
    fn check_candidate_requires_bin_java() {
        // A JAVA_HOME with a `release` file but no `bin/java` is not a
        // JVM — it is, e.g., the parent of a `.jdk` bundle.
        let root = tempdir();
        fs::write(root.join("release"), "JAVA_VERSION=\"21\"\n").unwrap();
        assert_eq!(check_candidate(&root, 21).unwrap(), None);
    }

    #[test]
    fn check_candidate_accepts_new_enough_and_rejects_old() {
        let root = tempdir();
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(root.join("bin").join("java"), b"#!/bin/sh\n").unwrap();
        fs::write(root.join("release"), "JAVA_VERSION=\"21.0.2\"\n").unwrap();

        assert_eq!(check_candidate(&root, 21).unwrap(), Some(root.clone()));
        assert_eq!(check_candidate(&root, 17).unwrap(), Some(root.clone()));

        // Below the floor is an error, not a silent skip: the caller wants
        // to say "found Java 8, needed 17" rather than "nothing found".
        match check_candidate(&root, 25) {
            Err(LauncherError::JvmTooOld { found, min_java, .. }) => {
                assert_eq!(found, 21);
                assert_eq!(min_java, 25);
            }
            other => panic!("expected JvmTooOld, got {other:?}"),
        }
    }

    #[test]
    fn classpath_uses_colon_not_semicolon() {
        // The load-bearing macOS difference. A `;`-joined classpath is not
        // a snug-side error — it is a Java-side ClassNotFoundException,
        // which is far harder to trace back here.
        let paths = vec![PathBuf::from("/tmp/a.jar"), PathBuf::from("/tmp/b.jar")];
        let joined = std::env::join_paths(&paths).unwrap();
        let s = joined.to_string_lossy();
        assert!(s.contains(':'), "expected ':' separator, got {s:?}");
        assert!(!s.contains(';'), "unexpected ';' separator in {s:?}");
    }

    #[test]
    fn java_home_tool_reports_a_real_jdk_on_this_machine() {
        // Only meaningful where a JDK exists, which is the point: prove
        // the discovery mechanism actually works rather than only that
        // the paths are spelled right.
        let Some(home) = java_home_tool(8) else {
            eprintln!("skipping: no JDK visible to /usr/libexec/java_home");
            return;
        };
        assert!(home.is_dir(), "{} is not a directory", home.display());
        assert!(locate_libjvm_dylib(&home).is_some());
    }

    #[test]
    fn jni_version_never_exceeds_the_discovered_jvm() {
        // The bug: a hardcoded JNIVersion::V21 made a discovered Java 17
        // fail at JNI_CreateJavaVM with "JNI call failed".
        assert_eq!(jni_version_for(None).1, JNIVersion::V1_8);
        assert_eq!(jni_version_for(Some(8)).1, JNIVersion::V1_8);
        assert_eq!(jni_version_for(Some(9)).1, JNIVersion::V9);
        assert_eq!(jni_version_for(Some(11)).1, JNIVersion::V10);
        assert_eq!(jni_version_for(Some(17)).1, JNIVersion::V10);
        assert_eq!(jni_version_for(Some(19)).1, JNIVersion::V19);
        assert_eq!(jni_version_for(Some(21)).1, JNIVersion::V21);
        assert_eq!(jni_version_for(Some(25)).1, JNIVersion::V21);
        // The label is what goes in the log, so it must be the readable
        // spec name rather than the raw `ver` field.
        assert_eq!(jni_version_for(Some(25)).0, "21");
        assert_eq!(jni_version_for(Some(17)).0, "10");
        assert_eq!(jni_version_for(None).0, "1.8");
    }

    fn tempdir() -> std::path::PathBuf {
        let base = std::env::temp_dir();
        // Counter is load-bearing: pid + nanos is not unique under
        // parallel tests on a coarse clock (see the same fix in snug-cli's
        // options_file tests).
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let unique = format!(
            "snug-macos-platform-{}-{}-{}",
            std::process::id(),
            n,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = base.join(unique);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
