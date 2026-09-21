<div align="center">

<img src="assets/snug-splash.png" alt="snug" width="512">

# snug

> A small, modern, Rust-based launcher-wrapper for Java fat JARs. Turns
> `MyApp-fat.jar` into `MyApp.exe` — without bundling a JVM.

</div>

---

> **THIS IS NOWHERE NEAR READY - BUT IT IS CLOSE, PLEASE COME BACK LATER**

---

Snug generates a single native Windows `.exe` that:

- contains the complete fat JAR
- embeds the application icon and optional splash image
- carries Windows application/version metadata (and an optional
  application manifest for DPI awareness, UAC, etc.)
- locates an installed Java 25+ at runtime (env, PATH, registry,
  common install paths)
- loads `jvm.dll` directly via JNI — no `javaw.exe` indirection — so the
  application shows up as `MyApp.exe` in Task Manager

It is conceptually similar to [exe4j] or [Launch4j], but small, modern,
Rust-based, and open source.

[exe4j]: https://www.ej-technologies.com/products/exe4j/overview.html
[Launch4j]: https://launch4j.sourceforge.net/

## Quick Demo

For a quick demo of a JavaFx java demo (with a splashscreen) have a look at 
and run the `./assets/snug-javafx-demo.exe`. 

<div align="center"><img src="assets/snug-javafx-demo-screenshot.png" alt="snug" width="302"></div>

## Status

**Pre-alpha.** Slices 1, 3, and 5 are done; the CLI builds a real
Windows `.exe` end-to-end with in-process resource stamping, and the
launcher runtime in the stub can locate its payload, extract the JAR to
a per-user cache, and discover a compatible JVM + `jvm.dll`. Slice 2 is
partial — the actual `JNI_CreateJavaVM` invocation step is currently
stubbed and returns `JniStub`; it needs Windows-machine validation with
the `jni` 0.22 invocation API before we wire it up.

| Slice | Status |
|-------|--------|
| CLI surface + embedded-payload format | done |
| Windows launcher runtime (JVM discovery + JNI + splash) | partial — runtime logic + cross-compiled stub in place; JNI launch stubbed, needs Windows validation |
| snug-cli builder: stub + payload concatenation, in-process `editpe` resource stamping | **done** |
| Resource stamping — `VS_VERSIONINFO` (`ProductName`, `CompanyName`, `FileDescription`, `LegalCopyright`, `FixedFileInfo` file/product version, `VFT_APP`) + optional `--icon` + optional `--manifest` | done |
| Native splash renderer (PNG via GDI+/WIC) | planned |
| Per-user cache + old-version cleanup | planned |
| GitHub Actions CI (windows-latest release) | planned |

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

# Inspect what would be built without writing anything:
snug app.jar -o App.exe --dry-run

# Pipe the encoded payload to stdout instead of building an EXE
# (useful for piping into other tools or for inspecting the format):
snug app.jar --emit-payload

# Use a `snug.options` file for default settings (CLI flags always override).
# Each line is parsed as if it were on the command line; `#` is a comment.
# `--input` (file or directory) and every other flag can live here too,
# so the JAR location doesn't have to be on the command line:
#   # snug.options (in cwd)
#   --input path/to/app.jar         # or path/to/lib-dir/
#   --name "My App"
#   --company "SynapticLoop"
#   --min-java 25
snug -o App.exe --name "Different Name"   # --name overrides the file

# Snug's own version (distinct from --version, which sets the app's version):
snug --snug-version
```

The produced `.exe` is a 64-bit Windows GUI binary that:
- contains the full fat JAR + icon + splash (when supplied) embedded at
  the tail behind an 8-byte `SNUGEMBD` magic trailer,
- displays as a Windows GUI executable (no console window),
- on launch scans itself for the `SNUGEMBD` trailer, decodes the
  postcard payload, extracts the JAR to
  `%LOCALAPPDATA%\snug\<company>\<name>\<jar-sha256>\app.jar`, locates
  a Java 25+ install + `jvm.dll`, and invokes the Java `main` class.

## Demo

A JavaFX demo JAR ships with the repo at
`assets/snug-javafx-demo.jar`. End-to-end build (from the repo root):

```powershell
cargo build --release -p snug-cli
.\target\release\snug.exe assets\snug-javafx-demo.jar `
    -o snug-javafx-demo.exe `
    --name "Snug JavaFX Demo" --company "SynapticLoop" --version 0.1.0 `
    --description "Snug JavaFX demo launcher" --copyright "© SynapticLoop" `
    --icon assets\snug-icon.png
