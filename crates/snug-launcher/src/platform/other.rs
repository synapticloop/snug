//! Non-Windows placeholder. Cross-compiled stubs will not actually run on
//! non-Windows hosts; this module exists so the binary still compiles
//! cleanly for local development and CI on macOS / Linux.

use std::path::Path;

use snug_format::SnugEmbedded;

use crate::LauncherError;

pub fn run(_self_path: &Path, _payload: &SnugEmbedded) -> Result<u32, LauncherError> {
    Err(LauncherError::UnsupportedPlatform)
}
