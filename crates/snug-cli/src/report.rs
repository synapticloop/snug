//! Build diagnostics: codes, severities, and the reporter that collects
//! and prints them.
//!
//! Every diagnostic carries a **stable code**. The code is what a user
//! quotes in a bug report and what documentation points at, so it must
//! not change when the wording does. Severity is a separate axis: the
//! same code can be raised as an error in one build and a warning in
//! another as the checks tighten.
//!
//! - **Error** — the build does not produce an executable.
//! - **Warning** — the build proceeds.
//! - **Info** — something worth knowing, with no effect on the build.

use std::path::Path;

use crate::build_log::BuildLog;

/// How serious a diagnostic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Informational. Never affects the build.
    Info,
    /// The build continues.
    Warning,
    /// The build does not produce an executable.
    Error,
}

impl Severity {
    /// The word used in the printed diagnostic.
    pub fn label(&self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }
}

/// The stable diagnostic codes.
///
/// Every code documents its cause, its remedy, and — where the check is
/// narrower than it sounds — what it does *not* cover.
pub mod codes {
    /// Arguments or options could not be parsed.
    pub const CFG_PARSE: &str = "SNUG-CFG-001";
    /// The effective configuration is invalid.
    pub const CFG_INVALID: &str = "SNUG-CFG-002";
    /// The input is missing or unusable.
    pub const INPUT_UNAVAILABLE: &str = "SNUG-INPUT-001";
    /// A directory entry could not be read.
    pub const INPUT_ENTRY_UNREADABLE: &str = "SNUG-INPUT-002";
    /// A supplied archive is not a readable ZIP.
    pub const JAR_INVALID: &str = "SNUG-JAR-001";
    /// Manifest attributes could not be used.
    pub const MANIFEST_UNUSABLE: &str = "SNUG-MANIFEST-001";
    /// No usable entry point was selected.
    pub const MAIN_NONE: &str = "SNUG-MAIN-001";
    /// The selected class is not on the supplied classpath.
    pub const MAIN_ABSENT: &str = "SNUG-MAIN-002";
    /// The entry-point method could not be verified.
    pub const MAIN_UNVERIFIED: &str = "SNUG-MAIN-003";
    /// Several JARs declare different entry points.
    pub const MAIN_CONFLICT: &str = "SNUG-MAIN-004";
    /// The same entry class appears in more than one JAR.
    pub const MAIN_DUPLICATE: &str = "SNUG-MAIN-005";
    /// A manifest names an external classpath entry.
    pub const DEP_CLASSPATH: &str = "SNUG-DEP-001";
    /// A nested JAR was found and not resolved.
    pub const DEP_NESTED: &str = "SNUG-DEP-002";
    /// Bytecode requires a newer Java than `--min-java`.
    pub const JAVA_TOO_NEW: &str = "SNUG-JAVA-001";
    /// Preview bytecode was found.
    pub const JAVA_PREVIEW: &str = "SNUG-JAVA-002";
    /// A supplied resource is invalid.
    pub const RESOURCE_INVALID: &str = "SNUG-RESOURCE-001";
    /// The splash image is larger than recommended.
    pub const RESOURCE_OVERSIZED: &str = "SNUG-RESOURCE-002";
    /// A localisation bundle is incomplete, or has drifted from the baseline.
    ///
    /// Its own code rather than `SNUG-RESOURCE-001`: that one is about an icon
    /// or a manifest that could not be prepared at all, and a translation that
    /// is missing a key is a different kind of problem with a different fix.
    pub const LOCALISATION_INVALID: &str = "SNUG-LOCALISATION-001";
    /// The output and the log, or the output and an input, are the same file.
    pub const OUTPUT_COLLISION: &str = "SNUG-OUTPUT-001";
    /// The output could not be written.
    pub const OUTPUT_WRITE: &str = "SNUG-OUTPUT-002";
    /// A write to the log failed.
    pub const LOG_WRITE: &str = "SNUG-LOG-001";
    /// The log destination could not be opened.
    pub const LOG_UNAVAILABLE: &str = "SNUG-LOG-002";
    /// Something unexpected went wrong.
    pub const INTERNAL: &str = "SNUG-INTERNAL-001";
}

/// One diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// The stable code.
    pub code: &'static str,
    /// How serious it is.
    pub severity: Severity,
    /// Which build stage raised it.
    pub stage: String,
    /// What happened.
    pub message: String,
    /// Where it happened, when that is known.
    pub location: Option<String>,
    /// What to do about it.
    pub hint: Option<String>,
}

/// Collects diagnostics and prints them to the console and the log.
pub struct Reporter {
    log: BuildLog,
    diagnostics: Vec<Diagnostic>,
    /// The stage the build is currently in.
    ///
    /// The closing summary names the stage a failure happened in, which
    /// is the first thing worth knowing about a failed build.
    stage: String,
}

