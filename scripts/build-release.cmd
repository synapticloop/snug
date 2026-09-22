@echo off
REM ===========================================================================
REM build-release.cmd - full release build pipeline for the snug Windows launcher
REM
REM snug.exe embeds bin\launcher-stub.exe at compile time via include_bytes!()
REM (see crates/snug-cli/src/stub.rs). This script keeps that embed fresh and
REM proves it survived the CLI rebuild.
REM
REM Pipeline:
REM   1. (cargo | cargo zig)build --release -p snug-launcher
REM   2. copy target\...\snug-launcher.exe -> bin\launcher-stub.exe
REM      The CLI embed_bytes!()s this exact file; cargo tracks it by content,
REM      so step 3 picks up the change automatically.
REM   3. (cargo | cargo zig)build --release -p snug-cli
REM   4. target\...\snug.exe assets\snug-javafx-demo.jar
REM      Reads snug.options from CWD, writes assets\snug-javafx-demo.exe.
REM   5. Verify snug.exe contains the stub bytes we just produced
REM      (scripts\verify-embedded-stub.ps1; sha256 substring check).
REM
REM Run from the repo root:
REM     .\scripts\build-release.cmd
REM
REM Flags:
REM     --SkipLauncherRebuild   Skip steps 1+2 (stub unchanged from prior build).
REM                             --CrossCompile is also ignored in this case.
REM     --SkipPackage           Skip step 4 (build launcher + CLI only).
REM     --SkipVerify            Skip step 5 (embedded-stub sha256 sanity check).
REM     --CrossCompile          Cross-compile to x86_64-pc-windows-gnu via
REM                             cargo-zigbuild (for non-Windows dev hosts).
REM                             Requires zig, cargo-zigbuild on PATH and the
REM                             x86_64-pc-windows-gnu rust target installed.
REM     --Clean                 cargo clean -p snug-launcher -p snug-cli first.
REM ===========================================================================

setlocal EnableExtensions EnableDelayedExpansion

REM Capture the script directory before setlocal can perturb %%~dp0 in some
REM code paths. Use this everywhere instead of %%~dp0 directly.
set "_SCRIPT_DIR=%~dp0"

REM ---------------------------------------------------------------------------
REM 0. Environment setup
REM ---------------------------------------------------------------------------

if not exist "Cargo.toml" (
    echo [build-release] Run from the repo root. Cargo.toml not found.
    exit /b 1
)

REM Make cargo reachable. The user's local Rust install often lives outside
REM the default PATH for non-interactive shells; add common locations if
REM they're missing.
set "PATH=%USERPROFILE%\.cargo\bin;%USERPROFILE%\.rustup\toolchains\stable-x86_64-pc-windows-gnu\bin;%PATH%"
where cargo >nul 2>nul
if errorlevel 1 (
    echo [build-release] cargo not on PATH. Install Rust ^https://rustup.rs/^) or set PATH to include its bin dir.
    exit /b 1
)

REM ---------------------------------------------------------------------------
REM Flag parsing
REM ---------------------------------------------------------------------------

set "SKIP_LAUNCHER_REBUILD=0"
set "SKIP_PACKAGE=0"
set "SKIP_VERIFY=0"
set "CROSS_COMPILE=0"
set "CLEAN=0"

:parse_args
if "%~1"=="" goto args_done
if /i "%~1"=="--SkipLauncherRebuild" set "SKIP_LAUNCHER_REBUILD=1"
if /i "%~1"=="--SkipPackage"         set "SKIP_PACKAGE=1"
if /i "%~1"=="--SkipVerify"          set "SKIP_VERIFY=1"
if /i "%~1"=="--CrossCompile"        set "CROSS_COMPILE=1"
if /i "%~1"=="--Clean"               set "CLEAN=1"
shift
goto parse_args

:args_done

if "!CROSS_COMPILE!"=="1" if "!SKIP_LAUNCHER_REBUILD!"=="1" (
    echo [build-release] --CrossCompile is a no-op when --SkipLauncherRebuild is set.
)

set "STUB=bin\launcher-stub.exe"
set "DEMO_JAR=assets\snug-javafx-demo.jar"
set "DEMO_EXE=assets\snug-javafx-demo.exe"

if "!CROSS_COMPILE!"=="1" (
    set "TARGET_TRIPLE=x86_64-pc-windows-gnu"
    set "BUILT_LAUNCHER_EXE=target\!TARGET_TRIPLE!\release\snug-launcher.exe"
    set "BUILT_CLI_EXE=target\!TARGET_TRIPLE!\release\snug.exe"
    set "BUILD_LAUNCHER_CMD=cargo zigbuild --target !TARGET_TRIPLE! --release -p snug-launcher"
    set "BUILD_CLI_CMD=cargo zigbuild --target !TARGET_TRIPLE! --release -p snug-cli"
    where cargo-zigbuild >nul 2>nul
    if errorlevel 1 (
        echo [build-release] --CrossCompile requires cargo-zigbuild on PATH. Install with:
        echo     cargo install cargo-zigbuild --locked
        exit /b 1
    )
) else (
    set "TARGET_TRIPLE=x86_64-pc-windows-msvc"
    set "BUILT_LAUNCHER_EXE=target\release\snug-launcher.exe"
    set "BUILT_CLI_EXE=target\release\snug.exe"
    set "BUILD_LAUNCHER_CMD=cargo build --release -p snug-launcher"
    set "BUILD_CLI_CMD=cargo build --release -p snug-cli"
)

