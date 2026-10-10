@echo off
REM ===========================================================================
REM build-windows.cmd - full release build pipeline for the snug Windows
REM launcher, dropper and dialog preview.
REM
REM snug.exe embeds bin\launcher-stub-windows-x86_64.exe at compile time
REM via include_bytes!() (see crates\snug-cli\src\stub.rs). This script keeps
REM that embed fresh and proves it survived the CLI rebuild.
REM
REM The product is Windows x86_64 binaries and nothing else. There is no
REM cross-artefact build path: the launcher stub must be linked by a Windows
REM toolchain, because `embed-resource` finds no resource compiler anywhere
REM else and silently produces an iconless binary. See "Build host" in
REM AGENTS.md.
REM
REM Pipeline:
REM   1. (cargo | cargo zig)build --release -p snug-launcher
REM   2. copy target\...\snug-launcher.exe -> bin\launcher-stub-windows-x86_64.exe
REM      The CLI embed_bytes!()s this exact file; cargo tracks it by content,
REM      so step 3 picks up the change automatically.
REM   3. (cargo | cargo zig)build --release -p snug-cli --bin snug --bin verify_icons
REM      verify_icons is built here rather than in step 9 so that the icon
REM      check is a plain existence test, not a nested cargo invocation in
REM      the middle of a verification step. It is a build tool, not a
REM      runtime dependency, and adds nothing to the shipped snug.exe.
REM   4. target\...\snug.exe assets\snug-javafx-demo-windows.jar
REM      Reads snug.options for the shared metadata, then snug.windows.options
REM      for the platform-specific --output, and writes
REM      assets\snug-javafx-demo.exe. The exe dir (target\release\) holds no
REM      options file, so the CWD copies are the ones read.
REM   5. Verify snug.exe contains the stub bytes we just produced
REM      (scripts\verify-embedded-stub.ps1; sha256 substring check).
REM   6. Stamp assets\snug-runner.png into snug.exe. UNCONDITIONAL - this is
REM      the primary shipping artefact, and it has no build.rs and no
REM      dependency on snug-launcher, so nothing else gives it an icon.
REM   7. Build snug_preview + stamp assets\snug-preview.png into it. The
REM      only step --SkipDevTools turns off; snug_preview.exe is how you look
REM      at snug's dialogs without building an app, and it ships anyway.
REM   8. Build the dropper, stamp assets\snug-dropper.png, and emit
REM      target\...\Build with Snug.exe beside snug.exe. The dropper's whole
REM      interface is its icon, so this is a shipped step with its own flag
REM      rather than something hidden behind --SkipDevTools.
REM   9. Verify each EXE's MAINICON really is the PNG it was stamped with
REM      (verify_icons; pixel-exact, plus a check for orphan RT_ICON entries
REM      left behind by an earlier stamp).
REM  10. Stage release\: the EXEs a user runs (snug, Build with Snug, and
REM      the snug_preview dialog preview) plus the demo JAR, so someone can
REM      try the whole drop-a-JAR flow before building a JAR of their own.
REM      They land in release\windows-<arch>\ - the same <os>-<arch>
REM      convention build-macos.sh stages macos-arm64/ and macos-x86_64/
REM      into. release\ ignores its own contents, so nothing here is
REM      tracked. Skip with --SkipRelease.
REM
REM Run from the repo root:
REM     .\scripts\build-windows.cmd
REM
REM Flags:
REM     --SkipLauncherRebuild   Skip steps 1+2 (stub unchanged from prior build).
REM                             --CrossCompile is also ignored in this case.
REM     --SkipPackage           Skip step 4 (build launcher + CLI only).
REM     --SkipVerify            Skip steps 5 and 9 (the embedded-stub sha256
REM                             check and the icon provenance check). Both are
REM                             post-build assertions over artefacts that are
REM                             otherwise perfectly well formed, so this is for
REM                             when you are debugging the checks themselves.
REM     --SkipDevTools          Skip step 7 (the snug_preview dialog preview).
REM                             It deliberately does NOT skip the snug.exe
REM                             icon stamp in step 6, which is a shipping
REM                             concern: snug-cli has no build.rs, so a
REM                             --SkipDevTools run would otherwise ship a
REM                             snug.exe with no icon at all.
REM     --SkipDropper           Skip step 8 (the Build with Snug beginner
REM                             shim). It is user-facing, so this is opt-out
REM                             rather than implied by --SkipDevTools.
REM     --SkipRelease           Skip step 10 (copy the shipping artefacts into
REM                             release\). The folder is regenerated in place,
REM                             so stale files from a previous run are NOT
REM     --CrossCompile          Cross-compile to x86_64-pc-windows-gnu via
REM                             cargo-zigbuild. Same architecture, different
REM                             toolchain - for inspecting a GNU-linked build,
REM                             not a route to a second architecture. Nothing
REM                             in a release depends on it.
REM                             Requires zig, cargo-zigbuild on PATH and the
REM                             x86_64-pc-windows-gnu rust target installed.
REM     --Clean                 cargo clean -p snug-launcher -p snug-cli
REM                             -p snug-dropper first. Does NOT empty
REM                             release\; stage it into a fresh folder if
REM                             you need a guaranteed-clean release. Note
REM                             that artefacts staged flat in release\ by
REM                             a pre-<os>-<arch> run are left where they
REM                             are; delete them by hand once.
REM ===========================================================================

