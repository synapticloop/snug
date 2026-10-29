@echo off
REM ===========================================================================
REM clean.cmd - reclaim disk space from Rust build intermediates.
REM
REM target\ is the largest thing on a snug working copy. Cargo keeps every
REM dependency's compiled form, every incremental compilation unit and every
REM build-script output, and it never throws any of it away on its own.
REM Measured on this workstation: target\ 5.4 GB, of which target\debug\
REM alone was 4.3 GB (deps 2.1 GB, incremental 1.8 GB, build 0.3 GB).
REM
REM Modes - exactly one:
REM
REM   (no flag)   cargo clean. The whole of target\. Reclaims 5.3 GiB.
REM   --Debug     target\debug\ only, via `cargo clean --profile dev`.
REM                Reclaims 4.2 GiB and leaves target\release\ alone, so an
REM                already-built snug.exe survives.
REM   --Release   target\release\ only, via `cargo clean --profile release`.
REM                Reclaims 1.0 GiB. This is the cargo RELEASE PROFILE - the
REM                release\ STAGING FOLDER is --Staging, not this.
REM   --Packages  `cargo clean -p snug-launcher -p snug-cli -p snug-dropper`.
REM                Reclaims 2.6 GiB. Same granularity as build.cmd --Clean,
REM                except this one also reports the space it recovered.
REM   --Staging   The release\ tree and the gitignored demo outputs in
REM                assets\. Does NOT touch target\ at all.
REM   --All       target\ AND the staging tree.
REM
REM Other flags:
REM
REM   --DryRun    Report what would go and how much space it is. Delete
REM                nothing. Combine with any mode.
REM   --Help      This text.
REM
REM What it will never touch:
REM
REM   bin\launcher-stub-*.exe   Tracked in git AND include_bytes!()-ed by
REM                               snug-cli. Deleting it breaks the CLI build
REM                               and dirties the tree.
REM   release\.gitignore         The one tracked file under release\. It is a
REM                               file, so the directory sweep below cannot
REM                               reach it.
REM   assets\*.jar               Tracked SOURCE assets (the demo JARs), as
REM                               opposed to the .exe and .app beside them,
REM                               which are build outputs. The extension
REM                               split is what makes that glob safe.
REM   Anything git tracks.       The staging pass asks git about every path
REM                               it is about to remove, and aborts the whole
REM                               operation before removing anything if the
REM                               answer is yes.
REM
REM snug.build.log is deliberately left alone: it is a couple of hundred
REM bytes of diagnostics, not bulk, and it is often the thing you wanted to
REM read.
REM
REM If a build is running, cargo clean waits for the target directory lock
REM and says so ("Blocking waiting for file lock on build directory"). That
REM is cargo being correct - cleaning under a running build would race it -
REM not a hang to kill.
REM
REM Run:
REM     .\scripts\windows\clean.cmd
REM     .\scripts\windows\clean.cmd --Debug
REM     .\scripts\windows\clean.cmd --All --DryRun
REM
REM The script anchors itself to the repo root from its own location, so
REM double-clicking it in Explorer works. build.cmd cannot do that: it reads
REM snug.options relative to the working directory, so it has to be invoked
REM from the repo root.
REM ===========================================================================

REM ===========================================================================
REM The subroutines live ABOVE the main entry point, and that is not a style
REM choice. `call :label` looks the label up from the current position in the
REM file first, and in this script - which is `goto`-navigated before it ever
REM reaches a `call` - the search fails for a label that lives further down
REM the file. It fails with "The system cannot find the batch label specified
REM - measure" and the subroutine simply never runs, so the size report comes
REM back empty and the reclaimed figure is garbage.
REM
REM Putting the subroutines first and jumping over them with `goto main`
REM makes every lookup resolve on the first try. If you move :measure,
REM :delete_file or :check_tracked below :main, this bug comes straight back
REM - it reproduces reliably on this machine, and the symptom is a silently
REM wrong summary rather than a crash.
REM
REM Second trap, same family: do NOT put a backtick command inside a
REM parenthesised block. cmd tracks quote state across the block, a
REM backtick region with quotes in it unbalances that state, and the block
REM swallows the lines after it - which shows up much later as a label that
REM cannot be found. `for /f "usebackq" in (`...`)` is fine at the top level
REM (:measure does it) and fatal inside a block (:check_tracked used to, and
REM broke `goto parse_args` three labels further down).
REM
REM Third trap, and the subtlest: inside a `call`ed subroutine, use `!var!`,
REM not `%var%`, for anything the subroutine sets itself. cmd expands the
REM subroutine's `%var%` references before the `set` that defines them has
REM run, so `set "_X=%~1"` followed by `del "%_X%"` deletes a file literally
REM named `_X`. The failure is quiet - `del` says "Could Not Find" and the
REM script carries on. :delete_file uses `!_FILE_REL!` throughout.
REM ===========================================================================

