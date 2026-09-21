@echo off
REM ===========================================================================
REM build-release.cmd — full release build pipeline for the snug Windows launcher
REM
REM Steps:
REM   1. cargo build --release -p snug-launcher
REM   2. Copy target\release\snug-launcher.exe -> bin\launcher-stub.exe
REM      (the CLI embeds the stub at compile time via include_bytes!())
REM   3. cargo build --release -p snug-cli
REM   4. target\release\snug.exe assets\snug-javafx-demo.jar
REM      (reads snug.options from CWD and produces assets\snug-javafx-demo.exe)
REM
REM Run from the repo root:
REM     .\scripts\build-release.cmd
REM
REM Flags:
REM     --SkipLauncherRebuild  Skip step 1+2 (stub unchanged from prior build)
REM     --SkipPackage          Skip step 4 (build launcher + CLI only)
REM ===========================================================================

setlocal EnableExtensions EnableDelayedExpansion

REM ---------------------------------------------------------------------------
REM 0. Environment setup
REM ---------------------------------------------------------------------------

REM Make cargo reachable. The user's local Rust install often lives outside
REM the default PATH for non-interactive shells; add it if it's missing.
if exist "%USERPROFILE%\.cargo\bin\cargo.exe" (
    set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
)
where cargo >nul 2>nul
if errorlevel 1 (
    echo [build-release] cargo not on PATH. Install Rust or set PATH to include its bin dir.
    exit /b 1
)

REM Sanity: must run from repo root (Cargo.toml next to us).
if not exist "Cargo.toml" (
    echo [build-release] Run from the repo root. Cargo.toml not found.
    exit /b 1
)

REM ---------------------------------------------------------------------------
REM Flag parsing
REM ---------------------------------------------------------------------------

set "SKIP_LAUNCHER_REBUILD=0"
set "SKIP_PACKAGE=0"

:parse_args
if "%~1"=="" goto args_done
if /i "%~1"=="--SkipLauncherRebuild" set "SKIP_LAUNCHER_REBUILD=1"
if /i "%~1"=="--SkipPackage"         set "SKIP_PACKAGE=1"
shift
goto parse_args

:args_done

set "STUB=bin\launcher-stub.exe"
set "BUILT_LAUNCHER_EXE=target\release\snug-launcher.exe"
set "BUILT_CLI_EXE=target\release\snug.exe"
set "DEMO_JAR=assets\snug-javafx-demo.jar"
set "DEMO_EXE=assets\snug-javafx-demo.exe"

echo.
echo Building release artefacts in %CD%

REM ---------------------------------------------------------------------------
REM 1. Release build of the launcher.
REM ---------------------------------------------------------------------------
if "!SKIP_LAUNCHER_REBUILD!"=="1" goto skip_launcher_rebuild

echo.
echo ==^> cargo build --release -p snug-launcher
cargo build --release -p snug-launcher
if errorlevel 1 (
    echo [build-release] cargo build --release -p snug-launcher failed with exit code %errorlevel%
    exit /b %errorlevel%
)
if not exist "%BUILT_LAUNCHER_EXE%" (
    echo [build-release] Expected launcher binary at %BUILT_LAUNCHER_EXE% but it was not produced.
    exit /b 1
)

REM 2. Refresh the committed stub. The CLI embeds this exact file via
REM    include_bytes!(); if we forget this step, the packaged EXE will
REM    use a stale stub from the previous build.
echo.
echo ==^> copy %BUILT_LAUNCHER_EXE% -^> %STUB%
copy /Y "%BUILT_LAUNCHER_EXE%" "%STUB%" >nul
if errorlevel 1 (
    echo [build-release] Failed to copy %BUILT_LAUNCHER_EXE% to %STUB%
    exit /b %errorlevel%
)

goto after_launcher_rebuild

:skip_launcher_rebuild
echo.
echo ==^> Skipping launcher rebuild + stub copy (--SkipLauncherRebuild)

:after_launcher_rebuild

REM ---------------------------------------------------------------------------
REM 3. Release build of the CLI.
REM ---------------------------------------------------------------------------

echo.
echo ==^> cargo build --release -p snug-cli
cargo build --release -p snug-cli
if errorlevel 1 (
    echo [build-release] cargo build --release -p snug-cli failed with exit code %errorlevel%
    exit /b %errorlevel%
)
if not exist "%BUILT_CLI_EXE%" (
    echo [build-release] Expected CLI binary at %BUILT_CLI_EXE% but it was not produced.
    exit /b 1
)

REM ---------------------------------------------------------------------------
REM 4. Package the demo JAR into assets\snug-javafx-demo.exe.
REM ---------------------------------------------------------------------------

if "!SKIP_PACKAGE!"=="1" goto skip_package

if not exist "%DEMO_JAR%" (
    echo [build-release] Demo JAR not found at %DEMO_JAR%. Build it first or pass --SkipPackage.
    exit /b 1
)

echo.
echo ==^> %BUILT_CLI_EXE% %DEMO_JAR%
"%BUILT_CLI_EXE%" "%DEMO_JAR%"
if errorlevel 1 (
    echo [build-release] snug package failed with exit code %errorlevel%
    exit /b %errorlevel%
)
if not exist "%DEMO_EXE%" (
    echo [build-release] Expected packaged EXE at %DEMO_EXE% but it was not produced.
    exit /b 1
)

goto after_package

:skip_package
echo.
echo ==^> Skipping packaging step (--SkipPackage)

:after_package

REM ---------------------------------------------------------------------------
REM Summary.
REM ---------------------------------------------------------------------------

echo.
echo OK
echo.
echo Sizes:
for %%I in ("%STUB%")          do echo   Launcher stub:        %STUB%          (%%~zI bytes)
for %%I in ("%BUILT_CLI_EXE%") do echo   snug CLI:             %BUILT_CLI_EXE% (%%~zI bytes)
for %%I in ("%DEMO_EXE%")      do echo   Demo EXE:             %DEMO_EXE%      (%%~zI bytes)

endlocal