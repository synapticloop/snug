# snug — Project context for AI agents

## What this project is

`snug` is a small, open-source Rust CLI that turns a Java fat JAR into a
single native Windows `.exe` launcher. It does **not** bundle a JVM; the
target machine is expected to have a compatible Java install (default
target: Java 25).

The intended workflow is:

```text
MyApp-fat.jar + metadata + icon + (splash)
            │
            ▼
        snug
            │
            ▼
        MyApp.exe   (GUI subsystem, 64-bit)
            │
            ├── embedded fat JAR
            ├── embedded icon
            ├── optional splash
            ├── locate Java 25+ (JAVA_HOME, JDK_HOME, PATH, registry, common)
            ├── locate jvm.dll
            ├── extract/cache fat JAR under %LOCALAPPDATA%\snug\<company>\<app>\<sha256>\
            ├── load jvm.dll directly via JNI
            ├── JNI_CreateJavaVM(...)
            └── invoke Java main class
```

The end result appears as `MyApp.exe` in Windows Task Manager, not as
`javaw.exe` — Snug loads `jvm.dll` directly rather than spawning
`javaw`.

## Scope — what snug is NOT

- not an installer (use the JVM's `jpackage` or similar)
- not a JVM distribution system
- not a `jpackage` replacement for installers
- not a `jlink` replacement
- not a Java dependency resolver / Maven front-end
- not a GraalVM native-image compiler

Snug's responsibility starts at "working fat JAR" and ends at "Windows
EXE". Building the fat JAR (including bundling JavaFX classes,
resources, and native libraries) is the caller's job.

## Architecture (planned)

```text
snug/
├── crates/
│   ├── snug-cli/         # CLI binary (clap)
│   ├── snug-builder/     # produces the EXE (template-per-build v1, stub v2)
│   ├── snug-launcher/    # runtime: JVM discovery, JNI load, splash, main invoke
│   ├── snug-format/      # shared embedded-payload types + codec
│   └── snug-dropper/     # "Build with Snug" — beginner drag-and-drop shim
├── examples/
└── tests/
```

A single-crate implementation is fine initially; split only when useful.
**Slice 1 (this commit)** uses a 2-crate workspace (`snug-format` +
`snug-cli`); the launcher and builder will be added as `snug-launcher` +
`snug-builder`.

`snug-dropper` is deliberately **not** folded into `snug-cli`. It is a
GUI-subsystem Windows binary that shells out to `snug.exe`, so linking it
would drag `clap` / `editpe` / `image` into a ~240 KB shim that needs
none of them.

## Build host

### Builds are native, never cross-artefact

**A platform's artefacts are built on that platform.** Windows targets are
built on Windows; macOS targets are built on macOS. There is no
cross-artefact toolchain and none is wanted.

This is not a limitation, it is what makes the precompiled-stub design
work at all. `snug-cli` embeds its launcher with `include_bytes!`, so the
launcher must exist *before* the CLI compiles. Built natively and
sequentially that is trivial — `scripts\build-release.cmd` step 3 builds
`snug-launcher` and copies it to `bin/launcher-stub.exe`, then the CLI
compile picks it up, and `scripts\verify-embedded-stub.ps1` proves the two
agree. Cross-compiling instead means maintaining a foreign toolchain
(`zig` / mingw) purely to refresh a binary that the target machine could
have produced itself, and `cargo-zigbuild` additionally has to track a
compatible `zig` version — a real source of breakage for no benefit. The
committed binary is a *bootstrap convenience* so a fresh clone can
`cargo build -p snug-cli` without first building the launcher; it is not
the source of truth. Each release pipeline regenerates it.

`cargo-zigbuild` therefore remains useful only as an optional
local-iteration convenience on a non-Windows box, and is not on the
release path. Nothing in the release process depends on it.

### Windows

- **CI / releases:** GitHub Actions `windows-latest` runner, so
  Authenticode signing is available.
- **Native Windows host:** no cross-compilation needed at all —
  `cargo build --release -p snug-launcher` produces the stub directly
  against the installed `x86_64-pc-windows-msvc` toolchain, and the
  produced EXE can be launched locally to exercise the runtime
  (extraction, JVM discovery, cache GC) against a real JDK.

```powershell
# regenerate the committed stub, then the CLI that embeds it
cargo build --release -p snug-launcher
Copy-Item target\release\snug-launcher.exe bin\launcher-stub.exe
```

### The `snug` CLI also builds and runs natively on macOS / Linux

Producing *Windows EXEs* is the job, but `snug` itself is an ordinary Rust
CLI that compiles for the host: `cargo build -p snug-cli`, `cargo run`, and
`cargo test --workspace --lib --bins --tests` all work on macOS and Linux,
and nothing in it is Windows-specific — it reads a JAR, writes a PE file,
and calls no OS API. The whole workspace builds there too, which matters
because macOS is where snug is *developed* (the launcher runtime is what
needs Windows).

`scripts/build-macos.sh` is the release path. It is not a replacement for
`scripts\build-release.cmd` — that one is the Windows pipeline (launcher
stub, dropper, demo JAR, icon stamping) and needs a native Windows host.
This one is macOS-only and touches nothing Windows produces.

```bash
scripts/build-macos.sh            # both arches, staged into release/
scripts/build-macos.sh --clean    # wipe release/macos-* first
```

`scripts/build-macos-demo.sh` is the demo half — the macOS counterpart of
step 4 plus the staging block in `scripts\build-release.cmd`. It calls
`build-macos.sh` (unless `--skip-cli`), then has the freshly built `snug`
package `assets/snug-javafx-demo-macos.jar` as a real
`assets/snug-javafx-demo.app` and stage it alongside the JAR. The demo is
built by the arch-matched `snug`, so a demo can never be an `.app` for the
wrong architecture.

It then **verifies** the artefact rather than assuming it is right, and
the first check is the one that catches the mistake this pipeline
originally shipped: `[[ -d "$DEMO_APP" ]]`. A `.app` must be a
*directory*; a flat file with a `.app` name is not a bundle and cannot
launch. The rest confirms the directories are traversable, the launcher is
executable and the host's arch, the deployment floor is right, `plutil`
likes the `Info.plist`, `codesign --verify` passes, and the payload is
present.

It also pre-flights the *runtime*, which is not the same as the build
succeeding. The demo classes ship as class file version 69.0 (Java 25) and
`snug` refuses anything below `--min-java`, so on a machine whose newest
JDK is older the script warns that the `.app` will not launch — rather
than leaving you to discover it from a window that never appears.

Two bash notes, because macOS still ships **bash 3.2**: expanding a
possibly-empty array under `set -u` (`"${arr[@]}"`) is an *unbound
variable* error there, so use the `${arr[@]+"${arr[@]}"}` form or an
if/else; and `cp -R` into an existing directory of the same name copies
*inside* it, quietly producing `x.app/x.app`.

```bash
# what it does, if you ever need to do it by hand:
rustup target add aarch64-apple-darwin
export MACOSX_DEPLOYMENT_TARGET=12.0
cargo build --release -p snug-cli --target x86_64-apple-darwin
cargo build --release -p snug-cli --target aarch64-apple-darwin
```

### Where the macOS launcher comes from

Same model as the Windows stub, and native-only building is what makes it
cheap: `build-macos.sh` builds `snug-launcher` for the target, copies it
into `bin/` as `launcher-stub-macos-<arch>`, and *then* compiles
`snug-cli`, which embeds it. The committed binary is a bootstrap
convenience exactly as `bin/launcher-stub.exe` is, not the source of
truth.

**One launcher per `snug` binary, matching its own arch.** The embedded
path is selected by `#[cfg(target_arch)]`, so `release/macos-arm64/snug`
embeds the arm64 launcher and `release/macos-x86_64/snug` embeds the
Intel one. Each binary can only build a `.app` for its own
architecture, which is a feature rather than a limitation: a Mac never
has to reason about a foreign Mach-O, and each artefact carries exactly
one launcher instead of two.

**The `.app` payload is a sibling file, not a stamped resource.** On
Windows the payload is written into the PE as `RT_RCDATA` via `editpe`,
which is why `editpe` sits in the build path. A macOS `.app` is a
*directory*, so the launcher needs no stamping at all: `snug` writes the
encoded payload to `Contents/Resources/<App>.snugpayload` and copies the
launcher into `Contents/MacOS/<App>` as a plain byte-copy. Strictly less
work than the Windows model, easier to inspect (the payload file can be
hexdumped), and it sidesteps the fact that there is no Mach-O resource
writer in the dependency graph — `editpe` is PE-only.

**The macOS *icon* is embedded in the Mach-O, and written by hand.**
Having no resource directory leaves a bare executable with nowhere to get
an icon: a bundled `.app` reads `Contents/Resources/App.icns`, but
`cargo run` has no bundle, and **macOS does not read an icon out of a Mach-O
the way Windows reads `MAINICON`**. So `build.rs` emits

```text
cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__icns,<out>/snug.icns
```

and writes the ICNS itself:

```text
'icns'  →  u32 be total length  →  per chunk: 4-byte type, u32 be length, PNG bytes
```

- The chunk table was **derived, not remembered**: read a real
  `App.icns` back out of a built `.app` and dumped its chunk list. That
  showed `iconutil` stores 16/32px as raw `ARGB` (`ic04`, `ic05`) and
  appends an optional `bpli` chunk; the PNG forms are equivalent to Icon
  Services, so neither difference earns a subprocess.
- **It was `iconutil` first, and that was wrong.** The first version shelled
  out, as `macos_bundle.rs` does, and failed whenever it could not be given
  a temp directory — leaving a binary with no icon behind a
  `cargo:warning` everyone learns to scroll past. A cosmetic default must
  not be a silent failure mode. Pure Rust also means a failure here is a
  real bug, so it panics like the rest of the script.
- **The size cost is real and deliberate**: the macOS stubs went from
  ~0.6 MB to ~3.1 MB, almost all of it the 2.2 MB icon, and the launcher
  is `include_bytes!`-ed into the `snug` CLI, so each macOS `snug` grows by
  the same. The 1024px chunk is 1.2 MB of that; dropping it is the lever
  if it ever matters, at the cost of a softer icon on a Retina tile. The
  Windows `MAINICON` is tens of KB and unaffected.
- Binned rather than un-binned link args, because a Mach-O section belongs
  to an executable — which means the preview's *test harness* carries it
  too. That is the cost of not having per-bin control from `build.rs`.

### Release layout

