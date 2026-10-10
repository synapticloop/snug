<#
.SYNOPSIS
Refresh bin/launcher-stub-windows-x86_64.exe from a fresh release build,
and report whether the committed stub matched beforehand.

.DESCRIPTION
`snug-cli` include_bytes!s the committed stub, so the stub is a build
artefact that happens to live in the repository. It goes stale whenever the
launcher it was built from changes -- and "whenever the launcher changes"
includes things that do not look like launcher changes at all. A workspace
*version bump* rewrites 653,904 bytes of the launcher binary, because every
crate's version is part of the rustc crate metadata that feeds symbol
mangling and MIR identity, even though `snug-launcher` embeds no version
string of its own.

So the rule is not "refresh the stub when I edit platform/windows.rs", it is
"refresh the stub whenever the launcher binary would differ". This script
answers that question in one command instead of leaving it to memory, which
is how it went stale once already.

Steps 1 and 2 of scripts/build-windows.cmd do exactly this; the rest of that
script builds and stages a release, which is not needed between ordinary
commits.

.PARAMETER CheckOnly
Do not build or copy. Compare the committed stub against an existing
`target\release\snug-launcher.exe` and exit 1 if they differ. Useful as a
pre-commit or pre-push gate, where a full release build is too slow.

.OUTPUTS
A one-line summary, and the two normalised hashes. Exit code is 0 when the
stub is in sync (after refreshing, when refreshing), 1 otherwise.

.NOTES
Uses only [System.IO.File] and [System.Security.Cryptography.SHA256], never
Get-FileHash: build-windows.cmd invokes this with `powershell -File`, and
that 5.1 child inherits pwsh's $env:PSModulePath, where the cmdlet does not
resolve. See commit 9861393 for the full chain.
#>
[CmdletBinding()]
param([switch]$CheckOnly)

$ErrorActionPreference = 'Stop'

# `Write-Error` under `-File` renders PowerShell's full exception record --
# a stack trace, a category and a FullyQualifiedErrorId -- which is noise in a
# tool whose entire output is a status line and two hashes. Write to stderr
# directly and control the exit code ourselves.
function Write-Fail([string]$Message) {
    [Console]::Error.WriteLine("refresh-stub: $Message")
}

$repo = Split-Path -Parent $PSScriptRoot
$stub = Join-Path $repo 'bin\launcher-stub-windows-x86_64.exe'
$fresh = Join-Path $repo 'target\release\snug-launcher.exe'

. (Join-Path $PSScriptRoot 'pe-stable-hash.ps1')

function Get-StubState {
    if (-not (Test-Path $stub)) { return 'missing' }
    if (-not (Test-Path $fresh)) { return 'no-fresh-build' }
    if ((Get-PeStableHash $stub) -eq (Get-PeStableHash $fresh)) { return 'in-sync' }
    return 'stale'
}

$before = Get-StubState

if ($CheckOnly) {
    Write-Host "[refresh-stub] check-only: committed stub is $before"
} else {
    Write-Host "[refresh-stub] building snug-launcher (release)..."
    cargo build --release --manifest-path (Join-Path $repo 'Cargo.toml') -p snug-launcher
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build -p snug-launcher failed with exit code $LASTEXITCODE"
    }
    if (-not (Test-Path $fresh)) {
        throw "cargo reported success but there is no launcher at $fresh"
    }
    Copy-Item $fresh $stub -Force
    Write-Host "[refresh-stub] copied $fresh -> $stub"
}

$after = Get-StubState
$committed = if (Test-Path $stub) { Get-PeStableHash $stub } else { '(none)' }

switch ($after) {
    'in-sync' {
        Write-Host "[refresh-stub] committed stub: $committed"
        if ($before -eq 'stale') {
            Write-Host "[refresh-stub] was stale, now refreshed -- stage bin\launcher-stub-windows-x86_64.exe"
        }
        exit 0
    }
    'missing' {
        Write-Fail "no committed stub at $stub. Build and copy it, then commit the result."
        exit 1
    }
    'no-fresh-build' {
        Write-Fail "stub exists but there is no $fresh to compare against. Run without -CheckOnly first."
        exit 1
    }
    default {
        Write-Host "[refresh-stub] committed stub: $committed"
        Write-Host "[refresh-stub] fresh build  : $(Get-PeStableHash $fresh)"
        Write-Fail "stub is out of date. Run scripts\refresh-stub.ps1 and commit the refreshed binary."
        exit 1
    }
}