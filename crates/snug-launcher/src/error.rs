use thiserror::Error;

use crate::localize;

/// Errors produced by the snug launcher runtime.
///
/// The `#[error("...")]` strings on each variant are the canonical
/// English wording used by `thiserror`'s generated `Display` impl
/// and by callers that just want `format!("{err}")` to produce
/// something sensible — e.g. stderr diagnostic lines or
/// pre-`init`-phase failures.
///
/// For user-facing UI (dialog bodies, message boxes), callers should
/// invoke [`localize_launcher_error`] instead. That helper looks up
/// the matching `err.*` key in the localization bundle and fills
/// placeholders from the variant's fields. Missing keys fall back to
/// the bundle's English baseline (which is the same text as the
/// `#[error]` literal), so end users always see English at minimum.
#[derive(Debug, Error)]
pub enum LauncherError {
    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("could not determine current executable path: {0}")]
    SelfPath(#[source] std::io::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("could not locate a Java {min_java}+ JVM in any of the configured discovery locations")]
    JvmNotFound { min_java: u16 },

    #[error("located JVM at {path} but its major version {found} is below the required minimum {min_java}")]
    JvmTooOld {
        path: std::path::PathBuf,
        found: u16,
        min_java: u16,
    },

    #[error("failed to load {library}: {message}")]
    LibraryLoad { library: String, message: String },

    #[error("`{symbol}` not found in {library}")]
    SymbolNotFound { library: String, symbol: String },

    /// Internal assumption broke during the launch flow (e.g., a
    /// splash handle was missing when it shouldn't be).
    #[error("internal: {0}")]
    InvalidState(&'static str),

    #[error("could not build JNI init args: {0}")]
    JniInit(String),

    #[error("JNI_CreateJavaVM failed: {0}")]
    JniCreate(String),

    #[error("AttachCurrentThread failed: {0}")]
    JniAttach(String),

    #[error("JNI invocation failed: {0}")]
    JniInvoke(String),

    #[error("no Main-Class found in JAR manifest and no --main-class override")]
    NoMainClass,

    #[error("Main-Class `{0}` could not be found or loaded by the JVM")]
    MainClassNotFound(String),

    #[error("Main-Class `{0}` does not have a `main(String[])` method")]
    NoMainMethod(String),

    #[error("Java `main` threw an exception: {0}")]
    JavaException(String),

    #[error("unsupported platform: snug-launcher currently only runs on Windows")]
    UnsupportedPlatform,

    #[error("embedded payload: {0}")]
    Format(#[from] snug_format::FormatError),
}

/// Render a [`LauncherError`] using the localization bundle.
///
/// Each variant delegates to `localize::t(...)` with the matching
/// `err.*` key, filling the named placeholders from the variant's
/// fields. If the bundle is missing the key (i.e. before
/// [`crate::localize::init`] has run, or for a translation file that
/// hasn't covered the variant yet), the lookup returns the raw key
/// string — we detect that and fall back to the `#[error]`-derived
/// English text instead, so end users never see raw key text.
pub fn localize_launcher_error(err: &LauncherError) -> String {
    let s = match err {
        LauncherError::Zip(e) => localize::t("err.zip", &[("0", &e.to_string())]),
        LauncherError::SelfPath(e) => {
            localize::t("err.self_path", &[("0", &e.to_string())])
        }
        LauncherError::Io(e) => localize::t("err.io", &[("0", &e.to_string())]),
        LauncherError::JvmNotFound { min_java } => localize::t(
            "err.jvm_not_found",
            &[("min_java", &min_java.to_string())],
        ),
        LauncherError::JvmTooOld {
            path,
            found,
            min_java,
        } => localize::t(
            "err.jvm_too_old",
            &[
                ("path", &path.display().to_string()),
                ("found", &found.to_string()),
                ("min_java", &min_java.to_string()),
            ],
        ),
        LauncherError::LibraryLoad { library, message } => localize::t(
            "err.library_load",
            &[("library", library), ("message", message)],
        ),
        LauncherError::SymbolNotFound { library, symbol } => localize::t(
            "err.symbol_not_found",
            &[("symbol", symbol), ("library", library)],
        ),
        LauncherError::InvalidState(msg) => localize::t("err.invalid_state", &[("0", msg)]),
        LauncherError::JniInit(s) => localize::t("err.jni_init", &[("0", s)]),
        LauncherError::JniCreate(s) => localize::t("err.jni_create", &[("0", s)]),
        LauncherError::JniAttach(s) => localize::t("err.jni_attach", &[("0", s)]),
        LauncherError::JniInvoke(s) => localize::t("err.jni_invoke", &[("0", s)]),
        LauncherError::NoMainClass => localize::lookup("err.no_main_class"),
        LauncherError::MainClassNotFound(s) => {
            localize::t("err.main_class_not_found", &[("0", s)])
        }
        LauncherError::NoMainMethod(s) => localize::t("err.no_main_method", &[("0", s)]),
        LauncherError::JavaException(s) => localize::t("err.java_exception", &[("0", s)]),
        LauncherError::UnsupportedPlatform => localize::lookup("err.unsupported_platform"),
        LauncherError::Format(e) => localize::t("err.format", &[("0", &e.to_string())]),
    };
    if looks_like_raw_key(&s) {
        err.to_string() // `thiserror`'s Display = the `#[error]` literal.
    } else {
        s
    }
}

/// Heuristic: returns true when `s` looks like an unresolved
/// localization key (`err.something`, no whitespace, no embedded
/// values). False positives are harmless — the worst case is we
/// fall back to English when the actual localized message happened
/// to start with `err.` and contain no spaces (which we author the
/// baseline to avoid).
fn looks_like_raw_key(s: &str) -> bool {
    s.starts_with("err.") && s.contains('.') && !s.chars().any(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No `localize::init` has run, so `localize::lookup` returns the
    /// raw key — we should fall back to the `#[error]` English
    /// literal and never display `err.io` to the user.
    #[test]
    fn localize_falls_back_to_english_when_uninitialised() {
        let err = LauncherError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            "disk on fire",
        ));
        let out = localize_launcher_error(&err);
        assert!(!out.starts_with("err."), "raw key leaked to user: {out:?}");
        assert!(out.contains("disk on fire"), "fallback lost the source: {out:?}");
    }

    #[test]
    fn looks_like_raw_key_distinguishes_keys_from_messages() {
        assert!(looks_like_raw_key("err.foo"));
        assert!(looks_like_raw_key("err.java_exception"));
        // Real English messages start with capital letters and have
        // spaces — never mistaken for a key.
        assert!(!looks_like_raw_key("ZIP error: x"));
        assert!(!looks_like_raw_key("I/O error: x"));
    }
}

#[cfg(windows)]
impl From<jni::errors::Error> for LauncherError {
    fn from(e: jni::errors::Error) -> Self {
        LauncherError::JniInvoke(e.to_string())
    }
}

#[cfg(windows)]
impl From<jni::errors::StartJvmError> for LauncherError {
    fn from(e: jni::errors::StartJvmError) -> Self {
        LauncherError::JniCreate(e.to_string())
    }
}
