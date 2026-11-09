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

.PARAMETER StopHolders
Explicitly stop other processes running the installed exe (for example a harness bridge).
Off by default: the script normally lists them and refuses. Only use this while you accept
that those processes (and any MCP session using them) will be terminated.

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

    [switch]$Inspect,

    [switch]$StopHolders
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

# --- Preflight: nothing is stopped until holders and destination are known -------------
$existing = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
$wasRunning = [bool]($existing -and $existing.State -eq "Running")

function Get-InstalledExeHolders {
    $target = [System.IO.Path]::GetFullPath($installedExe)
    @(Get-CimInstance Win32_Process -Filter "Name='hands.exe'" -ErrorAction SilentlyContinue |
        Where-Object {
            if (-not $_.ExecutablePath) { return $false }
            try { return ([System.IO.Path]::GetFullPath($_.ExecutablePath) -ieq $target) }
            catch { return $false }
        })
}

# Starts the task and confirms it actually reached Running (a successful cmdlet call only
# proves a start request was accepted).
function Start-TaskAndConfirm([string]$Name) {
    Start-ScheduledTask -TaskName $Name | Out-Null
    $deadline = (Get-Date).AddSeconds(3)
    while ((Get-Date) -lt $deadline -and (Get-ScheduledTask -TaskName $Name).State -ne "Running") {
        Start-Sleep -Milliseconds 200
    }
    return ((Get-ScheduledTask -TaskName $Name).State -eq "Running")
}

# The task's own server is `serve --pipe ...`; anything else (a harness bridge) is foreign.
# Match the whole command line (exe + exact action args) so a `do-task --goal "... serve --pipe ..."`
# invocation cannot be mistaken for the task's server.
$taskCommandRe = '^\s*"?' + [regex]::Escape($installedExe) + '"?\s+' + [regex]::Escape($pipeArgs) + '\s*$'
$foreign = @(Get-InstalledExeHolders | Where-Object { $_.CommandLine -notmatch $taskCommandRe })
if ($foreign.Count -gt 0 -and -not $StopHolders) {
    Write-Host "The installed exe is in use by processes other than the task:"
    $foreign | ForEach-Object { Write-Host ("  pid {0}  {1}" -f $_.ProcessId, ($_.CommandLine -replace '\s+', ' ')) }
    Fail "Stop those processes and re-run (Stop-Process -Id <pid> -Force), or re-run with -StopHolders. The task was not stopped."
}

New-Item -ItemType Directory -Force -Path $installDirFull | Out-Null
$problems = @(Get-WritableByUntrusted $installDirFull)
if ($problems.Count -gt 0) {
    Write-Host "Refusing to register: the install directory is modifiable by non-administrators:"
    $problems | ForEach-Object { Write-Host "  $_" }
    Fail "Choose an admin-only -InstallDir (default: $env:ProgramFiles\Hands). The task was not stopped."
}

# Stage and verify the source before touching the installed exe or the task.
$stagedExe = "$installedExe.new"
$backupExe = "$installedExe.old"
$srcHash = (Get-FileHash -LiteralPath $sourceExe -Algorithm SHA256).Hash
if ($sourceExe -ne $installedExe) {
    Copy-Item -LiteralPath $sourceExe -Destination $stagedExe -Force
    $stagedHash = (Get-FileHash -LiteralPath $stagedExe -Algorithm SHA256).Hash
    if ($stagedHash -ne $srcHash) {
        Remove-Item -LiteralPath $stagedExe -ErrorAction SilentlyContinue
        Fail "Staged copy does not match the source ($stagedHash vs $srcHash). The task was not stopped."
    }
}