setlocal EnableExtensions EnableDelayedExpansion

REM Capture the script directory before setlocal can perturb %%~dp0 in some
REM code paths. Use this everywhere instead of %%~dp0 directly.
set "_SCRIPT_DIR=%~dp0"

REM ---------------------------------------------------------------------------
REM 0. Environment setup
REM ---------------------------------------------------------------------------

if not exist "Cargo.toml" (
    echo [build-windows] Run from the repo root. Cargo.toml not found.
    exit /b 1
)

REM Make cargo reachable. The user's local Rust install often lives outside
REM the default PATH for non-interactive shells; add common locations if
REM they're missing.
set "PATH=%USERPROFILE%\.cargo\bin;%USERPROFILE%\.rustup\toolchains\stable-x86_64-pc-windows-gnu\bin;%PATH%"
where cargo >nul 2>nul
if errorlevel 1 (
    echo [build-windows] cargo not on PATH. Install Rust ^https://rustup.rs/^) or set PATH to include its bin dir.
    exit /b 1
)

REM ---------------------------------------------------------------------------
REM Flag parsing
REM ---------------------------------------------------------------------------

set "SKIP_LAUNCHER_REBUILD=0"
set "SKIP_PACKAGE=0"
set "SKIP_VERIFY=0"
set "SKIP_DEV_TOOLS=0"
set "SKIP_DROPPER=0"
set "SKIP_RELEASE=0"
set "CROSS_COMPILE=0"
set "CLEAN=0"

:parse_args
if "%~1"=="" goto args_done
if /i "%~1"=="--SkipLauncherRebuild" set "SKIP_LAUNCHER_REBUILD=1"
if /i "%~1"=="--SkipPackage"         set "SKIP_PACKAGE=1"
if /i "%~1"=="--SkipVerify"          set "SKIP_VERIFY=1"
if /i "%~1"=="--SkipDevTools"        set "SKIP_DEV_TOOLS=1"
if /i "%~1"=="--SkipDropper"         set "SKIP_DROPPER=1"
if /i "%~1"=="--SkipRelease"         set "SKIP_RELEASE=1"
if /i "%~1"=="--CrossCompile"        set "CROSS_COMPILE=1"
if /i "%~1"=="--Clean"               set "CLEAN=1"
shift
goto parse_args

:args_done

if "!CROSS_COMPILE!"=="1" if "!SKIP_LAUNCHER_REBUILD!"=="1" (
    echo [build-windows] --CrossCompile is a no-op when --SkipLauncherRebuild is set.
)

set "DEMO_JAR=assets\snug-javafx-demo-windows.jar"
set "DEMO_EXE=assets\snug-javafx-demo.exe"
set "PREVIEW_PNG=assets\snug-preview.png"
set "SNUG_CLI_PNG=assets\snug-runner.png"
set "DROPPER_PNG=assets\snug-dropper.png"
set "DROPPER_SHIPPED=Build with Snug.exe"
set "RELEASE_DIR=release"

REM ---------------------------------------------------------------------------
REM Every cargo call below goes through scripts\cargo-repro.ps1, which sets
REM CARGO_ENCODED_RUSTFLAGS for the reproducible-build remapping. See
REM scripts\reproducible-build.ps1 for why the launcher is not reproducible
REM across checkouts without it, and why the flag separator (\x1f) means this
REM script cannot set the variable itself.
REM
REM The flags must match what scripts\refresh-stub.ps1 uses, or the CI stub
REM check fails on a correctly-refreshed stub.
REM ---------------------------------------------------------------------------

