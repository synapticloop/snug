# Reproducible-build flags, shared by everything that produces the launcher stub.
#
# Dot-source this; it defines Set-ReproducibleRustflags and nothing else.
#
# WHY THIS EXISTS
#
# The committed stub is compared, byte for byte, against a fresh build. That
# only works if the build is reproducible -- and by default it is not, across
# checkouts. `file!()` expands to an absolute path, and cargo passes absolute
# paths for *registry* dependencies, so the finished launcher embeds:
#
#   * ~83 strings rooted at %CARGO_HOME%\registry, e.g.
#     C:\Users\<you>\.cargo\registry\src\...\base64-0.22.1\src\encode.rs
#   * ~20 more from the rustup toolchain's copy of the standard library,
#     e.g. ...\.rustup\toolchains\...\lib\rustlib\src\rust\library\alloc\src\str.rs
#
# CI's CARGO_HOME is C:\Users\runneradmin, so a build there embeds a
# different set of paths and can never match a stub committed from anywhere
# else. That is what made the CI step fail on a correctly-refreshed stub.
#
# The fix is to remap both roots onto fixed prefixes. Each machine remaps its
# OWN paths onto the SAME targets, so the outputs agree without either one
# hardcoding the other's layout.
#
# CARGO_ENCODED_RUSTFLAGS, not RUSTFLAGS: the separator between flags is
# \x1f (unit separator), which cannot appear in a path and therefore needs no
# quoting or escaping. Plain RUSTFLAGS is space-separated, so a space anywhere
# in CARGO_HOME silently splits one flag into two -- producing a binary that
# builds fine and is simply not the reproducible one. This form has no such
# caveat, which is also why build-windows.cmd goes through
# cargo-repro.ps1 instead of setting the variable itself: a .cmd file cannot
# hold a \x1f legibly.

function Set-ReproducibleRustflags {
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
    $rustupHome = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $env:USERPROFILE '.rustup' }

    $sep = [char]0x1f
    $flags = @(
        "--remap-path-prefix=$($cargoHome)\registry=/cargo/registry"
        "--remap-path-prefix=$rustupHome=/rustup"
    )

    # Preserve anything the caller already set; ours go first so they cannot
    # be shadowed by a later duplicate.
    if ($env:CARGO_ENCODED_RUSTFLAGS) {
        $flags += $env:CARGO_ENCODED_RUSTFLAGS -split $sep
    }
    $env:CARGO_ENCODED_RUSTFLAGS = ($flags | Where-Object { $_ }) -join $sep
}