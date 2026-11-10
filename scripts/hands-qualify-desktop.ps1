<#
.SYNOPSIS
Deterministic desktop-backend qualification runner (0127 DoD-1, DoD-4).

.DESCRIPTION
Two modes, no others:

  gate    fmt --check, clippy -D warnings, `cargo test -- --test-threads=1`, and the Node
          extension harness. Writes gate.json with each step's exit code and the summed
          pass/fail/ignored counts from cargo's `test result:` lines.

  matrix  Runs only the automated cells of spec 0127 §7.1 (named cargo filters or whole test
          files, always serial) and writes matrix.json: one record per cell with
          {row, track, cell, status, evidence, live_cell}. Live owner cells are never run here.

Honesty rules (spec §1b): a row is never PASS from this runner. Every row that has a live owner
cell, or a residual, is PARTIAL at best. A row with no automated cell is UNTESTED. A cell
that cannot run in this process is UNTESTED with its reason. Q13/Q14 are read from git and
recorded N/A where they do not apply. Q01's squat/GENERIC_WRITE arms need a medium caller:
when elevation-status reports High (or cannot be read), they are written UNTESTED with the
reason, not passed by a silent skip.

The runner never registers or changes a task, never stops a process, never elevates, never
sends input and never creates a listener. The only process it starts is cargo / node / and
the read-only `hands.exe elevation-status`.

.PARAMETER Mode
gate | matrix.

.PARAMETER Out
Evidence directory. Created if missing. gate.json or matrix.json is written here.

.PARAMETER RepoRoot
Product checkout. Default: the parent of this script's folder.

.PARAMETER ElevationOverride
Test seam only: `medium` or `high` replaces the elevation-status read.

.PARAMETER PlanOnly
Matrix only: skip the cargo runs and record each cell as PLANNED. Used by the locking test.

.PARAMETER Rows
Matrix only: restrict to these rows (for example Q01).
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet("gate", "matrix")]
    [string]$Mode,
    [Parameter(Mandatory = $true)]
    [string]$Out,
    [string]$RepoRoot,
    [ValidateSet("medium", "high")]
    [string]$ElevationOverride,
    [switch]$PlanOnly,
    [string[]]$Rows
)

$ErrorActionPreference = "Continue"

if (-not $RepoRoot) { $RepoRoot = Join-Path (Split-Path -Parent $MyInvocation.MyCommand.Path) ".." }
$RepoRoot = [System.IO.Path]::GetFullPath($RepoRoot)
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$OutDir = [System.IO.Path]::GetFullPath($Out)

function Write-Json([string]$Path, $Object) {
    $json = $Object | ConvertTo-Json -Depth 10
    [System.IO.File]::WriteAllText($Path, $json, [System.Text.UTF8Encoding]::new($false))
}

function Invoke-Native([string]$Exe, [string[]]$Arguments) {
    Push-Location $RepoRoot
    try {
        $lines = & $Exe @Arguments 2>&1 | ForEach-Object { $_.ToString() }
        $code = $LASTEXITCODE
    } finally {
        Pop-Location
    }
    return [pscustomobject]@{ Code = $code; Lines = @($lines) }
}

function Get-TestCounts([string[]]$Lines) {
    $passed = 0; $failed = 0; $ignored = 0
    foreach ($line in $Lines) {
        if ($line -match '^test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored') {
            $passed += [int]$Matches[1]
            $failed += [int]$Matches[2]
            $ignored += [int]$Matches[3]
        }
    }
    return [pscustomobject]@{ Passed = $passed; Failed = $failed; Ignored = $ignored }
}

function Get-GitRev() {
    Push-Location $RepoRoot
    try {
        $rev = (& git rev-parse --short=12 HEAD 2>$null)
        $status = (& git status --porcelain 2>$null)
    } finally {
        Pop-Location
    }
    return [ordered]@{ rev = [string]$rev; dirty = [bool]$status }
}