Two thin per-architecture binaries, **not** a universal2. arm64 is the
future-proof slice; x86_64 is kept only for the four Intel models that top
out at macOS 26 Tahoe (MacBook Pro 16" 2019, MacBook Pro 13" 2020, iMac
27" 2020, Mac Pro 2019). A universal2 would cost ~2× on disk, and
**macOS 27 Golden Gate is the final release with Rosetta 2** — so an
x86_64-only artefact stops working on Apple Silicon in macOS 28 anyway.
That asymmetry is why "just ship the Intel one" is no longer the
maximally-compatible single answer it used to be.

The *binary name is always `snug`*; the platform lives in the directory:

```text
release/
├── windows-x86_64/            # snug.exe, Build with Snug.exe, snug_preview.exe, demo JAR
├── macos-arm64/snug
└── macos-x86_64/snug
```

That is the point of the subdirectory: every platform invokes `snug`, so
the documentation never has to name two different binaries. `arm64` is
`uname -m` on Apple Silicon, which is what someone reading a directory
listing will recognise — the Rust target is spelled `aarch64`.

**Windows uses the same convention.** `build-release.cmd` stages into
`release\windows-x86_64\`, derived from the same `TARGET_ARCH` the cargo
triple implies, so the two pipelines agree on the shape and a future
Windows-on-Arm64 slice needs no new convention. The `x86_64` spelling
matches the macOS Intel directory rather than `amd64`, which is what
`uname -m` and a Rust target both use. The Windows bundle holds more than
one artefact (`snug.exe`, `Build with Snug.exe`, `snug_preview.exe`, the
demo JAR), and the co-location invariant between the first two is why
they must be staged *together* — so the directory, not the file name, is
what identifies the platform. Note that `RELEASE_SUBDIR` is derived
*after* the triple is selected: with `EnableDelayedExpansion`, `!VAR!`
expands where it is written, not where it was assigned, so reading
`TARGET_ARCH` one block earlier yields `release\windows-\`.

`build-macos.sh` verifies each artefact's `lipo` arch and its emitted
`minos` *before* staging and aborts on a mismatch, because a wrong-arch
or wrong-floor binary sitting in `release/` is the failure hardest to
notice downstream. It also `codesign -s -` ad-hoc: arm64 refuses to
execute a binary with no signature at all, and saying it explicitly keeps
that true if the linker's default ever changes. It executes a binary only
when the artefact's arch matches the host, so an Intel dev box verifies
the arm64 slice structurally rather than pretending to run it.

**Deployment-target floor.** rustc's *defaults* — and hard minimums — are
macOS 10.12 on Intel and 11.0 on ARM64. Don't ship at them: 10.12 Sierra
(2016) has had no security updates for years, and Apple Silicon cannot go
below 11.0 at all. `snug` sets `MACOSX_DEPLOYMENT_TARGET=12.0` (Monterey)
on **both** targets, which collapses the Intel/ARM asymmetry into one
floor. Nothing in the dependency graph forces anything higher — every
`snug-cli` dependency is pure Rust, and `ureq` / `native-tls` is
`cfg(windows)` precisely so a macOS `snug` doesn't link Security.framework
for an update check it never performs.

Two traps, both verified the hard way:

- **`cargo` does not fingerprint `MACOSX_DEPLOYMENT_TARGET`.** Change it,
  rebuild, and you silently get the cached binary at the *old* floor — no
  warning. That is exactly why `build-macos.sh` checks `minos` instead of
  trusting the build; to prove a new value took effect, use a fresh
  `CARGO_TARGET_DIR`, then confirm with `otool -l … | grep minos`.
- **A lower `clang` default can silently *raise* the floor.** rustc passes
  its own deployment target to the linker, but a C dependency built by
  `clang` without `MACOSX_DEPLOYMENT_TARGET` inherits the SDK version, and
  the linker warns and keeps the higher one. Only a concern if a `*-sys`
  crate is ever added.

**Distribution is the real macOS cost, not the build.** arm64 refuses to
execute a binary with *no* code signature, which the linker satisfies with
an ad-hoc signature automatically. The friction is Gatekeeper: a user who
downloads a release tarball *in a browser* picks up
`com.apple.quarantine` and macOS blocks the run. Either ship via Homebrew
— the formula ad-hoc-signs on download, which is how GitHub CLI dodges
notarization — or sign with a Developer ID and notarize with
`xcrun notarytool` + `stapler` (Apple has required `notarytool` over
`altool` / Xcode 13 since November 2023). Until one of those exists,
document `xattr -d com.apple.quarantine snug`.

## Embedded-payload format (`snug-format`)

The launcher locates its data by scanning for the 8-byte magic
`SNUGEMBD`. Wire layout:

```text
+--------+----------------+--------------+--------------+----------------------+
| magic  | format_version | payload_len  | payload_crc32| payload (postcard)   |
| 8 B    | u16 LE         | u32 LE       | u32 LE       | payload_len bytes    |
+--------+----------------+--------------+--------------+----------------------+
```

- `format_version` is currently `1`. Bump on incompatible wire changes.
- `payload_crc32` is CRC32/IEEE of the postcard bytes; the launcher
  validates before decoding.
- Payload is postcard-encoded `SnugPayload { config, jar, icon }` where
  `config` carries `LauncherConfig` with `AppMetadata`, `SplashConfig`,
  and `LauncherBehavior` (which embeds `JvmDiscovery`).

This layout is intentionally stub-friendly: the same encoded blob can be
`include_bytes!()`-embedded (template-per-build, v1) or appended to a
precompiled `launcher-stub.exe` (v2).

## Development slices

| # | Slice                                          | State      |
|---|------------------------------------------------|------------|
| 1 | CLI surface + embedded-payload format          | **done**   |
| 2 | Windows launcher runtime (JVM discovery + JNI) | **done, and verified on a Windows machine.** Payload self-scan (`RT_RCDATA`), cache path, manifest lookup, JVM discovery (env / PATH / registry / common), `jvm.dll` discovery, JNI (`JavaVM::with_libjvm` + `get_static_method_id` / `call_static_method`, with the JavaFX `Application.launch` fallback), and the bare-stub message are all wired. There is **no `JniStub` error** — that variant no longer exists; the earlier note claiming the launch path was stubbed was stale. A ~1.1 MB precompiled `bin/launcher-stub.exe` (PE32+ GUI x86-64) is committed and regenerated by `scripts\build-release.cmd`. |
| 3 | snug-cli builder: load stub via `include_bytes!()`, append payload, stamp icon/version/manifest via `editpe` | **done** — `snug app.jar -o App.exe` produces a real Windows `.exe` end-to-end. `include_bytes!` of the committed stub + encoded payload concatenation is unit + integration tested. Resource stamping is in-process via the [`editpe`](https://github.com/Systemcluster/editpe) crate (pure Rust, BSD-2-Clause, cross-platform — same code path runs on macOS, Linux, and Windows). `--icon` accepts PNG or ICO; `--manifest <XML>` embeds an arbitrary application manifest. |
| 4 | Stub-append v2 hardening (alignment, overlay vs append, sparse stubs), per-app resource stamping UX, CI on `windows-latest` to actually run the produced EXE against a real JDK | planned |
| 5 | Windows version-resource stamping              | **done** — `editpe` stamps `VS_VERSIONINFO` (ProductName / CompanyName / FileDescription / LegalCopyright) plus `FixedFileInfo` (signature, file/product version, VFT_APP) into every produced EXE. `--manifest <XML>` covers the application manifest gap. Icon dimension / MUI / translation coverage is follow-up. |
| 6 | Native splash renderer (PNG via GDI+/WIC)      | planned    |
| 7 | Per-user cache + old-version cleanup           | **done** — cache entries expire. `cache::touch` stamps a cached JAR's mtime on **every** launch that uses it, and `cache::sweep` runs off a detached thread (rate-limited to once per 6h by a `.sweep` stamp file) to delete the ones no longer wanted. Policy: keep the most recently used `KEEP_RECENT` (3) entries — the running build's own entries count toward that depth — up to a per-app byte budget of `clamp(build_bytes * 3, 512 MB, 8 GB)`, and evict anything untouched for `MAX_UNUSED_AGE` (30 days) regardless of rank. The current build is never evicted; everything else is recoverable because a missing entry is re-extracted from the payload on the next launch. Sweeping is *not* triggered by the builder — `snug-cli` never writes to the cache, so the launcher is the sole owner of the policy. |
| 8 | GitHub Actions CI (windows-latest release)     | planned    |
| 9 | `Build with Snug` beginner drag-and-drop shim   | **done** — separate `crates/snug-dropper` crate. Drop a `.jar` or a folder of JARs on `Build with Snug.exe` and it runs `snug.exe --name "Example Application Name" --company "Example Company Pty Ltd" -o "<parent>/Example Application Name.exe" <input>` behind an indeterminate marquee on a worker thread, then reports with a `MessageBoxW`. Double-click opens a command window in the EXE's own folder. Two or more items, or one item that is neither a `.jar` nor a directory, get a dialog and *then* the terminal as a hand-off. A build failure or a missing `snug.exe` gets an error dialog and **no** terminal. Built as `snug-dropper.exe`; renamed to `Build with Snug.exe` at packaging time. Must ship in the same folder as `snug.exe`. |
| 10 | Native macOS / Linux `snug` CLI builds    | **done** — `cargo build --workspace` and `cargo test --workspace --lib --bins --tests` pass on macOS (283 tests), and the CLI produces a real `PE32+ executable (GUI) x86-64` end-to-end from a Mac. `scripts/build-macos.sh` stages two thin per-arch binaries into `release/macos-arm64/snug` and `release/macos-x86_64/snug` at a macOS 12.0 deployment floor — same binary name on every platform, platform in the directory, so docs never name two binaries. No universal2: macOS 27 is the last release with Rosetta 2, so arm64 is the future-proof slice and x86_64 only serves the four Intel models that top out at macOS 26. Windows-only surfaces are gated: the six Win32 GUI modules in `snug-launcher` use file-level `#![cfg(windows)]` (joining `jdk_install.rs` / `splash.rs`), while the three Windows-only *binaries* use per-item `#[cfg(windows)]` plus a real non-Windows `main`. `editpe` was made unconditional (pure-Rust PE parsing — `snug-cli` needs it to build EXEs anywhere) and `ureq` Windows-only. Homebrew tap / Developer ID notarization is follow-up; see "Build host". |
| 11 | macOS launcher runtime (`libjvm.dylib` + JNI) | **done, verified against a real JDK on macOS.** `platform/macos.rs` mirrors `platform/windows.rs` step for step: cache extraction + sweep, log, `Main-Class` resolution, JVM discovery, `libjvm.dylib` load via the same `jni` 0.22 invocation API, and the `main(String[])` / JavaFX `Application.launch` dispatch. `tests/macos_launch_e2e.rs` compiles a real class with `javac`, packages it with `jar`, hands it to the launcher, and asserts the JVM actually runs it — it is skipped rather than failed when no JDK is present. **Not implemented:** splash, the Adoptium download flow, and error dialogs (all Win32); `DownloadJdkMode::Auto`/`Force` degrade to discovery-only and *log* that. |
| 12 | macOS `.app` bundle emitter | **done, verified by launching a real bundle.** The output extension selects the artefact: `-o MyApp.app` builds a macOS bundle, `-o MyApp.exe` a Windows one, and the *default* is the host's own — `snug app.jar` with no `-o` gives `app.app` on macOS and `app.exe` elsewhere. `macos_bundle.rs` writes `Contents/{Info.plist,MacOS/<App>,Resources/<App>.snugpayload,Resources/App.icns}` and ad-hoc signs it. The launcher is a **byte-copy** — unlike the Windows stub there is nothing to stamp, because a Mach-O has no resource directory and `editpe` is PE-only. `bin/launcher-stub-macos-<arch>` is embedded per `#[cfg(target_arch)]`, so each `snug` binary carries one launcher and can only build a `.app` for its own arch. |
| 13 | macOS Adoptium JDK download | **the flow works; the dialogs do not exist yet.** `jdk_install.rs` now compiles on macOS. `adoptium_target()` sends this host's `os`/`architecture` in Adoptium's own spelling, macOS assets unpack as `.tar.gz` (via `tar` + `flate2`), `find_java_home` understands the `*.jdk/Contents/Home` bundle and `bin/java`, and `open_in_browser` uses `/usr/bin/open`. Verified live: the query returns the correct `x64_mac_hotspot` asset, and the tarball/extraction path is covered by tests that build a real `.tar.gz` in Adoptium's shape. **Still Win32:** the splash, the download dialogs, and the error dialog — so `Auto` logs that it has nothing to ask with and falls back to discovery, while `Force` downloads and logs its progress. See the Backlog for the AppKit route. |

## Backlog

Deferred work, deliberately not done. Kept here rather than in
`HANDOFF.md`, which is a dated session snapshot pinned to a specific
commit — useful history, not a live list.

**Windows**
- **The Windows progress window never substitutes `phase_label`'s
  `{arch}`.** `progress_window.rs` passes `phase_label` to its `STATIC`
  control raw, so the initial text is literally
  `Downloading runtime ({arch})` — the placeholder is shown to the user
  rather than a platform name. macOS does fill it, from
  `jdk_install.progress.arch_macos_*`. The fix is to give Windows its own
  `arch_windows_*` keys and fill the control in `progress_status`'s
  equivalent, but it needs a real Windows run to confirm, so it is left
  rather than changed blind from a Mac.
  - Worth doing with the same care as the macOS half: an earlier version
    hardcoded `Windows x64` in the *shared* `phase_label`, so macOS
    announced itself as a Windows download. Splitting the value out fixed
    that, and left Windows without a value at all — the same bug one step
    further on.
- **Refresh `bin/launcher-stub.exe` on a Windows host.** Run
  `scripts\build-release.cmd`. Not urgent: every change since has been a
  `cfg(windows)` no-op, so the committed stub is functionally current.
  It is worth doing anyway as a proof that the committed binary really
  came from the current source.
- **Install `mingw-w64` and do a real Windows *link* build from macOS**
  (`brew install mingw-w64` gives `dlltool` + `windres`). This is
  *verification only* — the stub must still be built on Windows, per the
  native-only policy. Today we have `cargo check --all-targets` for the
  Windows target but no link, so a link-only problem would not be caught
  from a Mac.
- Note for whoever tries: a launcher cross-built from macOS gets **no
  `MAINICON`**. `embed-resource` finds no resource compiler, emits no
  `icon.lib` and no `cargo:rustc-link-arg`, and does not error — it
  silently produces an iconless binary. That is another reason the stub
  is a Windows build product.

**macOS**
- **The macOS dialogs are real `NSWindow`s — deliberately not `NSAlert`s.**
  `appkit.rs` is the macOS half of the `jdk_install::ui` seam. The
  metadata-failed, retry, terminal-failure, **install-consent** and
  download-progress windows are done; only the **launcher error window**
  (`error_window`, called from `main.rs`) is not. Reuse the same
  `dialogs()` strings — nothing in the seam formats a string of its own, so
  a `--localization <tag>` bundle translates the macOS dialogs exactly as
  it translates the Windows ones.
  - This reversed an earlier decision, and the reason is worth keeping.
    The progress window was an `NSAlert` because an alert gets a real
    window, correct focus and a working close box for free and needs no
    delegate. But `NSAlert` is a **modal**: it runs its own nested event
    loop, and the download gated the worker thread on a human clicking
    "Install". So a click in a modal alert was the only thing that could
    start a download, with no timeout — and a `.app` `execve`'d from a
    terminal has no GUI session for one. It degraded to logging, and
    degrading to logging still left the worker spinning. That is the
    deadlock recorded below; a plain window is what makes the two
    properties structural instead of accidental.
  - A plain `NSWindow` gets you no button responses for free, which is the
    cost: `NSControl::setTarget:`/`setAction:` are `unsafe fn`, so wiring a
    button to a selector means declaring an Objective-C class with
    `define_class!`. Hence two tiny stateless responders in the file —
    `ModalResponder` (button `tag` → `stopModalWithCode`, and
    `windowShouldClose` → a `DISMISSED` sentinel) and `ProgressResponder`
    (latches a cancel flag). **This is the one sanctioned departure from
    "no `unsafe` in any crate"**, and it is why `snug-launcher` carries
    `#![deny(unsafe_op_in_unsafe_fn)]` rather than `forbid(unsafe_code)`.
    A defined class has no safe `&AnyObject` conversion in objc2 0.6 (no
    blanket `AsRef<AnyObject>`), so the target is set through
    `Retained::as_ptr`.
  - `NSModalResponse` is an `isize` alias, not a newtype, and AppKit
    numbers buttons from 1000 — but with a hand-built window the button's
    `tag` *is* the response, so there is no 1000-offset to remember.
    `DISMISSED` is `-1` and every real index is `>= 0`, so a close box can
    never be mistaken for "button 0" — which for `retry` means "download it
    again".
  - Layout is explicit frames, not Auto Layout. A handful of constants is
    easier to reason about than a constraint graph for fixed-size windows.
    Buttons are placed by hand right-to-left because a horizontal
    `NSStackView`'s alignment constants describe *gravity*, not
    distribution, so asking it to right-align is a fight with the wrong
    tool.
  - The progress run loop is still pumped by hand with
    `NSRunLoop::runUntilDate`: it is not a modal session, so progress can be
    sampled between event turns, and `objc2-app-kit` 0.3.2 binds no
    `NSTimer`. Its Cancel button is labelled `prompt.button_cancel`, **not**
    `progress.cancel_button_during_download` (which reads "Install",
    because on Windows that label belongs to the *prompt* window where
    pressing it means "go ahead" — here the download is already running,
    so "Install" would be a lie).
  - `jdk_install.progress.phase_label` used to hardcode
    "Downloading runtime (Windows x64)" in the *shared* baseline, so a
    macOS download announced itself as a Windows one. It now takes
    `{arch}` and each platform fills it in. **Watch for this class of
    bug**: a platform-specific string in the shared baseline, or a wrong
    substitution key, shows up in the user's copy rather than in a
    compiler error.
  - It looks like a macOS dialog, *not* like the Windows one. That is a
    deliberate choice, not a compromise: AppKit supplies correct HiDPI,
    accessibility, keyboard focus and system integration, and a
    CoreGraphics re-implementation of `modal_window.rs` would have to
    re-earn each of those by hand. If pixel parity ever becomes the
    requirement, that is a different and much larger piece of work.
  - `NSAlert::new` takes a `MainThreadMarker`, so UI must be on the main
    thread. `run()` runs there and the *download* is on a worker thread,
    so it lines up — but when the marker is absent, every entry point
    degrades to logging rather than constructing a window from a thread
    that must not. That degradation returns **cancel**, not retry: a
    silent loop through a few hundred megabytes of downloads is exactly
    what looks like a hang.
  - A real `NSAlert` blocks until a human answers, so the module carries
    a test seam: `set_test_response` makes dialogs answer on demand and
    `last_shown` records what *would* have been displayed. The seam is
    process-global, so the tests in that module are serialised by a mutex
    — without that they overwrite each other's recording.
  - Placeholder keys are the localisation baseline's, not invented:
    `metadata_failed` fills `{major}`, **not** `{min_java}`, and getting
    that wrong leaves `{major}` visible in front of the user.
- **A macOS splash.** Win32 GDI+ / WIC today; CoreGraphics / CoreText if
  it is to match, or an `NSImage` view if it is to look native.
- **The `execve` hand-off is keyed to *a dialog was shown*, not *a JDK was
  installed*.** Getting that wrong was a bug, and one that only a person
  clicking Cancel could find. `run_event_loop()` is deliberately
  `NSApplication`-free so glass can create `NSApplicationFX` itself, but
  the Adoptium dialogs reach `activate_app()` — and therefore create an
  `NSApplication` — **before the JVM exists**, and that is irreversible.
  `auto` never gets there (it returns early rather than downloading
  unasked), which is why the demo looks fine by default.
  - The first version fired the hand-off only on `Ok(Some(home))`. Every
    other post-dialog path fell straight through and launched the app in a
    poisoned process, which reads as a dead window: glass sees
    `isEmbedded == YES`, demotes the process, and the user gets no menu
    bar, a bouncing Dock icon, and a window that will not take focus. All
    three paths — installed, **declined**, and **failed** — show a dialog
    first, so all three now hand off. See `hand_off_after_dialog` and
    `appkit::owns_app`.
  - **The lesson is the shape, not the bug.** The path that was tested by
    hand (a real download) and the paths that were only reasoned about
    (decline, failure) are not the same path, and the difference only
    appears when someone actually cancels something. Any hand-off keyed to
    an *outcome* rather than to the *side effect* that poisons the process
    will miss its other cases.
  - There is no way out by being careful about *when* snug creates the app:
    any pre-JVM AppKit window needs an `NSApplication`, and the first
    `+sharedApplication` wins permanently. Reordering cannot help.
  - **The answer is `execve` of the launcher itself.** The kernel replaces
    the image, which destroys the Objective-C state including the
    `NSApplication` just built, while the PID, audit session and
    LaunchServices registration survive — so it is a hand-off, not a
    relaunch: one process, one Dock tile, frontmost position kept.
    `exec_self_with_java_home` does it, guarded by `SNUG_RESUMED_AFTER_INSTALL`
    so the resumed pass discovers rather than offering the install a second
    time. The common path — a JVM already installed — never re-execs,
    because no dialog was shown.
  - **Two details that are easy to get wrong and were not obvious.** The log
    is opened with `log::init_appending` on the resumed pass: the path is a
    pure function of the app metadata and the primary JAR hash, so both
    passes compute the same file and a truncating `init` would erase the
    download progress — the record a user most wants when a download
    misbehaves. And if `exec` itself fails, the launcher logs that the menu
    bar will be missing and **carries on in-process**, because a working
    launch with a broken menu bar beats no launch at all.
  - How to check it, since `isEmbedded` is not visible from Java: build the
    demo with `--download-jdk=force`, confirm the log shows
    `handing off: re-exec …` and then, in the *resumed* pass,
    `resumed after the JDK install; discovering rather than offering again`,
    and then the two things that only exist when glass won the race — the
    **application menu** and the app's own `MenuBar` hand-over. To be
    certain rather than plausible, `lldb -p <pid>` and `po [NSApp class]`
    names `NSApplicationFX` when it is right and plain `NSApplication` when
    it is wrong.
- **Homebrew tap, or Developer ID + notarization.** Until one exists,
  document `xattr -d com.apple.quarantine snug`. See "Build host".
- **A fully dynamic macOS cache path.** Today the only hardcoded part is
  the `Library/Caches` subpath. The completely dynamic answer is
  `NSSearchPathForDirectoriesInDirectories`, which `objc2-foundation`
  0.3.2 does not bind — it would mean hand-declared FFI plus the `objc2`
  runtime, for the identical string. Revisit only if Apple actually
  moves the location.
- **`--bundle-id` for a real reverse-DNS prefix.** `CFBundleIdentifier`
  is currently derived from the company and app names
  (`com.synapticloop.snug-demo`). macOS treats it as a uniqueness key,
  so a vendor with an actual domain should be able to set it.
- **macOS splash, error dialogs, and the Adoptium download flow** — the
  Win32 three. Until then the launcher logs to the per-launch file and
  stderr.

**Docs**
- **README section for running on macOS**, and a note that the release
  layout is `release/<os>-<arch>/snug` so the binary name is the same
  everywhere.

**Tests**
- **`pid` + `as_nanos()` is not a unique test-directory key.** The
  macOS clock is coarse enough that two tests running in parallel can
  read the same tick, they share a directory, and the failure surfaces
  as a *wrong value* rather than a collision. Every `tempdir()` helper
  now carries a per-process `AtomicU64` counter for this reason; the
  remaining Windows-gated one is `snug_preview/windows_impl.rs`, which
  cannot affect a macOS run. When adding a helper, copy the counter
  form from `crates/snug-cli/src/options_file.rs`, whose comment
  records the whole mechanism. `jdk_install.rs` looked like a second
  case for the same reason, but that stopped being true when the
  Adoptium flow became cross-platform (slice 13) — it now compiles and
  runs on macOS, and its flake did land. Assume a module is live on
  macOS unless you have checked its `cfg`, not because a stale note
  says so.
- **An options file's `--input` is a default, not a lock.** The
  positional `[JAR|DIR]` and `--input` are declared `conflicts_with`
  each other, which is about a single command line; across sources the
  merge resolves it as precedence, because "the command line always
  wins" has to mean that too. So `snug.options` can carry `--input`
  for a bare `snug`, and `snug other.jar` still builds the other one.
  Two spellings of one slot, one `FlagSpec` rule — see
  `FlagSpec::cli_has_positional`.

## Versioning

Snug is **pre-1.0 and stays that way until the project is declared
finished.** Bump the version on every change; never land on `1.0.0`
by accident.

- **Single source of truth:** `[workspace.package] version` in the
  root `Cargo.toml`. All four crates inherit it via
  `version.workspace = true`, so that one line is the only thing to
  edit. `snug --snug-version` and `cargo metadata` read the compiled
  value via `env!("CARGO_PKG_VERSION")` in `snug-cli` (both the clap
  `version` attribute and the `--snug-version` flag). **`snug-launcher`
  does not embed the version at all** — which is why a version bump never
  invalidates `bin/launcher-stub.exe`, and why refreshing the stub is
  hygiene rather than a consequence of a release.
- **Default increment is the micro (patch) number:**
  `0.2.0` → `0.2.1` → `0.2.2`. Use this for fixes, copy changes,
  refactors, and anything that doesn't alter the CLI surface or the
  embedded-payload format.
- **Minor increments for notable milestones:** `0.2.x` → `0.3.0`. Use
  this for new CLI flags, new dialogs, launcher-runtime features, or
  any change to `snug-format`'s wire layout that requires a
  `format_version` bump.
- **Hard ceiling:** while the project is in development the major
  number stays `0`. Do **not** bump to `1.0.0` (or any `1.x`) as part
  of ordinary work — `1.0.0` is reserved for an explicit decision by
  the maintainer that snug is finished. If the minor number would
  otherwise creep past `0.9.x`, keep incrementing the micro number
  instead (`0.9.4` → `0.9.5`) rather than rolling over to `1.0.0`.
- `vendor/editpe` carries its own upstream version and is **not**
  part of this scheme.

## Conventions

- Rust edition **2024**, MSRV **1.85**.
- Errors via `thiserror` for library types, `anyhow` at the CLI
  boundary.
- No `unsafe` in any crate. (`#![forbid(unsafe_code)]` is set in
  `snug-format` and `snug-cli`.)
- **A file-level `#![cfg(windows)]` is correct in a library module and
  fatal in a binary root.** It is the right tool for `jdk_install.rs`,
  `splash.rs`, `modal_window.rs` and the rest of the Win32 GUI — the whole
  file genuinely is Windows. In a `[[bin]]` / `examples/` crate root it
  configures out `main` too, and you get
  `error[E0601]: main function not found`, which fails
  `cargo build --workspace` on macOS and Linux. So the Windows-only
  *binaries* — `snug-dropper`, `stamp_dropper_icon` — keep a real `main`
  and gate per item instead: `#[cfg(windows)]` on each item. Bin *names*
  must not change; `build-release.cmd` invokes them.
  - **`snug_preview` is the exception, and it is no longer Windows-only.**
    It uses the other shape: one `#[cfg] #[path = "..."] mod imp;` beside a
    real `main` that delegates, so the *implementation* is per-platform
    while the entry point is not. There are now two:
    `snug_preview/windows_impl.rs` and `snug_preview/macos_impl.rs`, gated
    `#[cfg(windows)]` and `#[cfg(target_os = "macos")]`, with a third
    `main` for platforms with neither.
  - **The two implementations are not ports of each other, and the
    difference is the point.** The Windows one is ~2000 lines because the
    Windows dialogs are hand-painted Win32 with no way to drive the real
    ones, so the previewer reimplements each window. The macOS dialogs are
    real `NSWindow`s behind ordinary functions, so `macos_impl.rs` *calls*
    them and reimplements nothing — shorter, and honest, because what you
    see is the window the launcher actually shows rather than a copy that
    can drift from it. It is also the reason the macOS preview can afford
    the dialog mascot at all: run from a terminal there is no bundle, so
    `--mascot` is effectively mandatory rather than a nicety.
- **Never hardcode a Windows path literal in a test of portable logic.**
  `Path::new(r"C:\work\App.jar")` is one opaque segment on macOS, so
  `parent()` returns `None` and the code takes its bare-filename fallback:
  the test then fails on an assumption about separators rather than on a
  bug. Build test paths by joining segments (see the `p()` helper in
  `snug-dropper/src/build.rs`) so both the input and the expectation use the
  host's real separators.
- **`cargo test` builds examples; `cargo build` does not.** On macOS / Linux
  the three Windows-only examples still need a `main`, so the portable test
  command is `cargo test --workspace --lib --bins --tests`.
- **A test temp dir keyed on `pid` + `SystemTime::now().as_nanos()` is
  not unique.** Tests share a pid, and macOS clock resolution is coarse
  enough that two tests running in parallel can read the same nanosecond.
  They then share a directory: one test's `snug.options` shows up inside
  another test's deliberately empty exe dir, or one test's `libs/*.jar`
  becomes visible to a sibling, and the failure reads as a logic bug
  rather than a collision. Observed twice before this was understood — as
  `resolve_uses_cwd_when_exe_dir_has_no_default` and
  `build_flags_a_first_jar_with_no_main_class`. Every test `tempdir()`
  helper therefore folds in a `static COUNTER: AtomicU64`. Only
  `snug_preview/windows_impl.rs` is left, and it is still Windows-gated.
  Note the flake this fixes is not only a name collision: these helpers
  write to `std::env::temp_dir()`, which is `/var/folders/…/T/` on macOS
  and is **not always writable** in a sandbox. So the same line can fail
  as a bare `Operation not permitted` from a `.unwrap()`. When a snug
  test panics on a temp path, suspect the environment before the logic.
- **snug must never create `NSApplication`. JavaFX has to win the
  `+sharedApplication` race, and the launcher used to take it away from it.**
  This is the single most important macOS rule in the file, and the way to
  state it is *whoever calls `+sharedApplication` first owns the
  application*. Apple's docs are explicit: to get an instance of a
  subclass, the **first** `+sharedApplication` call has to be on that
  subclass. JavaFX asks which class won, in `GlassApplication.m`:
  ```objc
  NSApplication *app = [NSApplicationFX sharedApplication];
  isEmbedded = ![app isKindOfClass:[NSApplicationFX class]];
  if (!isEmbedded) { /* set delegate, TransformProcessType, activate, [NSApp run] */ }
  else            { /* just fire willFinishLaunching and get out of the way */ }
  ```
  So if snug calls `[NSApplication sharedApplication]` first — which is
  exactly what a launcher wanting a dock icon seems like it should do — glass
  finds a plain `NSApplication`, concludes it is **embedded in another
  toolkit**, and takes the `else` branch, which **actively demotes the
  process**:
  ```objc
  else {
      ProcessSerialNumber psn;
      if (GetCurrentProcess(&psn) == noErr) {
          TransformProcessType(&psn, 4);   // kProcessTransformToUIElementApplication
      }
      [app setDelegate:self];
  }
  ```
  A **UI-element app is exactly the kind of process macOS does not give a
  menu bar to** — so this is not merely "some setup gets skipped", the
  process is reclassified into the category that has no menu bar. Note also
  that `[app setDelegate:self]` is in **both** branches; delegation is not
  what is lost, and an earlier version of this note wrongly listed it.
  - **That prediction was then confirmed on a real machine, and its failure
    mode is the tell.** A branch (`macos-snug-owns-nsapp`, since deleted) that
    deliberately took `NSApplication` back, set `Regular` activation policy
    and called `activate()` after the loop turned was built and run against
    the same demo. It lost, and not subtly: **no menu bar at all, the Dock
    icon bouncing, and the app window present but non-interactive** — a
    UI-element app's exact signature, since its windows sit outside the
    normal activation space and cannot take keyboard focus. So snug owning
    the app is not a smaller fix, it is strictly worse than the app merely
    losing a menu bar, and the race is load-bearing. Do not retry it.
  - **The window still appears** in the ordinary broken case. That is what
    makes it so expensive to diagnose: a snug-hosted JavaFX app looks
    completely healthy and simply has no working system menu bar, because a
    JavaFX `MenuBar` with `setUseSystemMenuBar(true)` is installed from
    `MenuBarSkin.setSystemMenu(stage)`, which is gated on the integration
    glass skipped.
  - snug **cannot** win that race legitimately, and that is what forces the
    design: glass can only run once the initial thread is inside a run
    loop, so whoever starts the loop necessarily creates
    `NSApplication` first. The way out is a run loop that does not *need*
    an `NSApplication` object at all — a `CFRunLoop` belongs to the thread,
    not the app. `run_event_loop()` therefore pumps a bare
    `NSRunLoop::runUntilDate` and touches no `NSApplication`. glass then
    creates `NSApplicationFX`, sets itself as delegate, and calls its own
    `[NSApp run]`.
  - **Consequence worth knowing: the loop is polled, so it has a stop
    flag.** `NSApplication::run` could only be ended by a main-thread
    `stop:`, which the VM worker is not allowed to issue, so failure paths
    hung forever. `stop_event_loop()` is settable from any thread, and that
    fixed a real latent bug: with the committed stub, a Main-Class that
    fails to resolve leaves the process **hung with no output at all**; the
    flag makes it exit in ~150 ms and print the error. Worth keeping a
    regression test for.
  - **A run loop is not an event loop. This cost two separate debugging
    sessions, and the two halves have opposite fixes.**
    `NSRunLoop::runUntilDate` runs the loop's *sources*; `NSEvent`
    dequeue-and-dispatch belongs to `NSApplication`. A window in a process
    that never calls `-[NSApplication nextEventMatchingMask:…]` **takes no
    clicks**, and AppKit's answer to a window that never responds is the
    spinning beachball. It looks like a hang, or like a window that is
    merely "not focused", so it is very easy to misattribute.
    - The **preview** owns its application — no JavaFX in the process, so
      nothing is competing for `+sharedApplication` — and therefore simply
      calls `[NSApp run]`, with the responder calling `app.stop(None)`.
      That is the documented AppKit pattern and it is safe *only* because
      the preview is alone.
    - The **launcher** cannot. glass calls `[NSApp run]` itself, inside
      `if (!isEmbedded)` in `GlassApplication.m`'s `runLoop:`, but only
      once `run_event_loop` has started — and `progress()` has already come
      and gone by then, on the far side of the worker hand-off. So at that
      point snug is still unambiguously the application's owner and the
      loop is its own to drain: `drain_appkit_events` calls
      `nextEventMatchingMask_untilDate_inMode_dequeue` once per tick, with
      `distantPast` as the expiration so it dequeues what is queued rather
      than waiting (waiting would stall the progress sample the loop exists
      to take).
    - **Enforced, not just documented:** `+sharedApplication` *creates* on
      first call, so a helper that merely wants an application to talk to
      it would quietly become the creator and win the race on the one path
      that must never. `OWNS_APP` records that `activate_app` — the only
      permitted creator — has already run, and `drain_appkit_events` is a
      no-op without it. Do not "simplify" that check away.
    - `NSDefaultRunLoopMode` is not bound in objc2-app-kit 0.3.2, and
      `NSRunLoopMode` is a typedef for `NSString`, so the default mode is
      spelled out as `kCFRunLoopDefaultMode`. Changing it is the kind of
      invisible constant that would silently stop mouse events again.
  - The Dock-bouncing symptom was never about pumping. AGENTS.md used to
    record "do not hand-pump the run loop, the Dock icon bounces forever".
    That was a misdiagnosis: bouncing meant AppKit was stuck in the
    launching state because *glass believed it was embedded* and therefore
    never called `finishLaunching` or `[NSApp run]` to complete the launch.
    With `isEmbedded` back to NO, glass finishes the launch itself and the
    bouncing is gone — see the note under the thread split below.
  - Calling `finishLaunching()` from the launcher is still forbidden, for
    the original and still-correct reason: it **consumes the
    `applicationDidFinishLaunching` notification** that JavaFX's
    `MacApplication` is waiting on to finish toolkit startup. With snug no
    longer creating `NSApplication` at all, the only surviving caller is
    `activate_app()`, which the Adoptium download dialogs still use — see
    the Backlog, because that is a real remaining hole.
  - The thread dump that identifies the *startup hang* (a separate, older
    failure): the `JavaFX-Launcher` thread is *still inside*
    `PlatformImpl.startup` → `QuantumToolkit.startup` →
    `MacApplication._runLoop` in native code, and there is **no "JavaFX
    Application Thread"** in the dump at all. A healthy JavaFX app always
    has one; its absence is the tell, long before any timeout.
  - `QuantumRenderer-*` parked on `LinkedBlockingQueue.take()` is the
    same story from the other end: the renderer is idle waiting for a
    window that was never created. "No suitable pipeline found" and
    "nothing happens" are the *same* failure at different stages.
  - What settled it, and the check to run first next time: the same JAR
    outside snug. `java -cp <jar> <MainClass>` opened a working window,
    which cleared the JDK, the natives, the JavaFX build and the demo's
    own code in one step, and left snug's hosting as the only variable.
    Reach for that before instrumenting the launcher. For anything to do
    with the menu bar, application activation or the Dock, this is *also*
    the test that isolates snug, because it is precisely the run where
    glass wins the `+sharedApplication` race.
  - A JavaFX `main` that never returns is **correct** — `Application.launch`
    blocks for the life of the app by design, and `main` sitting on a
    `CountDownLatch` inside `LauncherImpl.launchApplication` is the
    healthy case. "snug never returned from `call_static_method`" is not
    a diagnosis; the question is always what the JVM did next.
- **A GUI app needs the process's *initial* thread for the AppKit event
  loop, and `JNI_CreateJavaVM` takes it away.** This is the whole reason a
  snug-hosted JavaFX app used to hang: `JNI_CreateJavaVM` makes its caller
  the Java main thread, and Java's `main` parks on a `CountDownLatch` for
  the life of the app, so the initial thread is never free. AppKit then
  never delivers `applicationDidFinishLaunching:`, so
  `MacApplication.runLoop` never returns, so `PlatformImpl.startup` never
  completes and the `JavaFX Application Thread` is never created. The
  symptom is a dock icon bouncing with no window, **no exception, and
  nothing in the log after the JNI handoff.** AWT hangs identically on
  `JFrame`, so it is not a JavaFX problem.
  - **The fix is the thread split.** The VM lives on a worker thread; the
    process's initial thread runs `appkit::run_event_loop()`. That is the
    structure every Cocoa app has. `InitArgs` has to be built on the
    worker because it holds raw pointers and is not `Send`, and
    `DestroyJavaVM` has to be on the creating thread, so both ends of the
    VM stay on the worker together.
  - **The event loop must not call `finishLaunching`.** That call is what
    *delivers* `applicationDidFinishLaunching:`; calling it from the
    launcher consumes the notification before JavaFX has installed a
    delegate to observe it, and the hang returns with no error to explain
    it. An earlier attempt to reuse `activate_app()` at startup did
    exactly this and made the failure worse.
  - **The loop is hand-pumped on purpose, and it creates no
    `NSApplication`.** `run_event_loop()` runs `NSRunLoop::runUntilDate`
    in a `while !STOP_EVENT_LOOP` loop, which sounds like exactly the thing
    an earlier note here forbade. It is not the same thing, and the
    distinction is the whole fix: that earlier attempt hand-pumped *while
    also holding an `NSApplication` it had created*, so AppKit sat
    half-launched with nothing to finish it and the Dock icon bounced
    forever. Now the pump is deliberately `NSApplication`-free, glass owns
    the application object and calls `finishLaunching` / `[NSApp run]`
    itself, and the bounce is gone. **If you ever find yourself adding
    `NSApplication` back into this function, stop** — see the
    `+sharedApplication` race note above.
  - **The worker exits the process when the app finishes**, rather than
    unwinding. `run()` only returns for a main-thread `stop:`, and the
    worker is not allowed to issue one. Exiting also sidesteps the
    teardown trap: `DestroyJavaVM` waits on non-daemon threads a GUI
    toolkit keeps alive (an AppKit run loop, a keep-alive thread), so
    waiting for it is how "close the window and the process never goes
    away" happens. The log is flushed first, because `exit` skips
    destructors.
  - **The failure this was found via is worth keeping.** The tell was a
    `jstack` of the *hanging* case with no "JavaFX Application Thread" in
    it, against a `jstack` of the *working* `java -cp` case that has one.
    The thread list settled in a minute what four wrong hypotheses could
    not. See also the `java -cp` note below.
  - The [OpenJFX dev list](https://mail.openjdk.org/pipermail/openjfx-dev/2022-January/033289.html)
    describes the same constraint from the other side, and its suggested
    workaround is `JLI_Launch` rather than this split. Both work; this one
    keeps everything in-process and needs no `main` export, no `libjli`,
    and no C shim.
- **On macOS 14+ JavaFX no longer uses the deactivate/reactivate dance that
  fixes the menu bar, which changes how you debug it.** The original
  JDK-8233678 workaround made the app hide itself and reactivate on first
  activation. `GlassApplication.m` now does:
  ```objc
  triggerReactivation = YES;
  // ... no longer needed (and no longer works anyway) as of macOS 14
  if (@available(macOS 14.0, *)) { triggerReactivation = NO; requiresActivation = YES; }
  ```
  and activation moved into `applicationDidFinishLaunching:` as
  `if (!NSApp.isActive && requiresActivation) { dispatch_async(… activateIgnoringOtherApps:YES …); }`.
  So on 14+ the *only* thing that makes the system menu bar work is that
  `activateIgnoringOtherApps:` — and, before that, the `isEmbedded` race
  above actually having been won by glass.
  - **Debugging trap this creates:** the warning
    `Timeout while waiting for app reactivation` is evidence *only* on
    macOS below 14. On 14+ `waitForReactivation()` is never called, so the
    line is absent whether or not activation is broken, and treating its
    absence as proof that activation happened sends you the wrong way.
    That misreading cost a whole wrong hypothesis here.
  - The `"javafx"` logger *does* reach stderr with no configuration at all,
    so a warning that should have fired and did not is still real
    evidence — just confirm you are on an OS where it should fire. The
    giveaway is any other line from the same logger, e.g.
    `WARNING: Unsupported JavaFX configuration: classes were loaded from
    'unnamed module …'`.
  - **`-Dglass.taskbarApplication` is a red herring for this class of bug.**
    It reads as `!"false".equalsIgnoreCase(prop)`, so it is `true` when
    unset and is therefore already doing the right thing; setting it to
    `false` makes things worse, not better. More importantly it is read
    *inside* the `if (!isEmbedded)` block, so **no system property can
    rescue the embedded case** — nothing is configurable from outside.
- **`java -cp <jar> <MainClass>` is the test that ends the guessing.** One
  run of the app outside snug, on the same machine and same JDK, cleared
  the JDK, the JavaFX build, the natives and the app's own code in a
  single step and left snug's hosting as the only variable. It should
  have been the *second* thing tried, not the tenth. Instrumenting the
  launcher four times first produced four plausible theories, one
  regression that made a symptom worse, and none of them the cause.
- **`SNUG_DEBUG=1` turns on the fine-grained trace, and the lines worth
  having are the two that bracket the JNI call.** "Bouncing in the dock" —
  process alive, registered as a GUI app, no window — is the macOS
  signature of *blocked*, and the launcher's ordinary log cannot say where,
  because the last line before the block was `loading libjvm.dylib` and
  everything after it happens inside the JVM. So `log::debug` emits
  `jni: >>> calling main(String[]) …` immediately before
  `call_static_method` and `jni: <<< main returned cleanly` after.
  Nothing after the opening marker means the app itself is stuck, and no
  further snug-side logging will help — that wants a JVM thread dump
  (`jstack <pid>`, or `jcmd <pid> Thread.print`) taken while it hangs.
  Every debug line also carries `+NNNms` from log init, because the
  existing `[unix_secs]` prefix orders events but cannot distinguish a
  900 ms stall from a 0 ms one, and whole-second resolution hides exactly
  the pauses you are looking for.
  - The switch is deliberately forgiving — `1`, `true`, `on`, anything
    non-empty except `0`/`false`/`off` — because a debug switch that needs
    exact spelling is one nobody sets while chasing a hang.
  - The log is per-launch and truncated, so each run is a self-contained
    record; `SNUG_CACHE_DIR` still needs to point somewhere writable for
    the file to exist at all.
- **A JavaFX fat JAR must carry the platform's natives, and they belong at
  the JAR root.** `NativeLibLoader` looks each library up as a classpath
  *resource by leaf name* — `libglass.dylib`, `glass.dll`, `libglass.so` —
  so that is where the Maven `-mac` / `-win` / `-linux` classifier
  artifacts already put them, and where a flattened fat JAR has to leave
  them. (I "fixed" this into `com/sun/{glass,prism}/natives/<os>/` on the
  reasoning that the modular layout must be reproduced on the classpath.
  That was wrong, and the `-Dprism.verbose=true` trace showed it: nested,
  JavaFX emitted no "Loading library from resource" line at all. Trust the
  loader's own log over a plausible-looking theory.)
  - The three platforms cannot collide at the root — the names differ by
    extension — so one JAR *can* carry all of them, and doing so is fine.
    What snug's demo does instead is per-platform: `assets/snug-javafx-demo-windows.jar`
    carries the Windows natives (used by `scripts\build-release.cmd`) and
    `assets/snug-javafx-demo-macos.jar` the macOS ones (used by
    `scripts/build-macos-demo.sh`).
  - Because the names are platform-specific, a JAR missing its own
    platform's natives fails *only* at startup, with
    `Graphics Device initialization failed for : es2, sw` /
    `Error initializing QuantumRenderer: no suitable pipeline found`. That
    reads like a GPU or driver fault, so it sends you looking at graphics
    settings while the actual defect is in the packaging. Neither the
    bundle validator, `codesign`, nor the payload check can see it, which
    is why `build-macos-demo.sh` greps the JAR for the three required
    `.dylib`s and aborts before packaging.
  - **Classes and natives must come from the same OpenJFX build.** Read
    the build out of the JAR — `javafx.properties` gives
    `javafx.runtime.version`, `VersionInfo.class` carries the date — and
    pick the classifier jar to match. They do not line up by version
    number: `25:mac` is 2025-09-15, `25.0.1:mac` is 2025-10-21,
    `25.0.2:mac` is 2026-01-20.
  - Two more diagnostics that mislead:
    `java -jar demo.jar` can print *"Error: JavaFX runtime components are
    missing"* — a string that is **not in the JAR at all** — while the same
    JAR launched by explicit main class gets as far as the pipeline. And
    with `-Dprism.verbose=true` JavaFX reports a sandbox-style
    `Error copying library ... to cache ~/.openjfx/cache/... (Operation not
    permitted)` when the *only* problem is that it could not write its
    extraction cache. Neither is a JavaFX bug.
  - `--enable-native-access` must be `ALL-UNNAMED` for a classpath JAR.
    Naming a module (`javafx.graphics`) is a no-op the JVM reports as
    "Unknown module" — snug puts the JAR on the classpath, so there is no
    such module. JavaFX 25 calls the restricted `System.load`, and a future
    JDK will block it outright.
- **A `.app` is a *directory*, and its permissions are load-bearing.**
  `macos_bundle.rs` writes `Contents/{Info.plist, MacOS/<App>, Resources/}`
  and signs the result. Directories are `0755` because a directory needs
  `x` to be **traversable** — `0644` makes the bundle invisible to
  `execve`. The executable is `0755`, *not* the `0555` a sealed bundle
  would use: both run, but `0555` blocks `codesign`, and we emit an
  ad-hoc signature. Resources and `Info.plist` are `0644`. Note also that
  ad-hoc signing is not optional polish — **arm64 refuses to execute a
  binary with no code signature at all.**
- **`CFBundleName` is the application menu's title, and it is not the
  executable name.** Three keys in `Info.plist` look interchangeable and are
  not, and conflating two of them is what puts an internal slug in front of
  the user:
  ```text
  CFBundleExecutable    snug-javafx-demo    must match the file in Contents/MacOS
  CFBundleName          Snug JavaFX Demo    what macOS titles the application menu
  CFBundleDisplayName   Snug JavaFX Demo    Finder only, never the menu bar
  ```
  snug used to write the slug into `CFBundleName` as well, so the demo's
  first menu read "snug-javafx-demo" while the window and the Finder read
  "Snug JavaFX Demo". `bundle_name()` now emits the app's `--name`, capped
  at Apple's 16-character `CFBundleName` limit (counted in **characters**,
  not bytes, and `trim_end`ed so a cut mid-word leaves no trailing space).
  `CFBundleDisplayName` still carries the full name, so nothing is lost —
  which is also why capping is safe. It is the macOS counterpart of the
  `ProductName` the Windows build stamps into `VS_VERSIONINFO`, and it only
  became *visible* once the `isEmbedded` fix above restored the application
  menu; before that, nothing was reading it.
- **`JNIVersion` must not be pinned above the JVM you discovered.**
  Requesting a spec version newer than the loaded `libjvm` understands is
  how `JNI_CreateJavaVM` fails with a useless `JNI call failed`: with
  `JNIVersion::V21` hardcoded, a user who set `--min-java 17` got a good
  Java 17 discovered and loaded, and was then asked for a JNI 21 entry
  point that does not exist in it. `jni_version_for` maps the discovered
  major version to the matching spec (everything used here is JNI
  1.0-era, so nothing is lost). **`platform/windows.rs` still has this
  bug** — same hardcoded `V21`, same failure; it just needs a JVM older
  than 21 to show up, and snug's default is `min_java` 25.
- **`wants_app_bundle` is platform-neutral on purpose.** snug embeds
  exactly one launcher, chosen for the machine it was built on, so the
  default output is the artefact this build can actually produce: a
  macOS `snug` defaults to `App.app`, everything else to `App.exe`. The
  interesting half of the predicate is the answer on platforms that
  *cannot* honour it — a Windows `snug` has no macOS launcher embedded,
  so `-o Foo.app` is **refused with an explanation** rather than written
  as a flat PE named `Foo.app`. That flat file is not hypothetical: the
  repo shipped one, `assets/snug-javafx-demo.app`, until it was
  untracked. A `.app` is a *directory*; never produce a flat one.
  The reverse is deliberately still allowed — `-o App.exe` on a Mac
  builds a Windows EXE, because the PE stub is embedded
  unconditionally. That is how Windows artefacts get built from a Mac, so
  it should be something you ask for rather than something that happens.
- **Report download progress on *bytes*, not on phase.** `phase` is `0`
  for the entire download, so a poller that only watched the phase
  emitted one line and then went silent for the whole fetch — and a
  silent 114 MB download is indistinguishable from a hang, which is
  exactly how it was reported. `should_report` is extracted and tested
  for this reason: it reports every 5% *and* on any phase change, because
  a phase change is where "bytes" changes meaning. The 5% step keeps a
  200 ms poll from burying the log in thousands of identical lines.
- **`ureq` has no read timeout by default.** `timeout_read: None`
  means a connection that establishes and then goes quiet blocks
  forever, with no error, no retry, and nothing in the log. The agent is
  built in `http_agent()` with connect/read/write timeouts of 30s. A
  *per-read* timeout is the right shape rather than a total-download
  budget: each successful read resets it, so a slow-but-progressing
  185 MB fetch is never killed while a dead socket is caught in 30s and
  handed to the retry dialog. `timeout_connect` is stated even though it
  matches ureq's default, so a silent change there cannot silently
  change our behaviour.
- **Unregister a `.app` from Launch Services before deleting it.** A
  deleted bundle leaves a *stale* record pointing at the old path, and a
  rebuild under the same `CFBundleIdentifier` does not displace it. Finder
  then keeps resolving the identifier to the dead path and refuses the
  new one with `_LSOpenURLsWithCompletionHandler() failed with error
  -1712` — while the new bundle lints, verifies and runs perfectly when
  invoked directly. Nothing in that error points at its real cause, so
  both `macos_bundle::prepare_bundle_dir` and `build-macos-demo.sh` call
  `lsregister -u` first. It is advisory: the exit status is ignored,
  because a bundle LS never saw is not a reason to fail a build.
  Corollary worth remembering: a bundle invoked as
  `Foo.app/Contents/MacOS/Foo` **bypasses Launch Services entirely**, so
  it working is no evidence that double-clicking will.
- **A `ParentWindow` type alias is how a `cfg` split stays invisible.**
  `jdk_install::ui::ParentWindow` is `HWND` on Windows and `()` elsewhere.
  Because the alias *is* `HWND` there, every existing signature and call
  site compiles unchanged — the alias is the mechanism, not a wrapper.
  Prefer this to threading an `Option<isize>` through the flow.
- **The JDK download flow talks to the user only through `jdk_install::ui`.**
  Windows delegates to the hand-painted dialogs that already exist; every
  other platform uses **the same `dialogs()` strings** via the launcher log
  and stderr. Nothing in the seam formats a string of its own, so a
  `--localization <tag>` bundle translates the macOS output exactly as it
  translates the Windows one. The non-Windows behaviour is deliberately
  conservative: it never opens a browser unprompted (a `.app` launched
  from Finder has no terminal to have agreed to anything in), and it says
  out loud when it auto-retries, because a silent retry looks like a hang.
- **The Adoptium query is per-platform and the archive format follows.**
  `adoptium_target()` returns Adoptium's own spellings — `windows`/`x64`
  and `mac`/`aarch64` — because `arm64` and `x86_64` are Rust's, not
  theirs. Getting `os` or `architecture` wrong returns a perfectly
  well-formed metadata document for the *wrong* platform, so the mistake
  surfaces much later as an extraction failure rather than as a bad
  request. Windows assets are `.zip`, macOS assets `.tar.gz`, so
  `extract_jdk_archive` is split per platform and each side asserts the
  extension it expected.
- **Caches and Application Support are different directories, and the
  difference is load-bearing.** `cache::platform_cache_base()` answers
  "where does reclaimable data go" (`%LOCALAPPDATA%`, `~/Library/Caches`,
  `$XDG_CACHE_HOME`); `cache::platform_app_support_base()` answers "where
  does durable per-user app data go" (`~/Library/Application Support`,
  `$XDG_DATA_HOME`). macOS is the platform that makes the distinction
  real, because it is allowed to purge `Caches` under disk pressure.
  A reclaimed cache costs nothing — snug re-extracts the JARs and starts
  a fresh log — but an adopted JDK is a few hundred megabytes behind a
  real download, and re-paying that because the OS tidied up is a bug.
  `jdk_install_root()` therefore uses app support on macOS while
  `cache_root()` uses caches, and a test asserts the two resolve to
  *different* directories so the split cannot silently collapse.
- **The per-user cache base is asked of the OS, never hardcoded.**
  `cache::platform_cache_base` is `cfg`-split: `%LOCALAPPDATA%` via the
  Win32 API on Windows, `NSHomeDirectory() + Library/Caches` on macOS,
  `$XDG_CACHE_HOME` / `~/.cache` elsewhere. Two traps, both found
  empirically:
  - **`confstr(_CS_DARWIN_USER_CACHE_DIR)` is the obvious answer and it is
    wrong.** Its man page promises a location "not automatically cleaned
    by the system", but on macOS 15 it returns `/var/folders/<hash>/C/` —
    inside the per-boot temporary tree, which the system *does* purge.
    Reproducible with `env -i`, so it is not a launch artefact. The JAR
    cache there could be deleted out from under a running app. Use
    Foundation instead.
  - **`NSHomeDirectory()` returns an `NSString *`, not a `char *`.**
    Declaring it by hand as `-> *const c_char` compiles, links, and
    returns *garbage bytes* rather than failing — so use the
    `objc2-foundation` binding. (The older `objc` 0.2 crate does not
    compile on current rustc at all: `cannot find macro sel`.)
  - Note also that reading `LOCALAPPDATA` for a non-Windows target is
    never right: on macOS it is always unset, which is how the old code
    silently fell through to the Linux `~/.cache` convention.
- **The classpath separator is `:` on macOS and `;` on Windows, and a
  wrong one fails *inside the JVM*.** A `;`-joined classpath is not a
  snug-side error — it is one enormous bogus path, so classes fail to
  resolve and the symptom is a Java `ClassNotFoundException` with nothing
  pointing at snug. Build it with `std::env::join_paths`, never
  `join(";")`; same for `PATH` via `std::env::split_paths`. `main.rs` is
  shared between platforms, so it calls `platform::locate_payload` /
  `platform::run` and never branches on the OS itself.
- **Payload location is platform-specific, and the name is exported.**
  Windows stamps the payload into the PE's `RT_RCDATA`; macOS cannot
  (there is no Mach-O resource writer here, and `editpe` is PE-only), so
  the `.app` carries a sibling file `Contents/Resources/<App>.snugpayload`
  and the launcher is a plain byte-copy. `PAYLOAD_SUFFIX` is re-exported
  from `platform` precisely so the producer (`snug-cli` writing the
  bundle) and the consumer cannot drift — hand-writing the string in
  both places is how a `.sngpayload` / `.snugpayload` mismatch gets
  written, and it surfaces only as a bare "payload not found".
- Binary artefacts embedded in the launcher are referenced by their
  SHA-256 digest — that digest is also the cache key. It is a
  **content key, not an integrity check**: `ensure_cached` returns early
  on `dest.exists()` and never re-hashes, so a corrupted extracted entry
  is used as-is. Integrity of the embedded bytes is covered by the
  payload's CRC32 at the EXE level.
- **Multi-JAR builds key each JAR independently.** A directory input
  produces one `EmbeddedFile` per `*.jar` (sorted by filename, non-
  recursive), each with its own digest and its own cache directory. The
  first entry sorted by filename is the "primary": it supplies the
  `Main-Class` fallback and the log path. Because snug reports only
  `jars[0]`'s digest (build output and `snug.log` alike), a rebuild that
  changes only a later JAR is invisible in snug's own output even though
  it is fully picked up on the classpath.
- **`--cache-dir <DIR>` overrides the per-user cache root.** The field
  (`LauncherBehavior::cache_dir`) predates the flag and was previously
  unreachable — nothing set it, so every real build resolved to
  `%LOCALAPPDATA%`. It is now a `value_parser`-validated absolute path:
  the value is stamped into the payload and used at runtime on a machine
  the builder never sees, so a relative path would silently resolve
  against the end user's CWD.
- All CLI flags follow the brief: see `crates/snug-cli/src/cli.rs` for
  the canonical surface. Note: clap's auto-generated `--version` flag
  is disabled (via `disable_version_flag = true`) so the brief's
  `--version <APP-VERSION>` doesn't collide with it. Snug's own
  version is discoverable via `--snug-version` or `cargo metadata`.
  Resource stamping is unconditional; the prior `--rcedit <path>` and
  `--no-rcedit` flags are gone in favour of an in-process `editpe`
  integration. The new `--manifest <XML>` flag embeds an arbitrary
  Windows application manifest (XML).
- **A `Main-Class` need not have a `main` method.** `--find-main` lists
  every class in the input JAR(s) declaring `public static void
  main(String[])` (`crates/snug-cli/src/classfile.rs` parses the method
  table only — no bytecode). It is a **read-only diagnostic**: it never
  fails a build, because a legitimately missing `main` is normal. The
  committed `assets/snug-javafx-demo-windows.jar` is the standing example: its
  manifest names `synapticloop.snugjavafxdemo.HelloApplication`, a
  JavaFX `Application` subclass with only a constructor and `start(Stage)`
  — the launcher calls `Application.launch()` for it
  (`crates/snug-launcher/src/platform/windows.rs`). So any future
  "Main-Class must have a main method" check must special-case that
  shape, and a scan cannot fully detect it (the superclass may live in a
  different JAR or JDK module). The launcher already reports both
  failure modes at runtime via `err.main_class_not_found` and
  `err.no_main_method`, so build-time checks are about failing before
  shipping, not about new diagnostics.
- **`Main-Class` resolution is first-JAR-wins, and that is flagged.**
  For a multi-JAR input, snug reads `Main-Class` from `jars.first()`,
  which is the first JAR *sorted by filename* — a tie-break, not a
  policy. When two JARs declare *different* values, or when the first
  JAR declares none while a later one does, both `--find-main` and the
  build print a warning naming the declarations and the choice, and
  suggest `--main-class`. Both are **silent** when `--main-class` was
  passed, and when several manifests agree on the same class (not a
  conflict). The survey lives in
  `manifest::survey_main_classes`, over in-memory bytes via
  `manifest::read_main_class_from_bytes` — never a temp file.
- **`snug.options` files** are supported, in two layers. Precedence,
  highest first: **command line → `snug.<os>.options` → `snug.options`.**
  The OS token is Rust's own `std::env::consts::OS` spelling —
  `snug.macos.options`, `snug.windows.options`, `snug.linux.options` —
  *not* `mac` / `win`, because the project already spells these
  `macos` / `windows` in its cfg gates, script names and release layout.
  `os_options_file_name(os)` takes the token as a **parameter** so every
  platform's rules are testable from one host.
  - **An OS file is a partial override, not a second full file.** It
    carries only the values that differ, and the rest still fall through
    to `snug.options`. The motivating case is `--output`, whose extension
    *must* differ (`.exe` vs `.app`) — duplicating the whole file to
    change one line is how two copies drift apart. A file for another
    platform is never read, and a missing one is **silent**: it is
    opt-in, so reporting "not found" would make every build on a machine
    without one print noise. Only loaded files are listed, on stderr.
  - **`--options <path>` means that file *only*.** The OS tier is not
    consulted alongside it. That is deliberate: it is the escape hatch
    for a build whose options must not depend on which machine ran it,
    and it is what `scripts/build-macos-demo.sh` relies on to pin the
    shared `snug.options` while overriding `-o` on the command line.
  - **Location search is per-tier.** For each of the two names in turn:
    the snug executable's own directory first, then the CWD. So an
    exe-dir `snug.options` and a CWD `snug.<os>.options` both load and
    the OS file still wins. First-hit-per-name is also what makes
    `exe_dir == cwd` safe: it cannot yield the same file twice, which
    for `--jvm-arg` would *duplicate* a JVM argument rather than
    deduplicate it. Candidates are matched with `is_file()`, so a
    directory that happens to share a file's name is skipped.
  - **This repo's own demo builds use the tier, and `snug.options`
    deliberately carries no `--output`.** The value lives in
    `snug.windows.options` (`.exe`) and `snug.macos.options` (`.app`),
    because a platform-specific string in the shared file reads as though
    it applied everywhere and does not — which is the exact problem the
    split exists to fix. Both release scripts pass **no `-o` and no
    `--options`** and rely on the implicit CWD lookup:
    `scripts\build-release.cmd` runs `snug.exe assets\snug-javafx-demo-windows.jar`
    and then asserts `assets\snug-javafx-demo.exe` exists;
    `build-macos-demo.sh` reads the path back out of the files via its own
    `effective_flag` helper rather than hardcoding it. So the string lives
    in exactly one place, and each script fails loudly if its file is
    missing. `crates/snug-cli/tests/options_file.rs` locks this down by
    resolving the repo's real files for `macos`, `windows` *and* `linux`
    and parsing the merged argv for each — the Windows half cannot be
    exercised from a macOS host, which is why it is a test rather than a
    manual check.
  - `resolve_all` returns a **`Vec<PathBuf>` ordered lowest priority
    first** (`snug.options`, then the OS file), and `main.rs` layers them
    in that order. One option per line, parsed as shell-like tokens (so
    `--name "My App"` works with quotes and escapes); `#`-prefixed lines
    are comments. Repeatable flags (`--jvm-arg`, `--localization`)
    accumulate from *every* source, base file first, so JVM options
    arrive in a defined order.
  - **The layering is stripped high-to-low, not concatenated.** `merge`
    walks the layers from highest priority down while accumulating the
    set of flags already spoken for by a higher source, seeded with the
    command line's own. Concatenating two files that both set `--name`
    would leave two occurrences and clap aborts with "the argument
    '--name <NAME>' cannot be used multiple times" — the same failure
    recorded for file-vs-CLI below, now between two *files*. The set is
    built once per call and reused across layers so the cost stays at one
    clap `Command` build per invocation.
  - **Override matching is by flag *identity*, not by string.** Both the
    CLI tokens and the file tokens are resolved through a `FlagSpec`
    built from the `Cli` definition itself, so `-o` on the command line
    strips `--output` from the file and vice versa — and an OS layer
    strips the same flag out of the generic layer. This was a real bug:
    the merge used to compare flag *strings*, so the scaffolded
    `snug.options` (which writes `--output`) could not be overridden with
    the `-o` shown in `--help`, and clap aborted with "the argument
    '--output <EXE>' cannot be used multiple times". Hand-maintained
    tables are the trap, so there are none — the repeatable set comes
    from `ArgAction::Append` and the arity set from
    `ArgAction::takes_values()`. Do **not** reach for
    `Arg::get_num_args()` here: it reads a field clap only populates
    while *building* a command, so on a derived `Command` it reports
    `None` for every arg and value-skipping silently stops working,
    leaving a stripped flag's value behind as a stray positional.
  - **`--init-options` writes `snug.options` only, never the OS file.**
    A generated `snug.<os>.options` would be a full copy of the flag
    surface, which is precisely what the partial-override design is
    meant to avoid. Someone building for several platforms is better
    served by the documented example than by a file they must trim.
- **The `--init-*` family scaffolds project files and exits.** Two modes,
  both short-circuiting before options-file loading and before the
  JAR-required check, both strict (a dedicated mini-parser in
  `main.rs` accepts *only* `--init-*` flags, so
  `snug App.jar --init-options` is an error, not a build):
  `--init-options` writes the example `snug.options`
  (`init_options.rs`), and `--init-localizations` writes a
  `localisations/` directory of `snug-localisations.<tag>.txt` bundles
  (`init_localizations.rs`). Each has `--force` and `--stdout`
  modifiers, and they can be combined in one call to lay down a whole
  project skeleton. `--init-localizations` defaults to
  `./localisations` relative to the **CWD — the directory snug was
  invoked from, not the executable's own directory**; that is the
  deliberate opposite of the `snug.options` *read* order above,
  because translations are per-project source rather than a shipped
  artefact, and it matches `--init-options`' own CWD-relative
  default. `--init-localizations-tag <TAG>` (repeatable) scaffolds an
  extra translation template; `en` is always written.
- **A user-supplied `en` bundle replaces the built-in English
  baseline**, it does not collide with it: `localization::collect`
  puts the first user `en` in the baseline slot (position 0, lowest
  merge priority) instead of appending a second one, which
  `ensure_unique_tags` would reject. Without this, the obvious
  `--localization localisations` after `snug --init-localizations`
  would fail with a duplicate-tag error, since the scaffold always
  contains an `en` file. A *second* `en` in one build is still an
  error. The scaffold directory must contain nothing but
  `snug-localisations.<tag>.txt` files: `expand_user_paths` fails the
  build on any other entry, so no README, no `.bak`.
- **All launcher user-facing text lives in ONE file, and it is the
  localization catalog.** `crates/snug-format/assets/
  snug-localisations.en.txt` holds the error messages (`err.*`), the
  splash strings (`splash.*`), the JDK-install errors (`jdk.err.*`)
  *and* the dialog chrome (`jdk_install.*`, `generic.*`,
  `launcher.error.*`). There is deliberately no second file: a
  `dialogs.toml` existed until 0.4.0 and was merged in, because
  compile-time-only copy meant a localized app got a translated error
  body inside an English window. The file lives in `snug-format`
  because the CLI and the launcher both need it and `snug-format` is
  the one crate they both depend on; it is `include_str!`d exactly
  once in the workspace and re-exported as
  `snug_format::DEFAULT_EN_TEXT`, so the payload copy and the
  launcher's in-binary fallback can never drift. It sits inside that
  crate (not a top-level `assets/`) because `snug-format` sets
  `publish.workspace = true` and `cargo package` cannot include files
  from outside a package root.
  - **`Dialogs` is hand-assembled, not deserialized.** Sourcing the
    chrome from flat keys means a mistyped key resolves to the key
    literal at runtime — late and user-visible. `dialogs::dialogs()`
    therefore builds the struct out of `localize::lookup` calls, which
    keeps compile-checked field access at all ~16 call sites
    (`dialogs().jdk_install.prompt.title`) and confines key literals to
    one place. `localize::tests::every_localize_key_is_in_the_baseline`
    guards it in both directions: a looked-up key missing from the
    baseline fails, and a baseline key nothing reads fails (escape
    hatch: `KNOWN_UNREFERENCED`). **Add new lookups to its `KEYS`
    list.**
  - **`localize::lookup` falls back to the built-in baseline before
    `init` runs.** `DIALOGS` is a `OnceLock`, so a pre-`init` call
    would cache unresolved values permanently — a cached
    `jdk_install.prompt.title` is a permanently broken window. Since
    the English baseline is compiled in, pre-`init` lookups resolve
    against it (untranslated but correct) rather than returning keys.
    Only a key in neither the chain nor the baseline returns the key
    string.
  - **The bundle chain is replaceable, and cached copies are keyed by
    `localize::generation()`.** `BUNDLES` is an `RwLock<Bundles>`, not
    a `OnceLock` — `snug_preview`'s language dropdown calls
    `localize::set_bundles` to swap the active chain at runtime. Every
    consumer that caches resolved strings must therefore record the
    generation it built from and rebuild when it moves, or a language
    switch leaves stale copy on screen. `dialogs::dialogs()` does
    this (`DIALOGS` holds `(generation, &'static Dialogs)`); it
    `Box::leak`s a rebuild because the signature is `&'static Dialogs`
    and keeping it spared ~16 call sites an owned-value refactor. The
    leak is bounded by "language switches × one `Dialogs`", which only
    the dev preview ever exercises. **`init` is a thin alias for
    `set_bundles` and no longer first-call-wins.**
  - **Values are single-line.** A real newline is a literal `\n`; a
    literal `#` must be `\#`. The old TOML triple-quoted bodies
    (`jdk_install.prompt.content`, `.expanded`,
    `metadata_failed.content`) became one long line each —
    `multi_line_bodies_decoded_from_escapes` is the regression guard
    for that conversion.
  - `jdk_install.failure.content` is deliberately **empty**;
    `error_window` reads empty as "use the module default", and
    `failure_content_is_intentionally_empty` pins that.
- **Bundle *file* discovery lives in `snug-format`, not the CLI.**
  `discover_localization_files` / `tag_from_path` / `expected_filename`
  moved there (with a `LocalizationLoadError` instead of `anyhow`) so
  `snug-preview --localisation <dir>` resolves a path exactly the way
  `snug --localization <dir>` does. `snug-cli` re-exports all three,
  so its ~20 call sites are unchanged. Directory mode stays strict in
  both: a `README.md` or `.bak` beside the bundles is an error, not a
  skipped file. When moving this code, keep the error *wording* byte
  for byte — `collect_directory_errors_on_empty_directory` asserts on
  the exact "contains no `snug-localisations" substring.
- **`snug_preview` is the translation playground.** `-h/--help` and
  `--localisation` / `--localization` (aliases, both spellings
  accepted) are parsed by a hand-rolled `parse_args` taking an
  `IntoIterator`, so all its behaviour is unit-tested without
  spawning a process. Its Language dropdown lists every bundle it
  found with the built-in English baseline last, unless the user  supplied their own `en` — same replacement rule as a build. It
  **opens on the baseline**, resolved by `default_index` and never by
  hardcoding index 0: tags arrive in `discover_localization_files`
  filename-sorted order, so `de` sorts before `en` and an index-0
  default silently made every preview open in German while the closed
  combo — the only thing visible until you click it — showed just
  `de`. `main` and `WM_CREATE` both call `default_index`, so the
  combo and the live bundle chain cannot disagree. Its help text and
  dialog list are generated from `DIALOG_BUTTONS` so neither can
  drift. The label and the dropdown share one row: `COMBO_X` is
  derived from `COMBO_LABEL_W` so they can't overlap, and the label
  is vertically centred against the combo's *measured* client rect
  (`GetClientRect` + `DT_VCENTER`) because the closed field's height
  follows the font — a second set of Y constants would drift on any
  font or DPI change. `COMBO_H` is the height of the **dropped list**,
  not of the closed field, and it has to fit several rows: at 34 px
  (two 17 px item heights, less borders) the list showed only the
  selected tag with every other locale clipped away, which reads as
  "that bundle is missing" even though `CB_GETCOUNT` proved it loaded.
  `--icon <FILE>` swaps the artwork every dialog shows — the large
  mascot image *and* the title bar / Alt-Tab / taskbar — so a
  candidate icon can be judged without relinking the committed
  `bin/launcher-stub.exe`. Those two paths are genuinely separate (the
  mascot falls back via `find_best_icon_hicon`, the title bar via
  `load_exe_main_icon_hicon`), so `--icon` installs two process-wide
  overrides in `jdk_install.rs` (`set_window_icon_override`,
  `set_mascot_icon_override`) that both consult. The production
  launcher never sets them, so they cost one relaxed atomic read and
  are inert outside the dev tool. Decoding is two-tier and the split
  is load-bearing: `LoadImageW` takes `.ico` but returns NULL for a
  PNG from file despite being documented to support it since Vista
  (verified against a real 1254x1254 PNG), so `.png` falls through to
  a GDI+ blit into a 32-bpp top-down DIB. Neither route links the
  `image` crate into the shipped binary, which would fight the size
  budget to serve a dev-only tool.
  - **The info box carries a heading and up to two subtext lines.**
    `INFO_BOX_H` is 74 px (was 50) and `info_subtext_2` is a real
    catalog key on all five dialog groups, matching the existing
    `info_heading` / `info_subtext` pattern. Only `launcher.error` and
    `jdk_install.progress` render a box today — the other three pass
    `None` — so adopting the line elsewhere is a one-field flip with
    no catalog change. In `modal_window` the third line is
    `Option<&str>` and its control *and font* are only allocated when
    non-empty, so an empty line costs nothing; `progress_window` reads
    its strings straight off `Dialogs` and always creates the control.
  - **The macOS half is a different shape, not a port.** It uses real
    `NSButton` / `NSPopUpButton` in a hand-laid-out `NSWindow` rather than
    hand-painted Win32, so it is a fraction of the size for the same job.
    Its language choice comes back *with* the dialog rather than being
    applied live from the popup — one round trip instead of a callback
    into the previewer's state, and the reviewer sees the same thing
    either way: pick a language, click a dialog, read it.
  - **Two macOS-only traps in the picker, both caught by writing it rather
    than by clicking it.** Close carries tag `-1`, and clamping it with
    `max(0)` means clicking Close opens the *first* dialog; the sign has to
    be read in the responder. And `Option` is ambiguous between "not chosen
    yet" and "closed", so the picker needs a second resolved flag.
  - **The macOS picker calls `[NSApp run]`, the launcher never can.** The
    preview owns its application, so the documented AppKit pattern is
    correct there. The launcher does not, and must not — see the
    `+sharedApplication` race and the `drain_appkit_events` note. A run
    loop is not an event loop, and a window whose process never dequeues
    `NSEvent` takes no clicks and earns a spinning beachball.
  - **`WINDOW_W` / `WINDOW_H` are CLIENT dimensions in both dialog
    modules, and the window size is derived from them** via
    `AdjustWindowRectEx` before `CreateWindowExW`, exactly as
    `snug_preview` does for its launcher window. This is not cosmetic:
    the raw size used to be passed straight through, so the ~31 px
    caption came out of the client area and every bottom-of-stack
    element sat that much lower than its constant claimed. It was
    invisible while the info box was 50 px and cleared the fold, and
    only surfaced as a *clipped third line* once the box grew. The
    whole family shares **one height** (351 px client), built from a
    single anchor -- `INFO_BOX_Y = 220` -- plus the box, `BOTTOM_PAD`,
    the button row, and `BOTTOM_PAD` again. `BOTTOM_PAD` appearing
    twice is what makes the gap above the buttons provably equal to
    the margin below them, rather than two numbers that drift apart.
    `INFO_SUBTEXT2_Y_OFFSET` is derived from `INFO_SUBTEXT_Y_OFFSET`
    for the same reason.
  - **Buttons live in their own row below the info box**, right-aligned
    and flowing left, with the **primary at the far right** (the
    Windows task-dialog convention). They used to be fixed-width
    constants vertically centred *inside* the box's band, overlapping
    it. Widths are now measured per label, so a translated string of
    any length fits without a width table to maintain. Two traps worth
    remembering, both hit: `DrawTextW(DT_CALCRECT)` against an
    all-zero rect clips to the empty rect and reports 0 px, and
    `GetTextExtentPoint32W` -- the API to use instead, having no rect
    to clip against -- rejects `c = -1` outright and returns FALSE.
    Either mistake silently floors every button to the minimum width.
    Buttons also get an **explicit HFONT** (9 pt Segoe UI) rather than
    the system default: measuring the stock GUI font while the control
    *renders* in the themed font under-measures by ~15% and clips the
    label. `measure_button_width(hfont, label)` is shared between the
    two modules so they cannot drift; the padding constant lives in
    one place only.
  - `INFO_BOX_W` is `WINDOW_W - MARGIN * 2`, giving the box the same
    margin left and right. It used to be `MARGIN * 3`, leaving twice
    the left margin as whitespace on the right (16 in, 32 out) because
    the buttons once occupied that reserve.
  - The optional "Check for a newer version" link is **two stacked rows
    under the mascot** — the label, then the URL beneath it — not a row
    between the box and the buttons. Only `launcher.error` has one.
    `LINK_URL_W` is the mascot's width, and that is a real constraint
    rather than a preference: the error-content control occupies
    `CONTENT_Y .. CONTENT_Y + CONTENT_H` (108..196) in the column to
    the right, and the link block starts at y=170, so a full-width row
    would run into it. The label is full-width anyway (it measures
    ~180 px at the content font, and clipping it at either the mascot
    width or the 170 px inter-column gap cut it mid-word). The URL
    therefore **wraps inside the mascot column**, and wrapping needs
    `DT_EDITCONTROL | DT_WORDBREAK` — `DT_WORDBREAK` on its own only
    breaks at existing word boundaries, and a URL has none, so the
    text was silently clipped instead. Its hit-test rect is measured
    the same way as the buttons and was **zero-width** before the
    `DT_CALCRECT` fix, which made the link unclickable.
- **`snug-dropper` ("Build with Snug") is a beginner utility, and every
  decision follows from that.** It is its own crate, has no dependencies
  except `windows-sys`, and is **not** part of the launcher or the CLI.
  Four rules that look arbitrary but are not:
  - **The application identity is hardcoded** to `--name "Example
    Application Name"` / `--company "Example Company Pty Ltd"`, and the
    output file is pinned to `Example Application Name.exe`. The point of
    the literal word "Example" is that the artefact announces where it
    came from instead of pretending to be a product. Don't "fix" this
    into a config file or a prompt without asking.
  - **The output lands in the dropped item's *parent*,** for a JAR *and*
    for a folder. A dropped `build/libs` writes the EXE *beside* that
    folder — writing inside it would put the artefact somewhere the next
    `gradle clean` erases. The same parent is used as the child's working
    directory, so a project-local `snug.options` is picked up for free.
    Because of that, **ship no `snug.options` next to `snug.exe`**: the
    exe-dir copy outranks the CWD copy and would silently beat every
    project file.
  - **A build failure gets an error dialog and no terminal.** The dialog
    and `snug-build.log` are the only recourse on that path, so both have
    to stand alone. The terminal is reserved for *"you dropped something a
    starter can't do"* — which is the hand-off to the real tool.
  - **It never routes a path through a shell.** `cmd.exe /k` is spawned
    with the folder set as the process working directory, never
    `cmd /c start /d "<dir>"`, which would parse the path as a command
    line and break on `&`, `^`, or `(`.
  - **It does not link `snug-launcher`, and that is a size decision as
    much as a design one.** The launcher's dialogs are the *wrapped
    app's runtime* surfaces (mascot, Adoptium progress with a Cancel
    button, a "check for a newer version" link to the app's own repo) —
    the wrong chrome to report a build failure with. Reusing them would
    also take the binary from ~240 KB to >1.1 MB (`launcher-stub.exe`
    is snug-launcher alone) by dragging in `jni`, `ureq`, `zip` and
    `sha2`, and `snug-launcher`'s `build.rs` emits an *un-binned*
    `cargo:rustc-link-arg` for its `MAINICON` — which rustc applies to
    the final binary of any dependent, so the dropper would arrive with
    snug's icon already linked in, fighting the dropper's own. If
    branded chrome is ever wanted, the move is to extract the shared
    window code into a `snug-ui` crate that both depend on — its own
    slice, not a side effect of this tool.
  - **Its icon is stamped, not compile-time embedded.**
    `src/bin/stamp_dropper_icon.rs` puts `assets/snug-dropper.png` into
    the built EXE via `editpe`, mirroring `snug-launcher`'s
    `stamp_preview_icon`. The marquee then reads that same `MAINICON`
    back out of its own file with `LoadImageW` + `WM_SETICON`, because a
    custom-painted window gets no EXE icon for free (the `MessageBoxW`
    dialogs do). So one stamp covers Explorer, the taskbar, Alt-Tab,
    the marquee, and every message box. **The source may be any size
    ≥256 px** — `editpe` (with its `images` feature) downscales to
    `[256, 128, 48, 32, 24, 16]` with Lanczos3 before embedding, so a
    1024 px PNG is *not* carried whole. Measured on identical artwork:
    118,272 bytes added from a 1024 px source vs 112,640 from a 256 px
    one — a 5% difference that is resampling noise, not dead weight.
    1024 px is the better source regardless, since the 16/24/32 px
    entries downscale from real detail instead of being upscaled.
  - **`--package` is what emits the shipped name, and it must land
    beside `snug.exe`.** `stamp_dropper_icon --package [DIR]` stamps the
    icon and copies the binary to `DIR\Build with Snug.exe`, defaulting
    to the exe's own folder. That placement is load-bearing rather than
    tidy: the dropper resolves `snug.exe` relative to its *own* location
    at runtime, so separating the two turns every build into a "snug.exe
    could not be found" dialog. `build-release.cmd` step 7 runs it and
    is a first-class step with its own `--SkipDropper` flag — unlike
    `snug_preview`, this is a shipped artefact, not a dev tool, so it is
    not folded behind `--SkipDevTools`.
  - **Both stamp helpers resolve their default profile from
    `cfg!(debug_assertions)`, never from `PROFILE`.** `PROFILE` is
    documented as "set by cargo when building" and is a *build-script*
    variable; cargo does not forward it into the `cargo run` child
    environment. Reading it with an `"debug"` fallback made
    `cargo run --release --bin stamp_*_icon` stamp the **debug** exe,
    exit 0, and leave the release one untouched. `build-release.cmd`
    passes explicit paths, so the release pipeline never relied on the
    default; only the zero-argument path a developer uses while
    iterating on an icon was broken.
  - **`release\` is the shipping folder, and it ignores itself.**
    `build-release.cmd` step 8 stages `snug.exe`,
    `Build with Snug.exe`, `snug_preview.exe` and
    `snug-javafx-demo-windows.jar` in there; the nested `.gitignore` hides
    everything but itself, so a fresh clone has the directory and the
    rules without a single artefact. The two shipping EXEs are staged
    *together* on purpose — same co-location invariant as step 7, now
    enforced at the shipping boundary. The demo JAR is there so someone
    can try the whole drop-a-JAR flow before writing a JAR of their
    own, which is the entire pitch of the tool.
    `snug_preview.exe` is a dev tool and ships anyway: it is how you
    look at snug's dialogs and error copy without building and
    launching an app. It comes from step 6, so `--SkipDevTools` leaves
    it unbuilt and `:stage` warns instead of aborting — but note a
    *stale* `target\release\snug_preview.exe` from an earlier build
    still gets staged, because staging checks the path, not this run's
    flags. Step 8 never empties the directory either: a build script
    that silently deletes is a thing you regret, so an artefact dropped
    from the pipeline lingers into the next run. Use `--Clean` plus a
    fresh folder when you need a guaranteed-clean release.
- **cmd.exe has three quoting traps in `build-release.cmd`, all hit
  while writing step 8.** Each produced a silent no-op or a silent
  exit-1 rather than a parse error, so all three had to be found by
  reading the captured output, not the exit code:
  - **A `for` loop inside a `call`ed subroutine does not work.**
    `for %%I in (...) do echo ... ^(...)` under `:stage` interacts
    badly with the second expansion pass `CALL` performs: the escaped
    parens get mangled, the rest of the subroutine is swallowed along
    with its `exit /b`, and the caller then reads a stale errorlevel.
    A `for` at top level is fine — the "Sizes:" block depends on it.
    Keep the subroutine free of `for`, `%%` and escaped parens.
  - **A trailing `\` inside quotes on an `if exist` line is a hazard.**
    `if not exist "release\"` puts the backslash immediately before the
    closing quote, which some parsers treat as an escaped quote. Use
    `if not exist "release"` and let `mkdir` add the separator.
  - **A subroutine placed after the summary needs an explicit end of
    main flow.** With only `endlocal` between them, the script runs off
    the end of the summary and falls *into* the subroutine with no
    arguments, printing a bogus line and exiting via the subroutine's
    own `exit /b`. Terminate with `endlocal` + `exit /b 0` before the
    label; `CALL` still reaches it because the call happens earlier,
    while the `setlocal` scope is still open.
  This is the strongest argument yet for porting the pipeline to
  xtask — see the release-pipeline note below.
- **A drag-and-drop needs no drag-drop API.** Windows launches the target
  EXE with the dropped paths appended to its command line, so
  `argv[1..]` *is* the drop and "double-clicked" is the empty case. That
  makes the whole mode decision a pure function
  (`decide::decide_with` takes an injectable classifier, so the table is
  tested without touching disk) and keeps the bin to `#![cfg(windows)]`
  while the lib stays cross-platform — `cargo test --workspace` still
  passes on the macOS / Linux dev boxes AGENTS.md assumes.
- **A stale `WM_QUIT` silently eats the next modal dialog.** The dropper's
  marquee loop ends itself, so its `WM_DESTROY` handler must **not** call
  `PostQuitMessage`: the message stays in the thread queue, and
  `MessageBoxW` runs its own modal loop over that same queue — it sees
  the stale quit, tears itself down, and returns immediately. Symptom was
  the success and error dialogs flashing for one frame while the process
  exited 0. This is invisible to unit tests (the dialogs are modal
  Win32 surfaces a harness can't drive) and was caught only by running the
  real binary and polling `EnumWindows`.
- **`build::summarise` reports the *last* `anyhow` cause, not the
  first.** `snug` prints failures as `eprintln!("snug: {err:?}")`, so the
  chain runs outermost-context-first and originating-error-last. Leading
  with the head of that chain pointed a beginner at a manifest when the
  real problem was that the file was not a JAR at all. The full chain
  still goes to the log.
- Profile `release` is tuned for tiny binaries (`opt-level = "z"`, LTO,
  `panic = "abort"`, stripped). The launcher should be ~hundreds of KB
  not megabytes.
- **`build-macos.sh` used to never build the launcher at all.** It ran
  only `cargo build -p snug-cli`, which embeds `bin/launcher-stub-macos-
  <arch>` via `include_bytes!`. So it shipped the *committed* launcher, and
  **every change to `snug-launcher` was silently absent from the artefact**
  while the build stayed green and the bundle still validated. The
  symptom here was an hour of "why did my `SNUG_CACHE_DIR` do nothing?"
  against a launcher built at 17:05 from a source file edited at 21:58.
  The script now builds `-p snug-launcher` and copies it into `bin/`
  *before* `-p snug-cli`, in that order, and echoes which stub it
  refreshed. Never reorder those two: the CLI has to compile *after* the
  bytes it embeds exist. Check the string you just added actually appears
  in the stub (`strings bin/launcher-stub-macos-x86_64 | grep <NAME>`) —
  the failure mode is silence, not an error.
- **`/usr/libexec/java_home` can be non-functional in a sandbox.** It
  prints "Unable to locate a Java Runtime" and exits non-zero while a
  perfectly good JDK sits in `~/Library/Java/JavaVirtualMachines` and
  runs fine. Two consequences, both learned the hard way:
  `discover_jvm` falls back to scanning those directories, so the
  *launcher* is unaffected — but any **pre-flight** that trusts the tool
  alone reports a confident "no JDK found, the demo will not launch" for
  a machine that launches perfectly well. A check that contradicts the
  real code path is worse than no check. `build-macos-demo.sh` now walks
  both sources and reads each home's `release` file, exactly like
  `read_java_major` does.
- **`set -euo pipefail` turns a non-zero tool into a dead build script.**
  `java_home -V | sed …` looks harmless, but `pipefail` propagates
  `java_home`'s exit status to the pipeline, `set -e` then aborts the
  whole script at the pre-flight — the one place a missing tool must be
  survivable, since the fallback below it is the answer to it. A failing
  probe in a diagnostic needs `|| true`.
- **The cache root is not always writable, so it has an escape hatch.**
  `SNUG_CACHE_DIR` redirects the cache *base* (not the root — `snug/
  <company>/<app>/` is still appended, so two apps cannot collide) and
  only honours an absolute path, matching how `XDG_CACHE_HOME` is already
  treated. Without it, a locked-down home or CI runner fails at the very
  first step — extracting the JAR — with `Operation not permitted`, which
  is unrecoverable from the user's side. The decision function takes the
  raw `OsString` rather than reading the environment itself, so the tests
  are hermetic: `set_var` is process-global and a test that used it would
  race every other test in the binary and fail somewhere unrelated.
  Note the Adoptium install root (`jdk_install_root`) is a *different*
  base and has no equivalent override yet.
- **A GUI-subsystem `.app` gives you no console**, so a failed launch is a
  silent no-window. Every runtime fact has to come from the log file in
  the cache — which means redirect the cache first or there is nowhere to
  read from, and the launcher's very first act is to fail. On a machine
  with no GPU/display the run still gets a long way: cache extraction,
  discovery, `libjvm.dylib`, JNI, and the Java main class all succeed,
  and the *only* failure is `QuantumRenderer: no suitable pipeline
  found` from JavaFX. That is the sandbox, not snug — read it as
  "everything up to the toolkit worked".
- **The JDK download flow used to deadlock on macOS. Fixed, and the fix is
  structural.** `worker_thread` opened with
  `while !shared.started.load(Ordering::SeqCst) { … sleep(50ms) }` and
  would not open a single connection until a human clicked Install in the
  progress `NSAlert`; the main thread was meanwhile blocked inside
  `ui::progress`, which was the thing meant to provide that click. So the
  *only* way the download started was a click in a modal window. Exec the
  bundle from a terminal rather than launching it through Launch Services
  and the `NSAlert` has no GUI session to appear in — it degrades to
  logging, and degrading to logging still leaves the worker spinning on a
  click that can never arrive.
  - The evidence that distinguishes this from "a slow download" is on
    disk, and it is decisive: `install_root/<major>/` **is** created
    (`create_dir_all` runs before the worker spawns) while
    `install_root/<major>.tar.gz.tmp` is **not** — `download_to_disk`
    does `ureq.call()` and only then `File::create`. No temp file means
    the worker never left the spin, which means no connection was ever
    opened, which means the 30 s connect/read timeouts never got a
    chance to fire. Minutes of silence plus zero bytes is a hang, never
    bandwidth.
  - **The fix is `ui::consent` running before the worker is spawned.**
    The ask is now a step in its own right, taken while the only thing in
    flight is a human decision, and the spin loop is gone. Windows gets
    the same treatment for free: `prompt_window::show` already existed,
    fully written and reviewed, and was **never called by anything** — the
    `started` gate had quietly replaced it. So wiring `ui::consent` to it
    revives dead code and removes the gate on both platforms at once.
    Check for that class of thing before writing a new one: the
    localisation baseline had seven `jdk_install.prompt.*` keys, also
    entirely unused.
  - Generalisable: **never gate a worker thread on the UI.** A UI that
    fails to appear is not an error path, it is a hang, and it is
    invisible from the network side because no connection is ever opened.
    If a question must be asked, ask it before the thread exists.
  - `DownloadJdkMode::Force` makes this reachable from a *config file*,
    not just a flag — `snug.options` shipped with `--download-jdk=force`,
    so the demo downloaded a second 115 MB copy of JDK 25 on a machine
    that already had one. It is now `auto`.
- **Two `.app` bundles with the same name and company get the same
  `CFBundleIdentifier`, and Launch Services keys on that identifier.**
  So every scratch bundle built to test something can silently hijack
  the real one: `open` resolves the identifier to whichever record it
  likes and returns **`_LSOpenURLsWithCompletionHandler() failed with
  error -1712`**. This is not a property of the bundle — it validates,
  lints and verifies. Diagnose it with
  `lsregister -dump | grep -B4 <identifier>` and look at the *paths*,
  not the identifier.
  - Anything left in `~/.Trash` counts, because macOS indexes the
    Trash. So if the environment wraps `rm -rf` in a recoverable delete,
    every rebuild trashes the old bundle and the Trash copy competes
    under the same identifier — and it defeats the `lsregister -u`
    guard in `build-macos-demo.sh`, which unregisters the path *before*
    the delete and so has already been undone by the time it runs.
  - `lsregister -u` is a no-op where `lsregister` cannot scan: it
    reports `-10822` and bails *before* unregistering, so it silently
    fixes nothing. Check the `-dump` output afterwards instead of
    trusting the exit status.
  - The scratch bundles are the real hazard, so build them with a
    distinct `--name` (hence a distinct identifier), or delete them
    when done.

## Testing

```bash
cargo test --workspace
```

`snug-format` has integration roundtrip tests in
`crates/snug-format/tests/roundtrip.rs`. `snug-cli` has unit tests for
manifest parsing and payload assembly in `src/manifest.rs` and
`src/build.rs`, and end-to-end tests for the `--init-*` modes in
`tests/init_modes.rs` (these spawn the real binary in a temp CWD —
the CWD-relative default isn't observable from a unit test without
mutating process-global state).

`snug-dropper` has unit tests for the decision table, the argument
vector, and the stderr summariser in-crate, plus
`crates/snug-dropper/tests/end_to_end.rs`, which drives a **real**
`snug.exe` against `assets/snug-javafx-demo-windows.jar` and asserts the EXE
actually lands on disk. That test skips (rather than fails) when
`snug.exe` hasn't been built, because `cargo test -p snug-dropper` alone
doesn't build it — `cargo test --workspace` does. **The dialogs and the
marquee are not covered by any test**: they are modal Win32 surfaces a
harness cannot drive without blocking forever. Verify them by running the
real binary (see the `WM_QUIT` note above).

## Out-of-scope questions to defer

- Code signing / Authenticode — defer until release slice.
- Windows SmartScreen reputation — out of scope.
- Cross-platform launcher (mac `.app`, Linux ELF) — explicitly NOT in
  scope. Snug is Windows-only by design.
