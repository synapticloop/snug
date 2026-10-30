# Snug build validation and diagnostic logging — design

**Status:** implemented. This document is the design the implementation
follows; the reference for the codes it produces is
[`docs/diagnostics.md`](diagnostics.md).
**Source reviewed:** `1ad56f9479fed8efbef7177f9778785dfa551a09` (`origin/main` as of 2026-10-26).
**Recheck note:** the local `main` was two commits behind at `8f87cb9`; the delta was
documentation-only (`README.md` plus one `--options` help-text line). Every behavioural
claim below was re-verified against current source.

---

## 1. Confirmed decisions

These came from the user's requirements document plus the design discussion. They are
settled, not proposals.

| # | Decision |
|---|---|
| D1 | Every build writes diagnostics and progress to **both** the console and a log. |
| D2 | Default log destination is `snug.build.log` in the current working directory. Each build overwrites the previous log. |
| D3 | `--build-log <PATH>` overrides the destination and works in `snug.options` under normal precedence. |
| D4 | An existing directory means `snug.build.log` inside it; a file path means that filename; a **nonexistent** path is treated as a file (no extension guessing). The parent directory must already exist: snug does not create directories. The actual location is always reported. |
| D5 | The build log shows the effective options **and their origins**. Provenance is retained through loading and merging, not reconstructed afterwards. Repeatable options carry a source per value. |
| D6 | Option origins are captured by a **side-channel alongside the merged argv**, leaving clap's parsing behaviour untouched. |
| D7 | Class-version checking: the **selected entry class** exceeding `--min-java` is an **error**; any other class exceeding it is a **warning**. |
| D8 | **All** console output goes to **stderr**, including the dry-run summary moved off stdout. stdout is reserved for `--emit-payload` binary output. |
| D9 | When Quick Build runs, the authoritative build log lives in the **dropper's own folder**; the dropper passes `--build-log` explicitly. |
| D10 | The dropper's existing `snug-build.log` transcript is **kept** alongside the CLI's richer report. |
| D11 | Redaction masks **credential-shaped** `--jvm-arg` values only. Harmless flags stay visible. |
| D12 | The log's **first line** states the working directory, and every path after it is written **relative to that directory**. A path outside the working directory stays absolute. |

Adopted as stated in the requirements document, without further discussion:

- Relative paths resolve against the current working directory (the existing convention —
  `options_file.rs:34-48`).
- Clap parsing becomes fallible so ordinary parse errors can be logged. Help and version
  remain normal informational exits.
- If the requested log cannot be opened, the build stops **before** the executable is
  created, after attempting a temporary fallback log. The original build error is
  preserved if logging also fails.
- Partial log-write failures are reported explicitly; the CLI never claims a complete log
  was saved.
- No payload format change. Build reports and provenance live outside `LauncherConfig`
  and the embedded payload, so `format_version` stays at `7` and the committed launcher
  stub is **not** rebuilt.
- No log history in the first implementation. A user who wants to keep a run selects a
  different filename.
- `--dry-run` and `--emit-payload` become mutually exclusive.

---

## 2. Where the current code actually loses provenance

This is the central finding, and it drives D6.

```
argv ─────────────┐
snug.options ─────┼─→ load()  → Vec<String>          options_file.rs:266
snug.<os>.options ┘     (tokens; line numbers dropped)
                            ↓
                      merge() → Vec<String>          options_file.rs:513
                            (losers stripped, not reported)
                            ↓
                      Cli::parse_from(merged)        main.rs:82   ← one parse, one source
```

Two facts:

1. **`load()` discards the line number.** The file is already walked line by line — the
   `idx + 1` at `options_file.rs:294` exists for the syntax error — but tokens come back
   as bare `String`s. Per-key line numbers cannot be recovered later; they must be kept at
   read time.

2. **`merge()` deletes the losers.** `spec.strip_overridden(tokens, &seen)`
   (`options_file.rs:544`) drops a lower layer's flag *and* its value when a higher layer
   already supplied that flag. The merged argv holds only winners, so `--name` from
   `snug.options:4` and `--name` from the command line are indistinguishable — and the
   losing value is already gone.