```

The JAR's `Main-Class` (`synapticloop.snugjavafxdemo.HelloApplication`)
is read from its manifest, so no `--main-class` override is needed.

## Layout

```text
snug/
├── Cargo.toml                  # workspace root
├── AGENTS.md                   # project context for AI agents
├── HANDOFF.md                  # session-end notes for the next agent
├── README.md                   # this file
├── assets/
│   ├── snug-icon.png           # 1254×1254, used by README + --icon examples
│   └── snug-javafx-demo.jar    # JavaFX demo fat JAR (Main-Class read from manifest)
├── bin/
│   └── launcher-stub.exe       # precompiled cross-platform stub (PE32+ GUI x86-64, ~454 KB)
└── crates/
    ├── snug-format/            # embedded-payload types + postcard codec (the wire contract)
    ├── snug-launcher/          # Windows runtime: payload self-scan, cache, JVM/jvm.dll discovery, JNI launch
    └── snug-cli/               # CLI + stub-append builder + snug.options + editpe stamping
```

`snug-format` is the **contract** between the builder (CLI side) and the
launcher (runtime side). The wire layout is:

```text
+--------+----------------+--------------+--------------+----------------------+
| magic  | format_version | payload_len  | payload_crc32| payload (postcard)   |
| 8 B    | u16 LE         | u32 LE       | u32 LE       | payload_len bytes    |
+--------+----------------+--------------+--------------+----------------------+
```

The 8-byte magic is `SNUGEMBD`. `format_version` is currently `1`.
`payload_crc32` is CRC32/IEEE of the postcard bytes; the launcher
validates before decoding. Payload is a postcard-encoded
`SnugPayload { config, jar, icon }`.

The stub is currently appended (v1, `bin/launcher-stub.exe`); the
launcher locates the trailer by scanning itself for the magic. A v2
that stores the payload as an `RCDATA` resource is a planned
hardening slice.

## Building

Prerequisites: a Rust 1.85+ toolchain, plus `zig` 0.14+ and
`cargo-zigbuild` to (re)build the Windows stub from macOS or Linux.

```bash
# one-time setup on a fresh Mac/Linux dev box
brew install zig                                  # or download from ziglang.org
rustup target add x86_64-pc-windows-gnu
cargo install cargo-zigbuild --locked

# full workspace tests
cargo test --workspace

# build the snug CLI for the host platform
cargo build --release -p snug-cli

# regenerate the stub after changing snug-launcher code:
cargo zigbuild --target x86_64-pc-windows-gnu --release -p snug-launcher
cp target/x86_64-pc-windows-gnu/release/snug-launcher.exe bin/launcher-stub.exe
```

Notable test binaries:

- `crates/snug-format/tests/roundtrip.rs` — payload encode/decode
- `crates/snug-launcher/tests/stub_payload_roundtrip.rs` — proves the
  stub's locator still finds the real trailer among the false-positive
  `SNUGEMBD` substrings inside the stub binary
- `crates/snug-cli/tests/end_to_end.rs` — full CLI invocation,
  stub-append, PE resource stamp
- `crates/snug-cli/tests/editpe_roundtrip.rs` — `editpe`-based icon /
  version / manifest stamping roundtrip
- `crates/snug-cli/tests/options_file.rs`, `tests/no_args.rs` —
  `snug.options` config-file and no-args help behaviour

## CLI surface (canonical)

```text
snug [<jar>] [-o EXE] [--input JAR|DIR] [--name ...] [--company ...]
           [--version ...] [--description ...] [--copyright ...]
           [--min-java N] [--main-class CLASS]
           [--icon PNG/ICO] [--manifest XML] [--splash PNG] [--splash-ms MS]
           [--jvm-arg ARG]...
           [--emit-payload] [--dry-run] [--snug-version]
           [--options PATH]
```

Snug's own version (from `Cargo.toml`) is `--snug-version` (clap's
auto `--version` is disabled so the brief's `--version <APP-VERSION>`
doesn't collide with it).

Resource stamping is unconditional and in-process — there is no
`--rcedit` / `--no-rcedit` flag any more; the prior subprocess
integration was replaced by the pure-Rust
[`editpe`](https://github.com/Systemcluster/editpe) crate, which works
identically on macOS, Linux, and Windows.

## Licence

Dual-licensed under MIT OR Apache-2.0, at your option.

<div align="center"><img src="assets/snug-icon.png" alt="snug" width="256"></div>
