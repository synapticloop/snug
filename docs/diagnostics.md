# Snug build diagnostics

Every diagnostic snug raises carries a **stable code**. The code is what
you quote in a bug report and what this page points at, so it does not
change when the wording does. Severity is a separate axis — the same
check can be an error in one build and a warning in another as the checks
tighten.

Every build writes a log: `snug.build.log` in the directory snug was
invoked from, or wherever `--build-log` points. The log holds the
effective options with their origins, every diagnostic raised along the
way, and where the output went.

## Severity

| Severity | Effect |
|---|---|
| `error` | The build does not produce an executable. |
| `warning` | The build proceeds. |
| `info` | Worth knowing; no effect on the build. |

## The codes

### Configuration

#### `SNUG-CFG-001` — Arguments or options could not be parsed

**Cause.** Clap rejected the command line or an options file. The
message is clap's own, unchanged.

**Remedy.** Fix the argument the message names. The build log records
the failure, so a parse error is not lost to a console that has already
scrolled past.

**Covers.** Only the syntax. A file that parses but says something
nonsensical is a different code.

#### `SNUG-CFG-002` — The effective configuration is invalid

**Cause.** The options were syntactically fine but do not describe a
build snug can perform — no input at all, or an output that names a
macOS `.app` on a platform whose embedded launcher is a Windows PE.

**Remedy.** The message names the option to change.

---

### Input

#### `SNUG-INPUT-001` — The input is missing or unusable

**Cause.** The JAR or directory named by `--input` (or the positional
argument) does not exist, cannot be read, or is not a JAR.

**Remedy.** Check the path. The message shows the path snug tried.

#### `SNUG-INPUT-002` — A directory entry could not be read

**Cause.** A directory listed as the input contained an entry that could
not be read, so it was left out of the build.

**Why it is a warning and not an error.** The build carries on with the
JARs it *could* read. This used to be a silent skip — a failed read was
discarded and the build carried on with fewer JARs than the directory
held, with nothing on screen to say so.

**Remedy.** Check the directory's permissions if a JAR you expected is
missing from the build.

---

### Archives

#### `SNUG-JAR-001` — The supplied archive is not a readable ZIP

**Cause.** A file that was handed to snug as a JAR could not be opened
as a ZIP archive.

**Remedy.** Re-export the JAR, or remove it from the input.

**Covers.** Every supplied archive is opened, *including* when
`--main-class` overrides the manifest. An override must not turn an
archive that cannot be read into one that is silently embedded: the
build would succeed and the application would fail at runtime.

#### `SNUG-MANIFEST-001` — The manifest could not be read

**Cause.** A JAR's `META-INF/MANIFEST.MF` could not be read: the entry is
there, but reading it failed — a corrupted archive, most often.

**Remedy.** Re-export the JAR, or pass `--main-class <CLASS>` to name the
entry point directly. The manifest is only read when snug has to find the
entry point itself.

**Covers.** A JAR that is not a readable ZIP is reported as `SNUG-JAR-001`
instead, and the manifest check is skipped rather than reported twice.
This is the user's archive, not a snug bug, so it never arrives as
`SNUG-INTERNAL-001`.

---

### Entry point

#### `SNUG-MAIN-001` — No usable entry point was selected

**Cause.** No JAR declares a `Main-Class` and `--main-class` was not
given. The launcher would have nothing to call — neither a `main` method
nor the JavaFX `Application.launch` fallback can be reached without a
class name.

**Remedy.** Pass `--main-class <CLASS>`, or run `snug --find-main` to
list the classes that declare one.

**This is a behaviour change.** Snug used to build an executable with no
entry point and let it fail on the user's machine.

#### `SNUG-MAIN-002` — The selected class is not in any supplied JAR

**Cause.** `--main-class` names a class that no supplied JAR contains.

**Remedy.** Check the spelling, and check that the JAR containing it was
included in the input.

**Covers.** The classpath that is being embedded — the JARs snug was
given. A class that arrives some other way (a `Class-Path` manifest
reference, a nested JAR) is not on that classpath and is reported as
absent.

