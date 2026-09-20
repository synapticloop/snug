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
            ├── extract/cache fat JAR under %LOCALAPPDATA%\<company>\<app>\snug\<sha256>\
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
│   └── snug-format/      # shared embedded-payload types + codec
├── examples/
└── tests/
```

A single-crate implementation is fine initially; split only when useful.
**Slice 1 (this commit)** uses a 2-crate workspace (`snug-format` +
`snug-cli`); the launcher and builder will be added as `snug-launcher` +
`snug-builder`.

## Build host

Developers are expected to be on macOS / Linux; the build target is
**Windows `.exe`**. The intended build strategy:

- **Local dev:** `cargo-zigbuild` for cross-compiling from macOS / Linux
  to `x86_64-pc-windows-gnu`. No Visual Studio Build Tools needed.
  Requires `zig` (any recent 0.14+ release) and the
  `x86_64-pc-windows-gnu` Rust target installed via `rustup`.
- **CI / releases:** GitHub Actions `windows-latest` runner, so
  Authenticode signing is available.

```bash
# one-time setup on a fresh Mac/Linux dev box
brew install zig                                  # or download from ziglang.org
rustup target add x86_64-pc-windows-gnu
cargo install cargo-zigbuild --locked

# build the precompiled stub (committed to bin/launcher-stub.exe)
cargo zigbuild --target x86_64-pc-windows-gnu --release -p snug-launcher
cp target/x86_64-pc-windows-gnu/release/snug-launcher.exe bin/launcher-stub.exe
```

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
| 2 | Windows launcher runtime (JVM discovery + JNI) | partial — payload self-scan, cache path, manifest lookup, JVM discovery (env / PATH / registry / common), jvm.dll discovery, and the bare-stub launcher binary are all wired and compile-tested. **Actual `JNI_CreateJavaVM` launch path is stubbed** and returns a `JniStub` error; the `jni` 0.22 invocation API needs Windows-machine validation before we wire it up. A 454 KB precompiled `bin/launcher-stub.exe` (PE32+ GUI x86-64) is committed. |
| 3 | snug-cli builder: load stub via `include_bytes!()`, append payload, stamp icon/version/manifest via `editpe` | **done** — `snug app.jar -o App.exe` produces a real Windows `.exe` end-to-end. `include_bytes!` of the committed stub + encoded payload concatenation is unit + integration tested. Resource stamping is in-process via the [`editpe`](https://github.com/Systemcluster/editpe) crate (pure Rust, BSD-2-Clause, cross-platform — same code path runs on macOS, Linux, and Windows). `--icon` accepts PNG or ICO; `--manifest <XML>` embeds an arbitrary application manifest. |
| 4 | Stub-append v2 hardening (alignment, overlay vs append, sparse stubs), per-app resource stamping UX, CI on `windows-latest` to actually run the produced EXE against a real JDK | planned |
| 5 | Windows version-resource stamping              | **done** — `editpe` stamps `VS_VERSIONINFO` (ProductName / CompanyName / FileDescription / LegalCopyright) plus `FixedFileInfo` (signature, file/product version, VFT_APP) into every produced EXE. `--manifest <XML>` covers the application manifest gap. Icon dimension / MUI / translation coverage is follow-up. |
| 6 | Native splash renderer (PNG via GDI+/WIC)      | planned    |
| 7 | Per-user cache + old-version cleanup           | planned    |
| 8 | GitHub Actions CI (windows-latest release)     | planned    |

## Conventions

- Rust edition **2024**, MSRV **1.85**.
- Errors via `thiserror` for library types, `anyhow` at the CLI
  boundary.
- No `unsafe` in any crate. (`#![forbid(unsafe_code)]` is set in
  `snug-format` and `snug-cli`.)
- Binary artefacts embedded in the launcher are referenced by their
  SHA-256 digest — that digest is also the cache key.
- All CLI flags follow the brief: see `crates/snug-cli/src/cli.rs` for
  the canonical surface. Note: clap's auto-generated `--version` flag
  is disabled (via `disable_version_flag = true`) so the brief's
  `--version <APP-VERSION>` doesn't collide with it. Snug's own
  version is discoverable via `--snug-version` or `cargo metadata`.
  Resource stamping is unconditional; the prior `--rcedit <path>` and
  `--no-rcedit` flags are gone in favour of an in-process `editpe`
  integration. The new `--manifest <XML>` flag embeds an arbitrary
  Windows application manifest (XML).
- **`snug.options` files** are supported. The CLI resolves an options
  file via `--options <path>` (explicit) or `snug.options` in the
  current working directory (default). One option per line, parsed as
  shell-like tokens (so `--name "My App"` works with quotes and
  escapes); `#`-prefixed lines are comments. CLI flags always override
  file values (the file's matching flag is stripped at merge time).
  Repeatable flags (`--jvm-arg`) accumulate from both sources.
- Profile `release` is tuned for tiny binaries (`opt-level = "z"`, LTO,
  `panic = "abort"`, stripped). The launcher should be ~hundreds of KB
  not megabytes.

## Testing

```bash
cargo test --workspace
```

`snug-format` has integration roundtrip tests in
`crates/snug-format/tests/roundtrip.rs`. `snug-cli` has unit tests for
manifest parsing and payload assembly in `src/manifest.rs` and
`src/build.rs`.

## Out-of-scope questions to defer

- Code signing / Authenticode — defer until release slice.
- Windows SmartScreen reputation — out of scope.
- Cross-platform launcher (mac `.app`, Linux ELF) — explicitly NOT in
  scope. Snug is Windows-only by design.