impl Reporter {
    /// A reporter writing to `log`.
    pub fn new(log: BuildLog) -> Self {
        Self {
            log,
            diagnostics: Vec::new(),
            stage: "startup".to_string(),
        }
    }

    /// Record which stage the build is in.
    ///
    /// The stage is frozen once an error has been raised: the closing
    /// summary names the stage the build *failed* in, and the first
    /// failure is the cause — a later stage's error is usually a
    /// consequence of it.
    pub fn set_stage(&mut self, stage: &str) {
        if self.error_count() == 0 {
            self.stage = stage.to_string();
        }
    }

    /// The stage the build is in.
    pub fn stage(&self) -> &str {
        &self.stage
    }

    /// Raise an error. The build will not produce an executable.
    pub fn error(
        &mut self,
        code: &'static str,
        stage: &str,
        message: impl Into<String>,
    ) -> DiagnosticBuilder<'_> {
        self.build(Severity::Error, code, stage, message)
    }

    /// Raise a warning. The build proceeds.
    pub fn warning(
        &mut self,
        code: &'static str,
        stage: &str,
        message: impl Into<String>,
    ) -> DiagnosticBuilder<'_> {
        self.build(Severity::Warning, code, stage, message)
    }

    /// Raise an informational note.
    pub fn info(
        &mut self,
        code: &'static str,
        stage: &str,
        message: impl Into<String>,
    ) -> DiagnosticBuilder<'_> {
        self.build(Severity::Info, code, stage, message)
    }

    fn build(
        &mut self,
        severity: Severity,
        code: &'static str,
        stage: &str,
        message: impl Into<String>,
    ) -> DiagnosticBuilder<'_> {
        DiagnosticBuilder {
            reporter: self,
            diagnostic: Diagnostic {
                code,
                severity,
                stage: stage.to_string(),
                message: message.into(),
                location: None,
                hint: None,
            },
        }
    }

    /// Write a line to both the console and the log.
    ///
    /// Used for the parts of the report that are not diagnostics — the
    /// effective options block, the completion summary — so the console
    /// and the log always say the same thing.
    pub fn write_line(&mut self, line: &str) {
        eprintln!("{line}");
        self.log.write_line(line);
    }

    /// Write a line to the log only.
    pub fn log_only(&mut self, line: &str) {
        self.log.write_line(line);
    }

    /// The log's location, when there is one.
    pub fn log_path(&self) -> Option<&Path> {
        self.log.path()
    }

    /// Whether a write to the log failed.
    pub fn log_write_failed(&self) -> bool {
        self.log.write_failed()
    }

    /// Report a log that could not be written completely.
    ///
    /// A partial log is worse than no log if the user is not told, so this is
    /// raised rather than left for the user to discover. It carries a code so a
    /// bug report can quote it — which is the whole point of writing a log.
    pub fn report_log_write_failure(&mut self) {
        if !self.log_write_failed() {
            return;
        }
        let path = self
            .log_path()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        self.warning(
            codes::LOG_WRITE,
            "log",
            format!("the build log at {path} could not be written completely"),
        )
        .hint(
            "the build's own result is unaffected; check the disk space and whether the file is locked",
        )
        .emit();
    }

    /// Every diagnostic raised so far.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// How many errors were raised.
    pub fn error_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count()
    }

    /// How many warnings were raised.
    pub fn warning_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .count()
    }

    /// Whether any error was raised.
    pub fn has_errors(&self) -> bool {
        self.error_count() > 0
    }

    /// Record a diagnostic and print it.
    fn push(&mut self, diagnostic: Diagnostic) {
        let mut line = format!(
            "snug: {} {}: {}",
            diagnostic.severity.label(),
            diagnostic.code,
            diagnostic.message
        );
        if let Some(location) = &diagnostic.location {
            line.push_str(&format!(" [{location}]"));
        }
        eprintln!("{line}");
        self.log.write_line(&line);

        if let Some(hint) = &diagnostic.hint {
            let hint_line = format!("snug:   hint: {hint}");
            eprintln!("{hint_line}");
            self.log.write_line(&hint_line);
        }
        self.diagnostics.push(diagnostic);
    }
}

/// Builds one diagnostic, then emits it.
///
/// The builder exists so a diagnostic can carry a location and a hint
/// without every call site spelling out a struct literal.
pub struct DiagnosticBuilder<'a> {
    reporter: &'a mut Reporter,
    diagnostic: Diagnostic,
}

impl DiagnosticBuilder<'_> {
    /// Where the problem is, when that is known.
    pub fn location(mut self, location: impl Into<String>) -> Self {
        self.diagnostic.location = Some(location.into());
        self
    }

    /// What to do about it.
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.diagnostic.hint = Some(hint.into());
        self
    }

    /// Record the diagnostic and print it.
    pub fn emit(self) {
        self.reporter.push(self.diagnostic);
    }
}

