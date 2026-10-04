#!/usr/bin/env bash
#
# build-macos-demo.sh — build the macOS `snug` release, then package the
# JavaFX demo as a real `.app`.
#
# This is the macOS counterpart of step 4 + the staging block in
# scripts\build-release.cmd: that one builds assets\snug-javafx-demo.exe,
# this one builds assets/snug-javafx-demo.app. Same input JAR, same options
# files, same "build into assets/ then stage" shape — so the two platforms
# cannot drift apart on how a demo is produced.
#
# The output path is *not* passed on the command line. It lives in
# snug.macos.options (`--output assets/snug-javafx-demo.app`), which snug
# layers over the shared snug.options on a macOS host. That is what replaces
# the old `-o` workaround: previously this script had to override the `.exe`
# in snug.options on every invocation, because there was no way to say
# "different here, same everywhere else". See `effective_flag` below for why
# the script reads the value back instead of hardcoding it a third time.
#
# Usage:
#   scripts/build-macos-demo.sh [--clean] [--skip-cli]
#
#   --clean     wipe release/macos-* first, and pass --clean to
#               build-macos.sh
#   --skip-cli  assume the `snug` binaries are already built and only
#               package the demo. Useful for iterating on the demo alone.
#
# Environment:
#   MACOSX_DEPLOYMENT_TARGET   override the floor (default 12.0 Monterey)
#
# The demo is built by the `snug` that build-macos.sh *just* produced, for
# the host architecture. That is deliberate: it means the demo can only
# ever be a .app for the architecture it is shipped alongside, and the
# arm64 and x86_64 demos can never be confused for one another.

set -euo pipefail

readonly DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-12.0}"
readonly OPTIONS_FILE="snug.options"
readonly OS_OPTIONS_FILE="snug.macos.options"

CLEAN=0
SKIP_CLI=0
for arg in "$@"; do
    case "$arg" in
        --clean) CLEAN=1 ;;
        --skip-cli) SKIP_CLI=1 ;;
        # Print the header comment block, lines 2..34. Stop *before* the
        # blank line and `set -euo` rather than at a fixed total, so
        # editing the header above cannot silently truncate or overrun
        # the help text.
        -h|--help) sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "build-macos-demo: unknown argument '$arg' (try --help)" >&2; exit 2 ;;
    esac
done

readonly ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly RELEASE_DIR="$ROOT/release"
readonly DEMO_SUBDIR="macos-$(uname -m)"
readonly PLATFORM_DIR="$RELEASE_DIR/$DEMO_SUBDIR"
readonly STAGED_CLI="$PLATFORM_DIR/snug"
readonly LSREGISTER="/System/Library/Frameworks/CoreServices.framework/\
Frameworks/LaunchServices.framework/Support/lsregister"

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "build-macos-demo: must run on macOS (found $(uname -s))." >&2
    echo "                  Windows artefacts come from scripts\\build-release.cmd." >&2
    exit 1
fi

for tool in cargo plutil codesign lipo otool unzip; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "build-macos-demo: '$tool' not found on PATH." >&2
        exit 1
    fi
done

cd "$ROOT"

# ---------------------------------------------------------------- CLI

if [[ "$SKIP_CLI" == "1" ]]; then
    echo "==> --skip-cli: using the existing $STAGED_CLI"
    if [[ ! -x "$STAGED_CLI" ]]; then
        echo "build-macos-demo: $STAGED_CLI is missing; run scripts/build-macos.sh first." >&2
        exit 1
    fi
else
    # Deliberately not an array: macOS still ships bash 3.2, where
    # expanding a *possibly empty* array under `set -u` is an unbound
    # variable error. A plain if/else sidesteps the whole class of bug.
    if [[ "$CLEAN" == "1" ]]; then
        "$ROOT/scripts/build-macos.sh" --clean
    else
        "$ROOT/scripts/build-macos.sh"
    fi
    echo
fi

if [[ ! -x "$STAGED_CLI" ]]; then
    echo "build-macos-demo: expected a snug binary at $STAGED_CLI but it is not there." >&2
    exit 1
