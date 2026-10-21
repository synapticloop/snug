<div align="center">

<img src="assets/snug-splash.png" alt="snug" width="640">

# snug
> Snug deliberately does less. It creates a polished, self-managing launch experience for your Java application, then gets out of the way.

> A small, modern, Rust-based launcher-wrapper That turns Java jars into a clickable Windows application

</div>

---

> **THIS IS NOWHERE NEAR READY - BUT IT IS CLOSE, PLEASE COME BACK LATER**

---

# Quick-Start

1. Download `snug.exe` and `snug-javafx-demo-windows.jar`
2. Run the following command

```
snug.exe snug-javafx-demo-windows.jar -o snug-javafx-demo.exe --company "My Company" --name "My App"
```

Then either run

```
snug-javafx-demo.exe
```

Or double-click on the icon

## Quick Demo

For a quick demo of a JavaFX application (with a splash screen), run
`assets/snug-javafx-demo-windows.exe`.

<div align="center"><img src="assets/snug-javafx-demo-screenshot.png" alt="snug" width="302"></div>

# Why use Snug?

- **Make Java applications easy to run.** Turn your application into a 
  familiar Windows executable that users can just double-click on.
- **Remove Java setup from the user.** Snug takes care of finding or downloading 
  the required Java runtime.
- **Create a more polished application.** Add your own start up splash screen,
  icon, application name, version information and other metadata.
- **Make deployment simpler.** Give colleagues a straightforward executable 
  they can run without needing to understand what the underlying language is.
- **Keep applications local.** Useful for internal business tools that should 
  run on a user’s own computer rather than being hosted as a web application.
- **Reuse runtimes efficiently.** Applications can make use of downloaded 
  runtime components instead of bundling a complete Java runtime into every application.
- **Control how your application launches.** Configure Java requirements, 
  startup behaviour, arguments and other deployment details to suit your application.
- **Bridge the gap between building and distributing.** Snug handles the 
  awkward packaging and launching work that sits between a working Java application and something ready for everyday office use.

# What Snug is Not

- **Snug is not an application store or publishing platform.** It does not 
  host your application, provide a public download page, or publish releases 
  for you.  It builds locally, you can run it locally yourself, or give people a copy to run themselves.
