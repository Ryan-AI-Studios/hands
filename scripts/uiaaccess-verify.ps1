# Owner HITL verify: installed UIAccess exe must type a nonce and click an elevated control.
# Run from a NORMAL (non-elevated) console. Agent does not raise UAC.
# DoD-4 is observed effect, not ok:true.

[CmdletBinding()]
param(
    [string]$Installed = (Join-Path $env:ProgramFiles 'HelpingHands\hands.exe'),
    [string]$Nonce = $('hh0119-' + [guid]::NewGuid().ToString('N').Substring(0, 8)),
    [string]$SessionId = $('s-0119-' + [guid]::NewGuid().ToString('N').Substring(0, 8)),
    [int]$ClickX = 0,
    [int]$ClickY = 0
)

$ErrorActionPreference = 'Stop'

function Test-IsAdmin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($id)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

if (Test-IsAdmin) {
    throw "run this script from a normal non-elevated console; asInvoker inherits High IL from an elevated parent"
}

if (-not (Test-Path -LiteralPath $Installed)) {
    throw "missing $Installed; run scripts\uiaaccess-provision.ps1 elevated"
}

$statusJson = & $Installed elevation-status
if ($LASTEXITCODE -ne 0) {
    throw "elevation-status failed: $statusJson"
}
$status = $statusJson | ConvertFrom-Json
Write-Host $statusJson
if (-not $status.uiaccess_granted) {
    throw "uiaccess_granted is false on installed copy"
}
if (-not $status.secure_location) {
    throw "secure_location is false on installed copy"
}
if ([int]$status.integrity_rid -ge 0x3000) {
    throw "installed process is High IL ($($status.integrity_rid)); stop; do not claim Done"
}

Write-Host "Allowing elevated on desktop for session $SessionId"
& $Installed confirm --domain desktop --category elevated --mode once --session-id $SessionId
if ($LASTEXITCODE -ne 0) {
    throw "confirm elevated/desktop failed for session $SessionId"
}

Write-Host "Focus an elevated edit (elevated Notepad / PowerShell). Typing nonce: $Nonce"
& $Installed type --text $Nonce --session-id $SessionId
if ($LASTEXITCODE -ne 0) {
    throw "type refused or failed; High-IL still ungranted or fence-blocked"
}

if ($ClickX -ne 0 -or $ClickY -ne 0) {
    Write-Host "Clicking elevated control at $ClickX,$ClickY"
    $clickOut = & $Installed click --x $ClickX --y $ClickY --session-id $SessionId 2>&1 | Out-String
    Write-Host $clickOut
    if ($clickOut -match 'ElementFromPoint' -or $clickOut -match '0x80070005') {
        throw "click returned raw ElementFromPoint / 0x80070005"
    }
} else {
    Write-Host "Pass -ClickX and -ClickY for the elevated-control click (DoD-4 click half)."
}

Write-Host "Read back the nonce in the elevated window (owner visual check)."
Write-Host "DoD-4 is that observed effect. ok:true alone is not enough."
Write-Host "SESSION=$SessionId"
Write-Host "NONCE=$Nonce"
