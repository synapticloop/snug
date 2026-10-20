function Get-PeStableHash {
    param([Parameter(Mandatory)][string]$Path)

    $bytes = [System.IO.File]::ReadAllBytes($Path)
    if ($bytes.Length -lt 0x40 -or $bytes[0] -ne 0x4D -or $bytes[1] -ne 0x5A) {
        throw "$Path is not a PE image (no MZ signature)"
    }
    $pe = [BitConverter]::ToInt32($bytes, 0x3C)
    if ([BitConverter]::ToUInt32($bytes, $pe) -ne 0x00004550) {
        throw "$Path has no PE signature"
    }

    # 1. IMAGE_FILE_HEADER.TimeDateStamp. MSVC stamps wall-clock link time
    #    here, so two links of byte-identical source differ.
    [Array]::Clear($bytes, $pe + 8, 4)

    # 2. IMAGE_OPTIONAL_HEADER.CheckSum - a function of the whole image,
    #    which includes the timestamp above.
    [Array]::Clear($bytes, $pe + 88, 4)

    # 3. The debug *directory* (the table of 28-byte IMAGE_DEBUG_DIRECTORY
    #    records). Each record carries its own copy of the link timestamp
    #    and a PointerToRawData into .rdata. Mapped RVA -> file offset via
    #    the section table, since PE addresses are RVAs, not offsets.
    $isPe32Plus = [BitConverter]::ToUInt16($bytes, $pe + 24) -eq 0x20B
    $dataDir = $pe + 24 + $(if ($isPe32Plus) { 112 } else { 96 })
    $debugRva = [BitConverter]::ToUInt32($bytes, $dataDir + 6 * 8)
    $debugSize = [BitConverter]::ToUInt32($bytes, $dataDir + 6 * 8 + 4)

    if ($debugRva -ne 0 -and $debugSize -gt 0) {
        $optionalSize = [BitConverter]::ToUInt16($bytes, $pe + 20)
        $sectionCount = [BitConverter]::ToUInt16($bytes, $pe + 6)
        $sectionTable = $pe + 24 + $optionalSize
        for ($i = 0; $i -lt $sectionCount; $i++) {
            $section = $sectionTable + $i * 40
            $virtualSize = [BitConverter]::ToUInt32($bytes, $section + 8)
            $virtualAddress = [BitConverter]::ToUInt32($bytes, $section + 12)
            if ($debugRva -ge $virtualAddress -and $debugRva -lt ($virtualAddress + $virtualSize)) {
                $rawPointer = [BitConverter]::ToUInt32($bytes, $section + 20)
                $delta = $debugRva - $virtualAddress
                $span = [Math]::Min([int64]$debugSize, [int64]($virtualSize - $delta))
                if ($span -gt 0) { [Array]::Clear($bytes, $rawPointer + $delta, $span) }
                break
            }
        }
    }

    # 4. The debug *data*: an RSDS record holding a GUID that is fresh random
    #    per link and the absolute PDB path of whoever built it. The GUID
    #    makes two links of identical source differ; the path means a build
    #    from CI can never match a stub committed from a workstation, so
    #    both have to go.
    #
    #    Located by signature rather than by following the directory's
    #    PointerToRawData: 'RSDS' is unambiguous and needs no PE walking.
    for ($i = 0; $i -lt $bytes.Length - 4; $i++) {
        if ($bytes[$i] -ne 0x52 -or $bytes[$i + 1] -ne 0x53 -or
            $bytes[$i + 2] -ne 0x44 -or $bytes[$i + 3] -ne 0x53) { continue }
        # 'RSDS' + GUID(16) + Age(4) + NUL-terminated PDB path.
        $end = $i + 4
        while ($end -lt $bytes.Length - 1 -and $bytes[$end] -ne 0) { $end++ }
        [Array]::Clear($bytes, $i + 4, [Math]::Min($end - $i - 3, $bytes.Length - $i - 4))
        $i = $end
    }

    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        return ([BitConverter]::ToString($sha.ComputeHash($bytes)) -replace '-', '')
    } finally {
        $sha.Dispose()
    }
}