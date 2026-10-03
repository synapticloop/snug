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
readonly DEMO_JAR="assets/snug-javafx-demo.jar"
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

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "build-macos-demo: must run on macOS (found $(uname -s))." >&2
    echo "                  Windows artefacts come from scripts\\build-release.cmd." >&2
    exit 1
fi

for tool in cargo plutil codesign lipo otool; do
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
if [[ ! -f "$DEMO_JAR" ]]; then
    cat >&2 <<EOF
build-macos-demo: demo JAR not found at $DEMO_JAR.
                  Build it first, or skip the demo with --skip-cli.
EOF
    exit 1
fi
echo "    jar: $DEMO_JAR"

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
if [[ -d "$PLATFORM_DIR/snug-javafx-demo.app" ]]; then
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

# Pre-flight the *runtime*, not the build. The bundle above is a valid
# artefact either way, but it will refuse to launch on a machine with no
# suitable JDK, and the refusal is worth saying now rather than leaving
# someone to discover it from a window that never appears.
#
# The demo classes ship as class file version 69.0 (Java 25) and snug
# refuses anything below the `--min-java` in snug.options. That produced a
# perfectly correct but very confusing failure on a machine whose newest
# JDK was 23, so check it explicitly.
min_java="$(grep -oE '^[[:space:]]*--min-java[[:space:]]+[0-9]+' "$OPTIONS_FILE" 2>/dev/null |
    grep -oE '[0-9]+' | head -1)"
if [[ -n "$min_java" ]]; then
    newest_home="$(/usr/libexec/java_home -V 2>&1 |
        sed -n 's/^ *\([0-9][0-9.]*\).*/\1/p' | sort -V | tail -1)"
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
        echo "    WARNING: no JDK found via /usr/libexec/java_home; the demo will"
        echo "             not launch until one is installed."
        echo
    fi
else
    echo "    runtime: no --min-java in $OPTIONS_FILE; skipping the JDK pre-flight."
    echo
fi
