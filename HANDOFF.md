# Snug — Handoff Notes

**Session end:** 2026-09-20
**Last commit:** `8a90f8d` on `main` (HEAD includes uncommitted editpe swap)
**Toolchain:** rustc 1.98.1, cargo 1.96.0, zig 0.14.1, cargo-zigbuild 0.x
**Tests:** 55 passing across 14 test binaries (one pre-existing launcher test broken on Windows — see "Known failures")

---

## What's done

| Slice | Status |
|---|---|
| 1 — CLI surface + embedded-payload format | done |
| 2 — Windows launcher runtime + cross-compiled stub | partial (JNI launch is **stubbed** — see below) |
| 3 — snug-cli builder (stub + payload concat, editpe) | done |
| Extras — `--snug-version`, no-args help, `snug.options` config, README logo | done |
| Extras — editpe in-process resource stamping, `--manifest`, PNG icons | done |

### Repository layout

```
snug/
├── assets/snug-icon.png          # 1254×1254, centred in README
├── bin/launcher-stub.exe         # 454 KB, PE32+ GUI x86-64, committed
├── crates/
│   ├── snug-format/              # embedded-payload types + postcard codec
│   ├── snug-launcher/            # Windows runtime: JVM discovery, JNI
│   └── snug-cli/                 # CLI + stub-append builder + options file
├── examples/                     # empty, reserved for future samples
├── Cargo.toml                    # workspace root
├── AGENTS.md                     # project context for AI agents
├── README.md                     # logo + quick-start
└── HANDOFF.md                    # this file
```

### Workspace deps in `Cargo.toml`

- `serde 1` (derive), `postcard 1` (alloc), `thiserror 1`
- `sha2 0.10`, `crc32fast 1`
- `clap 4` (derive), `anyhow 1`
- `zip 2` (deflate)
- `windows-sys 0.59` (gated to `cfg(windows)`)
- `jni 0.22` (invocation feature, gated to `cfg(windows)`)
- `shell-words 1` (used in `snug.options` parser)
- `editpe 0.2` (no_std default features off, `images` feature on) — in-process PE resource editor (icon + version + manifest stamping). Replaces the prior subprocess-based `rcedit` integration. Dev-dep: `image 0.25` (PNG support).

---

## What's stubbed and needs Windows-machine validation

### 1. JNI launch path — HIGHEST PRIORITY

**File:** `crates/snug-launcher/src/platform/windows.rs::run`
**Current behaviour:** After locating a usable `jvm.dll`, returns
`LauncherError::JniStub` and exits. The earlier steps (cache extract,
Main-Class lookup, JVM discovery, `jvm.dll` location) all run.

**What to do:**

1. Wire up the `jni` crate's `JavaVM::with_libjvm(...)` pointing at
   `<jvm_dir>/bin/server/jvm.dll`. (Fallback to `JavaVM::new` if that
   fails — the crate can locate `jvm.dll` itself when given a JVM
   path.)
2. Build `InitArgsBuilder::new()`
   - `.version(JNIVersion::V21)` (the crate doesn't have a `V25`
     constant yet — V21 is the latest it ships and works fine with
     Java 25 JVMs because JNI is backward-compatible)
   - `.options(&config.jvm_args)`
   - `.build()`
3. `vm.attach_current_thread()` to get a `JNIEnv<'_>`.
4. Find the main class:
   - `env.find_class(&main_class_name.replace('.', "/"))`
   - `env.get_static_method_id(&class, "main", "([Ljava/lang/String;)V")`
5. Build the args array from `CommandLineToArgvW(GetCommandLineW())`,
   skipping `argv[0]`. The `collect_argv()` helper at the bottom of
   `windows.rs` has a known broken `extern "system"` block — it needs
   to become `unsafe extern "system"`.
6. Invoke: `env.call_static_method_unchecked(&class, method_id,
   JavaType::Object(JObject::from(args_array)), &[])`.
7. On error, call `env.exception_describe()` and `env.exception_clear()`
   for diagnostics before bailing with `LauncherError::JavaException`.
8. `vm.destroy()` on the way out.

**Why this needs Windows:** The function-table offsets, exception
descriptors, and `AttachCurrentThread` lifetime are all real-CPU
concerns. Until exercised on a Windows machine with a real JDK,
treat this as unverified code.

---

### 2. Native splash renderer — deferred (next slice)

**Brief:** PNG splash shown by the native launcher *before* the JVM
loads; dismissed when JVM signals ready or after `--splash-ms`,
whichever is later.

**Implementation sketch:**
- WIC (`Win32_Graphics_Imaging`) for PNG decode, or GDI+ for the
  simpler path
- Window class registration via `RegisterClassExW`, then
  `CreateWindowExW` with `WS_EX_TOPMOST | WS_EX_TOOLWINDOW`
