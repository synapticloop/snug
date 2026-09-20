//! Platform-specific launcher implementation.
//!
//! Currently only Windows is supported. On other platforms the launcher
//! returns [`LauncherError::UnsupportedPlatform`] so the binary can be
//! compiled and unit-tested (cross-platform logic only) but is a no-op
//! at runtime.

#[cfg(windows)]
mod windows;

#[cfg(not(windows))]
mod other;

#[cfg(windows)]
pub use self::windows::run;

#[cfg(not(windows))]
pub use self::other::run;
