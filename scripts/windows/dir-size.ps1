<#
.SYNOPSIS
Report the size of a directory tree, in whole megabytes.

.DESCRIPTION
Used by scripts\windows\clean.cmd to report how much disk space a clean
reclaimed. Prints ONE integer on stdout and exits 0.

Two deliberate choices:

* A missing path is 0, not an error. "How big is this folder" has the answer
  0 for a folder that is not there, and the caller wants to print a number
  either way.

* Whole megabytes, not fractions. The caller does the subtraction with
  `set /a`, which is 32-bit and cannot hold the byte count of a 5 GB target\
  (5.4e9 overflows a signed 32-bit int). Megabytes fit comfortably, and
  `set /a` then owns the arithmetic with no float parsing anywhere.

The enumeration uses System.IO rather than Get-ChildItem -Recurse: on a
28,000-file target\ the cmdlet took ~5 s, and this takes well under one.
Get-ChildItem builds a FileInfo object per entry and then measures those
objects; DirectoryInfo.EnumerateFiles streams them instead.

$ErrorActionPreference is 'Continue' on purpose: an unreadable file should
not abort the measurement, it should just go uncounted. The number is a
rough guide for a human, not an audit.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$Path
)

$ErrorActionPreference = 'Continue'

if (-not (Test-Path -LiteralPath $Path)) {
    Write-Output '0'
    exit 0
}

$sum = 0L
$root = New-Object System.IO.DirectoryInfo $Path
foreach ($file in $root.EnumerateFiles('*', [System.IO.SearchOption]::AllDirectories)) {
    try { $sum += $file.Length } catch { }
}

Write-Output ([int][math]::Round($sum / 1MB))