fi

# -------------------------------------------------------------- demo

echo "==> demo input"

# Per-platform demo JAR, falling back to the shared one.
#
# snug's payload carries one JAR, and JavaFX has to ship its *native*
# libraries, which are platform-specific binaries with platform-specific
# names (`libglass.dylib` / `glass.dll` / `libglass.so`). So one JAR cannot
# serve every platform: the Windows build keeps `snug-javafx-demo.jar` (see
# scripts\build-release.cmd) and this one takes `snug-javafx-demo-mac.jar`.
# The names differ by extension, so a JAR that does carry all three is not
# broken - it is just not what the macOS demo is built from.
DEMO_JAR_MAC="assets/snug-javafx-demo-mac.jar"
DEMO_JAR_SHARED="assets/snug-javafx-demo.jar"
if [[ -f "$DEMO_JAR_MAC" ]]; then
    DEMO_JAR="$DEMO_JAR_MAC"
elif [[ -f "$DEMO_JAR_SHARED" ]]; then
    DEMO_JAR="$DEMO_JAR_SHARED"
    echo "    note: $DEMO_JAR_MAC not found; falling back to $DEMO_JAR_SHARED."
    echo "          That one carries the Windows natives, so the demo will not"
    echo "          open a window on macOS."
else
    cat >&2 <<EOF
build-macos-demo: no demo JAR found. Looked for:
                  $DEMO_JAR_MAC  (carries the macOS natives)
                  $DEMO_JAR_SHARED
                  Build one first, or skip the demo with --skip-cli.
EOF
    exit 1
fi
echo "    jar: $DEMO_JAR"

# Fail *here* rather than at the user's desk. Missing natives are invisible
# to every other check in this script: the bundle is produced, it validates,
# it lints, it signs, and the payload is present. The demo just refuses to
# open a window later, on someone else's machine, with
# `Error initializing QuantumRenderer: no suitable pipeline found` - which
# reads like a GPU fault and sends people looking at drivers instead of at
# the packaging. This is the last place the mistake is still visible.
#
# `NativeLibLoader` looks each library up as a classpath *resource by leaf
# name*, so what matters is that a macOS build of each library is in the
# JAR, not which directory it sits in. Match on the `.dylib` suffix.
jar_names="$(unzip -Z1 "$DEMO_JAR" 2>/dev/null || true)"
mac_natives="$(printf '%s\n' "$jar_names" | grep -cE '(^|/)lib[^/]*\.dylib$' || true)"
missing=""
for required in libglass libprism_es2 libprism_sw; do
    if ! printf '%s\n' "$jar_names" | grep -qE "(^|/)${required}\\.dylib$"; then
        missing="$missing $required.dylib"
    fi
done
if [[ -n "$missing" ]]; then
    cat >&2 <<EOF
build-macos-demo: $DEMO_JAR is missing required macOS JavaFX natives:
$missing
                  (it has $mac_natives .dylib file(s) in total)

                  JavaFX will fail at startup with "Error initializing
                  QuantumRenderer: no suitable pipeline found" - which looks
                  like a GPU fault but is a packaging one.

                  Get them from org.openjfx:javafx-graphics:<ver>:mac, the
                  same build as the JAR's classes (read it out of
                  javafx.properties / VersionInfo), and put the .dylib files
                  in the JAR. build-release.cmd needs the -win artifact's
                  .dll files instead.
EOF
    exit 1
fi
echo "    JavaFX natives for macOS: $mac_natives"

# Both options files are resolved implicitly, from the CWD this script
# `cd`ed into at startup. No `--options` is passed, and that is now
# load-bearing rather than incidental: `--options <path>` means *that file
# only*, so naming snug.options here would suppress snug.macos.options and
# put the `.exe` back. Leaving the flag off is what layers the two.
#
# Nothing ships an options file next to the staged snug binary, so the
# exe-dir lookup finds nothing and the CWD copies are the ones read. Both
# are echoed so the dependency is visible in the log rather than being an
# accident of the working directory.
#
# A snug.<os>.options is optional in general, but *this* script depends on
# one: it carries the only value that differs on macOS.
for options_file in "$OPTIONS_FILE" "$OS_OPTIONS_FILE"; do
    if [[ ! -f "$options_file" ]]; then
        echo "build-macos-demo: $options_file is missing." >&2
        if [[ "$options_file" == "$OS_OPTIONS_FILE" ]]; then
            echo "                 This script needs it. It carries the .app output" >&2
            echo "                 path, and without it snug writes the Windows .exe" >&2
            echo "                 from snug.options straight into assets/ on macOS." >&2
        fi
        exit 1
    fi
    echo "    options: $options_file"
