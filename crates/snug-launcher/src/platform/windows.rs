//! Windows implementation: JVM discovery, jvm.dll load, JNI invocation.

use std::fs;
use std::io::Read;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};

/// `Win32` process-creation flag that prevents Windows from allocating a
/// new console for the child. Without this, spawning a console-subsystem
/// binary (like `java.exe`) from our GUI-subsystem launcher would flash a
/// command prompt window briefly before the child exits.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

use jni::objects::{JObject, JObjectArray, JString, JValue};
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

use snug_format::{DownloadJdkMode, FormatError, JvmDiscovery, SnugEmbedded};

use crate::cache;
use crate::jdk_install;
use crate::log;
use crate::manifest;
use crate::splash;
use crate::LauncherError;

/// Locate this launcher's embedded payload.
///
/// Windows stamps the payload into the EXE's `RT_RCDATA`, so the lookup is
/// a PE resource-tree walk over the running image. The macOS module
/// implements the same signature with a sibling-file lookup instead;
/// `main.rs` is shared, so it calls this through
/// [`crate::platform::locate_payload`] and never branches on the OS.
pub fn locate_payload(self_path: &Path) -> Result<Option<SnugEmbedded>, FormatError> {
    crate::payload_locator::find_in_file(self_path)
}

/// Orchestrate the launch: extract JAR to cache, locate JVM, load
/// jvm.dll, invoke Java `main`, return its exit code.
///
/// On success the JVM is destroyed before returning. The Windows
/// launcher is the JVM's sole host for this process; we never spawn
/// `javaw.exe`.
pub fn run(_self_path: &Path, embedded: &SnugEmbedded) -> Result<u32, LauncherError> {
    let config = &embedded.payload.config;
    let jars = &embedded.payload.jars;

    if jars.is_empty() {
        return Err(LauncherError::NoMainClass);
    }

    // 1. Extract every JAR to the per-user cache. Each JAR gets its
    //    own sha256-keyed subdirectory so multi-JAR builds can have
    //    collisions-free filenames and the launcher can reuse cached
    //    entries when only some of the JARs change.
    //
    //    Every JAR is then *touched*, not just the primary. The mtime is
    //    the launcher's only record of when an entry was last needed, and
    //    the periodic sweep evicts anything stale — so touching only
    //    `jars[0]` would let a shared library JAR age out despite
    //    constant use, and every launch would re-copy it in full.
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

    // 1b. Kick off cache housekeeping on a detached thread.
    //
    //     Placed here, immediately after every JAR has been touched,
    //     and deliberately *before* JVM discovery. Two constraints, both
    //     load-bearing:
    //
    //     - After the touches, because the sweep reads those mtimes.
    //       Running it earlier could evict an entry this very launch is
    //       about to reuse, which would then be re-extracted at once —
    //       defeating the point.
    //     - Before JVM discovery, so a launch that never reaches the
    //       JVM (no JDK, user cancels the download prompt) still cleans
    //       up. Garbage collection shouldn't depend on the app
    //       starting.
    //
    //     Nothing else writes to the cache root during launch, so it is
    //     safe to run concurrently with everything that follows.
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

    // 1a. Open the per-launch log next to the primary cached JAR.
    //     `init` truncates any prior session's file. Failures are
    //     non-fatal: we still launch, just without file logging.
    let log_path = cache::cached_log_path(&cache_root, &jars[0].sha256);
    match log::init(&log_path) {
        Ok(resolved) => {
            log::log(&format!(
                "snug-launcher starting — log file: {}",
                resolved.display()
            ));
        }
        Err(e) => {
            eprintln!(
                "snug-launcher: warning: could not open log file at {}: {e}",
                log_path.display()
            );
        }
    }
    log::log(&format!("app: {} / {}", config.app.company, config.app.name));
    log::log(&format!(
        "app version: {}",
        config.app.version
    ));
    log::log(&format!("cache_root: {}", cache_root.display()));
    log::log(&format!("primary JAR sha256: {}", cache::hex_lower(&jars[0].sha256)));
    log::log(&format!("cached jars: {}", cached_paths.len()));
    log::log(&format!("min-java: {}", config.min_java));
    log::log(&format!(
        "download-jdk mode: {:?}",
        config.behavior.download_jdk
    ));
    log::log(&format!(
        "forward args: {}",
        config.behavior.forward_args
    ));

    // 2. Resolve the Main-Class. For multi-JAR builds, the first JAR
    //    is the one whose manifest carries `Main-Class`.
    let main_class_name = match &config.main_class {
        Some(cls) => cls.clone(),
        None => manifest::read_main_class_required(&primary_jar_dest)?,
    };
    log::log(&format!("main class: {}", main_class_name));

    // 3. Locate a compatible JVM and the `jvm.dll` we'll load. The
    //    exact behaviour depends on `config.behavior.download_jdk`:
    //
    //    - `Off`  — discovery only. Bail if no JVM found.
    //    - `Auto` — discovery first; on miss, ask the user via a
    //      `TaskDialog` whether to download Temurin.
    //    - `Force` — skip discovery entirely. The user always gets
    //      the dialog; a previously cached Temurin (under
    //      `%LOCALAPPDATA%\snug\jdk\`) is silently reused.
    let mut jvm_dir = match config.behavior.download_jdk {
        DownloadJdkMode::Off | DownloadJdkMode::Auto => {
            let found = discover_jvm(&config.behavior.jvm_discovery, config.min_java)?;
            log::log(&format!(
                "JVM discovery: {}",
                found
                    .as_ref()
                    .map(|p| format!("found at {}", p.display()))
                    .unwrap_or_else(|| "no compatible JVM found".to_string())
            ));
            found
        }
        // Force — let `maybe_install` decide (its internal cache
        // check covers "already downloaded on a previous run").
        DownloadJdkMode::Force => {
            log::log("JVM discovery: skipped (download-jdk=force)");
            None
        }
    };
    let should_offer_install = match config.behavior.download_jdk {
        DownloadJdkMode::Off => false,
        DownloadJdkMode::Auto => jvm_dir.is_none(),
        DownloadJdkMode::Force => true,
    };
    if should_offer_install {
        let install_root = jdk_install_root();
        log::log(&format!("JDK install flow starting; root: {}", install_root.display()));
        let result = jdk_install::maybe_install(
            // No splash window yet (it's created at step 6) — the
            // dialog stands alone; `HWND_DESKTOP` keeps it visually
            // modal to the launcher process.
            std::ptr::null_mut(),
            config.min_java,
            &install_root,
        );
        match result {
            Ok(Some(new_home)) => {
                log::log(&format!("JDK install flow succeeded: {}", new_home.display()));
                // `std::env::set_var` is unsafe in Rust 2024 because
                // it's racy with `getenv` reads in other threads.
                // Single-threaded during launch means it's safe in
                // practice; we wrap in `unsafe` rather than serialise.
                unsafe {
                    std::env::set_var("JAVA_HOME", &new_home);
                }
                // `Force` skipped discovery; the install path also
                // picks the jvm.dll itself when `Force` was the
                // entrypoint. So this retry is only meaningful in
                // `Auto`, where `jvm_dir` was already `None`.
                let mut retry = config.behavior.jvm_discovery.clone();
                retry.explicit = Some(new_home);
                jvm_dir = discover_jvm(&retry, config.min_java)?;
            }
            Ok(None) => {
                log::log("JDK install flow: user cancelled or opened browser");
                // User cancelled or opened the download page in their
                // browser. For `Force` we skipped discovery upfront, so
                // re-run it now as a courtesy — the user may have a
                // compatible JDK elsewhere on the machine (GraalVM,
                // Zulu, Corretto, Oracle, custom install path, etc.)
                // that just isn't reachable via the registry keys or
                // common install dirs we check first. For `Auto`
                // discovery already returned `None`, so this is a
                // cheap no-op.
                if jvm_dir.is_none() {
                    log::log("post-cancel fallback: re-running discovery");
                    jvm_dir = discover_jvm(
                        &config.behavior.jvm_discovery,
                        config.min_java,
                    )?;
                }
            }
            Err(e) => {
                log::log(&format!("JDK install flow failed: {e}"));
                eprintln!("snug-launcher: JDK install flow failed: {e}");
                // Install failed for some non-user reason (network down,
                // Adoptium 5xx, SHA-256 mismatch, etc.). Same courtesy
                // fallback as the cancel arm — don't strand the user
                // on a usable local JDK just because the download
                // flow hiccupped.
                if jvm_dir.is_none() {
                    log::log("post-error fallback: re-running discovery");
                    jvm_dir = discover_jvm(
                        &config.behavior.jvm_discovery,
                        config.min_java,
                    )?;
                }
            }
        }
    }
    let jvm_dir = jvm_dir.ok_or_else(|| {
        log::log(&format!(
            "no compatible Java {}+ JVM found; aborting",
            config.min_java
        ));
        LauncherError::JvmNotFound { min_java: config.min_java }
    })?;
    log::log(&format!("using JAVA_HOME: {}", jvm_dir.display()));
    let jvm_dll = locate_jvm_dll(&jvm_dir).ok_or_else(|| {
        log::log(&format!(
            "jvm.dll not found under {}/bin/server or bin/client",
            jvm_dir.display()
        ));
        LauncherError::LibraryLoad {
            library: format!("jvm.dll under {}", jvm_dir.display()),
            message: "no jvm.dll found under bin/server or bin/client".into(),
        }
    })?;
    log::log(&format!("loading jvm.dll from: {}", jvm_dll.display()));

    // 4. Forward EXE command-line arguments to Java `main`, if configured.
    let argv_strings: Vec<String> = if config.behavior.forward_args {
        collect_argv()
    } else {
        Vec::new()
    };

    // 5. Build the JNI InitArgs. All cached JARs go on the classpath
    //    joined by `;` (the Java/Windows separator) so `find_class`
    //    resolves classes from any of them through the system loader.
    let classpath = cached_paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(";");
    let discovered_major = read_java_major(&jvm_dir)?;
    let (jni_label, jni_version) = jni_version_for(discovered_major);
    log::log(&format!(
        "requesting JNI {jni_label} from the discovered JVM (Java {discovered_major:?})"
    ));
    let mut builder = InitArgsBuilder::new().version(jni_version);
    for arg in &config.jvm_args {
        builder = builder.option(arg.clone());
    }
    builder = builder.option(format!("-Djava.class.path={classpath}"));
    let init_args = builder
        .build()
        .map_err(|e| LauncherError::JniInit(e.to_string()))?;

    // 6. Show the native splash window before we ask the JVM to start
    //    loading classes. The launcher dismisses it once
    //    `attach_current_thread` returns (the JVM is up and JavaFX is
    //    about to take over the screen). On any error path the
    //    SplashHandle's `Drop` impl cleans up the window + thread.
    let splash_handle = match &config.splash {
        Some(splash_cfg) => match splash::show(
            splash_cfg.image.width,
            splash_cfg.image.height,
            splash_cfg.image.bytes.clone(),
            splash_cfg.duration_ms,
        ) {
            Ok(h) => Some(h),
            Err(e) => {
                // Splash is best-effort; never block the launch on a
                // broken PNG or a GDI hiccup.
                eprintln!("snug-launcher: warning: failed to show splash: {e}");
                None
            }
        },
        None => None,
    };

    // 7. Create the JVM by loading jvm.dll directly.
    let jvm_dll_path = jvm_dll.clone();
    let vm = JavaVM::with_libjvm(init_args, || Ok::<_, jni::errors::StartJvmError>(jvm_dll_path.as_os_str()))
        .map_err(|e| LauncherError::JniCreate(e.to_string()))?;

    // 7. Attach the current thread, resolve Main-Class, build the
    //    String[] args, and invoke Java's entry point synchronously.
    //
    //    Standard Java SE apps expose a `public static void main(String[])`.
    //    JavaFX apps only override `start(Stage)` and must be launched via
    //    `Application.launch(Class, String[])`. If `main` is missing and
    //    the class extends `javafx.application.Application`, fall through
    //    to that path.
    let main_class_jni = main_class_name.replace('.', "/");
    let main_class_name_for_err = main_class_name.clone();
    let argv_for_main = argv_strings;
    // Move the splash handle into the closure so we can dismiss it as
    // soon as the JVM thread is attached — before `find_class` or
    // `Application.launch` blocks. The splash stays visible for at
    // least `duration_ms` from `show()`, then dismisses; that covers
    // the JNI_CreateJavaVM latency the user was hiding behind it.
    let mut splash_handle = splash_handle;

    let exit_code: i32 = vm
        .attach_current_thread(|env| -> Result<i32, LauncherError> {
            if let Some(h) = splash_handle.take() {
                h.dismiss();
            }

            let class = env.find_class(JNIString::new(&main_class_jni)).map_err(|e| {
                LauncherError::MainClassNotFound(format!("{main_class_name_for_err} ({e})"))
            })?;

            // Build args[] as a String[]. Empty-arg case still allocates a
            // zero-length array so we have a real JObject to hand to JNI.
            let args_arr: JObjectArray<JString> = if argv_for_main.is_empty() {
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

            // `JValue` is `Copy` and borrows the JObject, so we can build
            // it once and reuse across branches.
            let args_obj: &JObjectArray<JString> = &args_arr;
            let args_value: JValue = args_obj.into();

            let main_sig = RuntimeMethodSignature::from_str("([Ljava/lang/String;)V")
                .map_err(|e| LauncherError::JniInvoke(format!("parse main signature: {e}")))?;

            // Try standard `main(String[])` first.
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
                    // JavaFX fallback: invoke
                    //   Application.launch(userClass, args)
                    // iff the user's class extends javafx.application.Application.
                    let app_class = env
                        .find_class(JNIString::new("javafx/application/Application"))
                        .map_err(|e| {
                            LauncherError::JniInvoke(format!(
                                "find javafx.application.Application: {e}"
                            ))
                        })?;
                    let is_fx = env
                        .is_assignable_from(&app_class, &class)
                        .map_err(|e| {
                            LauncherError::JniInvoke(format!(
                                "isAssignableFrom(Application): {e}"
                            ))
                        })?;
                    if !is_fx {
                        return Err(LauncherError::NoMainMethod(
                            main_class_name_for_err.clone(),
                        ));
                    }
                    let launch_sig = RuntimeMethodSignature::from_str(
                        "(Ljava/lang/Class;[Ljava/lang/String;)V",
                    )
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
                    // Emit the full stacktrace to stderr before
                    // clearing — preserves rich detail for log-file
                    // debugging in console runs. The GUI subsystem
                    // build swallows stderr, so this is best-effort.
                    if env.exception_check() {
                        let _ = env.exception_describe();
                    }

                    // Extract just the user-facing detail message
                    // via `Throwable.getMessage()`. Falls back to
                    // `"No error message"` when getMessage() returns
                    // null/empty. Falls back to the raw JNI error
                    // string when no Java exception is pending (rare;
                    // covers genuine JNI failures like OOM inside a
                    // call).
                    let detail = take_exception_message(env).unwrap_or_else(|| {
                        format!("{main_class_name_for_err}: {e}")
                    });

                    Err(LauncherError::JavaException(detail))
                }
            }
        })?;

    // 9. Best-effort JVM teardown. The JNI spec says DestroyJavaVM
    //    waits for non-daemon threads; on a clean main() return there
    //    shouldn't be any. (The splash handle was moved into the
    //    attach closure and dismissed from there; if `attach_current_thread`
    //    was never called because of an earlier failure, its `Drop`
    //    impl still tears down the splash window + thread.)
    let _ = unsafe { vm.destroy() };

    Ok(exit_code as u32)
}

