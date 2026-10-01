# UIAccess provision for Hands.
# Admin (DoD-4): Root + Program Files. Non-admin (DoD-3): CurrentUser cert + writable signed copy.
# Reuses CN=HelpingHands UIAccess. Does not rewrite native-host JSON.
# One-line owner RunAs:
# Start-Process powershell -Verb RunAs -ArgumentList '-NoProfile -ExecutionPolicy Bypass -File C:\dev\Helping-Hands\hands\scripts\uiaaccess-provision.ps1'

[CmdletBinding()]
param(
    [switch]$NonAdmin
)

$ErrorActionPreference = 'Stop'
$Subject = 'CN=HelpingHands UIAccess'
$RepoRoot = Split-Path -Parent $PSScriptRoot
$InstallDir = Join-Path $env:ProgramFiles 'HelpingHands'
$InstallExe = Join-Path $InstallDir 'hands.exe'
$FeatureDir = Join-Path $RepoRoot 'target\uiaccess'

function Test-IsAdmin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($id)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Get-UiAccessCert {
    Get-ChildItem Cert:\CurrentUser\My -ErrorAction SilentlyContinue |
        Where-Object { $_.Subject -eq $Subject -and $_.HasPrivateKey } |
        Select-Object -First 1
}

function New-OrReuseUiAccessCert {
    $existing = Get-UiAccessCert
    if ($existing) {
        Write-Host "Reusing cert $($existing.Thumbprint) ($Subject)"
        return $existing
    }
    New-SelfSignedCertificate -Type CodeSigningCert -Subject $Subject `
        -CertStoreLocation Cert:\CurrentUser\My `
        -NotAfter (Get-Date).AddYears(10)
}

function Build-UiAccessExe {
    $env:CARGO_TARGET_DIR = $FeatureDir
    Push-Location $RepoRoot
    try {
        cargo build --release --features uiaccess
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build --features uiaccess failed"
        }
    } finally {
        Pop-Location
    }
    $built = Join-Path $FeatureDir 'release\hands.exe'
    if (-not (Test-Path $built)) {
        throw "missing feature PE $built"
    }
    Write-Host "Feature PE $built is unsigned; do not execute it (740 / ERROR_ELEVATION_REQUIRED). Sign via this script into Program Files."
    return $built
}

function Sign-Copy([string]$Source, [string]$Dest, $Cert) {
    $destDir = Split-Path -Parent $Dest
    New-Item -ItemType Directory -Force -Path $destDir | Out-Null
    Copy-Item -LiteralPath $Source -Destination $Dest -Force
    Set-AuthenticodeSignature -FilePath $Dest -Certificate $Cert | Out-Null
    Get-AuthenticodeSignature -FilePath $Dest | Format-List
}

$admin = Test-IsAdmin
if ($NonAdmin -or -not $admin) {
    if (-not $admin) {
        Write-Host "Not elevated. Non-admin path: CurrentUser cert + sign a writable copy."
        Write-Host "No Program Files write. No LocalMachine\Root."
        Write-Host "Owner HITL (DoD-4) one-liner:"
        Write-Host "Start-Process powershell -Verb RunAs -ArgumentList '-NoProfile -ExecutionPolicy Bypass -File $PSCommandPath'"
    }
    $cert = New-OrReuseUiAccessCert
    $built = Build-UiAccessExe
    $copy = Join-Path $RepoRoot 'target\uiaccess-nonadmin\hands.exe'
    Sign-Copy $built $copy $cert
    Write-Host "Non-admin signed copy: $copy (not a secure location; grant stays false)."
    Write-Host "Point harness MCP/CLI at the Program Files exe after owner RunAs provision."
    Write-Host "Do not rewrite native-host JSON. Launch the installed exe via Start-Process / ShellExecute (CreateProcess can return 740)."
    return
}

$cert = New-OrReuseUiAccessCert
$root = Get-ChildItem Cert:\LocalMachine\Root -ErrorAction SilentlyContinue |
    Where-Object { $_.Thumbprint -eq $cert.Thumbprint } |
    Select-Object -First 1
if (-not $root) {
    $store = New-Object System.Security.Cryptography.X509Certificates.X509Store('Root', 'LocalMachine')
    $store.Open('ReadWrite')
    $store.Add($cert)
    $store.Close()
    Write-Host "Installed $Subject into LocalMachine\Root ($($cert.Thumbprint))"
} else {
    Write-Host "Root already trusts $($cert.Thumbprint)"
}

$built = Build-UiAccessExe
Sign-Copy $built $InstallExe $cert
Write-Host "Installed: $InstallExe"
Write-Host "Point harness MCP/CLI at this exe for elevated input. Launch via Start-Process / ShellExecute."
Write-Host "Do not rewrite native-host JSON (Chrome CreateProcess of a UIAccess PE can return 740)."
Write-Host "Then from a NORMAL non-elevated console (not this RunAs shell):"
Write-Host "  $InstallExe elevation-status"
Write-Host "  scripts\uiaaccess-verify.ps1"
Write-Host "asInvoker inherits High IL from an elevated parent; that is a contract abort, not Done."
