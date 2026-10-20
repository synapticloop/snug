<#
.SYNOPSIS
A SHA-256 over the snug-launcher crate's source, independent of any toolchain.

.DESCRIPTION
Why this exists instead of hashing the built binary
---------------------------------------------------
The obvious check -- "does the committed stub equal a fresh build?" -- was
tried and it cannot work. It compares two *machines*: this repo's Windows
workstation and the `windows-latest` runner. On the first attempt they
disagreed on rustc (1.95.0 locally, 1.98.1 on the image, different LLVM
versions), and even with the compiler pinned the MSVC linker and Windows SDK
remain whatever each machine happens to have. Two independent environments
will never be byte-identical, so the check reported "out of date" on a
correctly refreshed stub -- which is worse than no check, because it cries
wolf.

Staleness, though, is a property of the *source*, not of the machine. So
hash the inputs. If the launcher source changes and nobody runs
refresh-stub.ps1, this fingerprint changes and CI notices. No compiler,
linker, SDK or checkout path is involved, so nothing outside the repository
can make it disagree.

What it does not prove: that the committed binary really was built from this
source. Someone could commit a stale stub and update the recorded fingerprint
by hand. That is a deliberate act, and a far weaker threat than the
accidental drift this replaces -- the mistake it actually prevents is
editing platform/windows.rs and not noticing the stub is now last release's
launcher.

Determinism notes:

  * Files are visited in ordinal order of their repo-relative path with
    forward slashes, so the result does not depend on directory enumeration
    order, which Windows does not guarantee.
  * CRLF is folded to LF before hashing. Git stores LF and this repo has
    core.autocrlf=true, so the working tree is CRLF locally but a checkout
    elsewhere may be LF. Without folding, identical source would fingerprint
    differently per machine -- the same trap as the binary comparison.
  * Per-file digests are combined into a final digest rather than streaming
    every byte through one hasher, because SHA256.ComputeHash is not
    incremental and Windows PowerShell 5.1 lacks IncrementalHash on older
    .NET Framework builds. Doubling is cheap here: the crate is a few MB.
#>

function Get-LauncherSourceFingerprint {
    param([Parameter(Mandatory = $true)][string]$Repo)

    $root = Join-Path $Repo 'crates\snug-launcher'
    if (-not (Test-Path $root)) {
        throw "no snug-launcher crate at $root"
    }

    # Extensions whose line endings are normalised below. Everything else --
    # notably assets\snug-icon.png, which build.rs embeds with
    # include_bytes! -- is hashed byte for byte, because folding CRLF inside a
    # PNG would quietly rewrite its contents and make the fingerprint depend
    # on a transformation that has no business touching binary data.
    $textExtensions = @('.rs', '.toml', '.md', '.txt', '.yml', '.yaml', '.json', '.xml', '.html', '.css', '.ps1', '.cmd')

    $entries = @(Get-ChildItem -LiteralPath $root -Recurse -File |
            ForEach-Object {
                $relative = $_.FullName.Substring($Repo.Length + 1).Replace('\', '/')
                [PSCustomObject]@{
                    Path = $relative
                    Full = $_.FullName
                    # A collation key that makes ordinary comparison ordinal:
                    # every character becomes a fixed-width hex field, so no
                    # culture rule (case folding, ignoring '-' or '_') can
                    # reorder two paths. Sort-Object's own ordinalness is not
                    # guaranteed across PowerShell versions or locales.
                    Key = -join ($relative.ToCharArray() | ForEach-Object { '{0:x4}' -f [int]$_ })
                }
            } |
            Sort-Object -Property Key)

    if ($entries.Count -eq 0) { throw "no source files found under $root" }

    $outer = [System.Security.Cryptography.SHA256]::Create()
    $inner = [System.Security.Cryptography.SHA256]::Create()
    try {
        foreach ($f in $entries) {
            $bytes = [System.IO.File]::ReadAllBytes($f.Full)
            $isText = $textExtensions -contains [System.IO.Path]::GetExtension($f.Path).ToLowerInvariant()
            if ($isText) {
                $normalised = [System.Collections.Generic.List[byte]]::new($bytes.Length)
                for ($i = 0; $i -lt $bytes.Length; $i++) {
                    # Fold CRLF to LF: skip the CR when a LF follows it.
                    if ($bytes[$i] -eq 0x0D -and ($i + 1) -lt $bytes.Length -and $bytes[$i + 1] -eq 0x0A) { continue }
                    $normalised.Add($bytes[$i])
                }
                $bytes = $normalised.ToArray()
            }
            $contentHash = $inner.ComputeHash($bytes)

            # Path + content hash. The path is length-prefixed so a rename
            # cannot collide with a content change of the same shape.
            $pathBytes = [System.Text.Encoding]::UTF8.GetBytes($f.Path)
            $row = [System.Collections.Generic.List[byte]]::new()
            $row.AddRange([System.BitConverter]::GetBytes([int]$pathBytes.Length))
            $row.AddRange($pathBytes)
            $row.AddRange($contentHash)

            $outer.TransformBlock($row.ToArray(), 0, $row.Count, $null, 0) | Out-Null
        }
        $outer.TransformFinalBlock([byte[]]@(), 0, 0) | Out-Null
        return ([System.BitConverter]::ToString($outer.Hash)).Replace('-', '')
    } finally {
        $outer.Dispose()
        $inner.Dispose()
    }
}