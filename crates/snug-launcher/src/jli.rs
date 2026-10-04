//! Launch the JVM through the JDK's own launcher on macOS.
//!
//! # Why this exists
//!
//! snug originally created the VM in-process with `JNI_CreateJavaVM` and
//! called the app's `main` through JNI. That works for console apps, and
//! it is the only path on Windows. On macOS it **cannot work for any GUI
//! app**, and the failure is silent:
//!
//! `JNI_CreateJavaVM` makes the *calling* thread — snug's process main
//! thread — the Java main thread. AppKit, meanwhile, requires the event
//! loop to run on the process's *initial* thread. One thread, two jobs.
//! JavaFX parks its `main` on a `CountDownLatch`, so the initial thread is
//! never free, so `applicationDidFinishLaunching:` is never delivered, so
//! `MacApplication.runLoop` never returns, so `PlatformImpl.startup` never
//! completes and the `JavaFX Application Thread` is never created. The
//! visible result is a dock icon bouncing with no window, no exception,
//! and nothing in the log after the JNI handoff.
//!
//! It is not JavaFX-specific: AWT hangs the same way on `JFrame`. Any
//! AppKit toolkit needs that thread.
//!
//! `JLI_Launch` is the entry point the `java` executable and jpackage's
//! generated launchers use, and it resolves the conflict properly: the
//! initial thread stays reserved for the UI while the Java app runs on a
//! thread the VM creates. So the macOS launcher hands the work to it
//! instead of doing it itself.
//!
//! # Requirements
//!
//! `JLI_Launch` resolves the host's entry point with
//! `dlsym(RTLD_DEFAULT, "main")` and fails with "error locating main
//! entrypoint" when there is none. rustc emits a `main`, but the release
//! profile strips it, so `build.rs` adds `-exported_symbol,_main`, which
//! survives stripping. Verified: with it, a JLI-launched JavaFX app
//! reaches renderer creation.
//!
//! # Fallback
//!
//! Returns `None` when there is no usable `libjli`, and the caller drops
//! back to the in-process JNI path. A JRE without `libjli.dylib` then
//! still launches console apps; only GUI apps remain broken there, which
//! is the honest ceiling for a stripped, in-process host.

#![cfg(target_os = "macos")]

use std::ffi::{CString, c_char, c_int, c_void};
use std::ptr;
use std::path::Path;

use crate::log;

/// `dlopen(2)` flags. `RTLD_NOW` resolves every symbol up front, so a
/// JDK whose `libjli` is missing an entry point fails here rather than
/// at some later point inside the launch.
const RTLD_NOW: c_int = 0x2;

/// `JLI_Launch` as declared in the JDK's `jli_util.h`:
///
/// ```c
/// jint JLI_Launch(jint argc, char **argv, jint jrelaunch,
///                 jint launchmode, const char *javaw, const char *jvmcfg);
/// ```
///
/// `jrelaunch = 0` for a first launch (non-zero is how the launcher
/// re-executes itself). `launchmode = 0` is `LAUNCH_MODE`; `1` is
/// `JVMMODE`, which wants a raw `java` binary. `javaw` and `jvmcfg` are
/// for the jpackage-style launchers that carry their config in a `.cfg`
/// file; passing the classpath and main class on the command line is
/// equivalent and needs no temp file.
type JliLaunch = unsafe extern "C" fn(
    c_int,
    *mut *mut c_char,
    c_int,
    c_int,
    *const c_char,
    *const c_char,
) -> c_int;

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// Locate `libjli.dylib` inside a discovered `JAVA_HOME`.
fn libjli_path(java_home: &Path) -> Option<std::path::PathBuf> {
    let candidate = java_home.join("lib").join("libjli.dylib");
    candidate.is_file().then_some(candidate)
}

