//! Library entry for the `snug` CLI. Exposes the modules needed by
//! integration tests; the binary entry point lives in `main.rs`.

#![forbid(unsafe_code)]

pub mod build;
pub mod build_log;
pub mod classfile;
pub mod cli;
pub mod effective;
pub mod find_main;
pub mod init_localizations;
pub mod init_options;
pub mod list_tokens;
pub mod localization;
pub mod macos_bundle;
pub mod manifest;
pub mod options_file;
pub mod provenance;
pub mod report;
pub mod resources;
pub mod stub;