goto main

REM ---------------------------------------------------------------------------
REM Measure a tree, in whole megabytes, into _SIZE_MB. A missing path is 0.
REM
REM Whole megabytes because the subtraction in the summary runs in `set /a`,
REM which is 32-bit and cannot hold the byte count of a 5 GB target\.
REM ---------------------------------------------------------------------------
:measure
set "_SIZE_MB=0"
if "%~1"=="" goto measure_end
if not exist "%~1" goto measure_end
for /f "usebackq delims=" %%s in (`powershell -NoProfile -ExecutionPolicy Bypass -File "%_SCRIPT_DIR%dir-size.ps1" "%~1"`) do set "_SIZE_MB=%%s"
:measure_end
exit /b 0

REM ---------------------------------------------------------------------------
REM Refuse the whole staging operation if anything it is about to remove is
REM tracked by git. Returns 1 if it is, 0 if it is safe to proceed.
REM
REM This runs BEFORE anything is deleted, not per file as the deletion loop
REM goes. A per-file check would still protect the tracked file itself, but
REM it would fire only after the paths listed above it had already been
REM removed - the script would report a clean refusal having half done the
REM job.
REM
REM Two shapes of check, because there are two shapes of deletion:
REM   * a FILE is unlinked, so the file itself has to be untracked;
REM   * a DIRECTORY is removed recursively, so ANY tracked file underneath it
REM     matters, however deep.
REM ---------------------------------------------------------------------------
:check_tracked
for /d %%D in ("release\*") do (
    if /i not "%%~nxD"==".git" if /i not "%%~nxD"==".gitignore" (
        git ls-files --error-unmatch -- "release/%%~nxD/" >nul 2>nul
        if not errorlevel 1 (
            echo [clean] REFUSING to remove release\%%~nxD - it contains tracked files.
            exit /b 1
        )
    )
)

for %%F in (assets\snug-javafx-demo*.exe assets\*.app) do (
    if exist "%%~fF" (
        git ls-files --error-unmatch -- "assets/%%~nxF" >nul 2>nul
        if not errorlevel 1 (
            echo [clean] REFUSING to remove a tracked file: assets\%%~nxF
            exit /b 1
        )
    )
)

if exist "snug-javafx-demo.exe" (
    git ls-files --error-unmatch -- "snug-javafx-demo.exe" >nul 2>nul
    if not errorlevel 1 (
        echo [clean] REFUSING to remove a tracked file: snug-javafx-demo.exe
        exit /b 1
    )
)
exit /b 0

REM ---------------------------------------------------------------------------
REM Delete one file. %1 = path relative to the repo root, forward slashes
REM (that is the form git pathspecs want).
REM ---------------------------------------------------------------------------
:delete_file
set "_FILE_GIT=%~1"
REM del wants the Windows form. `del "assets/x.exe"` fails with "The system
REM cannot find the path specified" because del does not read forward slashes
REM as separators - it needs the backslash form.
set "_FILE_WIN=!_FILE_GIT:/=\!"
if not exist "!_FILE_WIN!" goto delete_file_end

echo     !_FILE_WIN!
if "!DRY_RUN!"=="1" (
    echo       would remove
) else (
    del /f /q "!_FILE_WIN!"
    REM Check by existence, not by exit code: `del` on a missing file prints
    REM "Could Not Find" and the question that matters is whether the file
    REM is still there afterwards.
    if exist "!_FILE_WIN!" (
        echo [clean] Failed to remove !_FILE_WIN!
        set "FAILURES=1"
    )
)

:delete_file_end
exit /b 0

REM ===========================================================================
REM Main
REM ===========================================================================

:main
setlocal EnableExtensions EnableDelayedExpansion

set "_SCRIPT_DIR=%~dp0"

