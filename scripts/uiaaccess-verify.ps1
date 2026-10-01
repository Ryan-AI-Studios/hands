# Owner HITL verify: installed UIAccess exe must type a nonce into an elevated window.
# Agent does not raise UAC. DoD-4 is observed effect, not ok:true.

[CmdletBinding()]
param(
    [string]$Installed = (Join-Path $env:ProgramFiles 'HelpingHands\hands.exe'),
    [string]$Nonce = $('hh0119-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
)

$ErrorActionPreference = 'Stop'
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

Write-Host "Focus an elevated edit (elevated Notepad / PowerShell). Typing nonce: $Nonce"
& $Installed type --text $Nonce
if ($LASTEXITCODE -ne 0) {
    throw "type refused or failed; High-IL still ungranted or fence-blocked"
}
Write-Host "Read back the nonce in the elevated window (or a file that process wrote)."
Write-Host "DoD-4 is that observed effect. ok:true alone is not enough."
Write-Host "NONCE=$Nonce"
