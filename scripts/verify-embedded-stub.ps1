# ============================================================================
# scripts/verify-embedded-stub.ps1
#
# Byte-for-byte sanity check that <ExePath> contains the full contents of
# <StubPath> as a contiguous substring. Used by build-windows.cmd to prove
# snug.exe still embeds the launcher stub after a rebuild (catches stale
# include_bytes!() tracking if cargo's mtime/content cache ever drifts).
#
# Exits 0 on match, 1 on mismatch. Prints a one-line summary on match.
# ============================================================================

param(
    [Parameter(Mandatory = $true)] [string] $StubPath,
    [Parameter(Mandatory = $true)] [string] $ExePath
)

$ErrorActionPreference = 'Stop'

# SHA-256 via .NET rather than the Get-FileHash cmdlet.
#
# build-windows.cmd launches this script with `powershell -File`, and cmd is in
# turn launched by pwsh (the GitHub Actions default shell on Windows). Windows
# PowerShell 5.1 therefore inherits pwsh's $env:PSModulePath, which puts the
# PowerShell 7 module directories AHEAD of the Windows PowerShell ones. 5.1
# then autoloads the PS7 copy of Microsoft.PowerShell.Utility (7.0.0.0), which
# does not export Get-FileHash to Desktop edition, so the cmdlet simply is not
# there:
#
#     The term 'Get-FileHash' is not recognized as the name of a cmdlet...
#
# -NoProfile does NOT fix this - the shadowing comes from the inherited module
# path, not from a profile. Upstream: PowerShell/PowerShell#8635, fixed only on
# the Core side by #6850, and actions/runner-images#225, still live on PS 7.4.x.
#
# [System.Security.Cryptography.SHA256] lives in mscorlib, so it resolves under
# every PowerShell edition with no module autoloading involved at all. Both
# byte arrays are already in hand below, so this is strictly less work than the
# cmdlet was: no second read of the file, and no MemoryStream for the slice.
function Get-Sha256Hex {
    param([Parameter(Mandatory = $true)] [byte[]] $Bytes)

    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $hash = $sha.ComputeHash($Bytes)
    } finally {
        $sha.Dispose()
    }
    # BitConverter renders "AB-CD-EF"; Get-FileHash rendered "ABCDEF". Same
    # casing (upper), so the two remain interchangeable in any printed output.
    return ([System.BitConverter]::ToString($hash)).Replace('-', '')
}

$stubFull = (Resolve-Path -LiteralPath $StubPath).Path
$exeFull  = (Resolve-Path -LiteralPath $ExePath).Path

$stub = [System.IO.File]::ReadAllBytes($stubFull)
$exe  = [System.IO.File]::ReadAllBytes($exeFull)

$stubLen  = $stub.Length
$stubSha  = Get-Sha256Hex -Bytes $stub

if ($exe.Length -lt $stubLen) {
    Write-Host ("  FAIL: {0} ({1} B) is smaller than stub ({2} B)" -f $exeFull, $exe.Length, $stubLen) -ForegroundColor Red
    exit 1
}

# Naive O(n*m) byte search is fine here — n is ~1 MB and this runs once
# per release build. If we ever embed multi-MB stubs, swap in a Boyer-
# Moore-Horspool or just take the first 4 bytes as a quick probe.
$offset = -1
for ($i = 0; $i -le $exe.Length - $stubLen; $i++) {
    if ($exe[$i] -ne $stub[0]) { continue }
    $match = $true
    for ($j = 1; $j -lt $stubLen; $j++) {
        if ($exe[$i + $j] -ne $stub[$j]) { $match = $false; break }
    }
    if ($match) { $offset = $i; break }
}

if ($offset -lt 0) {
    Write-Host ("  FAIL: {0} bytes of stub not found inside {1}" -f $stubLen, $exeFull) -ForegroundColor Red
    Write-Host ("        stub sha256: {0}" -f $stubSha)
    exit 1
}

$slice    = $exe[$offset..($offset + $stubLen - 1)]
$sliceSha = Get-Sha256Hex -Bytes $slice

if ($sliceSha -ne $stubSha) {
    Write-Host "  FAIL: slice sha256 does not match stub sha256" -ForegroundColor Red
    Write-Host ("        stub sha256:  {0}" -f $stubSha)
    Write-Host ("        slice sha256: {0}" -f $sliceSha)
    exit 1
}

Write-Host ("  OK:   {0} contiguous bytes at offset 0x{1:X} of {2}" -f $stubLen, $offset, $exeFull)
Write-Host ("        stub sha256:  {0}" -f $stubSha)
exit 0
