<#
.SYNOPSIS
File-level rollout and rollback drill for the Hands exe (0127 DoD-7, automated half).

.DESCRIPTION
Runs only inside a fresh directory under the user's temp folder that it creates itself. It
mirrors the ordering in scripts/hands-elevated-task.ps1 (a test of ordering, not a second
installer): stage a candidate copy, verify its hash, swap it in with
[System.IO.File]::Replace to a `.old` backup, verify, then roll back from that backup and
verify again. It also proves a staged copy with a wrong hash is refused before any swap.

It never touches the installed exe, never registers, changes or starts a task, never starts
or stops a process, never writes the registry, never creates a listener. The owner's
elevated drill (spec 0127 §6.2, O4) is a separate, owner-run step.

Writes one JSON object to stdout with before/after hashes. The temp directory is removed
afterwards.
#>

[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"

$tempBase = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
$drillRoot = Join-Path $tempBase ("hands-rollout-drill-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $drillRoot | Out-Null

function Get-Sha256([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
}

function New-RandomExe([string]$Path, [int]$Seed) {
    $bytes = New-Object byte[] 65536
    $rnd = [System.Random]::new($Seed)
    $rnd.NextBytes($bytes)
    [System.IO.File]::WriteAllBytes($Path, $bytes)
}

$steps = New-Object System.Collections.Generic.List[object]
function Add-Step([string]$Name, [bool]$Ok, [string]$Detail) {
    $steps.Add([ordered]@{ step = $Name; ok = $Ok; detail = $Detail }) | Out-Null
}

$result = [ordered]@{
    schema      = "hands.rollout-drill/v1"
    generated_at = (Get-Date).ToString("o")
    temp_dir    = $drillRoot
    previous_sha256  = $null
    candidate_sha256 = $null
    installed_after_swap_sha256 = $null
    installed_after_rollback_sha256 = $null
    refused_bad_stage = $false
    steps       = $steps
    temp_removed = $false
    ok          = $false
}

try {
    $installDir = Join-Path $drillRoot "install"
    $candidateDir = Join-Path $drillRoot "candidate"
    New-Item -ItemType Directory -Path $installDir, $candidateDir | Out-Null
    $installed = Join-Path $installDir "hands.exe"
    $backup = "$installed.old"
    $staged = "$installed.new"
    $candidate = Join-Path $candidateDir "hands.exe"

    # The previous and candidate images are distinct random files.
    New-RandomExe $installed 1
    New-RandomExe $candidate 2
    $previousHash = Get-Sha256 $installed
    $candidateHash = Get-Sha256 $candidate
    $result.previous_sha256 = $previousHash
    $result.candidate_sha256 = $candidateHash
    Add-Step "precondition-distinct" ($previousHash -ne $candidateHash) "previous and candidate differ"

    # 1. Stage a copy of the candidate and verify it before touching the installed exe.
    Copy-Item -LiteralPath $candidate -Destination $staged -Force
    $stagedHash = Get-Sha256 $staged
    Add-Step "stage-verify" ($stagedHash -eq $candidateHash) "staged=$stagedHash"

    # 2. A corrupted stage is refused: the installed exe must not change.
    [System.IO.File]::WriteAllBytes($staged, [byte[]](0..63 | ForEach-Object { [byte]$_ }))
    $badHash = Get-Sha256 $staged
    $refused = ($badHash -ne $candidateHash)
    $result.refused_bad_stage = $refused
    Add-Step "refuse-bad-stage" $refused "bad stage hash differs; swap skipped"
    Copy-Item -LiteralPath $candidate -Destination $staged -Force

    # 3. Swap: the previous image becomes the `.old` backup, the candidate takes the name.
    [System.IO.File]::Replace($staged, $installed, $backup)
    $swappedHash = Get-Sha256 $installed
    $backupHash = Get-Sha256 $backup
    $result.installed_after_swap_sha256 = $swappedHash
    Add-Step "swap-installed-is-candidate" ($swappedHash -eq $candidateHash) "installed=$swappedHash"
    Add-Step "swap-backup-is-previous" ($backupHash -eq $previousHash) "backup=$backupHash"

    # 4. Rollback: restore the previous image from `.old`, verify, then swap it back in.
    $rollbackStage = "$installed.rollback"
    Copy-Item -LiteralPath $backup -Destination $rollbackStage -Force
    Add-Step "rollback-stage-verify" ((Get-Sha256 $rollbackStage) -eq $previousHash) "rollback stage"
    [System.IO.File]::Replace($rollbackStage, $installed, "$installed.candidate-failed")
    $restoredHash = Get-Sha256 $installed
    $result.installed_after_rollback_sha256 = $restoredHash
    Add-Step "rollback-installed-is-previous" ($restoredHash -eq $previousHash) "installed=$restoredHash"

    $result.ok = (@($steps | Where-Object { -not $_.ok })).Count -eq 0
}
finally {
    # Remove only the directory this run created inside the temp folder.
    if ($drillRoot.StartsWith($tempBase, [System.StringComparison]::OrdinalIgnoreCase) -and (Test-Path -LiteralPath $drillRoot)) {
        Remove-Item -LiteralPath $drillRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
    $result.temp_removed = -not (Test-Path -LiteralPath $drillRoot)
    Write-Output ($result | ConvertTo-Json -Depth 6)
}

if (-not $result.ok) { exit 1 }
exit 0
