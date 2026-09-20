use thiserror::Error;

/// Errors produced by the snug launcher runtime.
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

    #[error("JNI_CreateJavaVM returned error code {0}")]
    JniCreateVm(i32),

    #[error("JNI launch not yet implemented (slice 2 stub) — see crates/snug-launcher/src/platform/windows.rs")]
    JniStub,

    #[error("AttachCurrentThread failed: {0}")]
    JniAttach(i32),

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
