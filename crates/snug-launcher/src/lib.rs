//! Windows runtime that scans itself for the snug payload, locates a
//! compatible JVM, and launches the embedded fat JAR via JNI.
//!
//! The crate is also compiled into the **stub launcher binary** (a
//! precompiled `launcher-stub.exe`) which the `snug` CLI embeds via
//! `include_bytes!()` and appends a payload to at build time. At runtime
//! the stub binary scans its own file for the [`MAGIC`] prefix and
//! behaves accordingly.
//!
//! Two execution modes:
//!
//! 1. **With payload** — find SNUGEMBD in self, decode, locate JVM,
//!    load `jvm.dll`, invoke Java `main`.
//! 2. **Without payload (bare stub)** — emit a friendly "this is a bare
//!    stub, build it with snug" message and exit non-zero. Useful for
//!    catching accidental commits of the stub itself.

#![deny(unsafe_op_in_unsafe_fn)]
// SAFETY: the Windows implementation uses unsafe FFI for the JNI / Win32
// surface, restricted to thin wrappers around documented, stable APIs.
// Each unsafe block is wrapped in `unsafe extern "system"` (edition 2024)
// and confined to `#[cfg(windows)]` modules.

pub mod cache;
pub mod custom_dialog;
pub mod dialogs;
pub mod error;
pub mod jdk_install;
pub mod log;
pub mod manifest;
pub mod payload_locator;
pub mod progress_window;
pub mod splash;

pub mod platform;

pub use error::LauncherError;
pub use payload_locator::find_in_file;
