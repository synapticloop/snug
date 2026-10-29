@echo off
REM ===========================================================================
REM clean.cmd - reclaim disk space from the Rust build tree.
REM
REM target\ is the largest thing in a snug working copy. Cargo keeps every
REM dependency's compiled form, every incremental compilation unit and every
REM build-script output, and it never throws any of it away on its own.
REM Measured here: target\ 6.1 GB, against 37 MB of release\ staging and
REM 13 MB of demo output in assets\.
REM
REM This runs `cargo clean`, which removes the whole of target\ and reports
REM what it freed. Nothing tracked by git is touched: target\ is ignored, and
REM cargo only removes what it built.
REM
REM The release\ staging tree and the demo outputs in assets\ are left alone.
REM They are small, and they are not cargo's to remove.
REM
REM Any arguments are handed to cargo clean, so the finer-grained forms work
REM too - and so does cargo's own --dry-run:
REM
REM     .\scripts\windows\clean.cmd --profile dev
REM     .\scripts\windows\clean.cmd -p snug-cli
REM     .\scripts\windows\clean.cmd --dry-run
REM
REM Run:
REM     .\scripts\windows\clean.cmd
REM ===========================================================================
cargo clean --manifest-path "%~dp0..\..\Cargo.toml" %*
