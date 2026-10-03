//! Platform-specific launcher implementation.
//!
//! `windows` is the production target and carries the full runtime.
//! `macos` mirrors it with `libjvm.dylib` / a `:`-separated classpath and
//! macOS JVM discovery. Anything else gets a no-op so the binary still
//! compiles and the portable logic is unit-testable on any host.

#[cfg(windows)]
mod windows;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(not(any(windows, target_os = "macos")))]
mod other;

#[cfg(windows)]
pub use self::windows::locate_payload;
#[cfg(windows)]
pub use self::windows::run;

#[cfg(target_os = "macos")]
pub use self::macos::locate_payload;
#[cfg(target_os = "macos")]
pub use self::macos::run;
/// Payload filename suffix — `<App>.snugpayload`.
///
/// Exported because a producer and a consumer must agree on this name
/// exactly: `snug-cli` writes the file when it emits a bundle, and this
/// module reads it. Spelling the string out in both places is how a
/// `.sngpayload` / `.snugpayload` mismatch gets written.
#[cfg(target_os = "macos")]
pub use self::macos::PAYLOAD_SUFFIX;

#[cfg(not(any(windows, target_os = "macos")))]
pub use self::other::locate_payload;
#[cfg(not(any(windows, target_os = "macos")))]
pub use self::other::run;