#### `SNUG-MAIN-003` — The entry-point method could not be verified

**Cause.** Snug could not confirm that the selected class — or one of its
ancestors — declares `public static void main(String[])`. The class file
may not have been readable, or the chain may have left the supplied JARs.

**Why it is a warning.** The Windows launcher supports the JavaFX
`Application.launch` fallback, and a main method can be inherited. A
blanket requirement for a *declared* method would reject working
applications.

**Covers.** The ancestors are walked, so an inherited `main` is confirmed
rather than reported. A class that extends `javafx.application.Application`
is left alone too: the launcher starts it through `Application.launch`
rather than through `main`.

#### `SNUG-MAIN-004` — Several JARs declare different entry points

**Cause.** More than one supplied JAR declares a `Main-Class`, and they
disagree. Snug reads the first JAR by filename; that is an ordering
accident, not a decision anybody made.

**Remedy.** Pass `--main-class <CLASS>` to choose explicitly.

#### `SNUG-MAIN-005` — The same entry class appears in more than one JAR

**Cause.** Two JARs contain the same class. The first JAR by filename
wins.

**Remedy.** Remove the duplicate, or pass `--main-class` if the other
copy is the one you want.

---

### Dependencies

#### `SNUG-DEP-001` — A manifest names an external classpath entry

**Cause.** A JAR's manifest declares a `Class-Path` referring to
something outside the archive.

**Remedy.** Snug does not resolve, download or bundle manifest
`Class-Path` entries — that is what a dependency resolver is for. Build
the application as a fat JAR instead.

#### `SNUG-DEP-002` — A nested JAR was found and not resolved

**Cause.** The archive contains a JAR inside a JAR.

**Remedy.** Snug does not resolve nested JARs. Unpack them first, or use
a fat JAR that has them flattened.

**Covers.** One note per supplied JAR that holds them, so a Spring Boot
fat JAR does not produce a line per dependency.

---

### Bytecode

#### `SNUG-JAVA-001` — Bytecode needs a newer Java than `--min-java`

**Cause.** A class in the supplied JARs was compiled for a newer Java
release than `--min-java` allows. Class-file major 69 is Java 25; the
release is `major - 44`.

**Severity.** An **error** for the selected entry class — the build
would produce an executable that cannot start. A **warning** for every
other class: optional code may never load, and a fat JAR routinely
carries classes for newer releases than the one it runs on.

**Remedy.** Build with `--min-java` set to the release the entry class
needs, or supply a JAR compiled for the older release.

**Multi-release.** An archive that declares itself multi-release serves
the highest overlay at or below the configured release, falling back to
the root copy. An overlay for a *newer* Java never raises the minimum.

**Covers.** Every class in every supplied JAR. The version lives in the
first eight bytes of a class file, so the check reads eight bytes per
class and covers the whole archive — with no scan-window caveat. This is
a different guarantee from `SNUG-MAIN-003`, whose method-table check is
bounded by a 64 KB prefix read; the two are reported separately
because they are not equally strong.

**Does not cover.** Bytecode compatibility is not API compatibility. A
class compiled for Java 25 can still call a method that does not exist
in the Java 17 runtime you point `--min-java` at. Snug checks the class
*version*, not the API surface.

#### `SNUG-JAVA-002` — Preview bytecode

**Cause.** A class was compiled with preview features enabled, which
marks it with minor version `0xFFFF`.

**Remedy.** Pass `--jvm-arg=--enable-preview` to run preview classes.
Snug does not add it for you — a preview class is not runnable without
it, and silently enabling preview mode would change the behaviour of
every other class in the application too.

---

### Resources

#### `SNUG-RESOURCE-001` — A supplied resource is invalid

**Cause.** An icon, splash image or Windows manifest could not be
prepared: the image would not decode, or the manifest is not
well-formed XML.

**Remedy.** The message names the file.

**Covers.** The Windows manifest check is XML **well-formedness** only.
It is not a schema check, and it does not validate the manifest against
what the launcher actually activates.

