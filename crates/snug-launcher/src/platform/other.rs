//! Non-Windows placeholder. Cross-compiled stubs will not actually run on
//! non-Windows hosts; this module exists so the binary still compiles
//! cleanly for local development and CI on macOS / Linux.

use std::path::Path;

use snug_format::{FormatError, SnugEmbedded};

use crate::LauncherError;

/// No payload location to probe — there is no real runtime on this
/// platform, so nothing could be read from one anyway.
pub fn locate_payload(_self_path: &Path) -> Result<Option<SnugEmbedded>, FormatError> {
    Ok(None)
}

pub fn run(_self_path: &Path, _payload: &SnugEmbedded) -> Result<u32, LauncherError> {
    Err(LauncherError::UnsupportedPlatform)
}