function Invoke-Gate() {
    $steps = @(
        @{ name = "fmt";    exe = "cargo"; args = @("fmt", "--check") },
        @{ name = "clippy"; exe = "cargo"; args = @("clippy", "--all-targets", "--", "-D", "warnings") },
        @{ name = "test";   exe = "cargo"; args = @("test", "--", "--test-threads=1") },
        @{ name = "node-extension-harness"; exe = "node"; args = @("extension\tests\harness.js") }
    )
    $results = @()
    foreach ($step in $steps) {
        $run = Invoke-Native $step.exe $step.args
        $counts = Get-TestCounts $run.Lines
        $results += [ordered]@{
            name      = $step.name
            command   = (@($step.exe) + $step.args) -join " "
            exit_code = $run.Code
            passed    = $counts.Passed
            failed    = $counts.Failed
            ignored   = $counts.Ignored
            pass      = ($run.Code -eq 0)
        }
        Write-Host ("{0}: exit {1}" -f $step.name, $run.Code)
    }
    $toolchain = [ordered]@{
        rustc = ((Invoke-Native "rustc" @("-V")).Lines | Select-Object -First 1)
        node  = ((Invoke-Native "node" @("--version")).Lines | Select-Object -First 1)
    }
    $overall = ($results | Where-Object { -not $_.pass }).Count -eq 0
    $gate = [ordered]@{
        schema       = "hands.qualify-gate/v1"
        generated_at = (Get-Date).ToString("o")
        git          = Get-GitRev
        toolchain    = $toolchain
        test_threads = 1
        steps        = $results
        overall_pass = $overall
    }
    Write-Json (Join-Path $OutDir "gate.json") $gate
    return $overall
}

function Get-Elevation() {
    if ($ElevationOverride) {
        return [ordered]@{ source = "override"; privileged = ($ElevationOverride -eq "high"); integrity = $ElevationOverride }
    }
    $exe = Join-Path $RepoRoot "target\debug\hands.exe"
    if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
        return [ordered]@{ source = "unavailable"; privileged = $null; integrity = $null }
    }
    $run = Invoke-Native $exe @("elevation-status")
    $json = ($run.Lines | Where-Object { $_.StartsWith("{") } | Select-Object -First 1)
    if ($run.Code -ne 0 -or -not $json) {
        return [ordered]@{ source = "unreadable"; privileged = $null; integrity = $null }
    }
    $obj = $json | ConvertFrom-Json
    return [ordered]@{ source = "elevation-status"; privileged = [bool]$obj.privileged; integrity = [string]$obj.integrity }
}

