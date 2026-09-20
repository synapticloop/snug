# snug

> A small, modern, Rust-based launcher-wrapper for Java fat JARs. Turns
> `MyApp-fat.jar` into `MyApp.exe` — without bundling a JVM.

Snug generates a single native Windows `.exe` that:

- contains the complete fat JAR
- embeds the application icon and optional splash image
- carries Windows application/version metadata
- locates an installed Java 25+ at runtime
- loads `jvm.dll` directly via JNI (no `javaw.exe` indirection — the
  application shows up as `MyApp.exe` in Task Manager)

It is conceptually similar to [exe4j] or [Launch4j], but small, modern,
Rust-based, and open source.

[exe4j]: https://www.ej-technologies.com/products/exe4j/overview.html
[Launch4j]: https://launch4j.sourceforge.net/

## Status

**Pre-alpha, slice 3 of N — produces real Windows `.exe` files end-to-end.**
The CLI parses inputs, builds an embedded payload, concatenates a
precompiled launcher stub, and writes a working Windows GUI
executable. The launcher runtime inside the stub can locate its
payload, extract the JAR to a per-user cache, and discover a
compatible JVM. The actual `JNI_CreateJavaVM` invocation step inside
the launcher is currently stubbed and returns `JniStub` — it will be
wired up in a follow-up once we can validate it on a real Windows
machine.

| Slice | Status |
|-------|--------|
| CLI surface + embedded-payload format | done |
| Windows launcher runtime (JVM discovery + JNI + splash) | partial — runtime logic + cross-compiled stub in place; JNI launch stubbed, needs Windows validation |
| snug-cli builder: stub + payload concatenation, optional rcedit stamping | **done** |
| Resource stamping (rcedit integration, version-resource fields) | done in slice 3 |
| Native splash renderer (PNG via GDI+/WIC) | planned |
| Per-user cache + old-version cleanup | planned |
| GitHub Actions CI (windows-latest release) | planned |

## Quick start

```text
# Build a Windows .exe from a fat JAR (stub + payload concatenation):
snug app.jar -o App.exe --name "My App" --company "SynapticLoop" \
    --version 1.0.0 --main-class com.example.Main --min-java 25

# Skip resource stamping (build on macOS, stamp on Windows):
snug app.jar -o App.exe --no-rcedit

# Inspect what would be built without writing anything:
snug app.jar -o App.exe --dry-run

# Pipe the encoded payload to stdout (.snug-blob format):
snug app.jar --emit-payload
```

The produced `.exe` is a 64-bit Windows GUI binary that:
- contains the full fat JAR embedded at the tail,
- displays as a Windows GUI executable (no console window),
- on launch scans itself for the `SNUGEMBD` payload, decodes it,
  locates a Java 25+ install, extracts the JAR to
  `%LOCALAPPDATA%\<company>\<name>\snug\<sha256>\app.jar`, loads
  `jvm.dll`, and invokes the Java `main` class.

## Layout

```text
snug/
├── Cargo.toml                  # workspace root
├── crates/
│   ├── snug-format/            # embedded-payload types + codec
│   └── snug-cli/               # CLI binary
├── examples/                   # (future) sample Java apps to wrap
└── tests/                      # (future) cross-crate integration tests
```

`snug-format` is the **contract** between the builder (CLI side) and the
launcher (runtime side). The same encoded blob will be embedded via
`include_bytes!()` in slice 3 (template-per-build) and appended to a
precompiled stub in slice 4.

## Building

```bash
# Run all tests
cargo test --workspace

# Build the snug CLI
cargo build --release -p snug-cli

# Regenerate the stub after changing snug-launcher code:
cargo zigbuild --target x86_64-pc-windows-gnu --release -p snug-launcher
cp target/x86_64-pc-windows-gnu/release/snug-launcher.exe bin/launcher-stub.exe
```

## Licence

Dual-licensed under MIT OR Apache-2.0, at your option.