/// Property names that mark a JVM argument as carrying a credential.
///
/// Matched against whole dot/dash/underscore-separated segments, so
/// `db.password` and `api-token` are caught while `monkey` is not.
const SECRET_KEY_NAMES: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "pwd",
    "key",
    "credential",
    "auth",
    "salt",
    "signature",
];

/// Compound names that do not split into the segments above.
const SECRET_COMPOUND_NAMES: &[&str] = &[
    "apikey",
    "secretkey",
    "accesskey",
    "authtoken",
    "clientsecret",
    "refreshtoken",
    "privatekey",
];

/// Words that mark a *value* as a credential, wherever it appears.
const SECRET_VALUE_WORDS: &[&str] = &[
    "secret",
    "password",
    "passwd",
    "token",
    "credential",
    "apikey",
    "api_key",
];

/// Whether a JVM argument looks like it carries a credential.
///
/// Only credential-shaped values are masked. A blanket mask over every
/// `--jvm-arg` would hide `-Xmx2g` and `-Dfile.encoding=UTF-8` along
/// with the secrets, and a log that hides its own settings is a log
/// nobody can debug from.
///
/// A credential can be named by the property (`-Dapi.token=…`) or be
/// the value itself (`-javaagent:agent.jar=supersecret`), so both are
/// checked.
pub fn looks_credential_shaped(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();

    // `-Dkey=value` and `-javaagent:path=value`.
    if let Some((key, rest)) = lower.split_once('=') {
        // For a java agent the part before the `=` is a path, not a
        // property name, so only the value can be judged there.
        let key = key.strip_prefix("-d").unwrap_or(key);
        return key_is_secret(key) || value_looks_secret(rest);
    }

    // No `=` at all: the whole argument is the value.
    value_looks_secret(&lower)
}

/// Whether a property name marks its value as a credential.
fn key_is_secret(key: &str) -> bool {
    key.split(['.', '-', '_']).any(|segment| {
        SECRET_KEY_NAMES.contains(&segment) || SECRET_COMPOUND_NAMES.contains(&segment)
    })
}

/// Whether a value reads as a credential.
fn value_looks_secret(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    SECRET_VALUE_WORDS.iter().any(|word| lower.contains(word))
}

/// The masked form of a JVM argument.
///
/// The key is kept — `api.token` is not a secret, and knowing *which*
/// property was masked is the point — and only the value is replaced.
pub fn redact_jvm_arg(value: &str) -> String {
    if !looks_credential_shaped(value) {
        return value.to_string();
    }
    match value.split_once('=') {
        Some((head, _)) => format!("{head}=***"),
        None => "***".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_that_could_not_be_written_is_reported() {
        let mut reporter = Reporter::new(BuildLog::none());
        reporter.report_log_write_failure();
        assert_eq!(
            reporter.warning_count(),
            0,
            "a log that never existed is not a failed write"
        );

        let mut reporter = Reporter::new(BuildLog::none());
        reporter.log.fail_writes();
        reporter.report_log_write_failure();
        assert_eq!(reporter.warning_count(), 1);
        assert!(
            reporter
                .diagnostics()
                .iter()
                .any(|d| d.code == codes::LOG_WRITE),
            "the failure should carry its code"
        );
    }

    #[test]
    fn harmless_jvm_arguments_are_left_alone() {
        for value in [
            "-Xmx2g",
            "-Xms256m",
            "-Dfile.encoding=UTF-8",
            "-Djava.awt.headless=true",
            "--enable-native-access=ALL-UNNAMED",
        ] {
            assert!(
                !looks_credential_shaped(value),
                "`{value}` should not be treated as a credential"
            );
            assert_eq!(redact_jvm_arg(value), value);
        }
    }

    #[test]
    fn credential_shaped_arguments_are_detected() {
        for value in [
            "-Dapi.token=abc123",
            "-Ddb.password=hunter2",
            "-Dauth.secret=x",
            "-DapiKey=xyz",
            "-javaagent:/path/agent.jar=supersecret",
            "supersecret",
        ] {
            assert!(
                looks_credential_shaped(value),
                "`{value}` should be treated as a credential"
            );
        }
    }

    #[test]
    fn a_key_that_merely_contains_a_marker_is_not_a_credential() {
        // `monkey` ends in "key" but is not a credential, and masking it
        // would make the log harder to read for no safety gained.
        assert!(!looks_credential_shaped("-Dmonkey=banana"));
    }

    #[test]
    fn masking_keeps_the_property_name() {
        assert_eq!(redact_jvm_arg("-Dapi.token=abc123"), "-Dapi.token=***");
        assert_eq!(redact_jvm_arg("-Xmx2g"), "-Xmx2g");
    }
}