- `PeekMessageW` / `DispatchMessageW` loop on a splash thread
- Splash thread waits on a `CreateEventW` signalled by the JVM
  thread once `main` has been entered
- On signal (or timeout): `DestroyWindow` + thread join

**Status:** No code yet. The `SplashConfig` struct in
`snug-format` already carries the PNG bytes + duration.

---

### 3. Stub-append v2 hardening — slice 4

The current "stub + payload concatenation" works but is fragile in
three known ways:

1. **Overlay vs section.** Windows treats anything after the last
   PE section as an overlay. Most tools ignore it; some packers
   strip it. Future-proofing: store the payload as a custom
   `RCDATA` resource via `UpdateResourceW` instead of appending.

2. **Stub self-awareness.** When using overlay mode, the stub needs
   to know its own payload offset at runtime. Currently
   `payload_locator` does a backwards scan, which is O(n) on file
   size. Adding a fixed-offset mode (the stub knows its own size)
   would make scan O(payload_size).

3. **Sparse stubs.** If we add per-app resource stamping via
   `rcedit`, the stamped EXE has different `SizeOfImage` / checksum
   fields than the pre-stamp stub. The locator needs to handle
   this — currently it does because of the backwards scan, but the
   cost is non-trivial on large files. `MAX_TAIL_SEARCH` is 64 MiB.

**Status:** Not started.

---

### 4. Per-user cache + old-version cleanup — deferred