done

# The effective value of a scalar flag, resolved the way snug resolves it:
# the OS-specific file first, then the generic one.
#
# Read back out of the files rather than hardcoded here because the point of
# the split is a single source of truth per value. This script already
# hardcoded the bundle path once (as `-o`), and the options file now has to
# hold the same string — two copies of one path is precisely the drift the
# split exists to remove.
#
# Handles `--flag value` and one layer of double quotes, which is what
# options_file::load effectively hands clap. It does not reimplement
# shell_words: a value with a backslash escape or an inline `#` is out of
# scope for a demo script, and a wrong answer here is caught by the checks
# below rather than silently producing a bad bundle.
effective_flag() {
    local name="$1"; shift
    local file value
    for file in "$@"; do
        [[ -f "$file" ]] || continue
        value="$(grep -oE "^[[:space:]]*${name}[[:space:]]+.*$" "$file" 2>/dev/null |
            head -1 |
            sed -e "s/^[[:space:]]*${name}[[:space:]]*//" -e 's/^"//' -e 's/"$//')"
        if [[ -n "$value" ]]; then
            printf '%s\n' "$value"
            return 0
        fi
    done
    return 1
}

# Where the bundle goes, per the options files. This is the value that used
# to be passed as `-o "$DEMO_APP"`.
if ! DEMO_APP="$(effective_flag --output "$OS_OPTIONS_FILE" "$OPTIONS_FILE")"; then
    echo "build-macos-demo: no --output in $OS_OPTIONS_FILE or $OPTIONS_FILE." >&2
    exit 1
fi

# Fail *here* rather than at the user's desk. The output extension is what
# selects the artefact — `-o Foo.app` builds a bundle, `-o Foo.exe` builds a
# Windows PE — so a `.exe` here means a flat PE would be written into assets/
# and every check below would then be looking at a path that does not exist.
# That is the mistake the old unconditional `-o` was silently papering over,
# and it is exactly what .gitignore warns about: "a flat file with a .app name
# is not a bundle and cannot launch".
if [[ "$DEMO_APP" != *.app ]]; then
    cat >&2 <<EOF
build-macos-demo: the effective --output is '$DEMO_APP', which does not end
                  in .app.

                  On macOS the extension is what selects the artefact: a .app
                  is a *bundle* (a directory holding a Mach-O launcher and its
                  payload), a .exe is a Windows PE. Building a .exe here would
                  write a Windows binary into assets/.

                  The value comes from $OS_OPTIONS_FILE, then $OPTIONS_FILE.
                  Put this in the former:
                      --output assets/snug-javafx-demo.app
EOF
    exit 1
fi
echo "    output: $DEMO_APP (from the options files)"

# Remove any previous bundle *before* building. With the output path coming
# from a config file rather than from `-o` on the command line, a leftover
# directory at that path satisfies the "a .app must be a directory" check
# below without this build having produced anything at all — every
# verification step would then pass while inspecting a stale artefact.
#
# That is not hypothetical: it is exactly what happened the first time this
# script ran after the options split, because the staged `snug` predated the
# feature and quietly wrote a Windows .exe while an old bundle sat in assets/.
# A build that fails must fail here, where the reason is still visible.
#
# Unregister from Launch Services first, for the same reason the staging
# block does: a stale id pointing at a deleted path makes Finder refuse the
# rebuild with error -1712. Advisory if LS never saw it.
if [[ -e "$DEMO_APP" ]]; then
    if [[ -d "$DEMO_APP" ]]; then
        "$LSREGISTER" -u "$DEMO_APP" >/dev/null 2>&1 || true
    fi
    echo "    removing the previous $DEMO_APP so the checks below cannot pass on a stale bundle"
    rm -rf "$DEMO_APP"