/// Root directory for cached JDK installations on Windows.
///
/// Used by the auto-download path: `%LOCALAPPDATA%\snug\jdk\` is the
/// parent; individual JDKs are extracted into `<root>\<version>\`
/// subdirectories.
fn jdk_install_root() -> PathBuf {
    match std::env::var_os("LOCALAPPDATA") {
        Some(base) => PathBuf::from(base).join("snug").join("jdk"),
        None => std::env::temp_dir().join("snug").join("jdk"),
    }
}

/// Extract the pending Java exception's `getMessage()` — i.e. the
/// detail message passed to the exception's constructor — for use as
/// the user-facing error text.
///
/// Returns:
///
/// - `Some(message)` when a Java exception is pending. `message` is
///   the result of `Throwable.getMessage()`, or the literal
///   `"No error message"` if the exception has no detail message
///   (`getMessage()` returned `null` or an empty string).
/// - `None` when no Java exception is pending. The caller should
///   fall back to formatting the raw `jni::errors::Error` in that
///   case — those are genuine JNI failures (e.g. OOM inside a JNI
///   call) with no Java-side cause.
///
/// The pending exception is cleared as a side effect. Call
/// `env.exception_describe()` *before* this helper if you want the
/// full stacktrace printed to stderr for log-file debugging.
///
/// In jni 0.22 the `attach_current_thread` closure receives
/// `&mut jni::Env`. We take `&mut` here too because
/// `exception_occurred` / `call_method` need exclusive access. The
/// non-mutating helpers (`cast_local`, `exception_clear`) reborrow
/// `&*env` internally.
fn take_exception_message(env: &mut jni::Env<'_>) -> Option<String> {
    use jni::objects::JString;

    // Snapshot the pending exception. `exception_occurred` returns
    // it without clearing, so we can extract its message first.
    let exception = match env.exception_occurred() {
        Some(e) if !e.is_null() => e,
        _ => return None,
    };

    // Throwable.getMessage() — `()Ljava/lang/String;`. Returns
    // `null` for exceptions constructed without a detail message
    // (very common — e.g. `NullPointerException` thrown from a
    // native method, or user code that did `throw new MyException()`).
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
                // Downcast `JObject` → `JString`. `cast_local` does
                // an `IsInstanceOf` check at runtime; we know from
                // the method signature that getMessage returns a
                // String (or null), so the cast always succeeds when
                // the object isn't null. `to_string()` here is the
                // `Display::to_string` blanket impl (returns owned
                // `String`).
                let jstring: JString = (&*env).cast_local::<JString>(obj).ok()?;
                Some(jstring.to_string())
            }
        })
    })();

    // Always clear the pending exception so subsequent JNI calls
    // (including the `attach_current_thread` return path) aren't
    // poisoned.
    (&*env).exception_clear();

    let detail = detail_msg.unwrap_or_default();
    let trimmed = detail.trim();
    Some(if trimmed.is_empty() {
        "No error message".to_string()
    } else {
        trimmed.to_string()
    })
}
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

