#!/usr/bin/env bash
#
# build-macos.sh — build the `snug` CLI for macOS and stage it into
# release/<os>-<arch>/snug.
#
# Why a script and not a bare `cargo build`: two reasons, both learned the
# hard way.
#
#   1. `cargo` does NOT fingerprint MACOSX_DEPLOYMENT_TARGET. Change it and
#      rebuild and you silently get the cached binary at the *old* floor,
#      with no warning. This script verifies the emitted `minos` on every
#      artefact and fails loudly rather than shipping a stale one.
#
#   2. The staged *name* is always `snug`. The platform lives in the
#      directory (`release/macos-arm64/snug`, `release/macos-x86_64/snug`),
#      so every platform invokes the same `snug` and the documentation
#      never has to name two different binaries.
#
# Usage:
#   scripts/build-macos.sh [--clean]
#
#   --clean   remove release/macos-* before staging, for a guaranteed-clean
#             result. Without it, an artefact from a previous run that this
#             run no longer produces is left where it is.
#
# Environment:
#   MACOSX_DEPLOYMENT_TARGET   override the floor (default 12.0 Monterey)
#
# Not a replacement for scripts\build-windows.cmd — that one is the Windows
# pipeline (launcher stub, dropper, demo JAR, icon stamping) and needs a
# native Windows host. This one is macOS-only and touches nothing Windows
# produces.

set -euo pipefail

readonly DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-12.0}"

# Cargo target triple -> (release subdirectory, expected `lipo` arch).
# `arm64` is `uname -m` on Apple Silicon, which is what a user looking at a
# directory listing will recognise; the Rust target is called aarch64.
readonly TARGETS=(
    "aarch64-apple-darwin:macos-arm64:arm64"
    "x86_64-apple-darwin:macos-x86_64:x86_64"
)

CLEAN=0
for arg in "$@"; do
    case "$arg" in
        --clean) CLEAN=1 ;;
        -h|--help) sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "build-macos: unknown argument '$arg' (try --help)" >&2; exit 2 ;;
    esac
done

# Repo root = two levels up from scripts/.
readonly ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly RELEASE_DIR="$ROOT/release"

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "build-macos: must run on macOS (found $(uname -s))." >&2
    echo "            Windows artefacts come from scripts\\build-windows.cmd." >&2
    exit 1
fi

for tool in cargo lipo otool codesign; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "build-macos: '$tool' not found on PATH." >&2
        exit 1
    fi
done

cd "$ROOT"

# The deployment floor of an already-linked Mach-O. Handles both load
# commands: LC_BUILD_VERSION (`minos`) and the older
# LC_VERSION_MIN_MACOSX (`version`), which is what a 10.12-floored binary
# carries.
minos_of() {
    otool -l "$1" | awk '
        /LC_BUILD_VERSION/      { mode = "build"; next }
        /LC_VERSION_MIN_MACOSX/ { mode = "min";   next }
        mode == "build" && $1 == "minos"   { print $2; exit }
        mode == "min"   && $1 == "version" { print $2; exit }
    '
}

# A binary is only runnable if its arch matches the host. On an Intel Mac
# the arm64 artefact is verified structurally but never executed.
host_arch="$(uname -m)"

echo "==> macOS release build"
echo "    deployment target: $DEPLOYMENT_TARGET"
echo "    staging into:      release/<os>-<arch>/snug"
echo

