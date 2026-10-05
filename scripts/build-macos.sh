#!/usr/bin/env bash
#
# build-macos.sh — build the `snug` CLI and the `snug_preview` dev tool
# for macOS and stage them into release/<os>-<arch>/.
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
# Both artefacts are icon-stamped and the stamp is *verified*: a Mach-O
# carries its icon as an `__TEXT,__icns` section, and "the build script
# emitted a link-arg" is a claim about the build, not about the binary that
# ships. An iconless release is a cosmetic failure that stays invisible
# until a user looks at the file in a Finder window. `snug` takes
# assets/snug-runner.png (the same artwork Windows stamps into snug.exe) and
# `snug_preview` takes assets/snug-preview.png.
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
# Not a replacement for scripts\build-release.cmd — that one is the Windows
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
    echo "            Windows artefacts come from scripts\\build-release.cmd." >&2
    exit 1
fi

for tool in cargo lipo otool codesign python3; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "build-macos: '$tool' not found on PATH." >&2
        exit 1
    fi
done

cd "$ROOT"

# `snug-bundle` is a *build tool*, not a shipped artefact: it wraps a
# foreign-arch Mach-O in a bundle on the machine doing the building. So it
# is compiled for the HOST and run from `target/release`, while the binary
# it packages is the cross-compiled one from `target/$triple/release`.
#
# Building it per-target looks like the obvious symmetry and fails with
# `Bad CPU type in executable` on the first non-host arch — a build tool
# that cannot run on the build host is a build tool that cannot be used.
# Only the payload is cross-compiled; nothing else is.
cargo build --release -p snug-app-bundle --bin snug-bundle
readonly BUNDLER="$ROOT/target/release/snug-bundle"
if [[ ! -x "$BUNDLER" ]]; then
    echo "build-macos: expected a host build at $BUNDLER." >&2
    exit 1
fi

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

# The `Resources/App.icns` of a built bundle: its byte size, or empty if
# the bundle ships without an icon.
#
# This exists because "the bundler was asked for an icon" is not the same
# claim as "the icon is in the bundle". A cosmetic default that fails
# quietly ships a generic-icon release, and the failure only surfaces when
# a user looks at the file in a Finder window — long after the build went
# green.
icns_size_of() {
    local icns="$1/Contents/Resources/App.icns"
    [[ -f "$icns" ]] || return 0
    # From the ICNS header itself rather than `stat`, so a truncated or
    # empty file is caught by the length check below.
    python3 -c "
import struct, sys
d = open(sys.argv[1], 'rb').read()
if len(d) < 8 or d[:4] != b'icns':
    print(0)
else:
    print(struct.unpack('>I', d[4:8])[0])
" "$icns" 2>/dev/null || echo 0
}

# Fail unless `<bundle>` carries a well-formed `App.icns` of at least
# 1 KB. A real ICNS is ~1.7 MB; "the file exists" on its own would pass a
# truncated or zero-length one.
require_bundle_icon() {
    local bundle="$1" label="$2" size
    if [[ ! -d "$bundle" ]]; then
        echo "build-macos: $label is not a directory." >&2
        echo "            A .app must be a directory; a flat file with a .app name cannot launch." >&2
        return 1
    fi
    size="$(icns_size_of "$bundle")"
    if [[ -z "$size" || "$size" -eq 0 ]]; then
        echo "build-macos: $label has no usable Contents/Resources/App.icns." >&2
        echo "            Expected $3 to be converted and installed as the bundle icon." >&2
        return 1
    fi
    if (( size < 1024 )); then
        echo "build-macos: $label carries a ${size}-byte App.icns, which is too" >&2
        echo "            small to be a real icon (expect ~1.7 MB)." >&2
        return 1
    fi
    echo "    icon:     App.icns $size bytes ($3)"
}