fi

echo
echo "==> packaging the demo as a .app"
"$STAGED_CLI" "$DEMO_JAR"
echo

# ------------------------------------------------------------ verify

echo "==> verifying $DEMO_APP"

# Derived from the options files, so the payload's name follows the bundle's
# rather than being spelled out again here.
readonly APP_STEM="$(basename "$DEMO_APP" .app)"

# The single most important check, and the one that would have caught
# assets/snug-javafx-demo.app being a flat Windows PE with a .app name:
# a bundle is a *directory*. Everything below is meaningless unless this
# holds.
#
# Since $DEMO_APP now comes from the options files rather than from a `-o`,
# this is also the backstop for a misconfigured --output: if snug wrote
# somewhere else entirely, the expected path is simply absent.
if [[ ! -d "$DEMO_APP" ]]; then
    echo "build-macos-demo: $DEMO_APP is not a directory — a .app must be a bundle." >&2
    if [[ -f "$DEMO_APP" ]]; then
        echo "                 It exists as a *file*, so snug wrote a flat binary" >&2
        echo "                 rather than a bundle. Check the --output in" >&2
        echo "                 $OS_OPTIONS_FILE and $OPTIONS_FILE." >&2
    else
        # The likeliest cause by far: the build went to the generic
        # (Windows-shaped) --output, which is what happens when the snug
        # being run does not know about the per-OS tier at all. That is
        # what a stale staged binary does, since $STAGED_CLI is whatever
        # build-macos.sh last produced.
        generic_output="$(effective_flag --output "$OPTIONS_FILE" || true)"
        if [[ -n "$generic_output" && -f "$generic_output" ]]; then
            echo "                 snug wrote '$generic_output' — the --output from" >&2
            echo "                 $OPTIONS_FILE, ignoring $OS_OPTIONS_FILE." >&2
            echo
            echo "                 So this snug did not apply the per-OS options tier." >&2
            echo "                 Almost always a stale binary: $STAGED_CLI predates" >&2
            echo "                 the feature, or was staged from an older build." >&2
            echo "                 Re-run scripts/build-macos.sh, or drop --skip-cli." >&2
        else
            echo "                 Nothing was written there. snug may have" >&2
            echo "                 honoured a different --output than the one resolved" >&2
            echo "                 above — rerun with the build output visible." >&2
        fi
    fi
    exit 1
fi
echo "    is a directory: yes"

# A directory needs the execute bit to be traversable; without it the
# bundle is invisible to execve even though every file inside is right.
for dir in "$DEMO_APP" "$DEMO_APP/Contents" "$DEMO_APP/Contents/MacOS" "$DEMO_APP/Contents/Resources"; do
    mode="$(stat -f '%Lp' "$dir")"
    if [[ "${mode: -1}" != "5" && "${mode: -1}" != "7" ]]; then
        echo "build-macos-demo: $dir has mode $mode — not traversable (need 0755-ish)." >&2
        exit 1
    fi
done
echo "    directories traversable: yes"

# The executable is named after the bundle's file stem; resolve it from
# the directory rather than hardcoding a name that changes with -o.
executable="$(find "$DEMO_APP/Contents/MacOS" -maxdepth 1 -type f | head -1)"
if [[ -z "$executable" ]]; then
    echo "build-macos-demo: no executable in Contents/MacOS." >&2
    exit 1
fi
mode="$(stat -f '%Lp' "$executable")"
if [[ "${mode: -1}" != "5" && "${mode: -1}" != "7" ]]; then
    echo "build-macos-demo: $executable has mode $mode — not executable." >&2
    exit 1
fi
echo "    executable: $(basename "$executable") (mode $mode)"