REM scripts\windows\ -> repo root. %%~fI resolves the ..\.. for us.
for %%I in ("%_SCRIPT_DIR%..\..") do set "_REPO_ROOT=%%~fI"

cd /d "%_REPO_ROOT%"
if errorlevel 1 (
    echo [clean] Could not enter the repo root at %_REPO_ROOT%
    exit /b 1
)

REM Guard: this must be the snug repo, not just any cargo workspace. A clean
REM script that deletes 5 GB is worth one extra check.
if not exist "Cargo.toml" (
    echo [clean] Cargo.toml not found in %_REPO_ROOT% - not a cargo workspace.
    exit /b 1
)
if not exist "crates\snug-cli\Cargo.toml" (
    echo [clean] crates\snug-cli\Cargo.toml not found - this does not look like the snug repo.
    exit /b 1
)

REM cargo on PATH. The user's Rust install often lives outside the default
REM PATH for non-interactive shells, so add the usual location.
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
where cargo >nul 2>nul
if errorlevel 1 (
    echo [clean] cargo not on PATH. Install Rust ^https://rustup.rs/^) or add its bin dir to PATH.
    exit /b 1
)

REM git is optional: it only strengthens the staging pass by refusing to
REM delete anything it tracks.
set "GIT_AVAILABLE=0"
where git >nul 2>nul
if not errorlevel 1 set "GIT_AVAILABLE=1"

REM ---------------------------------------------------------------------------
REM Flag parsing
REM ---------------------------------------------------------------------------

set "MODE=full"
set "DRY_RUN=0"

:parse_args
if "%~1"=="" goto args_done
if /i "%~1"=="--Debug"    set "MODE=debug"
if /i "%~1"=="--Release"  set "MODE=release"
if /i "%~1"=="--Packages" set "MODE=packages"
if /i "%~1"=="--Staging"  set "MODE=staging"
if /i "%~1"=="--All"      set "MODE=all"
if /i "%~1"=="--DryRun"   set "DRY_RUN=1"
if /i "%~1"=="--Help"     goto usage
if /i "%~1"=="/?"         goto usage
shift
goto parse_args

:args_done

set "DO_CARGO=1"
set "DO_STAGING=0"
if "!MODE!"=="staging" set "DO_CARGO=0"
if "!MODE!"=="staging" set "DO_STAGING=1"
if "!MODE!"=="all" set "DO_STAGING=1"

set "CARGO_CLEAN_ARGS="
if "!MODE!"=="debug"    set "CARGO_CLEAN_ARGS=--profile dev"
if "!MODE!"=="release"  set "CARGO_CLEAN_ARGS=--profile release"
if "!MODE!"=="packages" set "CARGO_CLEAN_ARGS=-p snug-launcher -p snug-cli -p snug-dropper"

echo.
echo [clean] repo:  %_REPO_ROOT%
echo [clean] mode:  !MODE!
if "!DRY_RUN!"=="1" echo [clean] DRY RUN - nothing will be deleted.
if "!GIT_AVAILABLE!"=="0" echo [clean] WARNING: git not on PATH - the tracked-file guard is off.

set "TARGET_BEFORE=0"
set "TARGET_AFTER=0"
set "STAGING_BEFORE=0"
set "STAGING_AFTER=0"
set "FAILURES=0"

REM ---------------------------------------------------------------------------
REM 1. Validate, before anything is removed.
REM
REM This runs first, and for --All it runs before the cargo clean too. The
REM rule the script is selling is "it will never delete anything git
REM tracks", and that has to hold for the whole operation - not just for
REM the file the guard happens to notice last.
REM ---------------------------------------------------------------------------

if "!DO_STAGING!"=="0" goto validated
if "!GIT_AVAILABLE!"=="0" goto validated
call :check_tracked
if errorlevel 1 exit /b 1

:validated

REM ---------------------------------------------------------------------------
REM 2. Cargo intermediates
REM ---------------------------------------------------------------------------

if "!DO_CARGO!"=="1" goto do_cargo
goto after_cargo

:do_cargo
call :measure "target"
set "TARGET_BEFORE=!_SIZE_MB!"

echo.
echo ==^> cargo clean !CARGO_CLEAN_ARGS!

if "!DRY_RUN!"=="1" (
    cargo clean !CARGO_CLEAN_ARGS! --dry-run
) else (
    cargo clean !CARGO_CLEAN_ARGS!
)
if errorlevel 1 (
    echo [clean] cargo clean failed with exit code %errorlevel%
    exit /b %errorlevel%
)