A third fact that shapes the display: `--min-java`, `--splash-ms`, `--splash-max` and
`--download-jdk` carry clap-level defaults, so an explicit `--min-java 25` is
indistinguishable from the default after parsing. `[command line]` versus `[default]`
requires clap's `value_source`, not a post-hoc guess.

---

## 3. Architecture

### 3.1 Module layout

Everything stays inside `snug-cli`. No new crate, no general validation framework.

```
crates/snug-cli/src/
├── report.rs          NEW  reporter, diagnostic codes, severity, redaction
├── provenance.rs      NEW  Origin, Merged, token→origin attribution
├── build_log.rs       NEW  log destination resolution, open/truncate, fallback
├── options_file.rs    CHG  load() keeps line numbers; merge() reports losers
├── main.rs            CHG  fallible parse, wiring, reporting
├── build.rs           CHG  shared preflight + validation
├── classfile.rs       CHG  retain major/minor, add version + superclass scans
├── manifest.rs        CHG  main-section boundary, case-insensitive headers
└── find_main.rs       CHG  distinguish "no main" from "could not parse"
```

`snug-dropper` gains only the `--build-log` argument it passes through.

### 3.2 Flow

```
load options → merge (with origins) → resolve --build-log → open log
     │                                                             │
     ↓                                                             ↓
fallible clap parse ──────────────────────────────→ parse errors logged
     ↓
resolve inputs → inspect and prepare ──→ validation summary
     ↓
dry-run / emit-payload / write EXE
     ↓
finish report + log
```

The log opens **before** validation, but the destination is validated before the previous
log is truncated.

### 3.3 Provenance side-channel (D6)

```rust
struct Merged {
    argv: Vec<String>,            // exactly what clap parses today
    origins: Vec<Origin>,         // one per argv TOKEN — flag names and values alike
    overridden: Vec<Overridden>,  // values that lost, kept for the detailed log
}

enum Origin {
    CommandLine,
    OptionsFile { path: PathBuf, line: usize },
    Default,
    Inferred,
}
```

An origin is recorded for **every** token, not only flag tokens, so whichever index clap
reports is guaranteed to land on a valid entry.

- `load()` returns `Vec<Token { text: String, line: usize }>` instead of `Vec<String>`.
  Contained change: the line number is already in hand at `options_file.rs:294`.
- `merge()` attaches an origin to each token it emits. When `strip_overridden` drops a
  flag it **reports** the drop — flag name, value and origin — instead of discarding it.
- After the parse:
  - `matches.value_source(id)` → `DefaultValue` ⇒ `[default]`
  - `ValueSource::CommandLine` ⇒ `matches.index_of(id)` gives the argv index ⇒
    `origins[index]` ⇒ `[command line]` or `[snug.options:12]`
  - repeatable arguments ⇒ `matches.indices_of(id)` ⇒ one index per occurrence ⇒
    **per-value provenance**
  - derived values (output path, company falling back to the app name) are not in argv at
    all, so they are `Inferred` by construction
- `--build-log` must be registered in `FlagSpec::from_cli()` (`options_file.rs:514`) as a
  single-valued flag, and must **not** be listed in `META_FLAGS`
  (`options_file.rs:449`, `:464-511`), because meta-flags are stripped before parsing
  (`options_file.rs:543`).

**Why this and not per-layer typed parsing.** Clap's semantics stay untouched: conflicts
(`--input` versus positional, `--dry-run` versus `--emit-payload`), value parsers
(`validate_app_version`, `validate_cache_dir`), `allow_hyphen_values` on `--jvm-arg`,
`default_missing_value` on `--init-options` and `ArgAction::Version` all behave exactly
as they do today. Per-layer parsing would put each layer's flags in separate argvs, so a
conflict spanning two layers — `--emit-payload` on the command line, `--dry-run` in an
options file — would stop being detected unless a hand-written second copy of the
precedence rules were maintained alongside the `#[derive(Parser)]` definition. Two
sources of truth for "what is allowed" is where bugs live; the positional-versus-`--input`
rule at `options_file.rs:529-531` is already a hand-written special case.