if "!CROSS_COMPILE!"=="1" (
    set "TARGET_TRIPLE=x86_64-pc-windows-gnu"
    set "TARGET_ARCH=x86_64"
    set "BUILT_LAUNCHER_EXE=target\!TARGET_TRIPLE!\release\snug-launcher.exe"
    set "BUILT_CLI_EXE=target\!TARGET_TRIPLE!\release\snug.exe"
    set "BUILT_PREVIEW_EXE=target\!TARGET_TRIPLE!\release\snug_preview.exe"
    set "BUILT_STAMP_EXE=target\!TARGET_TRIPLE!\release\stamp_preview_icon.exe"
    set "BUILT_VERIFY_EXE=target\!TARGET_TRIPLE!\release\verify_icons.exe"
    set "CARGO_LAUNCHER_ARGS=zigbuild --target !TARGET_TRIPLE! --release -p snug-launcher"
    set "CARGO_CLI_ARGS=zigbuild --target !TARGET_TRIPLE! --release -p snug-cli --bin snug --bin verify_icons"
    set "CARGO_STAMP_ARGS=zigbuild --target !TARGET_TRIPLE! --release -p snug-launcher --bin stamp_preview_icon"
    set "CARGO_PREVIEW_ARGS=zigbuild --target !TARGET_TRIPLE! --release -p snug-launcher --bin snug_preview"
    set "BUILT_DROPPER_EXE=target\!TARGET_TRIPLE!\release\snug-dropper.exe"
    set "BUILT_DROPPER_STAMP_EXE=target\!TARGET_TRIPLE!\release\stamp_dropper_icon.exe"
    set "CARGO_DROPPER_ARGS=zigbuild --target !TARGET_TRIPLE! --release -p snug-dropper"
    where cargo-zigbuild >nul 2>nul
    if errorlevel 1 (
        echo [build-windows] --CrossCompile requires cargo-zigbuild on PATH. Install with:
        echo     cargo install cargo-zigbuild --locked
        exit /b 1
    )
) else (
    set "TARGET_TRIPLE=x86_64-pc-windows-msvc"
    set "TARGET_ARCH=x86_64"
    set "BUILT_LAUNCHER_EXE=target\release\snug-launcher.exe"
    set "BUILT_CLI_EXE=target\release\snug.exe"
    set "BUILT_PREVIEW_EXE=target\release\snug_preview.exe"
    set "BUILT_STAMP_EXE=target\release\stamp_preview_icon.exe"
    set "BUILT_VERIFY_EXE=target\release\verify_icons.exe"
    set "CARGO_LAUNCHER_ARGS=build --release -p snug-launcher"
    set "CARGO_CLI_ARGS=build --release -p snug-cli --bin snug --bin verify_icons"
    set "CARGO_STAMP_ARGS=build --release -p snug-launcher --bin stamp_preview_icon"
    set "CARGO_PREVIEW_ARGS=build --release -p snug-launcher --bin snug_preview"
    set "BUILT_DROPPER_EXE=target\release\snug-dropper.exe"
    set "BUILT_DROPPER_STAMP_EXE=target\release\stamp_dropper_icon.exe"
    REM No --bin here on purpose. snug-dropper has two bin targets: the
    REM shipped shim (src\main.rs, the `[[bin]]`) and the post-build stamp
    REM helper (src\bin\stamp_dropper_icon.rs, auto-discovered). Naming
    REM only the helper builds only the helper, so BUILT_DROPPER_EXE was
    REM never produced and this step exited 1 on any clean tree -- masked
    REM locally by a stale target\release\snug-dropper.exe, never masked
    REM in CI, where every earlier build is the debug profile. `-p` with no
    REM --bin builds both, which is the set the checks below expect.
    set "CARGO_DROPPER_ARGS=build --release -p snug-dropper"
)

REM Derived *after* the triple is chosen, because with delayed expansion
REM `!TARGET_ARCH!` expands where it is written, not where it was set.
REM Same <os>-<arch> convention as scripts/build-macos.sh, which stages
REM release/macos-arm64/ and release/macos-x86_64/. The artefact keeps its
REM own name (snug.exe) and the platform lives in the directory, so the
REM documentation never has to name two different binaries.
set "RELEASE_SUBDIR=windows-!TARGET_ARCH!"
set "STAGE_DIR=!RELEASE_DIR!\!RELEASE_SUBDIR!"

