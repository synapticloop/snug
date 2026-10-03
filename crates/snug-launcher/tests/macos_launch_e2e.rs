//! End-to-end: hand a real JAR to the macOS launcher and watch a real JVM
//! start and run `main`.
//!
//! This is the test that actually matters for the macOS runtime, because
//! the whole launch path is JNI: a typo in the classpath separator, a
//! wrong `libjvm.dylib` layout, or a mis-scoped `JAVA_VERSION` parse all
//! fail *inside the JVM*, where snug has no error of its own to report.
//!
//! The JAR is built with the JDK's own `javac` / `jar` rather than being
//! checked in, so the class really is a valid class and the manifest
//! really is a real `Main-Class` entry.
//!
//! Requires a JDK and is skipped without one, so a machine with no Java
//! still gets a green run rather than a confusing failure.

#![cfg(target_os = "macos")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use snug_format::{
    embedded_file, AppMetadata, LauncherBehavior, LauncherConfig, SnugEmbedded, SnugPayload,
};
use snug_launcher::platform;
// Taken from the same place the emitter uses, so a rename cannot make the
// test build a bundle the launcher then refuses to read.
use snug_payload::PAYLOAD_SUFFIX;

const MAIN_CLASS: &str = "com.example.SnugMacosSmoke";

/// A `JAVA_HOME` with both `javac` and `jar`, or `None` to skip.
fn jdk_home() -> Option<PathBuf> {
    let home = Command::new("/usr/libexec/java_home")
        .arg("-v")
        .arg("11")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim().to_string()))
        .filter(|p| p.join("bin").join("javac").is_file())?;
    Some(home)
}

fn tempdir() -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "snug-macos-e2e-{}-{}-{}",
        std::process::id(),
        n,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Compile a one-line class into a JAR with a real `Main-Class` manifest.
fn build_jar(home: &Path, work: &Path) -> Option<Vec<u8>> {
    let src_dir = work.join("src").join("com").join("example");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(
        src_dir.join("SnugMacosSmoke.java"),
        format!(
            "package com.example;\n\
             public class SnugMacosSmoke {{\n    \
             public static void main(String[] args) {{\n        \
             System.out.println(\"SNUG_MACOS_SMOKE_OK\");\n    \
             }}\n}}\n"
        ),
    )
    .unwrap();

    let classes = work.join("classes");
    fs::create_dir_all(&classes).unwrap();

    let javac = Command::new(home.join("bin").join("javac"))
        .arg("-d")
        .arg(&classes)
        .arg(src_dir.join("SnugMacosSmoke.java"))
        .output()
        .ok()?;
    assert!(
        javac.status.success(),
        "javac failed: {}",
        String::from_utf8_lossy(&javac.stderr)
    );

    let jar = work.join("smoke.jar");
    let jar_out = Command::new(home.join("bin").join("jar"))
        .arg("--create")
        .arg(format!("--file={}", jar.display()))
        .arg(format!("--main-class={MAIN_CLASS}"))
        .arg("-C")
        .arg(&classes)
        .arg(".")
        .output()
        .ok()?;
    assert!(
        jar_out.status.success(),
        "jar failed: {}",
        String::from_utf8_lossy(&jar_out.stderr)
    );

    Some(fs::read(&jar).unwrap())
}

fn payload_for(jar: Vec<u8>) -> SnugEmbedded {
    SnugEmbedded::new(SnugPayload {
        config: LauncherConfig {
            app: AppMetadata {
                name: "Snug macOS Smoke".into(),
                company: "SynapticLoop".into(),
                version: "0.0.1".into(),
                update_check_url: None,
                description: None,
                copyright: None,
            },
            main_class: Some(MAIN_CLASS.into()),
            // 8 so any JDK on the machine qualifies. This test is about
            // the launch path, not about version gating.
            min_java: 8,
            jvm_args: vec![],
            splash: None,
            behavior: LauncherBehavior {
                cache_dir: None,
                ..LauncherBehavior::default()
            },
        },
        jars: vec![embedded_file(jar)],
        icon: None,
        localizations: Vec::new(),
    })
}