**Refinement.** Each options file is additionally parsed in isolation, purely to produce
errors that name the file and line. The merged argv remains the source of truth for the
final parse.

**Guard.** A unit test asserts that `index_of` / `indices_of` point at the expected token
for a known argv. This is the one place a clap upgrade could silently misattribute a
value.

### 3.4 Reporter

A build-scoped reporter. A diagnostic carries:

```text
code, severity, stage, message, optional location, optional hint
```

- Severity: error / warning / info.
- **Errors prevent executable output.** Warnings permit the build.
- Independent errors are collected rather than short-circuiting.
- Dependent checks are skipped when their prerequisite fails, so one root cause does not
  produce a cascade of misleading errors.
- Codes stay stable when wording changes; severity is a separate axis.

### 3.5 Log destination (D2, D3, D4)

`--build-log` is located by a **pre-scan of the merged argv**, mirroring the existing
`find_options_flag` pattern at `options_file.rs:239-258`. This ordering is what lets the
log open before the full clap parse, so parse errors land in the log.

1. Scan the merged argv for `--build-log` / `--build-log=`.
2. Resolve the destination:
   - existing directory → `<dir>/snug.build.log`
   - existing file → that file
   - nonexistent path → that path, used as a file
   - no `--build-log` → `<cwd>/snug.build.log`
   - a path whose **parent directory does not exist** → `SNUG-LOG-002`. Snug does not create directories, so the open fails rather than making one.
3. Resolve the final path against the working directory.
4. Validate the destination, then open it. **Validate before truncating** — a log that
   cannot be written must not destroy the previous one.
5. If the destination cannot be opened: attempt a temporary fallback log, then **stop
   before the executable is created** (`SNUG-LOG-002`). If the build had already failed,
   the original error is preserved and the logging failure is appended.
6. Always report the actual log location.

If the options file itself fails to parse, the merged argv never exists, so the default
destination is used for the parse error. This is a deliberate, documented limitation.

### 3.6 Path protection

`guard_output_collision` (`build.rs:533-562`) currently compares the output against five
path-valued inputs — `jar`, `input`, `icon`, `splash`, `manifest` — and canonicalises
both sides with a fallback to the raw path.

It must additionally cover:

- the **log path**, in both directions: output must not alias the log, and the log must
  not alias the output
- the **options file** (`--options`, and the discovered `snug.options` /
  `snug.<os>.options`)
- the **localisation files** (`--localisation`, repeatable)
- the **JARs inside a directory input** — a real gap today: a directory input's JARs are
  never path-valued CLI arguments, so `snug -o build/libs/app.jar mylibs/` is not caught

---

## 4. Effective options display (D5)

Rendered to the console and written to the log:

```text
Effective build options:
  input:       app.jar                  [command line]
  output:      app.exe                  [inferred from input]
  name:        My Application           [snug.options:4]
  company:     My Application           [default: application name]
  version:     0.0.0                    [default]
  main-class:  com.example.Main         [JAR manifest: app.jar]
  min-java:    25                       [default]
  icon:        assets/app.ico           [snug.windows.options:8]
  splash:      (not set)                [not set]
  cache-dir:   %LOCALAPPDATA%\snug\My Company\My Application  [default]
  jvm-arg:     -Xmx2g                   [command line]
  jvm-arg:     -Dapi.token=***          [snug.options:9]  (masked)
```

A default that is a *path* is shown as a **template**, not as a resolved
path: the launcher resolves the per-user cache directory at runtime, on
the machine the app runs on, and that machine's `%LOCALAPPDATA%` is not
the builder's. The table names the platform's convention and the app's
namespace — the parts of the answer that are the same everywhere — and
the launcher fills in the base when the app first runs. The namespace
rules themselves live in `snug-format`, so the two cannot drift apart.

Label vocabulary:

| Label | Meaning |
|---|---|
| `[command line]` | Supplied on the command line. |
| `[<file>:<line>]` | Supplied in an options file, at that line. |
| `[default]` | A built-in default. |
| `[default: <rule>]` | Derived by a named built-in rule — e.g. `[default: application name]`. |
| `[inferred from <source>]` | Derived from another value — e.g. the output path from the input. |
| `[JAR manifest: <jar>]` | Read from a JAR's manifest. |
| `[not set]` | An optional setting that was not supplied. |

The **detailed log** additionally records overridden values with the origin that beat
them, e.g. `name: "Old Name" [snug.options:4] — overridden by [command line]`.

### 4.1 Redaction (D11)

Only credential-shaped `--jvm-arg` values are masked. The key name decides:

- `-D<key>=<value>` where `<key>` contains `token`, `secret`, `password`, `passwd`,
  `pwd`, `key`, `credential`, `auth`, `salt` or `signature` → the value is masked.
- `-javaagent:<path>=<value>` with the same key test → masked.
- A value that itself contains `token=`, `secret=` or `password=` → masked.

Everything else — `-Xmx2g`, `-Dfile.encoding=UTF-8` — stays visible, because a blanket
mask would make the log useless for its actual purpose.

The **count** of `--jvm-arg` values is always shown, masked or not, so a redacted line is
never mistaken for an absent one.

Environment variables are never dumped. The log records only `JAVA_HOME`, `JDK_HOME` and
whether `PATH` is set — not its value.

---

## 5. Validation policy

Errors, warnings and informational messages. Errors prevent executable output; warnings
permit the build. Independent errors are collected; dependent checks are skipped when
their prerequisite fails.

Out of scope, by decision: the application is not executed, Java is not required on the
builder machine, dependencies are not downloaded, and no complete static dependency
analysis is attempted.

### 5.1 Checks

| Check | Outcome | Notes |
|---|---|---|
| Input missing / unreadable / not a JAR | Error | `resolve_input_path` (`build.rs:367`) |
| Directory entry could not be read | Warning | Reported as `SNUG-INPUT-002`; the entry is skipped, and the build says so (`build.rs:444-462`) |
| `.JAR` files skipped by the case-sensitive filter | Not reported | The extension test at `build.rs:438` excludes uppercase `.JAR`; the file is skipped silently |
| Supplied JAR cannot be opened as a ZIP | Error | **Including with a `--main-class` override**, which today skips the manifest read at `build.rs:116-125` (`build.rs:117`) |
| Empty JAR | Error | Already enforced at `build.rs:492` |
| Manifest attributes unusable | Error | `SNUG-MANIFEST-001` |
| No usable selected entry point | Error | Suggests `--find-main` or `--main-class` |
| Selected class absent from the supplied classpath | Error | Multi-release aware |
| Entry-point method cannot be verified | **Warning** | Preserves the JavaFX `Application` fallback and inherited mains; the ancestor chain is walked (`build.rs:671`) |
| Conflicting main declarations | Warning | First-JAR selection preserved |
| Duplicate entry class across JARs | Warning | Names the winning JAR |
| Manifest `Class-Path` reference | Warning | Advisory; no automatic resolving or bundling |
| Nested JAR | Info | No recursive dependency resolution; not a rejection |
| Invalid icon / splash / Windows manifest | Error | Via shared preparation |
| Oversized splash | Warning | |
| Class file requires a newer Java than `--min-java` | Error for the entry class, Warning otherwise | §5.2 |
| Preview bytecode | Warning | Reported separately; `--enable-preview` is never injected |

### 5.2 Class-version checking (D7)

`classfile.rs:193-194` already reads major and minor and discards them. They are retained.

- Java release = `major - 44`. Major 69 is Java 25.
- The **selected entry class** is the class named by `--main-class`, or the manifest's
  `Main-Class` from the first JAR by sorted path.
- Its effective major version is resolved **multi-release aware**: if the JAR declares
  `Multi-Release: true`, the effective class is the highest
  `META-INF/versions/<n>/` overlay with `n ≤ min-java`; otherwise the root copy. A
  higher-version overlay alone must never raise the minimum.

  Note that `find_main_classes` skips `META-INF/versions/` wholesale
  (`classfile.rs:120-122`). That is correct for main-method detection and wrong for a
  version audit — the two scans need different overlay rules.