/// Copy `bytes` to `dest` (and create parents) unless a correct copy is
/// already there.
///
/// Delegates to [`crate::cache::ensure_cached_atomic`], which is where the
/// atomicity and truncation-repair rules live. `platform/macos.rs` still
/// carries its own copy of the old truncate-in-place version; it is
/// byte-identical to what this replaced and should adopt the shared helper
/// in one line whenever macOS is back in scope.
fn ensure_cached(dest: &Path, bytes: &[u8]) -> Result<(), LauncherError> {
    crate::cache::ensure_cached_atomic(dest, bytes)?;
    Ok(())
}

/// Try one candidate from a *scan* of many.
///
/// [`check_candidate`] reports a JDK that is too old as an error, and for a
/// single explicit choice that is right — if you set `JAVA_HOME` to a
/// Java 11 and asked for 17, "you have Java 11, you need 17" is the
/// answer you want.
///
/// Inside a scan it is wrong, and it was a real bug: the registry roots are
/// walked in a fixed order with `SOFTWARE\JavaSoft\JDK` first, so a
/// machine with both a JDK 11 and a JDK 25 hit the 11 and gave up, never
/// reaching the 25. Any ordinary developer machine with an old JDK left
/// installed could not discover the new one. So during a scan a too-old
/// JDK is recorded and stepped over, and only reported if nothing better
/// turns up.
fn scan_candidate(
    too_old: &mut Option<(PathBuf, u16)>,
    dir: &Path,
    min_java: u16,
) -> Result<Option<PathBuf>, LauncherError> {
    match check_candidate(dir, min_java) {
        Ok(found) => Ok(found),
        Err(LauncherError::JvmTooOld { path, found, .. }) => {
            if too_old.is_none() {
                *too_old = Some((path, found));
            }
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// Discover a JVM satisfying the configured `min_java`.
///
/// Walks the [`JvmDiscovery`] strategy in order and returns the first
/// directory that exposes a sufficiently new `java`.
///
/// The first three steps are *explicit choices* — `--jvm-home`, `JAVA_HOME`,
/// `JDK_HOME` — so `check_candidate` is used there and a too-old JDK is a
/// hard error, which is the more actionable message. From `PATH` onwards we
/// are surveying whatever the machine happens to have, so `scan_candidate`
/// steps over a too-old one instead.
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
    // Remembered across the scan; reported only if nothing qualifies.
    let mut too_old: Option<(PathBuf, u16)> = None;

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
                if let Some(ok) = scan_candidate(&mut too_old, candidate, min_java)? {
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
                if let Some(ok) = scan_candidate(&mut too_old, &path, min_java)? {
                    return Ok(Some(ok));
                }
            }
        }
    }
    if strategy.try_common {
        for c in common_install_paths() {
            if let Some(ok) = scan_candidate(&mut too_old, &c, min_java)? {
                return Ok(Some(ok));
            }
        }
    }
    // Stepping over a too-old JDK during the scan must not lose the
    // information: "you have Java 11, you need 25" is far more actionable
    // than a bare "not found", and is what this has always reported.
    if let Some((path, found)) = too_old {
        return Err(LauncherError::JvmTooOld {
            path,
            found,
            min_java,
        });
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
    // `java.exe` does not imply a *loadable* VM. A JRE-only or pruned install
    // has no `jvm.dll` under `bin\server` or `bin\client`, and accepting it
    // meant discovery returned a candidate the loader then could not use:
    // the run died at `locate_jvm_dll` with a fatal `LibraryLoad` and no
    // attempt at the registry or common-location candidates that would have
    // worked. Rejecting here is what turns that into "try the next one".
    if locate_jvm_dll(dir).is_none() {
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
    // `CREATE_NO_WINDOW` keeps the parent (GUI subsystem) from flashing a
    // console window for this short-lived `java -version` probe.
    let output = match std::process::Command::new(&java_exe)
        .arg("-version")
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    {
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
    // Cap at the highest version the JNI spec actually publishes for
    // `JNI_CreateJavaVM`. Tried building `(major << 16)` for majors above
    // 21 so a Java 25 VM would be offered its own level, the way the `java`
    // executable does — and it fails outright with "JNI_CreateJavaVM
    // failed: JNI call failed". `JNI_VERSION_21` is the ceiling; there is
    // no 22/23/24/25 constant, so a larger number is simply not a version
    // the VM recognises.
    match major {
        Some(m) if m >= 21 => ("21", JNIVersion::V21),
        Some(m) if m >= 20 => ("20", JNIVersion::V20),
        Some(m) if m >= 19 => ("19", JNIVersion::V19),
        Some(m) if m >= 10 => ("10", JNIVersion::V10),
        Some(m) if m >= 9 => ("9", JNIVersion::V9),
        _ => ("1.8", JNIVersion::V1_8),
    }
}

/// The well-known Windows JDK install locations.
///
/// These are *vendor* directories, not JDK homes. A current machine has
/// `C:\Program Files\Java\jdk-25.0.1\bin\java.exe` -- one level down from
/// where this used to stop -- so returning the vendor directory alone meant
/// `check_candidate`, which requires `bin\java.exe` *directly* beneath,
/// could never match. The whole `try_common` step was dead code, while
/// `snug-format/src/config.rs` promised the step worked.
///
/// Each root is expanded by [`expand_install_root`].
fn common_install_paths() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(pf) = read_env_var("ProgramFiles") {
        roots.push(pf.join("Java"));
        roots.push(pf.join("Eclipse Adoptium"));
    }
    if let Some(pf86) = read_env_var("ProgramFiles(x86)") {
        roots.push(pf86.join("Java"));
    }
    roots.iter().flat_map(|r| expand_install_root(r)).collect()
}

/// Candidates under one vendor directory: the directory itself, then each
/// immediate child.
///
/// The root is kept because a flat install (`...\Java\bin\java.exe`) is
/// unusual but legal, and probing it costs one `is_dir`.
///
/// Children are sorted so the scan is deterministic across runs and
/// machines. `check_candidate` rejects anything under `min_java` anyway, so
/// ordering only affects which of several *valid* JDKs wins.
fn expand_install_root(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf()];
    let Ok(entries) = fs::read_dir(root) else {
        return out;
    };
    let mut children: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    children.sort();
    out.extend(children);
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
        // Packing is `major*1_000_000 + minor*1_000 + patch`, so the
        // patch level occupies the *ones* place: 21.0.5 -> 21_000_005,
        // not 21_000_500.
        assert_eq!(parse_version_subkey("21.0.5").unwrap(), 21_000_005);
        // Build suffix is ignored — major.minor.patch is enough to
        // disambiguate patch levels for registry ordering.
        assert_eq!(
            parse_version_subkey("21.0.5+9").unwrap(),
            parse_version_subkey("21.0.5").unwrap()
        );
        assert!(parse_version_subkey("not").is_none());
    }

    // ---- F-06: JNI version follows the discovered JVM --------------------

    #[test]
    fn jni_version_never_exceeds_the_discovered_jvm() {
        // The bug: a hardcoded JNIVersion::V21 made a discovered Java 17
        // fail at JNI_CreateJavaVM with "JNI call failed", after discovery
        // had already accepted it.
        assert_eq!(jni_version_for(None).1, JNIVersion::V1_8);
        assert_eq!(jni_version_for(Some(8)).1, JNIVersion::V1_8);
        assert_eq!(jni_version_for(Some(9)).1, JNIVersion::V9);
        assert_eq!(jni_version_for(Some(11)).1, JNIVersion::V10);
        assert_eq!(jni_version_for(Some(17)).1, JNIVersion::V10);
        assert_eq!(jni_version_for(Some(19)).1, JNIVersion::V19);
        assert_eq!(jni_version_for(Some(21)).1, JNIVersion::V21);
        // Capped at 21 even for a Java 25 VM: building `(25 << 16)` by hand
        // fails with "JNI_CreateJavaVM failed: JNI call failed". So the
        // invariant is two-sided -- never offer less than the VM's floor,
        // and never offer a level the spec does not publish.
        assert_eq!(jni_version_for(Some(25)).1, JNIVersion::V21);
        assert_eq!(jni_version_for(Some(25)).1.major(), 21);
        assert_eq!(jni_version_for(Some(17)).1.major(), 10);
        // The label is what goes in the log, so it must be the readable spec
        // name rather than the raw `ver` field.
        assert_eq!(jni_version_for(Some(25)).0, "21");
        assert_eq!(jni_version_for(Some(17)).0, "10");
        assert_eq!(jni_version_for(None).0, "1.8");
    }

    // ---- F-07: a too-old JDK must not abort a scan -----------------------

    /// Build a fake JDK home whose `release` file advertises `version`.
    ///
    /// Also plants a `jvm.dll`, because `check_candidate` requires one: a
    /// home with `java.exe` but no loadable VM is exactly the shape F-09
    /// rejects, so a fixture without it would silently stop testing anything.
    fn fake_jdk(root: &Path, version: &str) -> PathBuf {
        let home = root.join(format!("jdk{version}"));
        std::fs::create_dir_all(home.join("bin").join("server")).unwrap();
        std::fs::write(home.join("bin").join("java.exe"), b"").unwrap();
        std::fs::write(home.join("bin").join("javaw.exe"), b"").unwrap();
        std::fs::write(
            home.join("bin").join("server").join("jvm.dll"),
            b"",
        )
        .unwrap();
        std::fs::write(home.join("release"), format!("JAVA_VERSION=\"{version}\"\n")).unwrap();
        home
    }
    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir();
        // Counter is load-bearing: pid + nanos is not unique under parallel
        // tests on a coarse clock.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = format!(
            "snug-win-jdk-test-{}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = base.join(unique);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_too_old_jdk_does_not_abort_the_scan() {
        // The bug: `SOFTWARE\JavaSoft\JDK` is walked first, so a machine
        // with both a JDK 11 and a JDK 25 hit the 11, returned JvmTooOld via
        // `?`, and never reached the 25. Any developer machine with an old
        // JDK still installed could not discover the new one.
        let dir = tempdir();
        let old_home = fake_jdk(&dir, "11");
        let new_home = fake_jdk(&dir, "25");

        let mut too_old = None;
        // Registry order, exactly as `discover_jvm` walks it: the old one
        // is visited first.
        let mut found = None;
        for c in [&old_home, &new_home] {
            if let Some(ok) = scan_candidate(&mut too_old, c, 25).unwrap() {
                found = Some(ok);
                break;
            }
        }

        assert_eq!(
            found.as_deref(),
            Some(new_home.as_path()),
            "the scan must step over the too-old JDK and reach the good one"
        );
        assert!(too_old.is_some(), "the too-old JDK should be remembered");
    }

    #[test]
    fn an_explicit_choice_of_a_too_old_jdk_is_still_an_error() {
        // The complement, and the reason the two paths use different
        // functions: `--jvm-home`/`JAVA_HOME`/`JDK_HOME` are deliberate, so
        // "you have Java 11, you need 25" is the answer the user wants
        // rather than something to scan past.
        let dir = tempdir();
        let home = fake_jdk(&dir, "11");
        let err = check_candidate(&home, 25).unwrap_err();
        match err {
            LauncherError::JvmTooOld { found, min_java, .. } => {
                assert_eq!(found, 11);
                assert_eq!(min_java, 25);
            }
            other => panic!("expected JvmTooOld, got {other:?}"),
        }
    }

    #[test]
    fn a_remembered_too_old_jdk_is_still_reported_when_nothing_qualifies() {
        // Stepping over it during a scan must not lose the information.
        // The macOS twin records this and then drops it (its
        // `discover_jvm` returns `Ok(None)`); Windows keeps the diagnostic.
        let dir = tempdir();
        let home = fake_jdk(&dir, "11");
        let mut too_old = None;
        assert!(scan_candidate(&mut too_old, &home, 25).unwrap().is_none());
        assert_eq!(too_old, Some((home, 11)));
    }

    // ---- F-08: vendor directories must be expanded ----------------------

    #[test]
    fn a_vendor_directory_expands_to_the_jdks_inside_it() {
        // The bug: `common_install_paths` returned `...\Program Files\Java`
        // and `check_candidate` wants `bin\java.exe` *directly* beneath, so
        // the whole `try_common` step could never match anything.
        let vendor = tempdir();
        let jdk_a = fake_jdk(&vendor, "17");
        let jdk_b = fake_jdk(&vendor, "25");
        std::fs::write(vendor.join("not-a-dir.txt"), b"x").unwrap();

        let candidates = expand_install_root(&vendor);
        let names: Vec<String> = candidates
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();

        assert_eq!(
            names.first().map(String::as_str),
            Some(vendor.file_name().unwrap().to_string_lossy().as_ref()),
            "the root itself is probed too, for a flat install"
        );
        assert!(
            candidates.contains(&jdk_a) && candidates.contains(&jdk_b),
            "both installed JDKs must be reachable, got {names:?}"
        );
        assert!(
            !names.contains(&"not-a-dir.txt".to_string()),
            "files are not JDK homes"
        );
        // Deterministic: children sorted, so two runs agree on which of
        // several valid JDKs wins.
        let tail = &names[1..];
        let mut sorted = tail.to_vec();
        sorted.sort();
        assert_eq!(tail, &sorted[..], "children must be sorted: {names:?}");
    }

    #[test]
    fn an_empty_vendor_directory_still_yields_itself() {
        // A vendor directory with nothing in it is not an error; it just has
        // no JDKs. Returning the root keeps a flat install possible and
        // makes the "read_dir failed" path indistinguishable from "empty".
        let vendor = tempdir();
        let absent = vendor.join("absent");
        assert_eq!(expand_install_root(&vendor), vec![vendor.clone()]);
        assert_eq!(expand_install_root(&absent), vec![absent]);
    }

    // ---- F-09: a candidate must be loadable ----------------------------

    #[test]
    fn a_jre_without_a_jvm_dll_is_not_accepted_as_a_candidate() {
        // The bug: `check_candidate` only asked for `java.exe`, so a JRE-only
        // install was returned as the discovery result and the run then died
        // at `locate_jvm_dll` with no attempt at any later candidate.
        let dir = tempdir();
        let jre = dir.join("jre-25");
        std::fs::create_dir_all(jre.join("bin")).unwrap();
        std::fs::write(jre.join("bin").join("java.exe"), b"").unwrap();
        std::fs::write(jre.join("release"), "JAVA_VERSION=\"25\"\n").unwrap();
        // Note: no bin\server, bin\client or bin\jvm.dll.

        assert_eq!(
            check_candidate(&jre, 25).expect("not an error, just not a candidate"),
            None,
            "a java.exe with no jvm.dll must not be offered to the loader"
        );
    }

    #[test]
    fn a_jvm_dll_anywhere_the_loader_looks_is_enough() {
        // The three layouts `locate_jvm_dll` probes must all satisfy the
        // check -- otherwise the gate and the loader disagree, which is the
        // same class of bug this finding is about.
        for layout in [
            ["bin", "server", "jvm.dll"].as_slice(),
            ["bin", "client", "jvm.dll"].as_slice(),
            ["bin", "jvm.dll"].as_slice(),
        ] {
            let dir = tempdir();
            let home = fake_jdk(&dir, "25");
            std::fs::remove_dir_all(home.join("bin").join("server")).unwrap();
            // One file, at the path the layout spells out.
            let p = layout.iter().fold(home.clone(), |acc, s| acc.join(s));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"")
                .unwrap_or_else(|e| panic!("layout {layout:?}, path {p:?}: {e}"));
            assert_eq!(
                check_candidate(&home, 25).expect("no error"),
                Some(home),
                "layout {layout:?} should be accepted"
            );
        }
    }
}
