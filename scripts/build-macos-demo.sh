#!/usr/bin/env bash
#
# build-macos-demo.sh — build the macOS `snug` release, then package the
# JavaFX demo as a real `.app`.
#
# This is the macOS counterpart of step 4 + the staging block in
# scripts\build-release.cmd: that one builds assets\snug-javafx-demo.exe,
# this one builds assets/snug-javafx-demo.app. Same input JAR, same
# options file, same "build into assets/ then stage" shape — so the two
# platforms cannot drift apart on how a demo is produced.
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
readonly DEMO_APP="assets/snug-javafx-demo.app"
readonly OPTIONS_FILE="snug.options"

CLEAN=0
SKIP_CLI=0
for arg in "$@"; do
    case "$arg" in
        --clean) CLEAN=1 ;;
        --skip-cli) SKIP_CLI=1 ;;
        -h|--help) sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
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

# Pass --options explicitly rather than relying on the CWD copy being
# picked up implicitly. Same file, but now the dependency is visible in
# the script rather than being an accident of where you ran it from.
demo_args=()
if [[ -f "$OPTIONS_FILE" ]]; then
    demo_args+=(--options "$OPTIONS_FILE")
    echo "    options: $OPTIONS_FILE (name/company/icon/splash/main-class)"
fi

# The -o below overrides the `--output assets/snug-javafx-demo.exe` in
# snug.options, which is the one that would otherwise send a macOS build
# to a Windows-shaped path. The `${arr[@]+...}` form is the portable way
# to expand a possibly-empty array under `set -u` (bash 3.2 would treat a
# bare "${arr[@]}" as unbound).
echo
echo "==> packaging the demo as a .app"
"$STAGED_CLI" ${demo_args[@]+"${demo_args[@]}"} -o "$DEMO_APP" "$DEMO_JAR"
echo

# ------------------------------------------------------------ verify

echo "==> verifying $DEMO_APP"

# The single most important check, and the one that would have caught
# assets/snug-javafx-demo.app being a flat Windows PE with a .app name:
# a bundle is a *directory*. Everything below is meaningless unless this
# holds.
if [[ ! -d "$DEMO_APP" ]]; then
    echo "build-macos-demo: $DEMO_APP is not a directory — a .app must be a bundle." >&2
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

if [[ ! -f "$DEMO_APP/Contents/Resources/snug-javafx-demo.snugpayload" ]]; then
    echo "build-macos-demo: the payload is missing from Contents/Resources." >&2
    exit 1
fi
payload_bytes="$(stat -f '%z' "$DEMO_APP/Contents/Resources/snug-javafx-demo.snugpayload")"
echo "    payload: snug-javafx-demo.snugpayload ($payload_bytes bytes)"

# ------------------------------------------------------------- stage

echo
echo "==> staging into $PLATFORM_DIR"
mkdir -p "$PLATFORM_DIR"
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
if [[ -d "$PLATFORM_DIR/snug-javafx-demo.app" ]]; then
    "$LSREGISTER" -u "$PLATFORM_DIR/snug-javafx-demo.app" >/dev/null 2>&1 || true
    rm -rf "$PLATFORM_DIR/snug-javafx-demo.app"
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
echo "    launch it with:  open \"$PLATFORM_DIR/snug-javafx-demo.app\""
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
# refuses anything below the `--min-java` in snug.options.
min_java="$(grep -oE '^[[:space:]]*--min-java[[:space:]]+[0-9]+' "$OPTIONS_FILE" 2>/dev/null |
    grep -oE '[0-9]+' | head -1)"
if [[ -n "$min_java" ]]; then
    newest_home="$(jdk_versions_visible | sort -V | tail -1)"
    if [[ -n "$newest_home" ]]; then
        newest_major="${newest_home%%.*}"
        if (( newest_major < min_java )); then
            echo "    WARNING: the demo needs Java $min_java+ (snug.options says --min-java $min_java)"
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
    echo "    runtime: no --min-java in $OPTIONS_FILE; skipping the JDK pre-flight."
    echo
fi