REM The committed stub this CLI embeds, named for the same <os>-<arch> the
REM release folder is, so bin\ holds one stub per target and they can be
REM told apart at a glance:
REM     launcher-stub-windows-x86_64.exe
REM     launcher-stub-macos-arm64
REM     launcher-stub-macos-x86_64
REM Derived from RELEASE_SUBDIR rather than spelled out, so the stub and
REM the folder it ships into cannot disagree about which target they are.
REM crates\snug-cli\src\stub.rs include_bytes!()s exactly this path, which
REM is why step 2 has to run before step 3.
set "STUB=bin\launcher-stub-!RELEASE_SUBDIR!.exe"

echo.
echo Building release artefacts in %CD%
echo   target triple:  !TARGET_TRIPLE!
echo   cross-compile:  !CROSS_COMPILE!
echo   staging into:   !STAGE_DIR!\

if "!CLEAN!"=="1" (
    echo.
    echo ==^> cargo clean -p snug-launcher -p snug-cli -p snug-dropper
    cargo clean -p snug-launcher -p snug-cli -p snug-dropper
    if errorlevel 1 (
        echo [build-windows] cargo clean failed with exit code %errorlevel%
        exit /b %errorlevel%
    )
)

REM ---------------------------------------------------------------------------
REM 1. Release build of the launcher.
REM ---------------------------------------------------------------------------
if "!SKIP_LAUNCHER_REBUILD!"=="1" goto skip_launcher_rebuild

echo.
echo ==^> cargo !CARGO_LAUNCHER_ARGS!
powershell -NoProfile -ExecutionPolicy Bypass -File "!_SCRIPT_DIR!scripts\cargo-repro.ps1" !CARGO_LAUNCHER_ARGS!
if errorlevel 1 (
    echo [build-windows] launcher build failed with exit code %errorlevel%
    exit /b %errorlevel%
)
if not exist "!BUILT_LAUNCHER_EXE!" (
    echo [build-windows] Expected launcher binary at !BUILT_LAUNCHER_EXE! but it was not produced.
    exit /b 1
)

REM 2. Refresh the committed stub. The CLI embed_bytes!()s this exact file;
REM    cargo tracks it by content hash, so step 3 sees the change without
REM    any extra wiring.
echo.
echo ==^> copy !BUILT_LAUNCHER_EXE! -^> !STUB!
copy /Y "!BUILT_LAUNCHER_EXE!" "!STUB!" >nul
if errorlevel 1 (
    echo [build-windows] Failed to copy !BUILT_LAUNCHER_EXE! to !STUB!
    exit /b %errorlevel%
)

goto after_launcher_rebuild

:skip_launcher_rebuild
echo.
echo ==^> Skipping launcher rebuild + stub copy ^(--SkipLauncherRebuild^)

:after_launcher_rebuild

REM ---------------------------------------------------------------------------
REM 3. Release build of the CLI, plus the verify_icons build tool.
REM ---------------------------------------------------------------------------

echo.
echo ==^> cargo !CARGO_CLI_ARGS!
powershell -NoProfile -ExecutionPolicy Bypass -File "!_SCRIPT_DIR!scripts\cargo-repro.ps1" !CARGO_CLI_ARGS!
if errorlevel 1 (
    echo [build-windows] CLI build failed with exit code %errorlevel%
    exit /b %errorlevel%
)
if not exist "!BUILT_CLI_EXE!" (
    echo [build-windows] Expected CLI binary at !BUILT_CLI_EXE! but it was not produced.
    exit /b 1
)
if not exist "!BUILT_VERIFY_EXE!" (
    echo [build-windows] Expected verify_icons at !BUILT_VERIFY_EXE! but it was not produced.
    exit /b 1
)

REM ---------------------------------------------------------------------------
REM 4. Package the demo JAR into assets\snug-javafx-demo.exe.
REM ---------------------------------------------------------------------------

if "!SKIP_PACKAGE!"=="1" goto skip_package

if not exist "!DEMO_JAR!" (
    echo [build-windows] Demo JAR not found at !DEMO_JAR!. Build it first or pass --SkipPackage.
    exit /b 1
)

echo.
echo ==^> !BUILT_CLI_EXE! !DEMO_JAR!
"!BUILT_CLI_EXE!" "!DEMO_JAR!"
if errorlevel 1 (
    echo [build-windows] snug package failed with exit code %errorlevel%
    exit /b %errorlevel%
)
if not exist "!DEMO_EXE!" (
    echo [build-windows] Expected packaged EXE at !DEMO_EXE! but it was not produced.
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
    echo [build-windows] embedded-stub verification failed - the CLI was rebuilt against a stale stub.
    exit /b 1
)

goto after_verify

:skip_verify
echo.
echo ==^> Skipping embedded-stub verification ^(--SkipVerify^)

:after_verify

