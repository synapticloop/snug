//! The build log: where it goes, and how it is written.
//!
//! Every build writes a log. The default destination is
//! `snug.build.log` in the *current working directory* — not beside the
//! executable — because every other path snug resolves is relative to the
//! working directory too, and a build run from two different directories
//! should not silently write to two different places.
//!
//! The destination is validated before the previous log is truncated. A
//! log that cannot be written must not cost the user the log they had.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The default log file name, used both as the default destination and
/// inside a directory passed to `--build-log`.
pub const DEFAULT_LOG_NAME: &str = "snug.build.log";

/// A build log on disk — or the absence of one.
///
/// The absence is a real state, not an error: `--find-main` and
/// `--list-tokens` are diagnostics rather than builds, so they write no
/// log and report no log location.
#[derive(Debug, Default)]
pub struct BuildLog {
    /// Where the log lives, when there is one.
    path: Option<PathBuf>,
    /// The open file, when there is one.
    file: Option<File>,
    /// Set once a write fails, so the run can say the log is incomplete
    /// rather than claiming a complete log was saved.
    write_failed: bool,
}

impl BuildLog {
    /// A log that does not exist and never will.
    pub fn none() -> Self {
        Self::default()
    }

    /// Where the log should live.
    ///
    /// The rules, in order:
    ///
    /// - no `--build-log`: `snug.build.log` in the working directory;
    /// - `--build-log` naming an existing directory: `snug.build.log`
    ///   inside it;
    /// - `--build-log` naming anything else: that path, used as a file.
    ///
    /// A path that does not exist yet is treated as a file. There is no
    /// extension guessing: `--build-log build` is a file called `build`,
    /// and a directory has to already exist to be treated as one.
    pub fn resolve(requested: Option<&Path>, cwd: &Path) -> PathBuf {
        let chosen = match requested {
            None => cwd.join(DEFAULT_LOG_NAME),
            Some(path) => {
                if path.is_dir() {
                    path.join(DEFAULT_LOG_NAME)
                } else {
                    path.to_path_buf()
                }
            }
        };
        if chosen.is_absolute() {
            chosen
        } else {
            cwd.join(chosen)
        }
    }

    /// Open the log at `path`, truncating it.
    ///
    /// The open happens *before* the truncation so that an unwritable
    /// destination leaves the previous log intact.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.set_len(0)?;
        Ok(Self {
            path: Some(path.to_path_buf()),
            file: Some(file),
            write_failed: false,
        })
    }

    /// The log's location, when there is one.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Whether a write to the log failed at some point.
    ///
    /// A partial log is worse than no log if the user is not told, so the
    /// run reports this rather than claiming a complete log was saved.
    pub fn write_failed(&self) -> bool {
        self.write_failed
    }

    /// Write one line, appending a newline.
    ///
    /// A failed write is recorded rather than propagated: the build's own
    /// outcome matters more than the log's, and the original error is
    /// what the user needs to see.
    pub fn write_line(&mut self, line: &str) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        if writeln!(file, "{line}").is_err() || file.flush().is_err() {
            self.write_failed = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_destination_is_in_the_working_directory() {
        let cwd = Path::new("/work");
        assert_eq!(
            BuildLog::resolve(None, cwd),
            PathBuf::from("/work/snug.build.log")
        );
    }

    #[test]
    fn an_existing_directory_gets_the_default_name_inside_it() {
        let dir = std::env::temp_dir().join("snug-log-test-dir");
        std::fs::create_dir_all(&dir).expect("create dir");
        let resolved = BuildLog::resolve(Some(&dir), Path::new("/work"));
        assert_eq!(resolved, dir.join(DEFAULT_LOG_NAME));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_nonexistent_path_is_used_as_a_file() {
        let cwd = Path::new("/work");
        let resolved = BuildLog::resolve(Some(Path::new("logs/build.txt")), cwd);
        assert_eq!(resolved, PathBuf::from("/work/logs/build.txt"));
    }

    #[test]
    fn an_absolute_request_is_left_alone() {
        let cwd = Path::new("/work");
        let resolved = BuildLog::resolve(Some(Path::new("/tmp/x.log")), cwd);
        assert_eq!(resolved, PathBuf::from("/tmp/x.log"));
    }

    #[test]
    fn opening_truncates_a_previous_log() {
        let dir = std::env::temp_dir().join("snug-log-truncate-test");
        std::fs::create_dir_all(&dir).expect("create dir");
        let path = dir.join(DEFAULT_LOG_NAME);
        std::fs::write(&path, "old contents").expect("write");

        let mut log = BuildLog::open(&path).expect("open");
        log.write_line("new contents");
        drop(log);

        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "new contents\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_log_with_no_file_writes_nothing_and_does_not_fail() {
        let mut log = BuildLog::none();
        log.write_line("ignored");
        assert!(log.path().is_none());
        assert!(!log.write_failed());
    }
}