echo.
echo Building release artefacts in %CD%
echo   target triple:  !TARGET_TRIPLE!
echo   cross-compile:  !CROSS_COMPILE!

if "!CLEAN!"=="1" (
    echo.
    echo ==^> cargo clean -p snug-launcher -p snug-cli
    cargo clean -p snug-launcher -p snug-cli
    if errorlevel 1 (
        echo [build-release] cargo clean failed with exit code %errorlevel%
        exit /b %errorlevel%
    )
)

REM ---------------------------------------------------------------------------
REM 1. Release build of the launcher.
REM ---------------------------------------------------------------------------
if "!SKIP_LAUNCHER_REBUILD!"=="1" goto skip_launcher_rebuild

echo.
echo ==^> !BUILD_LAUNCHER_CMD!
call !BUILD_LAUNCHER_CMD!
if errorlevel 1 (
    echo [build-release] launcher build failed with exit code %errorlevel%
    exit /b %errorlevel%
)
if not exist "!BUILT_LAUNCHER_EXE!" (
    echo [build-release] Expected launcher binary at !BUILT_LAUNCHER_EXE! but it was not produced.
    exit /b 1
)

REM 2. Refresh the committed stub. The CLI embed_bytes!()s this exact file;
REM    cargo tracks it by content hash, so step 3 sees the change without
REM    any extra wiring.
echo.
echo ==^> copy !BUILT_LAUNCHER_EXE! -^> !STUB!
copy /Y "!BUILT_LAUNCHER_EXE!" "!STUB!" >nul
if errorlevel 1 (
    echo [build-release] Failed to copy !BUILT_LAUNCHER_EXE! to !STUB!
    exit /b %errorlevel%
)

goto after_launcher_rebuild

:skip_launcher_rebuild
echo.
echo ==^> Skipping launcher rebuild + stub copy ^(--SkipLauncherRebuild^)

:after_launcher_rebuild

REM ---------------------------------------------------------------------------
REM 3. Release build of the CLI.
REM ---------------------------------------------------------------------------

echo.
echo ==^> !BUILD_CLI_CMD!
call !BUILD_CLI_CMD!
if errorlevel 1 (
    echo [build-release] CLI build failed with exit code %errorlevel%
    exit /b %errorlevel%
)
if not exist "!BUILT_CLI_EXE!" (
    echo [build-release] Expected CLI binary at !BUILT_CLI_EXE! but it was not produced.
    exit /b 1
)

REM ---------------------------------------------------------------------------
REM 4. Package the demo JAR into assets\snug-javafx-demo.exe.
REM ---------------------------------------------------------------------------

if "!SKIP_PACKAGE!"=="1" goto skip_package

if not exist "!DEMO_JAR!" (
    echo [build-release] Demo JAR not found at !DEMO_JAR!. Build it first or pass --SkipPackage.
    exit /b 1
)

echo.
echo ==^> !BUILT_CLI_EXE! !DEMO_JAR!
"!BUILT_CLI_EXE!" "!DEMO_JAR!"
if errorlevel 1 (
    echo [build-release] snug package failed with exit code %errorlevel%
    exit /b %errorlevel%
)
if not exist "!DEMO_EXE!" (
    echo [build-release] Expected packaged EXE at !DEMO_EXE! but it was not produced.
    exit /b 1
)

goto after_package

:skip_package
echo.
echo ==^> Skipping packaging step ^(--SkipPackage^)

:after_package

REM ---------------------------------------------------------------------------
REM 5. Verify snug.exe contains the stub bytes (sha256 substring match).
REM ---------------------------------------------------------------------------

if "!SKIP_VERIFY!"=="1" goto skip_verify

echo.
echo ==^> Verifying !BUILT_CLI_EXE! contains !STUB! bytes

powershell -NoProfile -ExecutionPolicy Bypass -File "!_SCRIPT_DIR!verify-embedded-stub.ps1" -StubPath "!STUB!" -ExePath "!BUILT_CLI_EXE!"
if errorlevel 1 (
    echo [build-release] embedded-stub verification failed - the CLI was rebuilt against a stale stub.
    exit /b 1
)

goto after_verify

:skip_verify
echo.
echo ==^> Skipping embedded-stub verification ^(--SkipVerify^)

:after_verify

REM ---------------------------------------------------------------------------
REM Summary.
REM ---------------------------------------------------------------------------

echo.
echo OK
echo.
echo Sizes:
for %%I in ("!STUB!")          do echo   Launcher stub:        !STUB!          ^(%%~zI bytes^)
for %%I in ("!BUILT_CLI_EXE!") do echo   snug CLI:             !BUILT_CLI_EXE! ^(%%~zI bytes^)
for %%I in ("!DEMO_EXE!")      do echo   Demo EXE:             !DEMO_EXE!      ^(%%~zI bytes^)

endlocal
