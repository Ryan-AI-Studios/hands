<#
.SYNOPSIS
Install hands.exe to an admin-only location and register the on-demand HandsElevated task.

.DESCRIPTION
Registers a Highest-privilege (RunLevel=Highest) scheduled task that runs
`hands.exe serve --pipe \\.\pipe\hands-elev` at High IL for the current user.
The default is ON-DEMAND (no trigger). -Trigger AtLogOn is an explicit, louder opt-in.

The task action is a COPY of -Exe placed under -InstallDir (default %ProgramFiles%\Hands).
A Highest task whose executable a medium-IL process can overwrite is a UAC bypass, so the
script refuses to register unless the installed exe, its owner, and every ancestor directory
are writable only by SYSTEM / Administrators / TrustedInstaller.

Run from an elevated shell. The script does not raise UAC and never launches an elevated child.

.PARAMETER Exe
Required. hands.exe to install (resolved to an absolute path before use).

.PARAMETER Trigger
"OnDemand" (default, zero triggers) or "AtLogOn".

.PARAMETER InstallDir
Admin-only directory that receives hands.exe. Default: $env:ProgramFiles\Hands.

.PARAMETER Inspect
Print the task definition that would be registered (JSON) and exit. Needs no elevation,
copies nothing, registers nothing.

.EXAMPLE
powershell -File hands-elevated-task.ps1 -Exe C:\dev\Helping-Hands\hands\target\release\hands.exe

.EXAMPLE
powershell -File hands-elevated-task.ps1 -Exe .\hands.exe -Inspect
#>

param(
    [Parameter(Mandatory = $true)]
    [string]$Exe,

    [ValidateSet("OnDemand", "AtLogOn")]
    [string]$Trigger = "OnDemand",

    [string]$InstallDir = (Join-Path $env:ProgramFiles "Hands"),

    [switch]$Inspect
)

$ErrorActionPreference = "Stop"
$taskName = "HandsElevated"
$pipeArgs = 'serve --pipe \\.\pipe\hands-elev'

function Fail([string]$Message) {
    [Console]::Error.WriteLine($Message)
    exit 1
}

# SIDs allowed to hold write-class rights on the installed exe and its ancestors.
$trustedSids = @(
    "S-1-5-18",      # SYSTEM
    "S-1-5-32-544",  # BUILTIN\Administrators
    "S-1-3-0"        # CREATOR OWNER (only meaningful as an inherit-only template)
)
$trustedNames = @("NT SERVICE\TrustedInstaller")
$fileWriteRights = [System.Security.AccessControl.FileSystemRights]::WriteData -bor `
    [System.Security.AccessControl.FileSystemRights]::AppendData -bor `
    [System.Security.AccessControl.FileSystemRights]::Delete -bor `
    [System.Security.AccessControl.FileSystemRights]::ChangePermissions -bor `
    [System.Security.AccessControl.FileSystemRights]::TakeOwnership
# AppendData on a directory only creates subdirectories; it cannot replace an existing file.
$dirWriteRights = [System.Security.AccessControl.FileSystemRights]::WriteData -bor `
    [System.Security.AccessControl.FileSystemRights]::DeleteSubdirectoriesAndFiles -bor `
    [System.Security.AccessControl.FileSystemRights]::Delete -bor `
    [System.Security.AccessControl.FileSystemRights]::ChangePermissions -bor `
    [System.Security.AccessControl.FileSystemRights]::TakeOwnership

function Test-TrustedIdentity($Identity) {
    try { $sid = $Identity.Translate([System.Security.Principal.SecurityIdentifier]).Value } catch { return $false }
    return ($trustedSids -contains $sid) -or ($trustedNames -contains $Identity.Value)
}

# Returns a list of problems; empty means only trusted principals can modify the path chain.
function Get-WritableByUntrusted([string]$FilePath) {
    $problems = New-Object System.Collections.Generic.List[string]
    $item = Get-Item -LiteralPath $FilePath -Force
    while ($null -ne $item) {
        $isDir = $item.PSIsContainer
        $mask = if ($isDir) { $dirWriteRights } else { $fileWriteRights }
        $acl = Get-Acl -LiteralPath $item.FullName
        if (-not (Test-TrustedIdentity ([System.Security.Principal.NTAccount]$acl.Owner))) {
            $problems.Add("$($item.FullName): owner $($acl.Owner) is not an administrator principal")
        }
        foreach ($rule in $acl.Access) {
            if ($rule.AccessControlType -ne "Allow") { continue }
            if ($rule.PropagationFlags -band [System.Security.AccessControl.PropagationFlags]::InheritOnly) { continue }
            if (($rule.FileSystemRights -band $mask) -eq 0) { continue }
            if (Test-TrustedIdentity $rule.IdentityReference) { continue }
            $problems.Add("$($item.FullName): $($rule.IdentityReference) has $($rule.FileSystemRights)")
        }
        $item = if ($isDir) { $item.Parent } else { $item.Directory }
    }
    return $problems
}

# Definition builder: no side effects. -Inspect and registration both read from here.
function New-HandsElevatedDefinition([string]$InstalledExe, [string]$TriggerMode) {
    $action = New-ScheduledTaskAction -Execute $InstalledExe -Argument $pipeArgs
    $principal = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType Interactive -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet `
        -AllowStartIfOnBatteries `
        -DontStopIfGoingOnBatteries `
        -ExecutionTimeLimit ([TimeSpan]::Zero) `
        -MultipleInstances IgnoreNew
    $triggers = @()
    if ($TriggerMode -eq "AtLogOn") {
        $triggers = @(New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME)
    }
    return [pscustomobject]@{
        Action    = $action
        Principal = $principal
        Settings  = $settings
        Triggers  = $triggers
    }
}