- **Snug does not have a built-in application update mechanism for your 
  application.** If you distribute a new version, you decide how that version is delivered to users. (That is not to say that you couldn't check for updates in your main application and point people at the new download location)
- **Snug does not require your application to be published publicly.** It is 
  well suited to internal applications that are simply copied to colleagues, placed on a shared drive, or distributed through whatever process your business already uses.  This bypasses code-signing the application, which can be expensive for the use cases.
- **Snug is not an installer framework.** Its primary job is to create a 
  native executable and manage the Java launch experience, rather than build a complete installation and uninstallation system. (This means that you will NOT get a Start menu icon or a desktop shortcut - but you can still run it from your desktop)
- **Snug does not provide cloud-based deployment infrastructure.** There is 
  no requirement for npm, GitHub Releases, a hosted service, or an online account simply to distribute an application.
- **Snug does not dictate how you distribute your software.** Once the 
  executable has been created, you remain in control of where it goes and who receives it.


Snug generates a single native Windows `.exe` that:

- contains the complete fat JAR
- embeds the application icon and optional splash image
- carries Windows application/version metadata (and an optional
  application manifest for DPI awareness, UAC, etc.)
- locates an installed Java 25+ at runtime (env, PATH, registry,
  common install paths)
- can (with `--download-jdk`) auto-download and verify Eclipse Temurin
  from Adoptium when no compatible JVM is present
- loads `jvm.dll` directly via JNI — no `javaw.exe` indirection — so the
  application shows up as `MyApp.exe` in Task Manager

It is conceptually similar to [exe4j] or [Launch4j], but small, modern,
Rust-based, and open source.

[exe4j]: https://www.ej-technologies.com/products/exe4j/overview.html
[Launch4j]: https://launch4j.sourceforge.net/


## Status

**Pre-alpha, and both launchers work on real machines.** The CLI builds a
real Windows `.exe` end-to-end with in-process resource stamping, and a
real macOS `.app` end-to-end with an ad-hoc signature. Both launcher
runtimes locate their payload, extract the JAR to a per-user cache,
discover a compatible JVM, load `jvm.dll` / `libjvm.dylib`, and invoke
the Java `main` class through the `jni` 0.22 invocation API. With
`--download-jdk` they will also fetch Eclipse Temurin from Adoptium,
verify its SHA-256 on disk, extract it, and hand the JVM home back to
discovery. The Windows path was validated on a Windows machine; the macOS
path on a Mac, including the JavaFX system menu bar.

| Slice | Status |
|-------|--------|
| CLI surface + embedded-payload format | done |
| Windows launcher runtime (JVM discovery + JNI) | **done, verified on a Windows machine** |
| macOS launcher runtime (`libjvm.dylib` + JNI, cache, JDK discovery) | **done, verified on a Mac** |
| snug-cli builder: load stub via `include_bytes!()`, stamp payload as an `RT_RCDATA` resource, in-process `editpe` icon/version/manifest stamping | **done** |
| macOS `.app` emitter (`Contents/{Info.plist,MacOS,Resources}`, ad-hoc signed) | **done** |
| Windows resource stamping — `VS_VERSIONINFO` (`ProductName`, `CompanyName`, `FileDescription`, `LegalCopyright`, `FixedFileInfo` file/product version, `VFT_APP`) + optional `--icon` + optional `--manifest` | done |
| macOS app menu / `CFBundleName` from `--name` | done |
| Per-user cache — `%LOCALAPPDATA%\snug` (Windows), `~/Library/Caches/snug` (macOS) | done |
| Per-launch log file (`<cache>\snug.log`, truncated on each launch) | done |
| Old-version cache cleanup | done — entries expire on use, swept at most once per 6h |
| Editable dialog text (`snug-format/assets/snug-localisations.en.txt`) | done, and shared by the Windows and macOS dialogs |
| Replacement-token contract (`snug --list-tokens`, `REPLACEMENT_TOKENS.md`) | done |
| macOS JDK download dialogs (consent, progress, retry, failure) | done |
| Native splash renderer (PNG via GDI+/WIC) | Windows only; not on macOS |
| GitHub Actions CI (windows-latest release) | planned |

The macOS launcher builds and runs natively too — macOS is where snug is
developed — and its dialogs carry the *same* localised strings as the
Windows ones, so a `--localization` bundle translates both. See
`AGENTS.md` for the platform notes, including the one genuinely
surprising constraint: the launcher must never create an `NSApplication`
before JavaFX does, or JavaFX concludes it is embedded in another toolkit
and the launched app loses its menu bar.

## Quick start

```text
# Wrap a fat JAR into a Windows .exe:
snug app.jar -o App.exe --name "My App" --company "SynapticLoop" \
    --version 1.0.0 --main-class com.example.Main --min-java 25

# Stamp a PNG icon and an arbitrary Windows application manifest
# (icon and manifest are optional; version info is always stamped):
snug app.jar -o App.exe --icon assets/icon.png --manifest assets/app.manifest.xml

# Forward extra JVM options (repeatable):
snug app.jar -o App.exe --jvm-arg=-Xms256m --jvm-arg=-Xmx2g \
                       --jvm-arg=-Dfile.encoding=UTF-8

# Show a PNG splash before the JVM starts (dismissed on ready or after --splash-ms):
snug app.jar -o App.exe --splash assets/splash.png --splash-ms 2500

# Pass the JAR via `--input` (also accepts a directory of JARs for a multi-JAR
# classpath; Main-Class is read from the first JAR's manifest):
snug --input path/to/app.jar  -o App.exe
snug --input path/to/lib-dir/ -o App.exe --main-class com.example.Main

# Auto-download Eclipse Temurin if the host has no compatible JDK.
# The value is required — a bare `--download-jdk` is an error.
# omitted               = `auto`: only on discovery miss (the default).
# `--download-jdk=force` = always show the dialog, bypass discovery.
# `--download-jdk=off`   = never offer a download.
snug app.jar -o App.exe --download-jdk=auto      # the default, explicit
snug app.jar -o App.exe --download-jdk=force     # always prompt
snug app.jar -o App.exe --download-jdk=off       # Java must be pre-installed

# Inspect what would be built without writing anything:
snug app.jar -o App.exe --dry-run

# Pipe the encoded payload to stdout instead of building an EXE
# (useful for piping into other tools or for inspecting the format):
snug app.jar --emit-payload

# Use a `snug.options` file for default settings (CLI flags always override).
# Each line is parsed as if it were on the command line; `#` is a comment.
# `--input` (file or directory) and every other flag can live here too,
# so the JAR location doesn't have to be on the command line:
#   # snug.options (read from the current working directory)
#   --input path/to/app.jar         # or path/to/lib-dir/
#   --name "My App"
#   --company "SynapticLoop"
#   --min-java 25
#   --download-jdk=force
# A bare `snug` then builds the file's `--input`. Passing a JAR positionally
# overrides it — `snug other.jar` builds *other* jar — which is the same
# "command line wins" rule every other flag follows. (The two are only
# mutually exclusive when both appear on one command line.)
snug -o App.exe --name "Different Name"   # --name overrides the file

# A value that genuinely differs per platform goes in a companion file named
# after the host, which overrides `snug.options` (CLI flags still win over it).
# It is a *partial* override: only the differing values belong in it.
#   # snug.macos.options  —  the demo's usual case is two lines
#   --input assets/myapp-mac.jar
#   --output build/MyApp.app
#
# Only the file matching the host is read, so a checked-in
# `snug.windows.options` cannot affect a macOS build, and a missing one is
# neither an error nor a warning. `--options <path>` means *that file only*,
# with the per-platform tier not consulted alongside it.

# Snug's own version (distinct from --version, which sets the app's version):
snug --snug-version
```

The output extension picks the artefact: `-o App.exe` builds a Windows
binary, `-o MyApp.app` builds a macOS bundle, and with no `-o` at all
the host's own platform is used. A macOS `.app` gets its icon from
`Contents/Resources/App.icns` and its payload from a sibling file
`Resources/<App>.snugpayload` -- macOS has no resource directory, which
is why the payload is a sibling rather than something stamped in.

The produced `.exe` is a 64-bit Windows GUI binary that:
- carries the full fat JAR + optional splash as a single encoded
  payload stamped into the PE as an `RT_RCDATA` resource (the icon is
  stamped separately, as `MAINICON`),
- displays as a Windows GUI executable (no console window),
- on launch reads that resource back out, decodes the postcard payload,
  extracts the JAR to
  `%LOCALAPPDATA%\snug\<company>\<name>\<jar-sha256>\app.jar`, locates
  a Java 25+ install + `jvm.dll`, and invokes the Java `main` class.

## Demo

A JavaFX demo JAR ships with the repo at
`assets/snug-javafx-demo-windows.jar`. End-to-end build (from the repo root):

```powershell
# Option A: the included build script (builds launcher + CLI + packages demo):
.\scripts\windows\build.cmd

# Option B: the equivalent by hand:
cargo build --release -p snug-launcher
copy /Y target\release\snug-launcher.exe bin\launcher-stub-windows-x86_64.exe
cargo build --release -p snug-cli
target\release\snug.exe assets\snug-javafx-demo-windows.jar ^
    -o snug-javafx-demo.exe ^
    --name "Snug JavaFX Demo" --company "SynapticLoop" --version 0.1.0 ^
    --description "Snug JavaFX demo launcher" --copyright "© SynapticLoop" ^
    --icon assets\snug-icon.png
```

The JAR's `Main-Class` (`synapticloop.snugjavafxdemo.HelloApplication`)
is read from its manifest, so no `--main-class` override is needed.

`scripts\windows\build.cmd` accepts `--SkipLauncherRebuild` (skip steps
1+2, useful when iterating on CLI-only code) and `--SkipPackage`
(skip the final `snug.exe` packaging step).

## Layout

```text
snug/
├── Cargo.toml                  # workspace root
├── AGENTS.md                   # project context for AI agents
├── HANDOFF.md                  # session-end notes for the next agent
├── README.md                   # this file
├── assets/
│   ├── snug-icon.png           # 1254×1254, used by README + --icon examples
│   ├── snug-javafx-demo-windows.jar  # JavaFX demo fat JAR, Windows natives
│   └── snug-javafx-demo-macos.jar    # same demo, macOS .dylib natives
├── bin/
│   ├── launcher-stub-windows-x86_64.exe       # precompiled Windows stub (PE32+ GUI x86-64, ~1.1 MB)
│   ├── launcher-stub-macos-arm64  # macOS launcher (Mach-O arm64, ~3.1 MB)
│   └── launcher-stub-macos-x86_64 # macOS launcher (Mach-O x86_64, ~3.1 MB)
├── scripts/
│   ├── windows/                # build.cmd + release.cmd + every supporting .ps1
│   │                           #   (.cmd + .ps1 — launcher stub, CLI, dropper,
│   │                           #    dialog preview, embedded-stub verify, etc.)
│   └── macos/                 # build-macos.sh + build-macos-demo.sh
│                                 #   (slim per-arch CLI builds + demo .app)
└── crates/
    ├── snug-format/            # embedded-payload types + postcard codec (the wire contract)
    │   ├── assets/             # snug-localisations.en.txt — canonical English baseline
    │   └── src/tokens.rs       # the `{slot}` contract; generates REPLACEMENT_TOKENS.md
    ├── snug-payload/           # writing and locating the payload (PE `RT_RCDATA` / `.app` sibling file)
    ├── snug-launcher/
    │   ├── assets/             # snug-icon.png (build.rs → MAINICON resource)
    │   └── src/dialogs.rs      # typed key tree, assembled from `localize::lookup`
    ├── snug-dropper/           # "Snug Quick Build" — the Win32 drag-and-drop shim
    └── snug-cli/               # CLI + payload stamping + snug.options + editpe resources
        └── assets/             # snug.options.example, written by `snug --init-options`
```

`snug-format` is the **contract** between the builder (CLI side) and the
launcher (runtime side). The wire layout is:

```text
+--------+----------------+--------------+--------------+----------------------+
| magic  | format_version | payload_len  | payload_crc32| payload (postcard)   |
| 8 B    | u16 LE         | u32 LE       | u32 LE       | payload_len bytes    |
+--------+----------------+--------------+--------------+----------------------+
```

The 8-byte magic is `SNUGEMBD`. `format_version` is currently `6`:

| Version | Change |
|---|---|
| 4 (2026-09) | `auto_download_jdk: bool` — offer an Adoptium download when JVM discovery fails |
| 5 (2026-09) | `download_jdk: DownloadJdkMode` (`off` / `auto` / `force`) replaced the bool |
| 6 (2026-09) | `SnugPayload.localizations: Vec<Localization>` — embedded runtime-string bundles |

The decoder rejects a payload whose version is *higher* than the build
understands, so old stubs need rebuilding when the version moves.
`payload_crc32` is CRC32/IEEE of the postcard bytes; the launcher
validates before decoding. Payload is a postcard-encoded
`SnugPayload { config, jars, localizations }` — there is no `icon`
field, because the icon bytes are re-read from disk at build time to
stamp the PE rather than carried in the payload.

The payload is not appended to the executable. It is stamped into the
PE's resource directory as an `RT_RCDATA` entry by
[`editpe`](https://github.com/Systemcluster/editpe), and the launcher
reads it back out through the same API. `bin/launcher-stub-windows-x86_64.exe`
is a *bare* stub with no such entry, which is how the launcher
recognises it and reports that no payload is embedded.

## Building

Prerequisites: a Rust 1.85+ toolchain. **Builds are native, never
cross-artefact** — Windows artefacts are built on Windows, macOS
artefacts on macOS. That is not a limitation, it is what makes the
precompiled-stub design work: `snug-cli` embeds its launcher with
`include_bytes!`, so the launcher has to exist *before* the CLI
compiles, and `scripts\windows\build.cmd` already does them in that
order. Cross-compiling would mean keeping a foreign toolchain alive
purely to refresh a binary the target machine could have produced itself.
The committed stubs are a bootstrap convenience for a fresh clone, not
the source of truth; every release pipeline regenerates them.

```bash
# one-time setup on a Mac: the x86_64-pc-windows-gnu target is
# *check-only* here, to prove the source still compiles for Windows.
# It is never used to produce a shipped artefact.
rustup target add x86_64-pc-windows-gnu
cargo check --all-targets --target x86_64-pc-windows-gnu

# full workspace tests. The portable form matters: plain `cargo test`
# also builds examples, and the Windows-only ones have no `main` off
# Windows — so they fail to *compile*, not fail to pass.
cargo test --workspace --lib --bins --tests

# build the snug CLI for the host platform
cargo build --release -p snug-cli

# regenerate the stub after changing snug-launcher code.
# Native only — see "Building" above for why there is no cross-artefact path.
# Windows:
cargo build --release -p snug-launcher
Copy-Item target\release\snug-launcher.exe bin\launcher-stub-windows-x86_64.exe -Force
# macOS (arm64 + x86_64 in one go, each landing in bin/):
scripts/macos/build-macos.sh
```

> **If you change `snug-launcher`, rebuild the stubs before testing.** The
> `snug` CLI embeds them with `include_bytes!`, so a stale stub means
> your change is silently not in the binary you are about to run. The
> release scripts do it for you; a hand-rolled `cargo build -p snug-cli`
> does not.

Notable test binaries:

- `crates/snug-format/tests/roundtrip.rs` — payload encode/decode
- `crates/snug-launcher/tests/stub_payload_roundtrip.rs` — proves the
  launcher reads the payload back out of the stamped `RT_RCDATA`
  resource, and still recognises a bare stub as payload-less
- `crates/snug-launcher/tests/...` — `find_java_home` roundtrips
  (Adoptium's nested `jdk-X.Y.Z/` layout), `dialogs` placeholder filling,
  live progress text formatting
- `crates/snug-launcher/tests/macos_launch_e2e.rs` — compiles a real class
  with `javac` and asserts the JVM actually runs it (skipped, not failed,
  when no JDK is present)
- `crates/snug-cli/tests/end_to_end.rs` — full CLI invocation,
  payload stamping, PE resource stamp
- `crates/snug-cli/tests/editpe_roundtrip.rs` — `editpe`-based icon /
  version / manifest stamping roundtrip
- `crates/snug-cli/tests/options_file.rs`, `tests/no_args.rs`,
  `tests/find_main.rs`, `tests/init_modes.rs` — `snug.options` config-file
  and no-args help behaviour, `--find-main`, and the `--init-*`
  scaffolding modes
- `crates/snug-dropper/tests/end_to_end.rs` — drives a real `snug.exe`
  and asserts the EXE lands on disk (skipped when `snug.exe` is unbuilt)

## CLI surface (canonical)

```text
snug [<jar>] [-o EXE] [--input JAR|DIR] [--name ...] [--company ...]
           [--version ...] [--description ...] [--copyright ...]
           [--update-url URL] [--min-java N] [--main-class CLASS]
           [--icon PNG/ICO] [--manifest XML] [--splash PNG] [--splash-ms MS]
           [--splash-max WxH|off]
           [--jvm-arg ARG]... [--cache-dir DIR]
           [--download-jdk <MODE>]          # off | auto | force (default: auto)
           [--localization TXT|DIR]...
           [--emit-payload] [--dry-run] [--find-main] [--snug-version]
           [--options PATH]
           [--list-tokens]
           [--init-options] [--init-localizations[=DIR]] [--init-localizations-tag TAG]...
```

Four of these are easy to misread:

- `--cache-dir` and `--update-url` are **stamped into the payload**, so
  they take effect on the *end user's* machine, not yours. `--cache-dir`
  is validated as an absolute path for exactly that reason (a relative one
  would resolve against the end user's CWD); `--update-url` gets no
  validation at all, so it is your own URL in your own binary — bake in
  what you actually mean.
- `--splash-max` is an **advisory warning threshold**, not a cap
  (default `640x360`, `off` silences it). The PNG is converted to raw
  `BGRA` and embedded uncompressed, so an oversized splash inflates
  every produced binary — snug warns and carries on regardless.
- `--list-tokens`, `--init-options` and `--init-localizations` are
  read-only or scaffolding modes: they short-circuit before the JAR is
  required, and they cannot be combined with a build.
- `--splash` / `--splash-ms` / `--splash-max` are **Windows-only**. The
  macOS bundle path drops the splash outright, so a `--splash` PNG on a
  `-o App.app` build is silently discarded rather than embedded.

`--download-jdk` takes one **required** value, validated against the three
modes:

- omitted — `auto` (the default): pop the dialog only when JVM discovery
  fails.
- `--download-jdk auto` / `--download-jdk=auto` — same as above, explicit.
- `--download-jdk force` / `--download-jdk=force` — always show the
  dialog, bypassing JVM discovery. Useful when the end user wants to
  install Temurin regardless of what's on `JAVA_HOME` / `PATH`.
- `--download-jdk off` / `--download-jdk=off` — no download flow at all.
- anything else is rejected before the build starts.

A bare `--download-jdk` is an **error**, not a synonym for `auto`. That is
deliberate: the flag used to be `require_equals`, which meant
`--download-jdk force` silently became `auto` while `force` fell through
to the positional `[JAR|DIR]` slot — surfacing as
`input path does not exist: force`, or `unexpected argument '<jar>'`, or
a `--input`/`[JAR|DIR]` conflict out of a shared options file. Every one
of those named something you never typed.

Snug's own version (from `Cargo.toml`) is `--snug-version` (clap's
auto `--version` is disabled so the brief's `--version <APP-VERSION>`
doesn't collide with it).

Resource stamping is unconditional and in-process — there is no
`--rcedit` / `--no-rcedit` flag any more; the prior subprocess
integration was replaced by the pure-Rust
[`editpe`](https://github.com/Systemcluster/editpe) crate, which works
identically on macOS, Linux, and Windows.

## JDK auto-download

With `--download-jdk`, the launcher can fetch a matching Temurin JDK
from `api.adoptium.net` when no compatible JVM is on the host.

The flow:

1. Cache check — scan the per-user JDK store for a previously-extracted
   Temurin whose `java -version` reports a sufficient major. Reused
   silently, and this happens **before any network access**, so a JDK
   snug already downloaded is never downloaded twice.
2. Metadata fetch — `GET https://api.adoptium.net/v3/assets/feature_releases/<major>/ga`
   with the host's own `os` and `architecture` in Adoptium's spelling
   (`os=windows&architecture=x64` / `os=mac&architecture=aarch64`).
   Returns the latest GA release with `binaries[].package.{link,
   checksum, size}` and `version_data.semver`.
3. Dialog — first-run users see **Install the runtime now / Cancel**
   (button text is editable — see below). Windows uses a `TaskDialog`;
   macOS uses a hand-built `NSWindow` rather than an `NSAlert`, because
   the consent question has to be
   answerable before a modal session exists — an `NSAlert` runs its own
   nested event loop, and gating a worker on a window that can never
   appear is how "minutes of silence and not one byte" happened.
4. Download + verify — fetch the archive to a temp file, stream-hash it
   with SHA-256, fail the install if the hash doesn't match the declared
   checksum. Windows unpacks a zip; macOS a `.tar.gz` (via `tar` +
   `flate2`).
5. Extract — unpack into the per-user store. Windows zips carry a
   leading directory and `find_java_home` walks up to 3 levels to locate
   `bin\java.exe`; macOS tarballs land as `<version>/Contents/Home`, and
   the same walk finds `bin/java`.
6. Re-discover — point discovery at what was just installed and re-run the
   discovery chain. The launcher continues as if Temurin had been there
   all along.

**On macOS the hand-off is an `execve` of the launcher itself.** Showing
those dialogs required an `NSApplication`, and creating one before JavaFX
does costs the launched app its menu bar. `execve` replaces the image —
destroying that application object — while the PID, audit session and
LaunchServices registration survive, so it is a hand-off rather than a
relaunch: one process, one Dock tile. The trigger is *a dialog was shown*,
not *a JDK was installed*; keying it to the install path alone left the
decline and failure paths launching into a poisoned process.

A live progress dialog drives the download. On Windows that is a
`TaskDialog` with `TDF_SHOW_PROGRESS_BAR` + a `TDN_TIMER` callback; on
macOS it is a plain window with an `NSProgressIndicator` and a status line
rewritten every tick with `Downloaded X MB of Y MB (Z%)` /
`Verifying SHA-256… (Z%)` / `Extracting… (Z%)`.

If `api.adoptium.net` itself is unreachable, the user sees a separate
"Could not reach Server - Snug" dialog with **Open the download page /
Cancel** so they can still install a runtime manually from the fallback
URL.

### The cache path is not `%LOCALAPPDATA%` on macOS

| | Windows | macOS |
|---|---|---|
| app cache + log | `%LOCALAPPDATA%\snug\<company>\<app>\<jar-sha256>\` | `~/Library/Caches/snug/<company>/<app>/<jar-sha256>/` |
| installed JDKs | `%LOCALAPPDATA%\snug\jdk\` | `~/Library/Application Support/snug/jdk/` |
| the only hardcoded macOS subpath | — | `Library/Caches`; everything above it is dynamic |

`Caches` for the extractable cache and `Application Support` for the
installed JDK is deliberate — one is regenerable from the payload, the
other is not.

## Per-launch log file

Every launch writes a structured trace to the per-user cache, alongside
the cached `app.jar`:

| | path |
|---|---|
| Windows | `%LOCALAPPDATA%\snug\<company>\<app>\<jar-sha256>\snug.log` |
| macOS | `~/Library/Caches/snug/<company>/<app>/<jar-sha256>/snug.log` |

The file is truncated on each launch so each session produces a
self-contained record -- with one exception, the macOS post-install
`execve` hand-off, which **appends** instead. That re-entered pass
re-derives the identical path, so a truncating open would erase the
download progress recorded by the pass that did the asking, which is
exactly the evidence a user wants when a download misbehaves.

```text
[1758370000] snug-launcher starting — log file: ...
[1758370000] app: SynapticLoop / Snug JavaFX Demo
[1758370000] cache_root: C:\Users\Admin\AppData\Local\snug\SynapticLoop\Snug JavaFX Demo
[1758370000] primary JAR sha256: abc…64hex…
[1758370000] cached jars: 1
[1758370000] min-java: 25
[1758370000] download-jdk mode: Force
[1758370000] main class: synapticloop.snugjavafxdemo.Launcher
[1758370000] JVM discovery: skipped (download-jdk=force)
[1758370000] JDK install flow starting; root: C:\Users\Admin\AppData\Local\snug\jdk
[1758370000] JDK cache miss under ...; need min_java=25
[1758370000] Adoptium metadata: version=25.0.4+101.0.LTS, size=141167264 bytes
[1758370000] Adoptium package link: https://github.com/adoptium/...
[1758370000] Adoptium SHA-256: 00c847d8…
[1758370000] phase 0 (download): url=..., expected_size=141167264 bytes
[1758370000] phase 0 complete: 141167264 bytes written to ...
[1758370000] phase 1 (verify SHA-256) starting
[1758370000] phase 1: computed SHA-256 = 00c847d8…
[1758370000] phase 1: SHA-256 verified
[1758370000] phase 2 (extract): zip=... → dest=...
[1758370000] phase 2: extracted JDK home = ...\jdk-25.0.4.1+1
[1758370000] JDK install flow succeeded: ...
[1758370000] using JAVA_HOME: ...
[1758370000] loading jvm.dll from: ...\jdk-25.0.4.1+1\bin\server\jvm.dll
```

Lines are mirrored to stderr so debug builds (console subsystem) see
the trace live; on release builds (GUI subsystem) stderr is detached
and the file is the only visible record.

## Editable dialog text

Every user-facing string the launcher shows lives in one place:
`snug-format/assets/snug-localisations.en.txt`. That covers the error
messages (`err.*`), the splash strings (`splash.*`), the JDK-install
errors (`jdk.err.*`) **and** the dialog chrome — titles, headings,
info-box copy, progress text and button labels (`runtime.*`,
`generic.*`, `launcher.error.*`). Edit the file, rerun
`scripts\windows\build.cmd`, and the rebuilt EXE picks up the change
with no Rust edits needed.

The catalog lives in `snug-format` because the CLI and the launcher both
need it and `snug-format` is the one crate they both depend on. It is
`include_str!`d exactly once in the whole workspace and re-exported as
`snug_format::DEFAULT_EN_TEXT`, so the copy baked into a payload and
the copy compiled into the launcher's in-binary fallback are the same
bytes by construction.

```text
runtime.prompt.title = Runtime Required - Snug
runtime.prompt.heading = Runtime missing
runtime.prompt.subheading = The required runtime was not found on this machine.
runtime.prompt.content = This application needs a runtime to launch. Snug can download the runtime for you (~{size_mb} MB) and install it.
runtime.prompt.info_subtext = The runtime is installed for your user account, so it does not need administrator rights.
runtime.prompt.button_download = Install the runtime now
runtime.prompt.button_cancel = Cancel

runtime.metadata_failed.title = Could not reach Server - Snug
runtime.metadata_failed.content = Technical detail:\n{error}
```

The dialog chrome is deliberately **vendor-neutral** — it says "the
runtime", not "Eclipse Temurin". That is what lets an application
reword the whole window for itself, which is the reason the chrome
lives in the same bundle as the errors. The `err.*` and `jdk.err.*`
strings stay technical and do still name Java, because they are
diagnostics rather than chrome.

Values are single-line. A real newline is a literal `\n`, a literal
`#` must be written `\#`, and `#` otherwise starts a comment. Templates
use `{name}` placeholders (no `{name:.spec}` formatters — apply
formatting to values in Rust before substitution). Unknown placeholders
are left intact so a typo never silently swallows text.

### Placeholders are part of the contract

A bundle has to match the baseline in **two** ways: the same keys, and
the same `{placeholders}` inside each value. Matching keys is what the
missing-key warning checks; matching placeholders is its own failure
mode, and a nastier one.

```console
$ snug --list-tokens
snug: localisation inventory for the built-in `en` baseline
      107 keys, 42 carry placeholders, 25 distinct tokens
...
  runtime.prompt.content  {size_mb}
  runtime.retry.subheading  {attempt, max_attempts}
  launcher.error.content  {error}
```

`snug --list-tokens` prints that whole table offline — no JAR, no
options file, no build. `snug --init-localizations --init-localizations-tag
de` then scaffolds a bundle whose values already carry the right slots,
so you translate rather than reconstruct.

`REPLACEMENT_TOKENS.md` at the repo root is the generated per-key
contract: which slots each key may use, which of them today's English
copy actually uses, and which keys may use none at all. It is emitted by
`snug_format::tokens::replacement_tokens_markdown()` and held to that
function by a test, so it cannot drift from the table behind it. The
"may use" column is deliberately *wider* than "English copy uses" — a
translator may reach for any slot the dialog really does supply, and the
one thing that is never allowed is a slot outside that list.

Why the care: the launcher substitutes by literal replace and leaves
unknown slots alone. A bundle that keeps `{version}` for a key that
dropped it renders perfectly, right up until the matching call site
stops passing `version` — and *then* the end user sees `{version}` in a
dialog. Nothing goes red in between. So `snug` warns at build time:

```console
$ snug App.jar --localization localisations/
snug: warning: localization file `fr` (tag `fr`) uses different placeholders
than the built-in English baseline on 2 key(s):
snug:   - runtime.progress.pct_label: baseline {pct}, bundle (none)
snug:       present in the baseline but missing here, so the translated sentence
             silently loses that value: {pct}
snug:   - runtime.retry.subheading: baseline {attempt} {max_attempts},
         bundle {attempt} {max_attempts} {version}
snug:       not in the baseline, so nothing will fill them and they print
             literally: {version}
snug:   (run `snug --list-tokens` for the full baseline contract)
```

Advisory, like the missing-key warning — the build still succeeds,
because the launcher falls back to English and a half-translated bundle
is better than a failed build. Both directions are reported, since the
silent one (a slot the baseline has and the translation dropped) is the
one that produces a sentence missing a fact with nothing to look wrong.

The `err.*` and several `jdk.err.*` keys use positional `{0}` rather
than a named slot; those are substituted by index, not name, and a
translation keeps its `{0}` in the same position.

Because the chrome is in the same bundle as the errors, `--localization`
translates the whole window, not just its contents — a localised build
gets a translated title *and* a translated "Cancel" button, and an app
can reword the chrome for itself ("Acme requires Java 25") by shipping
its own `en` bundle. The only copy that can't be overridden this way is
the fallback `MessageBoxW` title, which is rendered before the payload
is decoded.

`snug_launcher::dialogs::Dialogs` is assembled from these keys rather
than deserialized, so all ~16 call sites keep compile-checked field
access (`dialogs().runtime.prompt.title`) and the key literals live
in exactly one place. A lookup that matches nothing in the chain returns
the key itself, so the
`every_localize_key_is_in_the_baseline` test in `localize.rs` keeps that
from shipping — add new lookups to its `KEYS` list.

## Licence

Dual-licensed under MIT OR Apache-2.0, at your option.

---

<div align="center">
<img src="assets/snug-icon.png" alt="snug" width="256">
<p><strong>Say Hello to Snug.</strong></p>
</div>

---