REM ---------------------------------------------------------------------------
REM 6. Stamp assets\snug-runner.png into snug.exe.
REM
REM    Unconditional, and deliberately NOT inside --SkipDevTools.
REM
REM    snug-cli has no build.rs and does not depend on snug-launcher, so
REM    nothing in its build graph supplies a MAINICON - this stamp is the
REM    only reason snug.exe has an icon at all. The default snug-icon.png
REM    MAINICON that `snug-launcher`'s build.rs links in belongs to the
REM    launcher crate and does not reach the CLI, which means an
REM    unstamped snug.exe ships with no icon rather than a wrong one.
REM
REM    The stamp helper is built unconditionally for the same reason: it
REM    is a release tool, and skipping it to skip snug_preview would take
REM    the CLI's icon with it. These src/bin/ targets aren't produced by
REM    step 1's `cargo build -p snug-launcher`, which only builds the
REM    primary bin.
REM ---------------------------------------------------------------------------

echo.
echo ==^> cargo !CARGO_STAMP_ARGS!
powershell -NoProfile -ExecutionPolicy Bypass -File "!_SCRIPT_DIR!scripts\cargo-repro.ps1" !CARGO_STAMP_ARGS!
if errorlevel 1 (
    echo [build-windows] stamp_preview_icon build failed with exit code %errorlevel%
    exit /b %errorlevel%
)
if not exist "!BUILT_STAMP_EXE!" (
    echo [build-windows] Expected stamp helper at !BUILT_STAMP_EXE! but it was not produced.
    exit /b 1
)
if not exist "!SNUG_CLI_PNG!" (
    echo [build-windows] snug CLI icon PNG not found at !SNUG_CLI_PNG!.
    exit /b 1
)

echo.
echo ==^> !BUILT_STAMP_EXE! !BUILT_CLI_EXE! !SNUG_CLI_PNG!
"!BUILT_STAMP_EXE!" "!BUILT_CLI_EXE!" "!SNUG_CLI_PNG!"
if errorlevel 1 (
    echo [build-windows] snug.exe icon stamp failed with exit code %errorlevel%
    exit /b %errorlevel%
)

REM ---------------------------------------------------------------------------
REM 7. Build the dialog preview and stamp assets\snug-preview.png into it.
REM
REM    snug_preview.exe is a dev tool - it is how you look at snug's dialogs
REM    and error copy without building and launching an app - so this is
REM    the one step --SkipDevTools turns off. It ships in release\ anyway
REM    (step 10 warns rather than aborts if it is missing).
REM
REM    Its icon is snug-preview.png rather than the production
REM    snug-icon.png that compile_for_everything links into every dev-time
REM    bin. See crates\snug-launcher\src\bin\stamp_preview_icon.rs for why
REM    this needs to be a separate post-link step rather than a build.rs
REM    trick.
REM ---------------------------------------------------------------------------

if "!SKIP_DEV_TOOLS!"=="1" goto skip_dev_tools

echo.
echo ==^> cargo !CARGO_PREVIEW_ARGS!
powershell -NoProfile -ExecutionPolicy Bypass -File "!_SCRIPT_DIR!scripts\cargo-repro.ps1" !CARGO_PREVIEW_ARGS!
if errorlevel 1 (
    echo [build-windows] snug_preview build failed with exit code %errorlevel%
    exit /b %errorlevel%
)
if not exist "!BUILT_PREVIEW_EXE!" (
    echo [build-windows] Expected preview binary at !BUILT_PREVIEW_EXE! but it was not produced.
    exit /b 1
)
if not exist "!PREVIEW_PNG!" (
    echo [build-windows] Preview icon PNG not found at !PREVIEW_PNG!.
    exit /b 1
)

echo.
echo ==^> !BUILT_STAMP_EXE! !BUILT_PREVIEW_EXE! !PREVIEW_PNG!
"!BUILT_STAMP_EXE!" "!BUILT_PREVIEW_EXE!" "!PREVIEW_PNG!"
if errorlevel 1 (
    echo [build-windows] snug_preview.exe icon stamp failed with exit code %errorlevel%
    exit /b %errorlevel%
)

goto after_dev_tools

:skip_dev_tools
echo.
echo ==^> Skipping snug_preview build + icon stamp ^(--SkipDevTools^)

:after_dev_tools

