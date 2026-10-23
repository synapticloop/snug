<#
.SYNOPSIS
Run the same gates CI runs, in one command.

.DESCRIPTION
`.github/workflows/windows.yml` gates on a Clippy step
(`cargo clippy --workspace --all-targets -- -D warnings`) and a full test
run. Both are fine, and both are easy to forget locally -- `cargo check`
and `cargo test` neither run clippy's lint set nor fail on it, so a change
can be locally clean and still red on Actions.

That is not hypothetical. Six `assertions_on_constants` errors sat on main
for a while, invisible because the one `op_ref` error in
`snug-launcher`'s *lib* failed first, so clippy stopped before it reached
the `snug_preview` bin holding the other five. One error at a time, and
the rest stay hidden until the first is fixed.

This script exists so the local loop and the CI gate are the same shape.
It is not part of the release path: `build.cmd`, `build-macos.sh`
and `refresh-stub.ps1` deliberately do not call it, because those build a
reproducible artefact and a lint step does not belong in "the command that
produces the committed stub".

.PARAMETER Format
Actually reformat with `cargo fmt --all` instead of only reporting. Use it
before committing a change, not after: it rewrites every file that is not
already clean, and any `snug-launcher` file it touches moves
`bin/launcher-stub.source`, so follow it with `refresh-stub.ps1`.

.OUTPUTS
Per-gate pass/fail lines, then a summary. Exit code is 0 when every
*enforced* gate passed, 1 otherwise.

.NOTES
Formatting *is* enforced. The tree is clean, so a `cargo fmt --all
--check` failure from here on means a real change went in unformatted --
there is no inherited state for it to be confused with.

`--no-fail-fast` on the test run matches CI: plain `cargo test` stops at
the first failing binary, which is exactly the failure mode described
above, one level down.

Each gate decides pass/fail purely from `$LASTEXITCODE`, and
`$ErrorActionPreference` is 'Continue' so that a *caller* capturing
output cannot kill the script. See the note beside the assignment below
for the failure that motivated it.
#>
[CmdletBinding()]
param([switch]$Format)

# Deliberately 'Continue', not 'Stop'. Every gate below decides pass/fail
# from `$LASTEXITCODE` and never from exception state, so nothing is lost
# -- and this way the script survives a caller that redirects output.
#
# Under 'Stop', a caller doing `.\scripts\windows\check.ps1 *>&1 | Tee-Object`
# turns cargo's ordinary "Finished ..." progress line on stderr into a
# NativeCommandError record, which terminates the script at whichever
# gate happens to emit one. That is not hypothetical: it is exactly how
# this script first failed. A check script that dies when its output is
# captured is worse than no check script, because it looks like a pass.
$ErrorActionPreference = 'Continue'
# $PSScriptRoot = scripts/windows/; the parent is scripts/, and one more
# level up is the repo root. Two Split-Path -Parent calls land where cargo
# expects to run from.
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
Push-Location $repo
try {
    $results = [System.Collections.Generic.List[object]]::new()

    function Add-Gate {
        param([string]$Name, [int]$Code, [bool]$Enforced)
        $results.Add([pscustomobject]@{
            Gate     = $Name
            Code     = $Code
            Enforced = $Enforced
            Result   = if ($Code -eq 0) { 'pass' } else { 'FAIL' }
        })
    }

    # --- Gate 1: formatting -----------------------------------------------
    if ($Format) {
        Write-Host '==> cargo fmt --all (rewriting)' -ForegroundColor Cyan
        cargo fmt --all
        Add-Gate -Name 'cargo fmt --all' -Code $LASTEXITCODE -Enforced $true
    } else {
        Write-Host '==> cargo fmt --all --check' -ForegroundColor Cyan
        cargo fmt --all --check
        $fmt = $LASTEXITCODE
        if ($fmt -ne 0) {
            Write-Host ''
            Write-Host '    The tree is not rustfmt-clean. Run' -ForegroundColor Yellow
            Write-Host '        .\scripts\windows\check.ps1 -Format' -ForegroundColor Yellow
            Write-Host '    to reformat, then commit the result alongside whatever changed it.' -ForegroundColor Yellow
            Write-Host '    Formatting moved the launcher sources, so refresh the stub too:' -ForegroundColor Yellow
            Write-Host '        .\scripts\windows\refresh-stub.ps1' -ForegroundColor Yellow
            Write-Host ''
        }
        Add-Gate -Name 'cargo fmt --all --check' -Code $fmt -Enforced $true
    }

    # --- Gate 2: clippy --------------------------------------------------
    Write-Host '==> cargo clippy --workspace --all-targets -- -D warnings' -ForegroundColor Cyan
    cargo clippy --workspace --all-targets -- -D warnings
    Add-Gate -Name 'cargo clippy -D warnings' -Code $LASTEXITCODE -Enforced $true

    # --- Gate 3: tests ---------------------------------------------------
    Write-Host '==> cargo test --workspace --lib --bins --tests --no-fail-fast' -ForegroundColor Cyan
    cargo test --workspace --lib --bins --tests --no-fail-fast
    Add-Gate -Name 'cargo test --no-fail-fast' -Code $LASTEXITCODE -Enforced $true

    # --- Summary ---------------------------------------------------------
    Write-Host ''
    Write-Host '==================== summary ====================' -ForegroundColor Cyan
    foreach ($r in $results) {
        $colour = if ($r.Code -eq 0) { 'Green' } else { 'Red' }
        $tag = if ($r.Enforced) { 'gate   ' } else { 'advisory' }
        Write-Host ("  {0}  {1}  (exit {2})" -f $tag, $r.Gate.PadRight(44), $r.Code) -ForegroundColor $colour
    }

    $failed = @($results | Where-Object { $_.Enforced -and $_.Code -ne 0 })
    Write-Host ''
    if ($failed.Count -eq 0) {
        Write-Host 'all enforced gates passed' -ForegroundColor Green
        exit 0
    }
    Write-Host ("{0} enforced gate(s) failed: {1}" -f $failed.Count, (($failed | ForEach-Object { $_.Gate }) -join ', ')) -ForegroundColor Red
    exit 1
}
finally {
    Pop-Location
}