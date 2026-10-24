<#
.SYNOPSIS
Check that the committed launcher stub and its recorded source fingerprint
agree with the launcher source in this checkout.

.DESCRIPTION
`snug-cli` include_bytes!s the committed launcher stub, so the stub is a
build artefact that lives in the repository. Two artefacts have to travel
together for that to work:

  * bin\launcher-stub-windows-x86_64.exe  - the stub itself
  * bin\launcher-stub.source              - the fingerprint of the launcher
                                            source the stub was built from

Commit one without the other and the repository is inconsistent: the stub
embeds last commit's launcher, and CI's fingerprint gate catches it as
"the committed stub is stale".

This script is the single implementation of that check. It is called from
three places, deliberately:

  * .github\workflows\windows.yml  - the CI gate
  * scripts\windows\check.ps1      - the local gate loop
  * .githooks\pre-push             - the hook that refuses a bad push

A second copy of this logic, in YAML, is a second thing to drift.

The comparison is over the launcher *source*, not the stub's bytes, because
a byte comparison spans two machines: this repo's Windows workstation and
the `windows-latest` runner disagree on rustc, the MSVC linker and the
Windows SDK, so their launcher binaries can never be byte-identical. See
scripts\windows\launcher-source-fingerprint.ps1 for the full reasoning.

.OUTPUTS
One line on success. On failure, a diagnostic on stderr naming the two
fingerprints and the command that fixes it. Exit code is 0 when the pair
agrees, 1 otherwise.

.NOTES
Uses only [System.IO.File] and [System.Security.Cryptography.SHA256],
never Get-FileHash: build.cmd invokes this with `powershell -File`, and
that 5.1 child inherits pwsh's $env:PSModulePath, where the cmdlet does
not resolve. See commit 9861393 for the full chain.
#>
[CmdletBinding()]
param([string]$Repo)

# $Repo is resolved here rather than in a param() default, because
# [CmdletBinding()] breaks that. Windows PowerShell 5.1 evaluates
# parameter defaults *before* $PSScriptRoot is bound, so Split-Path is
# handed an empty string and the script dies on its first line, before it
# has done anything. `powershell -File` -- which is how the pre-push hook
# invokes this -- hits it too, while `pwsh` does not, so the bug shows up
# on one machine and not the other. Deriving it after the param block works
# everywhere, and it is what check.ps1 and refresh-stub.ps1 already do.
if (-not $Repo) {
    $Repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
}

# 'Continue', not 'Stop', for the same reason check.ps1 uses it: a caller
# that captures this script's output would otherwise turn cargo-style
# stderr chatter into a terminating NativeCommandError. This script's own
# failure path writes to stderr deliberately and sets the exit code by
# hand, so nothing is lost by not throwing.
$ErrorActionPreference = 'Continue'

$recordedPath = Join-Path $Repo 'bin\launcher-stub.source'

if (-not (Test-Path $recordedPath)) {
    [Console]::Error.WriteLine("check-stub-pair: no bin\launcher-stub.source in $Repo.")
    [Console]::Error.WriteLine('')
    [Console]::Error.WriteLine('  Run scripts\windows\refresh-stub.ps1 and commit the result.')
    exit 1
}

. (Join-Path $PSScriptRoot 'launcher-source-fingerprint.ps1')

$current = Get-LauncherSourceFingerprint -Repo $Repo
# The file is written by refresh-stub.ps1 as a bare hex digest followed by
# CRLF, so Trim() removes the terminator and nothing else. It is ASCII, so
# the default read encoding is exact.
$recorded = (Get-Content $recordedPath -Raw).Trim()

if ($current -eq $recorded) {
    Write-Host "check-stub-pair: committed stub matches the current launcher source"
    Write-Host "  fingerprint: $current"
    exit 0
}

[Console]::Error.WriteLine('check-stub-pair: the committed launcher stub does not match the launcher source.')
[Console]::Error.WriteLine('')
[Console]::Error.WriteLine("  launcher source now : $current")
[Console]::Error.WriteLine("  stub was built from : $recorded")
[Console]::Error.WriteLine('')
[Console]::Error.WriteLine('  The launcher source has changed since the committed stub was built,')
[Console]::Error.WriteLine('  so a fresh clone would embed the previous commit''s launcher.')
[Console]::Error.WriteLine('')
[Console]::Error.WriteLine('  Fix:')
[Console]::Error.WriteLine('      .\scripts\windows\refresh-stub.ps1')
[Console]::Error.WriteLine('      git add bin\launcher-stub-windows-x86_64.exe bin\launcher-stub.source')
[Console]::Error.WriteLine('      git commit --amend --no-edit')
exit 1