- Error when the effective major exceeds `min_java + 44`.
- Warning for any other class in the supplied JARs that exceeds it, because optional code
  may never load.
- Preview bytecode is reported separately: minor version `0xFFFF`.
- `--min-java` is never silently raised and `--enable-preview` is never injected.
- Bytecode compatibility does not prove Java API or runtime compatibility, and the log
  says so.

**Coverage and cost.** The version lives in the first eight bytes of a class file, so a
sweep of every class in every supplied JAR is cheap and complete. The 64 KB
`MAX_CLASS_PREFIX` cap (`classfile.rs:135-144`) constrains only **method-table** parsing,
which is a separate check with a separate coverage limit. The two are reported
separately, and the log states exactly what was and was not verified.

### 5.3 Manifest parsing

`manifest.rs:114-140` currently splits on the first `:`, compares `key.trim() !=
"Main-Class"` exactly (case-sensitive), and scans the whole file — so a `Main-Class:`
inside a *named section* matches. The fix:

- honour the main section boundary (stop at the first blank line)
- match the header case-insensitively
- keep the existing continuation handling (`manifest.rs:127-132`)

### 5.4 What the checks explicitly do not do

The log states this, because a reader otherwise assumes more than was done:

- Entry reads are prefix reads. Dropping the reader early skips the CRC check — which is
  deliberate (`classfile.rs:136-140`) — so there is **no full archive-entry integrity
  audit**. Only the manifest entry's own CRC is verified, by reading it in full.
- The Windows manifest check is XML **well-formedness** only
  (`resources.rs:54-92`), not schema or activation validation.
- The manifest survey currently clones whole JARs (`build.rs:345-349`,
  `file.bytes.clone()`). That clone is removed.
- Skipped or incomplete checks are reported explicitly rather than passed over in silence.

---

## 6. Console and log output (D8)

All console output moves to **stderr**, including the dry-run summary, which currently
uses `println!` at `main.rs:196-263`. stdout is reserved for `--emit-payload` binary
output.

`snug: built <path>` stays on stderr (`main.rs:185` for Windows, `main.rs:163` for
macOS) because Quick Build parses it (`dropper/src/build.rs:133-138`).

### Paths are relative to the working directory

The log's **first line** states the working directory:

```text
snug: working directory C:\Projects\Demo
```

Every path after it is written **relative to that directory**, so the absolute prefix
appears once instead of on every line:

```text
snug: working directory C:\Projects\Demo
snug: loaded options from snug.options
  input:   Demo.jar  [command line]
  Output:   Demo.exe
  Log:      snug.build.log
```

A path **outside** the working directory stays absolute: there is no relative form of
it that is not a guess about where the reader is standing.

The relativising happens on the text of a finished line
(`crates/snug-cli/src/paths.rs`), not on `Path` values, because most of what a build
prints is assembled from several sources — a diagnostic's message, a location, a hint
— and a path reaches the log relative whether it was formatted by snug or by a library
snug called.

`snug: built <path>` is relative for the same reason, so Quick Build resolves it
against the child's working directory before naming it in its dialog.

Completion block, success:

```text
Build succeeded.
  Errors:   0
  Warnings: 2
  Output:   Demo.exe
  Log:      snug.build.log
```

Completion block, failure:

```text
Build failed during resource validation.
  Errors:   1
  Warnings: 2
  Output:   Demo.exe (not written)
  Log:      snug.build.log
```

The dry-run wording changes from `snug: dry-run — no files written`
(`main.rs:196`), which becomes false once a log file is written, to something that
distinguishes "no application output" from "log written".

**Dry run is not side-effect-free today.** It still reads and SHA-256s every JAR and
decodes the splash, because the whole of `build_payload` runs before the dry-run branch
(`main.rs:138` versus `:150-152`). The log records what dry run actually did.

**`--emit-payload` and `--dry-run` become mutually exclusive.** Today `--emit-payload`
returns first and silently pre-empts a dry run (`main.rs:141-152`).