#### `SNUG-RESOURCE-002` — The splash image is larger than recommended

**Cause.** The splash PNG is larger than the launcher's preferred size,
so it will be scaled at runtime.

**Remedy.** Shrink the image. The build proceeds.

---

### Localisation

#### `SNUG-LOCALISATION-001` — A localisation bundle is incomplete, or has drifted

**Cause.** A supplied translation bundle does not cover every key in the
built-in English baseline, or it uses different placeholders than the
baseline does.

**Why it is a warning and not an error.** A missing key falls back to the
built-in English text, so the launcher still runs and the build is not
worth refusing. A drifted placeholder is reported rather than repaired.

**Remedy.** Copy the missing keys across from the baseline, and run
`snug --list-tokens` for the placeholder contract.

---

### Output and log

#### `SNUG-OUTPUT-001` — The output and an input are the same file

**Cause.** The output path names one of the build's inputs: a JAR, the
options file, a localisation file, the build log, or a JAR inside a
directory input.

**Remedy.** Pass a different output path. Snug reads every input into
memory before writing anything, so proceeding would destroy the input
with no backup.

#### `SNUG-OUTPUT-002` — The output could not be written

**Cause.** The executable could not be created — the path is not
writable, or the icon, version resource or manifest could not be
stamped into it.

**Remedy.** The message carries the underlying cause.

#### `SNUG-LOG-001` — A write to the log failed

**Cause.** The log could not be written completely, usually because the
disk filled or the file was locked.

**Remedy.** The build's own result is unaffected — snug reports this
rather than claiming a complete log was saved.

#### `SNUG-LOG-002` — The log destination could not be opened

**Cause.** The requested `--build-log` path could not be opened for
writing, or it names one of the build's own input files. A path whose
parent directory does not exist is the common case: snug does not create
directories, so the open fails with the operating system's own "path not
found" rather than making the directory first.

**Remedy.** Point `--build-log` somewhere writable, and make sure the parent
directory exists. Snug stops before
creating the executable: a build whose report nobody can read is not a
build worth shipping. The reason is written to a temporary log when one
can be made.

---

### Internal

#### `SNUG-INTERNAL-001` — Something unexpected went wrong

**Cause.** A failure snug did not anticipate. The message carries the
underlying error.

**Remedy.** This is a bug. Please report it with the build log.

---

## The build log

The log is a transcript of the run: the options that were loaded, the
effective options with their origins, every diagnostic, and the closing
summary.

The effective-options table is the part worth reading. Each value is
shown with where it came from:

```text
Effective build options:
  input:       assets/snug-javafx-demo-windows.jar  [snug.windows.options:74]
  output:      assets/snug-javafx-demo-windows.exe  [snug.windows.options:75]
  name:        Snug JavaFX Demo                     [snug.options:75]
  min-java:    25                                   [snug.options:80]
  jvm-arg:     --enable-native-access=ALL-UNNAMED   [snug.options:112]
  allow-multiple-instances: false                   [default]
```

| Origin | Meaning |
|---|---|
| `[command line]` | Typed on the command line. |
| `[<file>:<line>]` | Read from an options file, at that line. |
| `[default]` | A built-in default. |
| `[default: <rule>]` | Derived by a named built-in rule. |
| `[inferred from <source>]` | Derived from another value. |
| `[JAR manifest: <jar>]` | Read from a JAR's manifest. |
| `[not set]` | An optional setting that was not supplied. |

A repeatable option such as `--jvm-arg` carries an origin **per value**,
because two sources can each contribute one.

### What the log does not contain

- **Environment variables.** Snug records `JAVA_HOME` and `JDK_HOME` and
  whether `PATH` is set. It does not dump the environment.
- **Unredacted credentials.** A `--jvm-arg` whose property name or value
  reads as a credential — `token`, `secret`, `password`, `key`,
  `credential` — is masked as `***`, and the row is marked `(masked)`.
  The key is kept, because knowing *which* property was masked is the
  point. Harmless arguments such as `-Xmx2g` are shown in full.