# Automated cells per spec 0127 §7.1. `cargo` holds the argument list passed to `cargo test`.
# `dynamic` is a cell that runs only at a medium caller; at High it is written UNTESTED.
$matrixRows = @(
    @{ row = "Q01"; track = "0120"; live = "0126 O1-O12 (NOT_RUN)";
       cells = @(
           @{ name = "bridge squat/instance/desk/disconnect (elevation_cli)"; cargo = @("--test", "elevation_cli") },
           @{ name = "bridge never becomes a desk"; cargo = @("--lib", "bridge_never_becomes_a_desk") },
           @{ name = "bridge pipe up, mutex free"; cargo = @("--lib", "bridge_pipe_up_mutex_free_says_no_desk_driver") },
           @{ name = "bridge pipe up, mutex denied"; cargo = @("--lib", "bridge_pipe_up_mutex_denied_does_not_connect") }
       );
       dynamic = @{ name = "restricted pipe / non-admin cannot add instances"; cargo = @("--lib", "bridge_client_opens_a_restricted_pipe_and_a_non_admin_cannot_add_instances") } },
    @{ row = "Q02"; track = "0120/0126"; live = "S6 with a medium caller (O6)";
       cells = @(
           @{ name = "second writer stop aborts in-flight type"; cargo = @("--lib", "stop_request_from_second_writer_aborts_in_flight_type") },
           @{ name = "stop older than TTL does not freeze"; cargo = @("--lib", "stop_request_older_than_ttl_does_not_freeze_a_later_action") }
       ) },
    @{ row = "Q03"; track = "0120"; live = "O4 (NOT_RUN)";
       cells = @( @{ name = "elevated task script"; cargo = @("--test", "elevated_task_script") } ) },
    @{ row = "Q04"; track = "0126"; live = "O6 console/terminal/stop/lifecycle (NOT_RUN)";
       cells = @( @{ name = "elevated qualify harness"; cargo = @("--test", "elevated_qualify_script") } ) },
    @{ row = "Q05"; track = "0122"; live = "tests/uia_action_live.rs (7 ignored; O6 scratch)";
       cells = @( @{ name = "uia-action CLI"; cargo = @("--test", "uia_action_cli") } ) },
    @{ row = "Q06"; track = "0123"; live = "0123 H1-H6 (UNTESTED)";
       cells = @(
           @{ name = "sref1 identity grammar and binding"; cargo = @("--lib", "identity::") },
           @{ name = "find ref fixtures"; cargo = @("--lib", "find::") }
       ) },
    @{ row = "Q07"; track = "0124"; live = "0124 H2/H3 (UNTESTED)";
       cells = @(
           @{ name = "outcome truth"; cargo = @("--lib", "outcome::") },
           @{ name = "postcondition failure aborts step"; cargo = @("--lib", "postcondition_failed_aborts_step") }
       ) },
    @{ row = "Q08"; track = "0125"; live = "0125 H1-H7 (UNTESTED)";
       cells = @( @{ name = "scoped find CLI"; cargo = @("--test", "find_cli") } ) },
    @{ row = "Q09"; track = "0128"; live = "0128 H1-H6 (UNTESTED)";
       cells = @( @{ name = "DOM hit-test fixtures"; cargo = @("--test", "hittest_cli") } ) },
    @{ row = "Q10"; track = "0129"; live = "0129 owner cells (UNTESTED)";
       cells = @( @{ name = "CSS-to-physical transform fixtures"; cargo = @("--test", "transform_cli") } ) },
    @{ row = "Q11"; track = "0130"; live = "0130 H1-H8 (O3 needed)";
       cells = @(
           @{ name = "changes CLI"; cargo = @("--test", "changes_cli") },
           @{ name = "extension harness (Node)"; cargo = @("--test", "extension_harness") }
       ) },
    @{ row = "Q12"; track = "0131"; live = "0131 H2-H8 (O3 needed); end-to-end observe() frame test is a named residual";
       cells = @( @{ name = "frames unit tests"; cargo = @("--lib", "frames::") } ) },
    @{ row = "Q13"; track = "0132"; live = "none (validator only)"; git_gate = "track(0132)";
       cells = @( @{ name = "site skill references validator"; cargo = @("--test", "site_skills") } ) },
    @{ row = "Q14"; track = "0133"; live = "O2 (default decline)"; na = "owner-gated spike; default decline (O2)"; cells = @() },
    @{ row = "Q15"; track = "native host doctor"; live = "R5 (NOT_RUN)";
       cells = @( @{ name = "host doctor unit tests"; cargo = @("--lib", "host_doctor::") } ) },
    @{ row = "Q16"; track = "0127 DoD-2/3"; live = "R1/R5 (probe post-state NOT_RUN)";
       cells = @( @{ name = "build identity (CLI and MCP)"; cargo = @("--test", "build_identity") } ) },
    @{ row = "Q17"; track = "0120/0126"; live = "S6 with two processes (O6)";
       cells = @(
           @{ name = "second driver refused while mutex held"; cargo = @("--lib", "labelled_mutex_second_create_is_ok_with_already_exists") },
           @{ name = "mutex name constant"; cargo = @("--lib", "mutex_name_constant") }
       ) },
    @{ row = "Q18"; track = "gate"; live = "residual: 0120 lock-order inversion (serial gate only)";
       cells = @(); na = "covered by -Mode gate (gate.json); not a matrix cell" },
    @{ row = "Q19"; track = "0126"; live = "R5 (NOT_RUN)";
       cells = @( @{ name = "elevation status tests"; cargo = @("--lib", "elevation::") } ) }
)

if ($Rows) {
    $matrixRows = @($matrixRows | Where-Object { $Rows -contains $_.row })
}