**Parse errors keep clap's exit code.** `try_parse_from` replaces `parse_from`
(`main.rs:82`); on `Err`, the error is logged and then `clap::Error::exit()` is called,
which preserves clap's exact rendering and its exit code 2. Help and version remain
normal informational exits with code 0.

---

## 7. Quick Build integration (D9, D10)

The dropper runs the child with `cwd = input.parent()`
(`dropper/src/build.rs:95-102`), so the CLI's *default* log would land beside the
dropped JAR while the dropper's transcript lands in the dropper's own folder — two logs,
two places. D9 removes that split:

- The dropper passes `--build-log <dropper folder>/snug.build.log` explicitly.
- The authoritative report is the CLI's `snug.build.log` in the dropper's folder.
- The dropper's existing `snug-build.log` transcript is **kept** (D10), retaining the raw
  child capture.

Both files live in the dropper's folder under different names (`snug.build.log` and
`snug-build.log`). The dialog names the CLI log as the place to look for diagnostics and
falls back to naming the transcript if the CLI log is absent — which is the case when the
child never started.

The child's working directory does not change, so the existing behaviour of picking up a
project-local `snug.options` beside the JAR is preserved.

---

## 8. Diagnostic codes

Stable across wording changes. Severity is a separate axis.

| Code | Meaning |
|---|---|
| `SNUG-CFG-001` | Arguments or options cannot be parsed. |
| `SNUG-CFG-002` | Invalid effective configuration. |
| `SNUG-INPUT-001` | Input unavailable or unusable. |
| `SNUG-INPUT-002` | Input directory entry could not be read. |
| `SNUG-JAR-001` | Supplied archive is not a readable ZIP. |
| `SNUG-MANIFEST-001` | Unusable manifest attributes. |
| `SNUG-MAIN-001` | No selected entry point. |
| `SNUG-MAIN-002` | Selected class absent from the supplied classpath. |
| `SNUG-MAIN-003` | Entry-point method could not be verified. |
| `SNUG-MAIN-004` | Conflicting main declarations. |
| `SNUG-MAIN-005` | Duplicate entry class across supplied JARs. |
| `SNUG-DEP-001` | External manifest `Class-Path` reference. |
| `SNUG-DEP-002` | Nested JAR not resolved. |
| `SNUG-JAVA-001` | Bytecode exceeds `--min-java`. |
| `SNUG-JAVA-002` | Preview bytecode. |
| `SNUG-RESOURCE-001` | Invalid supplied resource. |
| `SNUG-RESOURCE-002` | Oversized splash. |
| `SNUG-LOCALISATION-001` | Localisation bundle incomplete, or drifted from the baseline. |
| `SNUG-OUTPUT-001` | Output or log path collision. |
| `SNUG-OUTPUT-002` | Output write or install failure. |
| `SNUG-LOG-001` | Log write failure. |
| `SNUG-LOG-002` | Log destination unavailable. |
| `SNUG-INTERNAL-001` | Unexpected internal failure. |

Each documented code states its cause, its remedy and its validation limits.

---

## 9. Implementation phases

**Phase 1 — Reporter and logging.** Reporter, diagnostic codes, always-on logging,
parse-error capture, the provenance side-channel, redaction, comprehensive path
protection.

**Phase 2 — Shared preflight.** Every JAR validated as an archive, entry-point selection
and presence, manifest parsing fixes, shared resource preparation, dry-run parity.

**Phase 3 — Class versions.** Version and preview checks with multi-release handling,
dependency advice, duplicate-class warnings.

**Phase 4 — Quick Build and documentation.** Dropper integration, diagnostic code
reference.

---

## 10. Test plan

Provenance and precedence, repeatable options with per-value origins, defaults versus
inferred versus unset, early parse failures, log overwrite behaviour, directory log
destinations, collisions, unwritable log paths, sensitive-value masking, dry-run parity,
binary stdout under `--emit-payload`, JavaFX and inherited main support, multi-release
JARs, and preservation of an existing EXE after a failed build.

Two additions from this review:

- A test asserting that `index_of` / `indices_of` map to the expected token, which guards
  the provenance side-channel against a clap upgrade.
