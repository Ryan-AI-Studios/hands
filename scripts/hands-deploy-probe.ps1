<#
.SYNOPSIS
Read-only deployed-state probe for Hands (0127 DoD-3).

.DESCRIPTION
Reports the five facts of spec 0127 §1b as separate fields and never merges them:
installed exe, dev exe, the HandsElevated task, the live hands.exe processes (with their image
hashes), the desk mutex and pipe, the Chrome native-host registration, and the unpacked
extension version. It is a probe, not an installer.

Writes: exactly one file, the -Out JSON. Never writes the registry, never starts, stops or
signals a process (unless -RunVersion is given, see below), never registers or changes a
task, never elevates, never opens the desk, never creates a listener.

-RunVersion is the single opt-in exception: it runs `<exe> --version` (which exits at once
and touches no desk) to read the printed identity line. Without it, version_line is null.

.PARAMETER Out
Path of the JSON file to write. Its parent directory must already exist.

.PARAMETER InstalledExe
Elevated server exe. Default: %ProgramFiles%\Hands\hands.exe.

.PARAMETER DevExe
Dev build exe. Default: the repo's target\release\hands.exe.

.PARAMETER ExtensionDir
Owner-supplied unpacked extension folder (reads manifest.json). Optional.

.PARAMETER RunVersion
Opt in to running `--version` on each exe to read the identity line.
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Out,
    [string]$InstalledExe = (Join-Path $env:ProgramFiles "Hands\hands.exe"),
    [string]$DevExe,
    [string]$ExtensionDir,
    [switch]$RunVersion
)

$ErrorActionPreference = "Stop"

$taskName = "HandsElevated"
$deskMutexName = 'Local\hands-desk-driver'
$pipeLeaf = "hands-elev"
$nativeHostKey = 'HKCU:\Software\Google\Chrome\NativeMessagingHosts\com.helpinghands.host'

function Get-ExeFacts([string]$Path, [bool]$Run) {
    $facts = [ordered]@{
        path         = $Path
        exists       = $false
        size         = $null
        sha256       = $null
        mtime        = $null
        version_line = $null
    }
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        return $facts
    }
    $item = Get-Item -LiteralPath $Path
    $facts.exists = $true
    $facts.size = [int64]$item.Length
    $facts.mtime = $item.LastWriteTime.ToString("o")
    $facts.sha256 = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
    if ($Run) {
        $line = & $Path --version 2>$null
        if ($LASTEXITCODE -eq 0 -and $line) {
            $facts.version_line = ([string]($line | Select-Object -First 1)).Trim()
        }
    }
    return $facts
}

function Get-NodeText($Xml, [string]$XPath, $Ns) {
    $node = $Xml.SelectSingleNode($XPath, $Ns)
    if ($null -eq $node) { return $null }
    return [string]$node.InnerText
}

function Get-TaskFacts() {
    $facts = [ordered]@{
        name          = $taskName
        present       = $false
        action        = $null
        arguments     = $null
        run_level     = $null
        logon_type    = $null
        trigger_count = $null
        state         = $null
        last_result   = $null
        status        = $null
    }
    $xmlText = & schtasks.exe /Query /TN $taskName /XML 2>$null
    if ($LASTEXITCODE -ne 0 -or -not $xmlText) {
        return $facts
    }
    $facts.present = $true
    $xml = [xml]($xmlText -join "`n")
    $ns = New-Object System.Xml.XmlNamespaceManager($xml.NameTable)
    $ns.AddNamespace("t", "http://schemas.microsoft.com/windows/2004/02/mit/task")
    $facts.run_level = Get-NodeText $xml "//t:Principals/t:Principal/t:RunLevel" $ns
    $facts.logon_type = Get-NodeText $xml "//t:Principals/t:Principal/t:LogonType" $ns
    $facts.trigger_count = @($xml.SelectNodes("//t:Triggers/*", $ns)).Count
    $facts.action = Get-NodeText $xml "//t:Actions/t:Exec/t:Command" $ns
    $facts.arguments = Get-NodeText $xml "//t:Actions/t:Exec/t:Arguments" $ns
    $enabled = Get-NodeText $xml "//t:Settings/t:Enabled" $ns
    $facts.state = if ($enabled -eq "false") { "Disabled" } else { "Enabled" }
    $listText = & schtasks.exe /Query /TN $taskName /FO LIST /V 2>$null
    foreach ($line in @($listText)) {
        if ($line -match '^\s*Last Result:\s*(.+)$') { $facts.last_result = $Matches[1].Trim() }
        if ($line -match '^\s*Status:\s*(.+)$') { $facts.status = $Matches[1].Trim() }
    }
    return $facts
}