REM ---------------------------------------------------------------------------
REM 8. Build the beginner dropper (Build with Snug.exe).
REM
REM    A shipped artefact, not a dev tool, so it is a first-class step with
REM    its own --SkipDropper flag. The stamp helper has no build.rs of its own:
REM    the icon is applied post-link, which is what lets the artwork change
REM    without a Rust rebuild. See crates\snug-dropper\src\bin\stamp_dropper_icon.rs.
REM
REM    --package emits the shipped copy under its real display name, beside
REM    snug.exe. That placement is load-bearing: the dropper resolves snug.exe
REM    relative to its own location at runtime, so separating the two turns
REM    every build into a "snug.exe could not be found" dialog.
REM ---------------------------------------------------------------------------

if "!SKIP_DROPPER!"=="1" goto skip_dropper

echo.
echo ==^> cargo !CARGO_DROPPER_ARGS!
powershell -NoProfile -ExecutionPolicy Bypass -File "!_SCRIPT_DIR!scripts\cargo-repro.ps1" !CARGO_DROPPER_ARGS!
if errorlevel 1 (
    echo [build-windows] dropper build failed with exit code %errorlevel%
    exit /b %errorlevel%
)
if not exist "!BUILT_DROPPER_EXE!" (
    echo [build-windows] Expected dropper binary at !BUILT_DROPPER_EXE! but it was not produced.
    exit /b 1
)
if not exist "!BUILT_DROPPER_STAMP_EXE!" (
    echo [build-windows] Expected dropper stamp helper at !BUILT_DROPPER_STAMP_EXE! but it was not produced.
    exit /b 1
)
if not exist "!DROPPER_PNG!" (
    echo [build-windows] Dropper icon PNG not found at !DROPPER_PNG!.
    exit /b 1
)

echo.
echo ==^> !BUILT_DROPPER_STAMP_EXE! --package
REM Explicit exe path: the helper's own profile default resolves to target\release,
REM which is wrong under --CrossCompile. The --package dir defaults to the
REM exe's own folder, which is where snug.exe already lives.
"!BUILT_DROPPER_STAMP_EXE!" "!BUILT_DROPPER_EXE!" "!DROPPER_PNG!" --package
if errorlevel 1 (
    echo [build-windows] dropper icon stamp / package failed with exit code %errorlevel%
    exit /b %errorlevel%
)

REM --package defaults to the built exe's own folder, so derive the shipped
REM path from that exe rather than assuming where snug.exe ended up.
for %%I in ("!BUILT_DROPPER_EXE!") do set "DROPPER_DIR=%%~dpI"
set "DROPPER_PACKAGED=!DROPPER_DIR!!DROPPER_SHIPPED!"
if not exist "!DROPPER_PACKAGED!" (
    echo [build-windows] Expected !DROPPER_PACKAGED! but it was not produced.
    exit /b 1
)

REM Co-location is a runtime invariant, not tidiness: the dropper resolves
REM snug.exe relative to its own path, so a dropper shipped without it can
REM only ever show "snug.exe could not be found".
if not exist "!DROPPER_DIR!snug.exe" (
    echo [build-windows] ERROR: snug.exe is not in !DROPPER_DIR!
    echo [build-windows] !DROPPER_SHIPPED! resolves snug.exe relative to its own
    echo [build-windows] location and will fail on every machine. Pass the dropper
    echo [build-windows] stamp helper an explicit --package dir beside snug.exe.
    exit /b 1
)

goto after_dropper

:skip_dropper
echo.
echo ==^> Skipping Build with Snug build + package ^(--SkipDropper^)

:after_dropper

REM ---------------------------------------------------------------------------
REM 9. Verify each shipped EXE's MAINICON is the PNG it was stamped with.
REM
REM    Every stamp above exits non-zero when a stamp *errors*, so a broken
REM    resource directory or an unreadable PNG already aborts the run. What
REM    that cannot catch is the quiet failure: a stamp that never applied
REM    and left the crate's default icon in place, the right helper pointed
REM    at the wrong EXE so the whole folder wears one face, or orphan
REM    RT_ICON entries left over from an earlier run inflating every file.
REM
REM    verify_icons re-derives the icon each EXE should hold and compares it
REM    pixel for pixel, so the answer is about *provenance* - this artwork,
REM    in this file - rather than the much weaker "some icon is present".
REM
REM    Only the artefacts that were actually built are checked, so
REM    --SkipDropper and --SkipDevTools still produce a coherent (if
REM    partial) verification rather than a spurious failure.
REM
REM    One invocation per artefact rather than one invocation with three
REM    pairs, on purpose. Accumulating a quoted argument list into a
REM    variable means embedding `""` inside a `set "VAR=..."` and relying
REM    on the trailing `"""` parsing the way you meant it to, which is
REM    the same family of quoting trap documented at the bottom of this
REM    file. Here each path is quoted exactly once, by the call itself.
REM ---------------------------------------------------------------------------