function Invoke-Matrix() {
    $elevation = Get-Elevation
    $isMedium = ($elevation.source -ne "unavailable" -and $elevation.source -ne "unreadable" -and $elevation.privileged -eq $false)
    $records = @()
    foreach ($row in $matrixRows) {
        if ($row.ContainsKey("na")) {
            $records += [ordered]@{ row = $row.row; track = $row.track; cell = "row"; status = "N/A"; evidence = $row.na; live_cell = $row.live }
            continue
        }
        if ($row.ContainsKey("git_gate")) {
            Push-Location $RepoRoot
            try { $found = [bool](& git log --oneline -F --grep $row.git_gate 2>$null) } finally { Pop-Location }
            if (-not $found) {
                $records += [ordered]@{ row = $row.row; track = $row.track; cell = "row"; status = "N/A"; evidence = "no $($row.git_gate) commit in git at this head"; live_cell = $row.live }
                continue
            }
        }
        foreach ($cell in $row.cells) {
            $cmd = "cargo test " + ($cell.cargo -join " ") + " -- --test-threads=1"
            if ($PlanOnly) {
                $records += [ordered]@{ row = $row.row; track = $row.track; cell = $cell.name; status = "PLANNED"; evidence = $cmd; live_cell = $row.live }
                continue
            }
            $run = Invoke-Native "cargo" (@("test") + $cell.cargo + @("--", "--test-threads=1"))
            $counts = Get-TestCounts $run.Lines
            if ($run.Code -ne 0 -or $counts.Failed -gt 0) { $status = "FAILED" }
            elseif ($counts.Passed -eq 0) { $status = "UNTESTED" }
            else { $status = "PARTIAL" }
            $records += [ordered]@{ row = $row.row; track = $row.track; cell = $cell.name; status = $status;
                evidence = "$cmd => exit $($run.Code), $($counts.Passed) passed, $($counts.Failed) failed, $($counts.Ignored) ignored";
                live_cell = $row.live }
        }
        if ($row.ContainsKey("dynamic")) {
            $dyn = $row.dynamic
            if ($isMedium -and -not $PlanOnly) {
                $cmd = "cargo test " + ($dyn.cargo -join " ") + " -- --test-threads=1"
                $run = Invoke-Native "cargo" (@("test") + $dyn.cargo + @("--", "--test-threads=1"))
                $counts = Get-TestCounts $run.Lines
                $status = if ($run.Code -ne 0 -or $counts.Failed -gt 0) { "FAILED" } elseif ($counts.Passed -eq 0) { "UNTESTED" } else { "PARTIAL" }
                $records += [ordered]@{ row = $row.row; track = $row.track; cell = $dyn.name; status = $status;
                    evidence = "$cmd => exit $($run.Code), $($counts.Passed) passed, $($counts.Failed) failed"; live_cell = $row.live }
            } elseif ($PlanOnly -and $isMedium) {
                $records += [ordered]@{ row = $row.row; track = $row.track; cell = $dyn.name; status = "PLANNED"; evidence = "cargo test " + ($dyn.cargo -join " "); live_cell = $row.live }
            } else {
                $reason = "High IL: squat/GENERIC_WRITE arms need a medium caller"
                if (-not $isMedium -and $elevation.source -ne "override") { $reason = "$reason (elevation-status $($elevation.source))" }
                $records += [ordered]@{ row = $row.row; track = $row.track; cell = $dyn.name; status = "UNTESTED"; evidence = "reason: $reason; elevation source=$($elevation.source) privileged=$($elevation.privileged)"; live_cell = $row.live }
            }
        }
    }
    $matrix = [ordered]@{
        schema       = "hands.qualify-matrix/v1"
        generated_at = (Get-Date).ToString("o")
        git          = Get-GitRev
        plan_only    = [bool]$PlanOnly
        elevation    = $elevation
        records      = $records
    }
    Write-Json (Join-Path $OutDir "matrix.json") $matrix
    return $true
}

if ($Mode -eq "gate") {
    $ok = Invoke-Gate
    if ($ok) { exit 0 } else { exit 1 }
} else {
    $null = Invoke-Matrix
    exit 0
}