#[test]
fn macos_launcher_runs_a_real_jar_through_a_real_jvm() {
    let Some(home) = jdk_home() else {
        eprintln!("skipping: no JDK with javac/jar found via /usr/libexec/java_home");
        return;
    };
    let work = tempdir();
    let Some(jar) = build_jar(&home, &work) else {
        eprintln!("skipping: could not invoke javac/jar");
        return;
    };
    eprintln!("using JDK at {}", home.display());

    let embedded = payload_for(jar);
    let encoded = snug_format::encode(&embedded).unwrap();

    // --- Bare layout: payload as a sibling of the executable. ----------
    //
    // The payload filename is built from `PAYLOAD_SUFFIX` rather than
    // written out by hand. Hardcoding it here is exactly how a
    // `.sngpayload` / `.snugpayload` mismatch gets written, and it fails
    // as a bare "payload not found" with nothing pointing at the typo.
    let bare_dir = work.join("bare");
    fs::create_dir_all(&bare_dir).unwrap();
    let fake_exe = bare_dir.join("SnugMacOSSmoke");
    fs::write(&fake_exe, b"not a real Mach-O; run() is called directly below").unwrap();
    fs::write(
        bare_dir.join(format!("SnugMacOSSmoke.{PAYLOAD_SUFFIX}")),
        &encoded,
    )
    .unwrap();

    let found = platform::locate_payload(&fake_exe)
        .unwrap()
        .expect("bare sibling payload should be found");
    assert_eq!(found.payload.config.main_class.as_deref(), Some(MAIN_CLASS));

    // --- Bundled layout: Contents/Resources/<App>.snugpayload. ----------
    let app_dir = work.join("Snug Smoke.app").join("Contents");
    let macos_dir = app_dir.join("MacOS");
    fs::create_dir_all(&macos_dir).unwrap();
    fs::create_dir_all(app_dir.join("Resources")).unwrap();
    let bundled_exe = macos_dir.join("Snug Smoke");
    fs::write(&bundled_exe, b"fake").unwrap();
    fs::write(
        app_dir
            .join("Resources")
            .join(format!("Snug Smoke.{PAYLOAD_SUFFIX}")),
        &encoded,
    )
    .unwrap();

    let found = platform::locate_payload(&bundled_exe)
        .unwrap()
        .expect("bundled Resources payload should be found");
    assert_eq!(found.payload.config.main_class.as_deref(), Some(MAIN_CLASS));

    // A launcher with no payload at all is `None`, not an error — that is
    // how `main.rs` recognises a bare stub.
    let lonely = work.join("lonely");
    fs::create_dir_all(&lonely).unwrap();
    let lonely_exe = lonely.join("Lonely");
    fs::write(&lonely_exe, b"fake").unwrap();
    assert!(platform::locate_payload(&lonely_exe).unwrap().is_none());

    // --- The real thing: boot a JVM and run main. ----------------------
    //
    // Run with a private cache dir so the test never collides with (or
    // sweeps) a real app's cache.
    let mut embedded = embedded;
    embedded.payload.config.behavior.cache_dir = Some(work.join("cache"));

    let code = platform::run(&fake_exe, &embedded)
        .unwrap_or_else(|e| panic!("macOS launcher run() failed: {e:?}"));
    assert_eq!(code, 0, "launcher should report a clean exit");

    // Prove the JAR really landed on disk rather than the run being a
    // no-op that happened to return 0. Searched recursively instead of
    // assuming a layout: `cache::cached_jar_path` owns that shape, and a
    // test that hardcodes it breaks the next time the cache layout moves.
    fn any_jar(dir: &Path) -> bool {
        let Ok(entries) = fs::read_dir(dir) else {
            return false;
        };
        entries.filter_map(|e| e.ok()).any(|entry| {
            let p = entry.path();
            if p.is_dir() {
                any_jar(&p)
            } else {
                p.extension().is_some_and(|e| e == "jar")
            }
        })
    }

    let cache = work.join("cache");
    assert!(
        cache.is_dir(),
        "cache root should exist at {}",
        cache.display()
    );
    assert!(
        any_jar(&cache),
        "expected an extracted .jar somewhere under {}",
        cache.display()
    );
}