# Fail unless `<bundle>` claims `.jar` in its `CFBundleDocumentTypes`.
#
# This is the one check that cannot be inferred from the icon or the
# layout. A bundle with a perfect `App.icns` and a launchable Mach-O
# still does nothing when you drop a JAR on it if Finder was never told
# the app accepts one — the drag simply bounces back, which reads to a
# beginner as "this app is broken" with nothing on screen to explain it.
# It is a one-line plist key, and it is the entire mechanism the dropper
# exists to provide.
require_drop_target() {
    local bundle="$1" plist="$1/Contents/Info.plist"
    if ! plutil -extract CFBundleDocumentTypes xml1 -o - "$plist" 2>/dev/null | grep -q "jar"; then
        echo "build-macos: $(basename "$bundle") does not claim .jar in CFBundleDocumentTypes." >&2
        echo "            Finder will refuse to drop a JAR on it and the app will" >&2
        echo "            appear broken. Re-bundle with --accepts-jar." >&2
        return 1
    fi
    echo "    drop:     accepts .jar via CFBundleDocumentTypes"
}

echo "==> macOS release build"
echo "    deployment target: $DEPLOYMENT_TARGET"
echo "    staging into:      release/<os>-<arch>/{snug,snug_preview}"
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

    # The dialog previewer. A dev tool, but it ships in the release
    # folder on both platforms: it is how you look at snug's dialogs and
    # error copy without building and launching an app. It lives in
    # snug-launcher rather than snug-cli, so it is a separate cargo
    # invocation — `--bin` is what keeps it from rebuilding the launcher
    # we already staged as the embedded stub.
    #
    # Its icon is `assets/snug-preview.png`, applied declaratively by
    # snug-launcher's build script via a per-bin
    # `rustc-link-arg-bin=snug_preview=…`. On Windows the same override
    # needs the post-link `stamp_preview_icon` helper, because the
    # resource compiler emits an un-binned link-arg. There is no macOS
    # equivalent to run, and nothing to forget.
    cargo build --release -p snug-launcher --bin snug_preview --target "$triple"

    preview="$ROOT/target/$triple/release/snug_preview"
    if [[ ! -f "$preview" ]]; then
        echo "build-macos: expected $preview but it was not produced." >&2
        exit 1
    fi

    preview_arch="$(lipo -archs "$preview")"
    if [[ "$preview_arch" != "$expect_arch" ]]; then
        echo "build-macos: snug_preview for $triple is arch '$preview_arch', expected '$expect_arch'." >&2
        exit 1
    fi

    preview_minos="$(minos_of "$preview")"
    if [[ "$preview_minos" != "$DEPLOYMENT_TARGET" ]]; then
        echo "build-macos: snug_preview for $triple links against macOS $preview_minos, expected $DEPLOYMENT_TARGET." >&2
        exit 1
    fi

    # The dialog previewer, shipped as an `.app` bundle.
    #
    # It is something a user *clicks*, not something they type, so it
    # ships the way macOS expects a clickable thing to ship: a bundle
    # whose icon lives in `Contents/Resources/App.icns`. That is
    # categorically different from `snug` above, which is a terminal
    # command and therefore carries **no icon at all** — an icon on a
    # CLI is 1.7 MB of Mach-O nobody will ever look at.
    #
    # It lives in snug-launcher rather than snug-cli, so it is a separate
    # cargo invocation; `--bin` keeps that from rebuilding the launcher
    # we already staged as the embedded stub.
    cargo build --release -p snug-launcher --bin snug_preview --target "$triple"

    preview="$ROOT/target/$triple/release/snug_preview"
    if [[ ! -f "$preview" ]]; then
        echo "build-macos: expected $preview but it was not produced." >&2
        exit 1
    fi

    preview_arch="$(lipo -archs "$preview")"
    if [[ "$preview_arch" != "$expect_arch" ]]; then
        echo "build-macos: snug_preview for $triple is arch '$preview_arch', expected '$expect_arch'." >&2
        exit 1
    fi

    preview_minos="$(minos_of "$preview")"
    if [[ "$preview_minos" != "$DEPLOYMENT_TARGET" ]]; then
        echo "build-macos: snug_preview for $triple links against macOS $preview_minos, expected $DEPLOYMENT_TARGET." >&2
        exit 1
    fi

    dest_dir="$RELEASE_DIR/$subdir"
    if [[ "$CLEAN" == "1" ]]; then
        rm -rf "$dest_dir"
    fi
    mkdir -p "$dest_dir"

    # `snug` is a command: sign it, ship it bare, no icon.
    #
    # Ad-hoc sign explicitly. arm64 refuses to execute a binary with no
    # code signature at all; the linker adds one automatically, but saying
    # so here means this stays true if that default ever changes. Ad-hoc is
    # enough to *run* — satisfying Gatekeeper for a browser-downloaded
    # release needs a Developer ID plus notarisation, which is out of
    # scope for this script (see AGENTS.md, "Build host").
    codesign --force --sign - "$built"
    # cp, not mv: target/ stays as the build cache, which is what makes a
    # second run of this script fast.
    cp "$built" "$dest_dir/snug"
    chmod +x "$dest_dir/snug"

    # `snug-bundle` writes and signs the bundle in one step.
    "$BUNDLER" \
        --binary "$preview" \
        --out "$dest_dir/snug_preview.app" \
        --name "snug_preview" \
        --id "com.synapticloop.snug.preview" \
        --icon "$ROOT/assets/snug-preview.png" \
        || exit 1

    # Icons last, so a bundle that lost its artwork is caught on the
    # artefact about to ship rather than on whatever is in target/.
    require_bundle_icon "$dest_dir/snug_preview.app" "snug_preview.app" "assets/snug-preview.png" || exit 1

    # The beginner dropper, `Build with Snug.app`.
    #
    # It has to be a bundle for a reason that is not cosmetic: a bare
    # Mach-O cannot be a drop target at all. Finder only hands a bundle
    # what its CFBundleDocumentTypes claims and delivers it through
    # `application:openFile:`, so the icon is a side effect of the thing
    # that makes the tool work rather than a separate decoration.
    #
    # The display name has a space, which is the whole point — the file
    # people drag onto says what it does. It lives *beside* `snug`,
    # never inside the bundle, because the dropper resolves `snug`
    # relative to its own location.
    cargo build --release -p snug-dropper --target "$triple"

    dropper="$ROOT/target/$triple/release/snug-dropper"
    if [[ ! -f "$dropper" ]]; then
        echo "build-macos: expected $dropper but it was not produced." >&2
        exit 1
    fi

    dropper_arch="$(lipo -archs "$dropper")"
    if [[ "$dropper_arch" != "$expect_arch" ]]; then
        echo "build-macos: snug-dropper for $triple is arch '$dropper_arch', expected '$expect_arch'." >&2
        exit 1
    fi

    # `--accepts-jar` writes the CFBundleDocumentTypes claim. Without it
    # the bundle is a perfectly good app that Finder will not let anyone
    # drop anything on.
    "$BUNDLER" \
        --binary "$dropper" \
        --out "$dest_dir/Build with Snug.app" \
        --name "Build with Snug" \
        --id "com.synapticloop.snug.buildwithsnug" \
        --icon "$ROOT/assets/snug-dropper.png" \
        --accepts-jar \
        || exit 1

    require_bundle_icon "$dest_dir/Build with Snug.app" "Build with Snug.app" "assets/snug-dropper.png" || exit 1
    require_drop_target "$dest_dir/Build with Snug.app" || exit 1

    echo "    staged: release/$subdir/snug ($(lipo -archs "$dest_dir/snug"), macOS $actual_minos, no icon)"
    echo "    staged: release/$subdir/snug_preview.app ($(lipo -archs "$dest_dir/snug_preview.app/Contents/MacOS/snug_preview"), macOS $preview_minos)"
    echo "    staged: release/$subdir/Build with Snug.app ($(lipo -archs "$dest_dir/Build with Snug.app/Contents/MacOS/Build with Snug"))"
    echo
done

echo "==> done. Release tree:"
find "$RELEASE_DIR" -maxdepth 3 \( -name 'snug' -o -name '*.app' -o -name '*.exe' \) 2>/dev/null | sort | sed "s|^$ROOT/|    |"