# --- Stopped interval: any failure here restores the exe and the task ------------------
$installedWasReadOnly = $false
$swapped = $false
$failure = $null
$rollbackFailed = $false
$rolledBack = $false
$rollbackError = ""
$restarted = $false
try {
    if ($wasRunning) {
        Write-Host "Stopping running $taskName before replacing its exe."
        Stop-ScheduledTask -TaskName $taskName
        $deadline = (Get-Date).AddSeconds(10)
        while ((Get-Date) -lt $deadline -and (Get-InstalledExeHolders).Count -gt 0) {
            Start-Sleep -Milliseconds 250
        }
    }

    $holders = @(Get-InstalledExeHolders)
    if ($holders.Count -gt 0 -and $StopHolders) {
        Write-Host "Stopping processes using the installed exe (because -StopHolders was given):"
        for ($attempt = 0; $attempt -lt 3 -and $holders.Count -gt 0; $attempt++) {
            $holders | ForEach-Object {
                Write-Host ("  stopping pid {0}  {1}" -f $_.ProcessId, ($_.CommandLine -replace '\s+', ' '))
                Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue
            }
            Start-Sleep -Milliseconds 500
            $holders = @(Get-InstalledExeHolders)
        }
    }
    if ($holders.Count -gt 0) {
        $holders | ForEach-Object { Write-Host ("  pid {0}  {1}" -f $_.ProcessId, ($_.CommandLine -replace '\s+', ' ')) }
        throw "the installed exe is still in use"
    }

    if ($sourceExe -ne $installedExe) {
        if (Test-Path -LiteralPath $installedExe) {
            $installedWasReadOnly = (Get-Item -LiteralPath $installedExe).IsReadOnly
            if ($installedWasReadOnly) {
                Set-ItemProperty -LiteralPath $installedExe -Name IsReadOnly -Value $false
            }
            # A just-exited server can keep its image mapped for a moment; retry the swap.
            $replaced = $false
            for ($attempt = 0; -not $replaced; $attempt++) {
                try {
                    [System.IO.File]::Replace($stagedExe, $installedExe, $backupExe)
                    $replaced = $true
                } catch {
                    if ($attempt -ge 4) { throw }
                    Start-Sleep -Milliseconds 700
                }
            }
            $swapped = $true
        } else {
            Move-Item -LiteralPath $stagedExe -Destination $installedExe
            $swapped = $true
        }
        $dstHash = (Get-FileHash -LiteralPath $installedExe -Algorithm SHA256).Hash
        if ($dstHash -ne $srcHash) { throw "installed exe hash differs from source ($dstHash vs $srcHash)" }
    } else {
        $dstHash = $srcHash
    }

    $problems = @(Get-WritableByUntrusted $installedExe)
    if ($problems.Count -gt 0) {
        Write-Host "Refusing to register: the exe would be modifiable by non-administrators:"
        $problems | ForEach-Object { Write-Host "  $_" }
        throw "the installed exe would be modifiable by non-administrators"
    }
} catch {
    $failure = $_
    if ($swapped -and (Test-Path -LiteralPath $backupExe)) {
        try {
            Copy-Item -LiteralPath $backupExe -Destination $installedExe -Force
            $rolledBack = $true
        } catch {
            $rollbackFailed = $true
            $rollbackError = $_.Exception.Message
        }
    }
} finally {
    Remove-Item -LiteralPath $stagedExe -ErrorAction SilentlyContinue
    if ($installedWasReadOnly -and (Test-Path -LiteralPath $installedExe)) {
        try { Set-ItemProperty -LiteralPath $installedExe -Name IsReadOnly -Value $true }
        catch { Write-Warning "Could not restore the ReadOnly attribute: $($_.Exception.Message)" }
    }
    if ($null -ne $failure -and $wasRunning -and -not $rollbackFailed) {
        try {
            $restarted = Start-TaskAndConfirm $taskName
        } catch {
            Write-Warning "Could not restart $($taskName): $($_.Exception.Message)"
        }
    }
    if (-not $rollbackFailed) {
        Remove-Item -LiteralPath $backupExe -ErrorAction SilentlyContinue
    }
}

if ($null -ne $failure) {
    if ($rollbackFailed) {
        Fail "Replacing the installed exe failed ($($failure.Exception.Message)) and rollback also failed ($rollbackError). The previous exe is kept at $backupExe and the task was not restarted; restore it manually and run schtasks /Run /TN HandsElevated."
    }
    $tail = if (-not $wasRunning) { "The task was not running before and remains stopped." }
    elseif ($restarted) { "The task was restarted." }
    else { "The task could NOT be restarted; run schtasks /Run /TN HandsElevated." }
    if ($rolledBack) {
        Fail "Replacing the installed exe failed: $($failure.Exception.Message) The previous exe was restored. $tail"
    }
    Fail "Replacing the installed exe failed: $($failure.Exception.Message) $tail"
}

# --- Registration: a failure restarts the task rather than leaving it stopped ----------
try {
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
    if ($Trigger -eq "OnDemand" -and $triggerCount -ne 0) {
        throw "the on-demand task has $triggerCount trigger(s)"
    }
} catch {
    $restarted = $false
    if ($wasRunning) {
        try {
            $restarted = Start-TaskAndConfirm $taskName
        } catch {
            Write-Warning "Could not restart $($taskName): $($_.Exception.Message)"
        }
    }
    $tail = if (-not $wasRunning) { "The task was not running before and remains stopped." }
    elseif ($restarted) { "The task was restarted." }
    else { "The task could NOT be restarted; run schtasks /Run /TN HandsElevated." }
    Fail "Registering the task failed: $($_.Exception.Message) $tail"
}

Write-Host "Installed exe : $installedExe (sha256 $dstHash)"
Write-Host "Action        : $($task.Actions[0].Execute) $($task.Actions[0].Arguments)"
Write-Host "Principal     : $($task.Principal.UserId) / $($task.Principal.LogonType) / $($task.Principal.RunLevel)"
Write-Host "Trigger count : $triggerCount"

Write-PrivilegeNotice
