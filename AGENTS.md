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

## Versioning

Snug is **pre-1.0 and stays that way until the project is declared
finished.** Bump the version on every change; never land on `1.0.0`
by accident.

- **Single source of truth:** `[workspace.package] version` in the
  root `Cargo.toml`. All three crates inherit it via
  `version.workspace = true`, so that one line is the only thing to
  edit. `snug --snug-version` and `cargo metadata` read the compiled
  value; `snug-launcher/src/main.rs` exposes it as `env!("CARGO_PKG_VERSION")`.
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
- **A `Main-Class` need not have a `main` method.** `--find-main` lists
  every class in the input JAR(s) declaring `public static void
  main(String[])` (`crates/snug-cli/src/classfile.rs` parses the method
  table only — no bytecode). It is a **read-only diagnostic**: it never
  fails a build, because a legitimately missing `main` is normal. The
  committed `assets/snug-javafx-demo.jar` is the standing example: its
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
- **`snug.options` files** are supported. The CLI resolves an options
  file via `--options <path>` (explicit) or, with no flag,
  `snug.options` **next to the snug executable first, then**
  `snug.options` in the current working directory. So a
  `snug.exe` shipped alongside its `snug.options` carries its
  defaults wherever it is invoked from; the CWD copy is the
  fallback, not the primary. One option per line, parsed as
  shell-like tokens (so `--name "My App"` works with quotes and
  escapes); `#`-prefixed lines are comments. CLI flags always
  override file values (the file's matching flag is stripped at
  merge time). Repeatable flags (`--jvm-arg`) accumulate from both
  sources. Resolution lives in `options_file::resolve`, which takes
  the CWD and the exe dir (`options_file::current_exe_dir()`) so the
  precedence order is testable without spawning the binary.
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
  found with the built-in English baseline last, unless the user
  supplied their own `en` — same replacement rule as a build. It
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
`src/build.rs`, and end-to-end tests for the `--init-*` modes in
`tests/init_modes.rs` (these spawn the real binary in a temp CWD —
the CWD-relative default isn't observable from a unit test without
mutating process-global state).

## Out-of-scope questions to defer

- Code signing / Authenticode — defer until release slice.
- Windows SmartScreen reputation — out of scope.
- Cross-platform launcher (mac `.app`, Linux ELF) — explicitly NOT in
  scope. Snug is Windows-only by design.
