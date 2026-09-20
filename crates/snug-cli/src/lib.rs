//! Library entry for the `snug` CLI. Exposes the modules needed by
//! integration tests; the binary entry point lives in `main.rs`.

#![forbid(unsafe_code)]

pub mod build;
pub mod cli;
pub mod manifest;
pub mod rcedit;
pub mod stub;