function Write-PrivilegeNotice {
    Write-Host ""
    Write-Host "IMPORTANT: A Highest task for this user is a STANDING ELEVATION PATH."
    Write-Host "Any process running as this user can run: schtasks /Run /TN HandsElevated"
    Write-Host "with NO UAC prompt. This is inherent to the mechanism, not a bug."
    Write-Host ""
    Write-Host "schtasks /Run needs an INTERACTIVE LOGON SESSION."
    Write-Host "A WSL or SSH session with no interactive desktop is NOT the runner."
    Write-Host "The task must not be aimed at session 0."
    Write-Host ""
    Write-Host "Revoke (unregister): Unregister-ScheduledTask -TaskName 'HandsElevated' -Confirm:`$false"
    Write-Host "Stop running server: schtasks /End /TN HandsElevated"
}

if (-not (Test-Path -LiteralPath $Exe)) { Fail "Exe not found: $Exe" }
# Task Scheduler's working directory is %windir%\System32; relative paths fail at run time.
$sourceExe = (Resolve-Path -LiteralPath $Exe).Path
$installDirFull = [System.IO.Path]::GetFullPath($InstallDir)
$installedExe = Join-Path $installDirFull "hands.exe"
$definition = New-HandsElevatedDefinition -InstalledExe $installedExe -TriggerMode $Trigger

if ($Inspect) {
    $summary = [ordered]@{
        TaskName          = $taskName
        SourceExe         = $sourceExe
        Execute           = $definition.Action.Execute
        Arguments         = $definition.Action.Arguments
        UserId            = $definition.Principal.UserId
        LogonType         = [string]$definition.Principal.LogonType
        RunLevel          = [string]$definition.Principal.RunLevel
        TriggerCount      = ($definition.Triggers | Measure-Object).Count
        ExecutionTimeLimit = [string]$definition.Settings.ExecutionTimeLimit
        MultipleInstances = [string]$definition.Settings.MultipleInstances
        InstalledNow      = (Test-Path -LiteralPath $installedExe)
    }
    $summary | ConvertTo-Json -Compress
    Write-PrivilegeNotice
    exit 0
}

# Require an elevated shell; never self-elevate.
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Fail "This script must be run from an elevated (Administrator) shell. It does not raise UAC and does not self-elevate."
}

# Stop only this task's own server so the installed exe is not locked.
$existing = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
if ($existing -and $existing.State -eq "Running") {
    Write-Host "Stopping running $taskName before replacing its exe."
    Stop-ScheduledTask -TaskName $taskName
    Start-Sleep -Seconds 1
}

New-Item -ItemType Directory -Force -Path $installDirFull | Out-Null
if ($sourceExe -ne $installedExe) {
    Copy-Item -LiteralPath $sourceExe -Destination $installedExe -Force
}
$srcHash = (Get-FileHash -LiteralPath $sourceExe -Algorithm SHA256).Hash
$dstHash = (Get-FileHash -LiteralPath $installedExe -Algorithm SHA256).Hash
if ($srcHash -ne $dstHash) { Fail "Installed exe hash differs from source ($dstHash vs $srcHash)." }

$problems = @(Get-WritableByUntrusted $installedExe)
if ($problems.Count -gt 0) {
    Write-Host "Refusing to register: the exe would be modifiable by non-administrators:"
    $problems | ForEach-Object { Write-Host "  $_" }
    Fail "Choose an admin-only -InstallDir (default: $env:ProgramFiles\Hands). A Highest task must not run a user-writable file."
}

if ($Trigger -eq "AtLogOn") {
    Register-ScheduledTask -TaskName $taskName -Action $definition.Action -Principal $definition.Principal `
        -Settings $definition.Settings -Trigger $definition.Triggers -Force | Out-Null
    Write-Host "Registered $taskName with AtLogOn trigger."
    Write-Warning "A High-IL server will start at EVERY logon for this user."
} else {
    # OnDemand: no -Trigger, so -Force replaces any existing logon trigger.
    Register-ScheduledTask -TaskName $taskName -Action $definition.Action -Principal $definition.Principal `
        -Settings $definition.Settings -Force | Out-Null
    Write-Host "Registered $taskName ON-DEMAND (no trigger)."
}

# Read back what Task Scheduler actually stored.
$task = Get-ScheduledTask -TaskName $taskName
# Count <Triggers> children in the stored XML. `@($null).Count` is 1 in Windows PowerShell 5.1,
# so counting the Triggers property directly reports a phantom trigger.
$storedXml = [xml](Export-ScheduledTask -TaskName $taskName)
$triggerCount = $storedXml.Task.Triggers.ChildNodes.Count
Write-Host "Installed exe : $installedExe (sha256 $dstHash)"
Write-Host "Action        : $($task.Actions[0].Execute) $($task.Actions[0].Arguments)"
Write-Host "Principal     : $($task.Principal.UserId) / $($task.Principal.LogonType) / $($task.Principal.RunLevel)"
Write-Host "Trigger count : $triggerCount"
if ($Trigger -eq "OnDemand" -and $triggerCount -ne 0) {
    Fail "On-demand task still has $triggerCount trigger(s). Unregister-ScheduledTask -TaskName 'HandsElevated' -Confirm:`$false and re-run."
}

Write-PrivilegeNotice