if "!SKIP_VERIFY!"=="1" goto skip_icon_verify

echo.
echo ==^> Verifying MAINICON provenance

set "ICON_FAILURES=0"
call :verify_icon "!BUILT_CLI_EXE!" "!SNUG_CLI_PNG!"
call :verify_icon "!BUILT_PREVIEW_EXE!" "!PREVIEW_PNG!"
call :verify_icon "!DROPPER_PACKAGED!" "!DROPPER_PNG!"

if "!ICON_FAILURES!"=="0" goto after_icon_verify

echo.
echo [build-windows] Icon verification failed for !ICON_FAILURES! artefact(s).
echo [build-windows] An EXE does not carry the icon it was stamped with, or a
echo [build-windows] previous stamp left orphan icon bytes behind. The build is
echo [build-windows] NOT releasable - the folder would ship a wrong or missing icon.
exit /b 1

:skip_icon_verify
echo.
echo ==^> Skipping embedded-stub + icon provenance verification ^(--SkipVerify^)

:after_icon_verify

REM ---------------------------------------------------------------------------
REM 10. Stage the release directory.
REM
REM    release\windows-x86_64\ is the folder a user unpacks: snug.exe, the
REM    Build with Snug shim, the snug_preview dialog preview, and the demo
REM    JAR so the whole drop-a-JAR flow can be tried before writing a JAR of
REM    their own. The directory ignores its own contents, so nothing in it
REM    is tracked.
REM
REM    The <os>-<arch> subdirectory is the same convention
REM    scripts/build-macos.sh uses for release/macos-arm64/ and
REM    release/macos-x86_64/: the artefact keeps its own name and the
REM    platform lives in the directory. Every platform therefore invokes
REM    `snug` / `snug.exe` and the documentation never has to name two
REM    different binaries.
REM
REM    snug_preview.exe is a dev tool and ships anyway: it is how you look
REM    at snug's dialogs and error copy without building and launching an
REM    app. It comes from step 7, so --SkipDevTools leaves it unbuilt and
REM    :stage warns instead of aborting.
REM
REM    The two EXEs are staged together on purpose. Build with Snug.exe
REM    resolves snug.exe relative to its own path, so a release folder with
REM    one and not the other is broken by construction -- the same
REM    invariant step 8 asserts, now enforced at the shipping boundary.
REM
REM    A missing artefact warns rather than aborts, so --SkipDropper and
REM    --SkipDevTools still produce a coherent (if partial) folder. Anything
REM    already sitting in release\ is left alone, so an artefact dropped from
REM    the pipeline in one run lingers in the next; use --Clean and stage
REM    into a fresh folder when you need a guaranteed-clean release.
REM ---------------------------------------------------------------------------

if "!SKIP_RELEASE!"=="1" goto skip_release

echo.
echo ==^> staging !STAGE_DIR!\
if not exist "!STAGE_DIR!" mkdir "!STAGE_DIR!"
if errorlevel 1 (
    echo [build-windows] Failed to create !STAGE_DIR!\
    exit /b 1
)

call :stage "!BUILT_CLI_EXE!" "snug.exe"
if errorlevel 1 exit /b 1
call :stage "!DROPPER_PACKAGED!" "!DROPPER_SHIPPED!"
if errorlevel 1 exit /b 1
REM A dev tool, but the one you open to inspect snug's dialogs and error
REM copy without building and launching an app. Built in step 7, so
REM --SkipDevTools leaves it unbuilt and :stage warns rather than aborting.
call :stage "!BUILT_PREVIEW_EXE!" "snug_preview.exe"
if errorlevel 1 exit /b 1
call :stage "!DEMO_JAR!" "snug-javafx-demo-windows.jar"
if errorlevel 1 exit /b 1

goto after_release

:skip_release
echo.
echo ==^> Skipping release staging ^(--SkipRelease^)

:after_release

REM ---------------------------------------------------------------------------
REM Summary.
REM ---------------------------------------------------------------------------

