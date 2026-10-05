//! `snug-dropper` — the "Build with Snug" beginner front-end.
//!
//! A standalone Windows shim, and its own workspace crate: it never links
//! the builder, the launcher, or `clap` (the only dependency is the raw
//! `windows-sys` FFI surface for the dialogs). It launches `snug.exe` as
//! a child process and reports what happened in a dialog.
//!
//! Three modes, decided by [`decide`] from the dropped items alone:
//!
//! | Drop                        | Behaviour                                    |
//! |-----------------------------|----------------------------------------------|
//! | a single `.jar`             | build it, marquee, then a success dialog      |
//! | a single folder             | build it as a multi-JAR classpath             |
//! | two or more items           | say so, then open a command window            |
//! | one unusable item           | say so, then open a command window            |
//! | nothing (a double-click)    | open a command window                         |
//! | the build itself fails      | error dialog naming the log — no window      |
//!
//! Application identity is hardcoded to the literals in [`build`] on
//! purpose: this is a starter utility for absolute beginners, and an
//! artefact called `Example Application Name.exe` announces where it came
//! from instead of pretending to be a product.
//!
//! The module split is the point of the crate: everything that can be
//! decided or formatted without a window lives here in cross-platform
//! pure code with the tests attached, and only [`ui`] talks to Win32.

pub mod build;
pub mod decide;
pub mod terminal;

/// Windows dialogs and the marquee. Pure Win32, so it stays behind the
/// platform gate rather than dragging `windows-sys` into a Mac build.
#[cfg(windows)]
pub mod ui;

/// The AppKit half: `NSApplication`, the `openFile:` delegate that makes
/// drag-and-drop possible at all on macOS, and the result dialogs.
#[cfg(target_os = "macos")]
pub mod macos;