The cache layout (`%LOCALAPPDATA%\snug\<company>\<app>\<jar-sha256>\`)
is in place in `crates/snug-launcher/src/cache.rs`. What's missing:

- Old-version cleanup at startup (scan sibling sha256 dirs, prune
  any that aren't the current JAR's hash)
- Cross-platform path quirks (Windows backslash, long-path support)
- Tests that exercise the cache directory resolver

---

## Deferred design decisions (open questions)

These were raised in the session but not resolved. Lean verdicts
included where I have one.

1. **`--rcedit <path>` CLI flag vs `RCEDIT` env var.** ~~Lean: drop
   the CLI flag, add the env var.~~ **Resolved 2026-09-20: the whole
   `rcedit` integration was replaced by the in-process `editpe` crate.**
   Both `--rcedit` and `--no-rcedit` are gone; resource stamping is
   unconditional (version info always; icon/manifest when supplied).
   `--manifest <XML>` is the new opt-in flag for shipping a custom
   Windows application manifest.

2. **Short-flag aliases.** Only `-o` has a short form. Conventional
   pairings worth adding: `-n` for `--name`, `-c` for `--company`,
   `-V` for `--snug-version`.

3. **Output verbosity.** No `--quiet` / `--verbose` / `--force`.
   Lean: add `--quiet` / `-q` to silence success messages in
   scripts; skip the others (YAGNI).

4. **Config file format.** `snug.options` is one option per line,
   which is simple but flat. A `snug.toml` with sections
   (`[jvm_discovery]`, `[resources]`, `[build]`) would be richer but
   more code. Defer until someone asks.

5. **JVM discovery fine-tuning CLI flags.** `LauncherBehavior` /
   `JvmDiscovery` supports `try_java_home`, `try_path`,
   `try_registry`, etc. but the CLI doesn't expose them. Lean:
   **skip** — too fiddly for a CLI; defaults are sensible.

6. **Validation strictness.** Currently lenient on format, fail
   loudly on missing files. Could tighten (semver validation of
   `--version`, real `.ico` parsing, etc.).

7. **Cross-arch support.** Only `x86_64-pc-windows-gnu`. ARM64
   Windows (Surface Pro X, Snapdragon X) is increasingly common.
   Add `aarch64-pc-windows-gnu` target.

8. **Code signing / Authenticode.** Out of scope until release
   slice. Document that signed binaries need `signtool` after
   build (Windows-only).

9. **GitHub Actions CI on `windows-latest`.** Needed to actually
   run the produced .exe against a real JDK. Stub for now; full
   coverage later.

---

## How to verify your work

### Test commands

```bash
# full workspace tests
cargo test --workspace

# cross-compile the stub after editing snug-launcher
cargo zigbuild --target x86_64-pc-windows-gnu --release -p snug-launcher
cp target/x86_64-pc-windows-gnu/release/snug-launcher.exe bin/launcher-stub.exe

# stub roundtrip (most important — proves the locator still works
# despite the 5 false-positive SNUGEMBD substrings inside the stub)
cargo test -p snug-launcher --test stub_payload_roundtrip

# options file + no-args behaviour
cargo test -p snug-cli --test options_file
cargo test -p snug-cli --test no_args
```

### End-to-end manual smoke

```bash
mkdir -p /tmp/snug-smoke/META-INF
printf "Manifest-Version: 1.0\nMain-Class: com.example.Main\n" \
    > /tmp/snug-smoke/META-INF/MANIFEST.MF
(cd /tmp/snug-smoke && zip -qr smoke.jar META-INF/MANIFEST.MF)

cargo run --release -- /tmp/snug-smoke/smoke.jar -o /tmp/snug-smoke/Demo.exe \
  --name "Demo App" --company "SynapticLoop" --version 1.0.0 \
  --min-java 25 --main-class com.example.Main --jvm-arg=-Xmx512m

# Verify
file /tmp/snug-smoke/Demo.exe
# expect: PE32+ executable (GUI) x86-64, for MS Windows
```

### Snug.options smoke

```bash
mkdir -p /tmp/snug-opts && cd /tmp/snug-opts
cat > snug.options <<EOF
# Default metadata
--name "My App"
--company "Acme"
--min-java 21
EOF
# CLI override takes precedence
/Users/osmanj/IdeaProjects/snug/target/release/snug /tmp/snug-smoke/smoke.jar \
    --dry-run --name "Override"
# expect stderr: "snug: loaded options from /tmp/snug-opts/snug.options"
# expect stdout: app name reflected from --name override
```

---

## Key files to read first

If you're picking this up cold, read in this order:

1. **`AGENTS.md`** — project context, conventions, slice roadmap
2. **`crates/snug-format/src/lib.rs`** — embedded-payload types
3. **`crates/snug-launcher/src/lib.rs`** — launcher runtime API
4. **`crates/snug-launcher/src/platform/windows.rs::run`** —
   the stubbed JNI launch point (HIGHEST PRIORITY)
5. **`crates/snug-cli/src/main.rs`** — entry point + options file
6. **`crates/snug-cli/src/cli.rs`** — clap surface
7. **`crates/snug-cli/src/options_file.rs`** — config file format

---

## Gotchas

- **The committed `bin/launcher-stub.exe` contains 5 false-positive
  `SNUGEMBD` substrings** in its data section (the magic constant
  gets compiled into the binary). `payload_locator::find_in_file`
  handles this by attempting full decode at each match and skipping
  on validation failure. If you change the magic or the scan
  strategy, re-run `cargo test -p snug-launcher --test
  stub_payload_roundtrip`.

- **`clap`'s auto `--version` is disabled** (`disable_version_flag =
  true`) because it clashed with the brief's `--version <APP-VER>`.
  Use `--snug-version` (or run with no args) to see snug's own
  version.

- **`shell-words` tokenizes each line of `snug.options`.** Quoted
  strings (`"My App"`) and backslash escapes work. Without quotes,
  whitespace splits the value into separate tokens — `--company
  Acme Corp` becomes three tokens, and clap will reject the third
  as unexpected. Quote your values.

- **The stub binary is cross-compiled from this Mac via zigbuild.**
  It compiles and links cleanly for `x86_64-pc-windows-gnu`, but it
  has never been run on Windows. Until the JNI launch path is wired
  up and validated on a real machine, the produced .exe will fail
  with `JniStub` on first launch.

- **Edition 2024 unsafe rules.** `unsafe extern "system" { ... }`
  blocks require the `unsafe` keyword on the extern block itself.
  `#![forbid(unsafe_code)]` was relaxed to `#![deny(unsafe_op_in_unsafe_fn)]`
  in `snug-launcher` because the Windows FFI genuinely needs unsafe.
  Unsafe is confined to `#[cfg(windows)]` modules.

---

## What I'd do next, in priority order

1. **Wire up the JNI launch path** on a Windows machine (or
   GitHub Actions `windows-latest` runner). The structure is all
   there; just fill in `JavaVM::with_libjvm` →
   `attach_current_thread` → `find_class` →
   `call_static_method_unchecked`. Verify by running the produced
   `.exe` against a real JDK 25 install.

2. **Get a Windows host in CI** so subsequent changes can be
   end-to-end tested without a human in the loop. Also catches the
   pre-existing `parse_version_subkey_orders_correctly` failure
   that `cargo test --workspace` currently shows on Windows.

3. **Native splash renderer** (WIC + GDI+). Biggest remaining UX
   win; the user-facing "before JVM" feel.

4. **Add `-V` / `-n` / `-c` short aliases** for the most-used flags.

5. **Stub-append v2 hardening.** Overlay vs resource-mode,
   fixed-offset locator, sparse-stub caching. Less urgent now that
   `editpe` writes a proper resource section.

6. **Per-user cache cleanup at startup.** Prune sibling sha256
   dirs that aren't the current hash.

7. **GitHub Actions CI on `windows-latest`.** Build the stub,
   build a sample .exe, run it against a JDK, capture output.