expected_arch="$(uname -m)"
actual_arch="$(lipo -archs "$executable")"
if [[ "$actual_arch" != "$expected_arch" ]]; then
    echo "build-macos-demo: launcher is '$actual_arch' but this host is '$expected_arch'." >&2
    echo "                 Each snug binary embeds only its own arch's launcher." >&2
    exit 1
fi
echo "    launcher arch: $actual_arch (matches host)"

floor="$(otool -l "$executable" | grep -E 'minos|version ' | head -1 | awk '{print $NF}')"
if [[ "$floor" != "$DEPLOYMENT_TARGET" ]]; then
    echo "build-macos-demo: launcher links against macOS $floor, expected $DEPLOYMENT_TARGET." >&2
    exit 1
fi
echo "    deployment floor: macOS $floor"

# plutil is macOS's own plist validator — the authoritative check that
# Launch Services will accept it.
if ! plutil_out="$(plutil -lint "$DEMO_APP/Contents/Info.plist" 2>&1)"; then
    echo "build-macos-demo: Info.plist is invalid: $plutil_out" >&2
    exit 1
fi
echo "    Info.plist: $plutil_out"

if ! codesign --verify --verbose=1 "$DEMO_APP" 2>&1; then
    echo "build-macos-demo: the bundle's signature does not verify." >&2
    exit 1
fi
echo "    signature: verified (ad-hoc)"

if [[ ! -f "$DEMO_APP/Contents/Resources/$APP_STEM.snugpayload" ]]; then
    echo "build-macos-demo: the payload is missing from Contents/Resources." >&2
    echo "                 Expected $APP_STEM.snugpayload (named after the bundle)." >&2
    exit 1
fi
payload_bytes="$(stat -f '%z' "$DEMO_APP/Contents/Resources/$APP_STEM.snugpayload")"
echo "    payload: $APP_STEM.snugpayload ($payload_bytes bytes)"

# ------------------------------------------------------------- stage

echo
echo "==> staging into $PLATFORM_DIR"
mkdir -p "$PLATFORM_DIR"

# Where the bundle lands in the release tree, from the same $DEMO_APP.
readonly STAGED_APP="$PLATFORM_DIR/$(basename "$DEMO_APP")"

# Remove the previous staged bundle first. `cp -R` into an existing
# directory of the same name copies *inside* it, which would quietly
# produce release/…/snug-javafx-demo.app/snug-javafx-demo.app.
#
# Unregister from Launch Services *first*. Deleting a registered .app
# leaves a stale record pointing at the old path, and the rebuilt bundle
# has the same identifier - so Finder keeps resolving the id to the dead
# path and refuses to open the new one with
# `_LSOpenURLsWithCompletionHandler() failed with error -1712`. The new
# bundle validates, lints and verifies, so that error is very hard to
# connect to its real cause. Advisory: a bundle LS never saw is fine, so
# the exit status is ignored.
if [[ -d "$STAGED_APP" ]]; then
    "$LSREGISTER" -u "$STAGED_APP" >/dev/null 2>&1 || true
    rm -rf "$STAGED_APP"
fi
cp -R "$DEMO_APP" "$PLATFORM_DIR/"
cp "$DEMO_JAR" "$PLATFORM_DIR/"

echo
echo "==> done. Release tree for $DEMO_SUBDIR:"
# Filter .DS_Store: Finder drops one in as soon as the folder is browsed,
# and it is not part of the release.
find "$PLATFORM_DIR" -mindepth 1 -maxdepth 1 ! -name '.DS_Store' | sort |
    sed "s|^$PLATFORM_DIR/|    |"
echo
echo "    launch it with:  open \"$STAGED_APP\""
echo
echo "    note: the splash is not implemented on macOS, so the launcher's log"
echo "          will say so and carry on. The Adoptium download flow is"
echo "          likewise discovery-only for now — a compatible JDK must"
echo "          already be installed."
echo