for entry in "${TARGETS[@]}"; do
    IFS=: read -r triple subdir expect_arch <<<"$entry"

    if ! rustup target list --installed 2>/dev/null | grep -qx "$triple"; then
        echo "==> $triple"
        echo "    target not installed; adding it"
        rustup target add "$triple"
    fi

    echo "==> $triple"
    # Exported rather than passed inline so every cargo/rustc invocation in
    # this process sees it, and so a `#[link]`-style child cannot disagree.
    export MACOSX_DEPLOYMENT_TARGET="$DEPLOYMENT_TARGET"

    # The launcher must be built and copied into bin/ BEFORE snug-cli
    # compiles, because snug-cli embeds it with `include_bytes!`
    # (`macos_bundle.rs`, selected by `#[cfg(target_arch)]`).
    #
    # Omitting this step is silent, which is what made it worth fixing.
    # Nothing errors: the build succeeds, the CLI runs, the `.app` is
    # produced and validates — and it contains the *committed* launcher.
    # So a change to snug-launcher is quietly absent from the artefact
    # while every check still passes. That is precisely how a launcher fix
    # can sit in the working tree, "verified" by a green demo build, and
    # never reach a release. The committed stub is a bootstrap convenience
    # for a fresh clone (so `cargo build -p snug-cli` works before you have
    # built anything), never the source of truth for a release build.
    cargo build --release -p snug-launcher --target "$triple"
    launcher_built="$ROOT/target/$triple/release/snug-launcher"
    if [[ ! -f "$launcher_built" ]]; then
        echo "build-macos: expected $launcher_built but it was not produced." >&2
        exit 1
    fi
    # $expect_arch is the `lipo` arch, which is also the stub's filename
    # suffix (arm64 / x86_64) — the same spellings macos_bundle.rs embeds.
    stub_dest="$ROOT/bin/launcher-stub-macos-$expect_arch"
    cp "$launcher_built" "$stub_dest"
    echo "    launcher: refreshed bin/launcher-stub-macos-$expect_arch from $triple"

    cargo build --release -p snug-cli --target "$triple"

    built="$ROOT/target/$triple/release/snug"
    if [[ ! -f "$built" ]]; then
        echo "build-macos: expected $built but it was not produced." >&2
        exit 1
    fi

    # Verify before staging: a wrong-arch or wrong-floor binary that lands
    # in release/ is exactly the failure that is hardest to notice later.
    actual_arch="$(lipo -archs "$built")"
    if [[ "$actual_arch" != "$expect_arch" ]]; then
        echo "build-macos: $triple produced arch '$actual_arch', expected '$expect_arch'." >&2
        exit 1
    fi

    actual_minos="$(minos_of "$built")"
    if [[ "$actual_minos" != "$DEPLOYMENT_TARGET" ]]; then
        cat >&2 <<EOF
build-macos: $triple links against macOS $actual_minos, expected $DEPLOYMENT_TARGET.

    This is almost certainly cargo's cache: MACOSX_DEPLOYMENT_TARGET is not
    part of the build fingerprint, so a binary built at a different floor
    is reused as-is. Rebuild with a clean target directory:

        CARGO_TARGET_DIR=target-macos cargo build --release -p snug-cli \\
            --target $triple
EOF
        exit 1
    fi

    if [[ "$actual_arch" == "$host_arch" ]]; then
        version="$("$built" --snug-version)"
        echo "    runs here: $version"
    else
        echo "    not executable on this host ($host_arch); verified structurally only"
    fi

    # Ad-hoc sign explicitly. arm64 refuses to execute a binary with no
    # code signature at all; the linker adds one automatically, but saying
    # so here means this stays true if that default ever changes. Ad-hoc is
    # enough to *run* — satisfying Gatekeeper for a browser-downloaded
    # release needs a Developer ID plus notarisation, which is out of
    # scope for this script (see AGENTS.md, "Build host").
    codesign --force --sign - "$built"

    dest_dir="$RELEASE_DIR/$subdir"
    if [[ "$CLEAN" == "1" ]]; then
        rm -rf "$dest_dir"
    fi
    mkdir -p "$dest_dir"
    # cp, not mv: target/ stays as the build cache, which is what makes a
    # second run of this script fast.
    cp "$built" "$dest_dir/snug"
    chmod +x "$dest_dir/snug"

    echo "    staged: release/$subdir/snug ($(lipo -archs "$dest_dir/snug"), macOS $actual_minos)"
    echo
done

echo "==> done. Release tree:"
find "$RELEASE_DIR" -type f -name 'snug' -o -type f -name '*.exe' 2>/dev/null | sort | sed "s|^$ROOT/|    |"