echo.
echo OK
echo.
echo Sizes:
for %%I in ("!STUB!")            do echo   Launcher stub:          !STUB!            ^(%%~zI bytes^)
for %%I in ("!BUILT_CLI_EXE!")   do echo   snug CLI:               !BUILT_CLI_EXE!   ^(%%~zI bytes^)
for %%I in ("!DEMO_EXE!")        do echo   Demo EXE:               !DEMO_EXE!        ^(%%~zI bytes^)
for %%I in ("!BUILT_PREVIEW_EXE!") do ( if exist "!BUILT_PREVIEW_EXE!" echo   snug_preview:           !BUILT_PREVIEW_EXE! ^(%%~zI bytes^) )
for %%I in ("!BUILT_DROPPER_EXE!")     do ( if exist "!BUILT_DROPPER_EXE!" echo   Dropper built:          !BUILT_DROPPER_EXE! ^(%%~zI bytes^) )
for %%I in ("!DROPPER_PACKAGED!")     do ( if exist "!DROPPER_PACKAGED!" echo   !DROPPER_SHIPPED!:  !DROPPER_PACKAGED! ^(%%~zI bytes^) )
echo.
echo Release directory: !STAGE_DIR!\
for %%I in ("!STAGE_DIR!\snug.exe")               do ( if exist "!STAGE_DIR!\snug.exe" echo     snug.exe                 !STAGE_DIR!\snug.exe ^(%%~zI bytes^) )
for %%I in ("!STAGE_DIR!\!DROPPER_SHIPPED!")     do ( if exist "!STAGE_DIR!\!DROPPER_SHIPPED!" echo     !DROPPER_SHIPPED!: !STAGE_DIR!\!DROPPER_SHIPPED! ^(%%~zI bytes^) )
for %%I in ("!STAGE_DIR!\snug_preview.exe")      do ( if exist "!STAGE_DIR!\snug_preview.exe" echo     snug_preview.exe        !STAGE_DIR!\snug_preview.exe ^(%%~zI bytes^) )
for %%I in ("!STAGE_DIR!\snug-javafx-demo-windows.jar") do ( if exist "!STAGE_DIR!\snug-javafx-demo-windows.jar" echo     snug-javafx-demo-windows.jar !STAGE_DIR!\snug-javafx-demo-windows.jar ^(%%~zI bytes^) )

REM End the main flow here. :stage and :verify_icon below are reachable only
REM through CALL, which is what stops the pipeline running off the end of the
REM summary and into a subroutine with no arguments.
endlocal
exit /b 0

REM Check one <exe> <png> pair's MAINICON, counting any failure rather than
REM aborting, so a single run reports every artefact that is wrong instead
REM of only the first.
REM
REM A missing exe is a skip rather than a failure: --SkipDropper and
REM --SkipDevTools are legitimate ways to run this pipeline, and a partial
REM release should still have its built artefacts checked.
REM
REM Like :stage below, this is reached only via CALL and so is written
REM without `for`, `%%` or parenthesised blocks. `if ... ( ... )` inside a
REM call-ed subroutine re-expands on the way in, and the block's closing
REM paren is exactly the kind of thing that gets eaten - the caller then
REM reads a stale errorlevel and the failure vanishes. Straight-line gotos
REM cost four lines and cannot misparse.
:verify_icon
if not exist "%~1" goto verify_icon_missing
"!BUILT_VERIFY_EXE!" "%~1" "%~2"
if errorlevel 1 goto verify_icon_bad
exit /b 0
:verify_icon_missing
echo   skip    %~nx1 -- not built
exit /b 0
:verify_icon_bad
REM set /a rather than a fixed value so three bad artefacts report three.
set /a ICON_FAILURES+=1
exit /b 0

REM Copy one artefact into release\, reporting what happened.
REM
REM Placed after the main flow and reached only via CALL, so the pipeline
REM above still reads top to bottom. `exit /b` returns to the CALLer rather
REM than ending the script, which is what lets one failing copy abort the
REM run while one missing file does not.
REM
REM Deliberately free of for-loops and escaped parens. A `for %%I in (...)
REM do echo ... ^(...)` line inside a call-ed subroutine does not survive
REM cmd: the second expansion pass CALL performs mangles the escaped
REM parens, the rest of the subroutine gets swallowed along with its
REM `exit /b`, and the caller then reads a stale errorlevel. Sizes are
REM reported in the top-level "Sizes:" block instead, where a for-loop is
REM known to behave.
:stage
set "_STAGE_SRC=%~1"
set "_STAGE_NAME=%~2"
if not exist "!_STAGE_SRC!" (
    echo   skip    !_STAGE_NAME! -- not built
    exit /b 0
)
copy /Y "!_STAGE_SRC!" "!STAGE_DIR!\!_STAGE_NAME!" >nul
if errorlevel 1 (
    echo   FAILED to copy !_STAGE_SRC! into !STAGE_DIR!
    exit /b 1
)
echo   staged  !_STAGE_NAME!
exit /b 0
