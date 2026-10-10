<#
.SYNOPSIS
Run cargo with the reproducible-build flags set, then exit with cargo's code.

.DESCRIPTION
`build-windows.cmd` needs every cargo invocation in it to carry the same
`CARGO_ENCODED_RUSTFLAGS` as `refresh-stub.ps1`, or the launcher it produces
will not match the committed stub. It cannot set that variable itself: the
separator between flags is \x1f, and a .cmd file cannot hold that legibly --
it would be an invisible control character committed to a script people read.

So the pipeline calls this instead of calling cargo directly.

Arguments are passed straight through to cargo, e.g.

    powershell -File scripts\cargo-repro.ps1 build --release -p snug-launcher
#>
[CmdletBinding()]
param([Parameter(ValueFromRemainingArguments = $true)][string[]]$CargoArgs)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'reproducible-build.ps1')

Set-ReproducibleRustflags

if (-not $CargoArgs) {
    [Console]::Error.WriteLine('cargo-repro: no cargo arguments given')
    exit 1
}

# CARGO_ENCODED_RUSTFLAGS makes cargo take the flags over from RUSTFLAGS
# entirely. Clear the other one rather than leaving both set and letting
# precedence depend on the cargo version. Assignment, not Remove-Item, so
# this needs no filesystem-deletion capability to run.
$env:RUSTFLAGS = ''

& cargo @CargoArgs
exit $LASTEXITCODE