# Print one Java version per line for every JDK visible on this machine.
#
# This deliberately mirrors the *launcher's* discovery order
# (`platform/macos.rs` `discover_jvm`) instead of trusting one tool:
# `/usr/libexec/java_home -v <min>` first, then a walk of the well-known
# `JavaVirtualMachines` directories and the Homebrew `opt` symlinks.
#
# The directory walk is not belt-and-braces. `java_home` is unavailable
# inside sandboxes and some locked-down environments, where it prints
# "Unable to locate a Java Runtime" and exits non-zero *even with a working
# JDK installed and on PATH*. A pre-flight that trusted it alone therefore
# reported a confident "no JDK found - the demo will not launch" for a
# machine that launches perfectly well, which is worse than silence: it
# sends you chasing a Java problem that does not exist. Falling back to the
# same directories the launcher falls back to is what keeps this check and
# the real launch path from disagreeing.
#
# Versions come from each home's `release` file (`JAVA_VERSION="25.0.4.1"`),
# which is also where the launcher reads them from - no process is executed
# and nothing depends on a `java` being on PATH.
jdk_versions_visible() {
    local root entry home release

    if [[ -x /usr/libexec/java_home ]]; then
        # `|| true` is load-bearing under this script's `set -euo pipefail`.
        # `java_home -V` exits non-zero when it finds nothing, and with
        # `pipefail` that status becomes the pipeline's, so `set -e` would
        # abort the whole build script at the pre-flight - the one place a
        # missing tool must be survivable, since the directory walk below
        # is exactly the answer to it.
        /usr/libexec/java_home -V 2>/dev/null |
            sed -n 's/^ *\([0-9][0-9.]*\).*/\1/p' || true
    fi

    for root in \
        "/Library/Java/JavaVirtualMachines" \
        "${HOME:-}/Library/Java/JavaVirtualMachines"
    do
        [[ -d "$root" ]] || continue
        for entry in "$root"/*/; do
            release="${entry}Contents/Home/release"
            [[ -f "$release" ]] || continue
            sed -n 's/^JAVA_VERSION="\(.*\)"$/\1/p' "$release"
        done
    done

    # Homebrew's openjdk lives behind an `opt` symlink rather than in a
    # JavaVirtualMachines directory. Both prefixes are probed because this
    # is an Apple Silicon *or* Intel build.
    for home in \
        "/opt/homebrew/opt/openjdk/libexec/openjdk.jdk/Contents/Home" \
        "/usr/local/opt/openjdk/libexec/openjdk.jdk/Contents/Home"
    do
        [[ -f "$home/release" ]] || continue
        sed -n 's/^JAVA_VERSION="\(.*\)"$/\1/p' "$home/release"
    done
}

# Pre-flight the *runtime*, not the build. The bundle above is a valid
# artefact either way, but it will refuse to launch on a machine with no
# suitable JDK, and the refusal is worth saying now rather than leaving
# someone to discover it from a window that never appears.
#
# The demo classes ship as class file version 69.0 (Java 25) and snug
# refuses anything below the `--min-java` the build actually used, so this
# reads it back through `effective_flag` rather than grepping snug.options
# alone — otherwise raising `--min-java` in snug.macos.options would leave
# this check warning about the wrong number.
min_java="$(effective_flag --min-java "$OS_OPTIONS_FILE" "$OPTIONS_FILE" 2>/dev/null || true)"
if [[ -n "$min_java" ]]; then
    newest_home="$(jdk_versions_visible | sort -V | tail -1)"
    if [[ -n "$newest_home" ]]; then
        newest_major="${newest_home%%.*}"
        if (( newest_major < min_java )); then
            echo "    WARNING: the demo needs Java $min_java+ (the options files say --min-java $min_java)"
            echo "             but the newest JDK here is $newest_home."
            echo "             The .app built and staged correctly; it will refuse to"
            echo "             launch until a suitable JDK is installed."
            echo
        else
            echo "    runtime: newest JDK here is $newest_home, demo needs $min_java+ — OK"
            echo
        fi
    else
        echo "    WARNING: no JDK found (checked /usr/libexec/java_home and the"
        echo "             JavaVirtualMachines directories); the demo will not"
        echo "             launch until one is installed."
        echo
    fi
else
    echo "    runtime: no --min-java in $OS_OPTIONS_FILE or $OPTIONS_FILE; skipping the JDK pre-flight."
    echo
fi
