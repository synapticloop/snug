@echo off
REM ===========================================================================
REM release.cmd - run the gates CI runs, then build the Windows release
REM artefact.
REM
REM Chains scripts\windows\check.ps1 (gates: cargo fmt --all --check,
REM cargo clippy --workspace --all-targets -- -D warnings, cargo test
REM --workspace --lib --bins --tests --no-fail-fast) and
REM scripts\windows\build.cmd (the 10-step release pipeline: launcher stub
REM refresh, snug-cli, demo JAR package, embedded-stub verify, snug.exe icon
REM stamp, snug_preview, dropper, icon verify, stage into
REM release\windows-x86_64\).
REM
REM Why a wrapper rather than baking the gates into build.cmd: the two
REM scripts serve different concerns. build.cmd produces the shipping
REM artefact and must succeed even when the tree is red - a developer
REM investigating a failing test sometimes wants the build to land in
REM release\ so they can poke at it. check.ps1 is the local loop for "the
REM tree is green"; it has different exit semantics (--no-fail-fast on the
REM test run, per-gate exit codes) and a different output shape (per-gate
REM pass/fail lines). Mixing them would lose information either way. See
REM scripts\windows\check.ps1:13-22 for the rationale in the source.
REM
REM Usage:
REM     .\scripts\windows\release.cmd
REM
REM Flags:
REM     --SkipCheck        Skip check.ps1 and go straight to build.cmd.
REM                         Useful when you want the artefact for diagnosis
REM                         despite a failing gate.
REM     --CheckOnly        Run check.ps1 and stop. The opposite of --SkipCheck:
REM                         verify the tree is green without spending the
REM                         build budget.
REM
REM Any other flags (--SkipLauncherRebuild, --SkipPackage, --SkipVerify,
REM --SkipDevTools, --SkipDropper, --SkipRelease, --CrossCompile, --Clean,
REM -Format) are forwarded to check.ps1 / build.cmd unchanged.
REM ===========================================================================

setlocal EnableExtensions EnableDelayedExpansion

REM Capture the script directory before setlocal can perturb %%~dp0 in some
REM code paths. Use this everywhere instead of %%~dp0 directly.
set "_SCRIPT_DIR=%~dp0"

REM ---------------------------------------------------------------------------
REM 0. Environment setup
REM ---------------------------------------------------------------------------

if not exist "Cargo.toml" (
    echo [release] Run from the repo root. Cargo.toml not found.
    exit /b 1
)

REM ---------------------------------------------------------------------------
REM Flag parsing
REM ---------------------------------------------------------------------------

set "SKIP_CHECK=0"
set "CHECK_ONLY=0"
set "FORWARDED_FLAGS="

:parse_args
if "%~1"=="" goto args_done
if /i "%~1"=="--SkipCheck"  set "SKIP_CHECK=1"
if /i "%~1"=="--CheckOnly"  set "CHECK_ONLY=1"
REM Anything else is forwarded. Quote-stripped so build.cmd can parse it the
REM same way it does when invoked directly.
if /i not "%~1"=="--SkipCheck" if /i not "%~1"=="--CheckOnly" set "FORWARDED_FLAGS=!FORWARDED_FLAGS! %~1"
shift
goto parse_args

:args_done

if "!SKIP_CHECK!"=="1" if "!CHECK_ONLY!"=="1" (
    echo [release] --SkipCheck and --CheckOnly are mutually exclusive.
    exit /b 1
)

REM ---------------------------------------------------------------------------
REM 1. Gates: fmt, clippy -D warnings, cargo test --no-fail-fast
REM ---------------------------------------------------------------------------

if "!SKIP_CHECK!"=="1" goto skip_check

echo.
echo ==^> scripts\windows\check.ps1 !FORWARDED_FLAGS!
powershell -NoProfile -ExecutionPolicy Bypass -File "!_SCRIPT_DIR!check.ps1" !FORWARDED_FLAGS!
if errorlevel 1 (
    echo [release] check.ps1 failed with exit code %errorlevel% - aborting before the build.
    exit /b %errorlevel%
)

:skip_check

if "!CHECK_ONLY!"=="1" (
    echo.
    echo ==^> --CheckOnly set; skipping build.cmd.
    exit /b 0
)

REM ---------------------------------------------------------------------------
REM 2. Release pipeline: launcher stub, CLI, demo, verify, icons, stage
REM ---------------------------------------------------------------------------

echo.
echo ==^> scripts\windows\build.cmd !FORWARDED_FLAGS!
call "!_SCRIPT_DIR!build.cmd" !FORWARDED_FLAGS!
if errorlevel 1 (
    echo [release] build.cmd failed with exit code %errorlevel%
    exit /b %errorlevel%
)

echo.
echo [release] done.