function Get-HandsProcesses() {
    $rows = @()
    $procs = @(Get-CimInstance -ClassName Win32_Process -Filter "Name = 'hands.exe'" -ErrorAction SilentlyContinue)
    foreach ($p in $procs) {
        $imageSha = $null
        if ($p.ExecutablePath -and (Test-Path -LiteralPath $p.ExecutablePath -PathType Leaf)) {
            try {
                $imageSha = (Get-FileHash -LiteralPath $p.ExecutablePath -Algorithm SHA256).Hash
            } catch {
                $imageSha = $null
            }
        }
        $rows += [ordered]@{
            pid          = [int]$p.ProcessId
            image_path   = $p.ExecutablePath
            image_sha256 = $imageSha
            command_line = $p.CommandLine
        }
    }
    return ,$rows
}

function Test-DeskMutexPresent() {
    # Open-only: OpenExisting never creates or acquires the mutex, and it is disposed at once.
    # It requests the default mutex rights; a denied open means present but not ours.
    try {
        $m = [System.Threading.Mutex]::OpenExisting($deskMutexName)
        $m.Dispose()
        return $true
    } catch [System.Threading.WaitHandleCannotBeOpenedException] {
        return $false
    } catch [System.UnauthorizedAccessException] {
        # Present, but this caller may not open it (a High-IL owner); the probe reports present.
        return $true
    }
}

function Test-PipePresent() {
    $names = @([System.IO.Directory]::GetFiles('\\.\pipe\'))
    return [bool]($names | Where-Object { $_ -match ("(^|\\)" + [regex]::Escape($pipeLeaf) + "$") })
}

function Get-NativeHostFacts() {
    $facts = [ordered]@{
        hkcu_default  = $null
        manifest_path = $null
        target_path   = $null
        target_sha256 = $null
        target_exists = $false
    }
    $key = Get-Item -LiteralPath $nativeHostKey -ErrorAction SilentlyContinue
    if ($null -eq $key) {
        return $facts
    }
    $facts.hkcu_default = [string]$key.GetValue("")
    $manifest = $facts.hkcu_default
    if (-not $manifest -or -not (Test-Path -LiteralPath $manifest -PathType Leaf)) {
        return $facts
    }
    $facts.manifest_path = $manifest
    $json = Get-Content -LiteralPath $manifest -Raw | ConvertFrom-Json
    $target = [string]$json.path
    if ($target -and -not [System.IO.Path]::IsPathRooted($target)) {
        $target = Join-Path (Split-Path -Parent $manifest) $target
    }
    $facts.target_path = $target
    if ($target -and (Test-Path -LiteralPath $target -PathType Leaf)) {
        $facts.target_exists = $true
        $facts.target_sha256 = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash
    }
    return $facts
}

function Get-ExtensionFacts() {
    $facts = [ordered]@{ dir = $null; manifest_version = $null }
    if (-not $ExtensionDir) { return $facts }
    $facts.dir = $ExtensionDir
    $manifest = Join-Path $ExtensionDir "manifest.json"
    if (Test-Path -LiteralPath $manifest -PathType Leaf) {
        $facts.manifest_version = [string]((Get-Content -LiteralPath $manifest -Raw | ConvertFrom-Json).version)
    }
    return $facts
}

if (-not $DevExe) {
    $DevExe = Join-Path (Split-Path -Parent $MyInvocation.MyCommand.Path) "..\target\release\hands.exe"
}
$DevExe = [System.IO.Path]::GetFullPath($DevExe)

$installed = Get-ExeFacts -Path $InstalledExe -Run $RunVersion.IsPresent
$dev = Get-ExeFacts -Path $DevExe -Run $RunVersion.IsPresent
$processes = Get-HandsProcesses

# A holder is a live hands.exe whose image hash is not the installed hash (or cannot be read).
# The probe reports it. It never stops it.
$staleHolders = @()
foreach ($proc in $processes) {
    if ($installed.sha256 -and $proc.image_sha256 -eq $installed.sha256) { continue }
    $staleHolders += [ordered]@{
        pid          = $proc.pid
        image_path   = $proc.image_path
        image_sha256 = $proc.image_sha256
        reason       = if ($null -eq $proc.image_sha256) { "image hash unavailable" } else { "image hash differs from installed" }
    }
}

$report = [ordered]@{
    schema          = "hands.deploy-probe/v1"
    generated_at    = (Get-Date).ToString("o")
    read_only       = $true
    run_version     = $RunVersion.IsPresent
    installed       = $installed
    dev             = $dev
    task            = Get-TaskFacts
    processes       = $processes
    stale_holders   = $staleHolders
    desk_mutex_present = Test-DeskMutexPresent
    pipe_present    = Test-PipePresent
    native_host     = Get-NativeHostFacts
    extension       = Get-ExtensionFacts
}

$json = $report | ConvertTo-Json -Depth 8
[System.IO.File]::WriteAllText($Out, $json, [System.Text.UTF8Encoding]::new($false))
Write-Output $json