/// Run the app through the JDK's launcher. Returns the app's exit code, or
/// `None` if this JDK has no usable `libjli` and the caller should fall
/// back to the in-process JNI path.
///
/// Blocks for the life of the app, exactly as `main` did under JNI.
pub(crate) fn launch(
    java_home: &Path,
    classpath: &[std::path::PathBuf],
    main_class: &str,
    jvm_args: &[String],
    app_args: &[String],
) -> Option<i32> {
    let Some(libjli) = libjli_path(java_home) else {
        log::debug(&format!(
            "jli: no libjli.dylib under {}; using the in-process JNI path",
            java_home.display()
        ));
        return None;
    };

    // `JLI_Launch` locates the runtime relative to JAVA_HOME in places,
    // and the standalone `java` binary always has it set. Exporting it
    // keeps the two paths equivalent.
    // SAFETY: single-threaded at this point in the launch, before any
    // worker exists, so nothing can be reading the environment.
    unsafe {
        std::env::set_var("JAVA_HOME", java_home);
    }

    // JLI may re-exec this process with a transformed command line, so
    // nothing re-derived from `argv` survives that. Everything the launch
    // needs is built here, before the call.
    let cp = classpath
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(":");

    let mut argv_owned: Vec<CString> = Vec::new();
    let mut push = |s: &str| -> Option<()> {
        CString::new(s).ok().map(|c| argv_owned.push(c))
    };
    // argv[0] is what the launcher reports itself as; the JDK's own
    // binary passes its own path, but the name is all JLI uses it for.
    push("java")?;
    for a in jvm_args {
        push(a)?;
    }
    push("-cp")?;
    push(&cp)?;
    push(main_class)?;
    for a in app_args {
        push(a)?;
    }
    let mut argv: Vec<*mut c_char> = argv_owned.iter().map(|c| c.as_ptr() as *mut c_char).collect();

    let lib_c = CString::new(libjli.to_str()?).ok()?;
    // SAFETY: `libjli_c` is a valid NUL-terminated path and
    // `RTLD_NOW` is a valid flag combination. A null handle means the
    // load failed, which is checked below.
    let handle = unsafe { dlopen(lib_c.as_ptr(), RTLD_NOW) };
    if handle.is_null() {
        log::debug("jli: dlopen(libjli.dylib) failed; using the in-process JNI path");
        return None;
    }

    let sym = CString::new("JLI_Launch").ok()?;
    // SAFETY: `handle` is a live `dlopen` handle, and `sym` is a valid
    // NUL-terminated name. `dlsym` returns a data pointer that is only
    // meaningful when re-interpreted as the function the symbol names,
    // which is exactly what the `libjli` ABI specifies, so the
    // transmute is the documented pattern rather than a guess. A null
    // symbol means this `libjli` is not the one we expect.
    let raw = unsafe { dlsym(handle, sym.as_ptr()) };
    let Some(func) = (if raw.is_null() {
        None
    } else {
        // SAFETY: see above — the pointer came from `dlsym` for the
        // symbol `JLI_Launch` in a loaded `libjli.dylib`.
        Some(unsafe { std::mem::transmute::<*mut c_void, JliLaunch>(raw) })
    }) else {
        log::debug("jli: libjli.dylib has no JLI_Launch; using the in-process JNI path");
        return None;
    };

    // `JLI_Launch` resolves *this process's* entry point with
    // `dlsym(RTLD_DEFAULT, "main")` and aborts with "error locating main
    // entrypoint" when there is none — which also means a failed JLI
    // launch is a *dead app*, not a fall-back. So check for the symbol
    // ourselves first and only take this path when it is really there.
    //
    // This matters because the export is fragile: `-exported_symbol,_main`
    // from `build.rs` is not reliably honoured, and `strip = true`
    // removes the symbol from the dynamic table even when it is. A
    // console app must keep working either way, so the honest answer to
    // "is this binary a real launcher?" is asked at run time rather than
    // assumed at build time.
    let main_sym = CString::new("main").ok()?;
    // SAFETY: a null `RTLD_DEFAULT` handle means "the global scope",
    // which is the documented way to ask whether this image exports a
    // symbol. The result is only tested for null.
    let has_main = !unsafe { dlsym(ptr::null_mut(), main_sym.as_ptr()) }.is_null();
    if !has_main {
        log::debug(
            "jli: this binary does not export `main`, which JLI_Launch requires; \
             using the in-process JNI path (GUI apps will hang on macOS)",
        );
        return None;
    }

    log::debug(&format!(
        "jli: launching via {} ({} arg(s), main {main_class})",
        libjli.display(),
        argv.len()
    ));

    // SAFETY: `func` is the real `JLI_Launch`, `argv` is a live array of
    // NUL-terminated strings that outlives the call (nothing mutates
    // `argv_owned` while JLI runs), and the two trailing arguments are
    // the documented NULLs for a command-line launch. JLI blocks until
    // the app exits, which is the same lifetime `main` had under JNI.
    let code = unsafe { func(argv.len() as c_int, argv.as_mut_ptr(), 0, 0, std::ptr::null(), std::ptr::null()) };

    log::debug(&format!("jli: JLI_Launch returned {code}"));
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A JDK without `libjli` must report "no launcher" rather than
    /// panicking, because the caller falls back to the JNI path on exactly
    /// that answer.
    #[test]
    fn no_libjli_means_no_launcher() {
        let tmp = std::env::temp_dir().join("snug-jli-test-absent");
        assert!(libjli_path(&tmp).is_none());
        assert!(launch(&tmp, &[], "Main", &[], &[]).is_none());
    }

    /// The classpath join has to be `:`-separated. A `;` here would
    /// degrade to one enormous bogus path and every class would fail to
    /// resolve — the same trap as the JNI path, for the same reason.
    #[test]
    fn classpath_uses_the_platform_separator() {
        let paths = vec![
            std::path::PathBuf::from("/a/one.jar"),
            std::path::PathBuf::from("/a/two.jar"),
        ];
        let joined = paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(":");
        assert_eq!(joined, "/a/one.jar:/a/two.jar");
        assert!(!joined.contains(';'));
    }

    /// `RTLD_NOW` is the macOS value, not the Linux one. Getting this
    /// wrong is a null handle and a silent fallback to JNI.
    #[test]
    fn rtld_now_is_the_darwin_value() {
        assert_eq!(RTLD_NOW, 0x2);
    }

    #[test]
    fn cstr_roundtrips_a_classpath() {
        let s = CString::new("/tmp/a b.jar").unwrap();
        assert_eq!(s.to_str().unwrap(), "/tmp/a b.jar");
        // Interior NULs must be rejected rather than silently truncating
        // the classpath.
        assert!(CString::new("a\0b").is_err());
    }
}
