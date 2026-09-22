# ============================================================================
# scripts/verify-embedded-stub.ps1
#
# Byte-for-byte sanity check that <ExePath> contains the full contents of
# <StubPath> as a contiguous substring. Used by build-release.cmd to prove
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

$stubFull = (Resolve-Path -LiteralPath $StubPath).Path
$exeFull  = (Resolve-Path -LiteralPath $ExePath).Path

$stub = [System.IO.File]::ReadAllBytes($stubFull)
$exe  = [System.IO.File]::ReadAllBytes($exeFull)

$stubLen  = $stub.Length
$stubSha  = (Get-FileHash -LiteralPath $stubFull -Algorithm SHA256).Hash

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
$ms       = New-Object System.IO.MemoryStream(, $slice)
$sliceSha = (Get-FileHash -InputStream $ms -Algorithm SHA256).Hash
$ms.Dispose()

if ($sliceSha -ne $stubSha) {
    Write-Host "  FAIL: slice sha256 does not match stub sha256" -ForegroundColor Red
    Write-Host ("        stub sha256:  {0}" -f $stubSha)
    Write-Host ("        slice sha256: {0}" -f $sliceSha)
    exit 1
}

Write-Host ("  OK:   {0} contiguous bytes at offset 0x{1:X} of {2}" -f $stubLen, $offset, $exeFull)
Write-Host ("        stub sha256:  {0}" -f $stubSha)
exit 0