:after_cargo
if "!DO_CARGO!"=="0" goto cargo_done
call :measure "target"
set "TARGET_AFTER=!_SIZE_MB!"

:cargo_done

REM ---------------------------------------------------------------------------
REM 3. Staging tree: release\ plus the gitignored demo outputs in assets\.
REM ---------------------------------------------------------------------------

if "!DO_STAGING!"=="1" goto do_staging
goto after_staging

:do_staging
call :measure "release"
set "STAGING_BEFORE=!_SIZE_MB!"

echo.
echo ==^> Staging tree

REM release\: every subdirectory is a build output. release\.gitignore is
REM the one tracked file in there, and it is a FILE, so `for /d` - which
REM matches directories only - cannot reach it. The name test below is a
REM second layer, not the first. :check_tracked has already established that
REM no tracked file lives underneath any of these directories.
for /d %%D in ("release\*") do (
    if /i not "%%~nxD"==".git" if /i not "%%~nxD"==".gitignore" (
        echo     release\%%~nxD
        if "!DRY_RUN!"=="1" (
            echo       would remove
        ) else (
            rmdir /s /q "%%~fD"
            if exist "%%~fD" (
                echo [clean] Failed to remove %%~fD
                set "FAILURES=1"
            )
        )
    )
)

REM assets\ and the repo root: the tracked files there are .png, .xml and
REM .jar. The build outputs are the .exe and .app beside them, and that
REM extension split is what makes these globs safe - the demo .jar is a
REM SOURCE asset that build.cmd copies into release\, it does not produce.
for %%F in (assets\snug-javafx-demo*.exe assets\*.app) do (
    if exist "%%~fF" (
        call :delete_file "assets/%%~nxF"
        if errorlevel 1 exit /b 1
    )
)

REM The same file name at the repo root, which .gitignore lists separately.
if exist "snug-javafx-demo.exe" (
    call :delete_file "snug-javafx-demo.exe"
    if errorlevel 1 exit /b 1
)

:after_staging
if "!DO_STAGING!"=="0" goto staging_done
call :measure "release"
set "STAGING_AFTER=!_SIZE_MB!"

:staging_done

REM ---------------------------------------------------------------------------
REM 4. Summary
REM ---------------------------------------------------------------------------

set /a "_TARGET_DELTA=!TARGET_BEFORE! - !TARGET_AFTER!"
set /a "_STAGING_DELTA=!STAGING_BEFORE! - !STAGING_AFTER!"

echo.
echo [clean] Summary
if "!DO_CARGO!"=="1" echo     target\    !TARGET_BEFORE! MB -^> !TARGET_AFTER! MB   (reclaimed !_TARGET_DELTA! MB)
if "!DO_STAGING!"=="1" echo     release\   !STAGING_BEFORE! MB -^> !STAGING_AFTER! MB   (reclaimed !_STAGING_DELTA! MB)

if "!FAILURES!"=="1" (
    echo [clean] One or more paths could not be removed - see above.
    exit /b 1
)

if "!DRY_RUN!"=="1" (
    echo [clean] Dry run complete. Nothing was deleted.
    exit /b 0
)

echo [clean] done.
exit /b 0

REM ---------------------------------------------------------------------------
REM Usage
REM ---------------------------------------------------------------------------
:usage
echo.
echo clean.cmd - reclaim disk space from Rust build intermediates.
echo.
echo Usage:
echo     scripts\windows\clean.cmd [mode] [--DryRun]
echo.
echo Modes (one only):
echo     (no flag)   cargo clean. The whole of target\. Reclaims 5.3 GiB.
echo     --Debug     target\debug\ only. Reclaims 4.2 GiB, keeps target\release\.
echo     --Release   target\release\ only. Reclaims 1.0 GiB.
echo     --Packages  The three workspace crates only. Reclaims 2.6 GiB.
echo     --Staging   release\ tree and the gitignored assets\ outputs.
echo     --All       target\ and the staging tree.
echo.
echo Other flags:
echo     --DryRun    Report what would go and how much space it is. Delete nothing.
echo     --Help      This text.
echo.
echo It never touches bin\launcher-stub-*.exe, release\.gitignore, the tracked
echo assets\*.jar source assets, or anything else git tracks.
exit /b 0