- A test asserting that the effective-options rendering distinguishes an explicitly
  supplied `--min-java 25` from the default, which is the case clap's `value_source` is
  there to solve.

---

## 11. Corrections to the original source review

Verified against current source; each of these changes the implementation.

1. **"No main class" is currently silent, not an error.** `build.rs:47-56` yields
   `main_class: None` and the build proceeds. `SNUG-MAIN-001` is a genuine behaviour
   change, not a report of an existing error.
2. **Two stale artefacts follow from that.** The dropper fixture at
   `dropper/src/build.rs:431` references a "no Main-Class" message that no code emits,
   and the dry-run summary prints `"(from manifest)"` when the main class is `None`
   (`main.rs:206-212`).
3. **The manifest parser is weaker than "tolerates invalid UTF-8" suggests.**
   `manifest.rs:114-140` matches `Main-Class` case-sensitively and scans the whole file,
   so a `Main-Class:` inside a named section matches.
4. **The class-version split is cleaner than assumed.** The version lives in the first
   eight bytes, so a full sweep is cheap; the 64 KB cap limits only method parsing.
5. **The collision guard has a gap beyond the log.** `build.rs:533-562` covers five
   path-valued inputs and misses the options file, the localisation files, the log, and
   the JARs inside a directory input.
6. **The dropper's own doc comment is stale.**
   `dropper/src/build.rs:56-58` claims "`snug.options` beside `snug.exe` outranks the
   CWD copy", but `options_file.rs:192-202` confirms the executable's directory is never
   searched — and commit `19d7625` corrected the CLI help text to match the code.
7. **Minor.** `main.rs:261` reads `"  total exE:"`.

---

## 12. Still open

Not blocking, and not asked.

- **`--quiet` / `-q`.** Already on the AGENTS.md backlog as a CLI-surface item. D8 makes
  it more relevant: with everything on stderr, a quiet mode needs to suppress the
  effective-options block and the completion block too, not just the success line.
- **The "validation strictness" backlog item** in AGENTS.md appears partly stale.
  `--version` already has a value parser (`resources::validate_app_version`) and the
  icon is already decoded through editpe. This design makes the *reporting* of those
  failures uniform rather than adding new strictness.
- **Whether the effective-options block should be suppressible at all.** The requirement
  is that it always shows, so it is always shown.
---

## 13. What the implementation changed

Five places where building it showed the design was wrong, or incomplete.

### `--find-main` and `--list-tokens` keep stdout

D8 says everything goes to stderr. The two diagnostic modes are the
exception: `--find-main` documents that its output pipes to a file
(`snug App.jar --find-main > entry-points.txt`), and moving it to
stderr would break that documented use for no gain. The rule applies to
*build* output, which is what stdout has to stay clean for.

### The dry-run summary no longer repeats the options

The summary used to print `name`, `output`, `main-class`, `min-java`,
`icon`, `manifest` and `splash`. The effective-options table prints all
of them, with their origins. Printing both meant two places to change
and two places to look - and a test asserting on "min-java: 21" could
not tell which of the two it was reading.

The summary now carries only what the table cannot say: how many JARs
were embedded and the first one's hash, which localisation bundles were
embedded, which resource path is about to run, and the stub size.

### The class-version scan covers every class

The design said the version sweep and the method-table scan had
different coverage limits, and that both had to be reported
separately. That is right, but the framing understated the first one.
The version lives in the first eight bytes of a class file, so reading
it costs eight bytes per class and covers *every* class in the archive
with no window at all. The check is therefore stated as complete, and
only the method-table check carries a caveat.

### The log destination is protected before the log is opened

The log is truncated when it opens, so a `--build-log` that names one
of the build's inputs destroys that input before any guard can run. The
check therefore happens in `open_build_log`, against the paths
discovered from the merged argv - which is why the log opens before the
full parse in the first place.

### "No entry point" is a behaviour change

`SNUG-MAIN-001` - no JAR declares a `Main-Class` and `--main-class` was
not given - is an **error**. Snug used to build an executable with no
entry point and let it fail on the user's machine, which is the worst
place to find out. This is the one check in the set that can stop a
build that used to succeed.