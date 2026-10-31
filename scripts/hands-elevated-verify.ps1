<#
.SYNOPSIS
Send a nonce to an elevated window through the medium-IL stdio bridge.

.DESCRIPTION
Assumes the HandsElevated task is already running (`schtasks /Run /TN HandsElevated`).
Starts `hands mcp --bridge \\.\pipe\hands-elev` (medium IL), performs the MCP handshake
(`initialize` 2024-11-05, read the result, `notifications/initialized`), optionally sends
`activate` for -Window, then sends one `type` call with a nonce and prints every response.

This script is only a sender. `ok:true`, a tool result, or observe text does NOT pass
verification: the nonce must be read back from the elevated target (screenshot or a file
the target writes). It does not register the task, does not raise UAC, and is not run by
`cargo test`.

`type` refuses unless the foreground window has a focused editable leaf, so without -Window
the script waits (Read-Host) until the operator confirms the elevated target is focused.

.PARAMETER Exe
hands.exe used for the bridge. Default: %ProgramFiles%\Hands\hands.exe.

.PARAMETER Window
Optional window selector (pid, title, or hwnd:hex). When given, `activate` is sent before `type`.

.PARAMETER Nonce
Text to type. Default: ELEVTEST-<8 hex>.

.PARAMETER NoPrompt
Skip the focus prompt. Only valid together with -Window.

.PARAMETER StopFirst
Send the MCP `stop` tool before `type` and report what `type` returns afterwards.
#>

param(
    [string]$Exe = (Join-Path $env:ProgramFiles "Hands\hands.exe"),
    [string]$Window,
    [string]$Nonce = ("ELEVTEST-" + [Guid]::NewGuid().ToString("N").Substring(0, 8)),
    [switch]$NoPrompt,
    [switch]$StopFirst
)

$ErrorActionPreference = "Stop"
$pipeName = '\\.\pipe\hands-elev'
if ($NoPrompt -and -not $Window) {
    [Console]::Error.WriteLine("-NoPrompt requires -Window; this script does not guess a window.")
    exit 1
}
if (-not (Test-Path -LiteralPath $Exe)) {
    [Console]::Error.WriteLine("Exe not found: $Exe")
    exit 1
}

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = (Resolve-Path -LiteralPath $Exe).Path
$psi.Arguments = "mcp --bridge $pipeName"
$psi.UseShellExecute = $false
$psi.CreateNoWindow = $true
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$utf8 = New-Object System.Text.UTF8Encoding($false)
$psi.StandardOutputEncoding = $utf8

Write-Host "Bridge: $($psi.FileName) $($psi.Arguments)"
Write-Host "Nonce : $Nonce"
$bridge = [System.Diagnostics.Process]::Start($psi)
# Windows PowerShell 5.1 has no ProcessStartInfo.StandardInputEncoding: write UTF-8 (no BOM) ourselves.
$writer = New-Object System.IO.StreamWriter($bridge.StandardInput.BaseStream, $utf8)

function Send($Message) {
    $writer.WriteLine(($Message | ConvertTo-Json -Compress -Depth 10))
    $writer.Flush()
}

function Receive([int]$Id, [int]$TimeoutMs = 15000) {
    while ($true) {
        $task = $bridge.StandardOutput.ReadLineAsync()
        if (-not $task.Wait($TimeoutMs)) {
            $err = ""
            if ($bridge.HasExited) { $err = $bridge.StandardError.ReadToEnd() }
            throw "timeout waiting for response id=$Id. bridge exited=$($bridge.HasExited) stderr=$err"
        }
        $line = $task.Result
        if ($null -eq $line) {
            throw "bridge closed stdout. stderr=$($bridge.StandardError.ReadToEnd())"
        }
        $parsed = $line | ConvertFrom-Json
        if ($parsed.PSObject.Properties.Name -contains "id" -and $parsed.id -eq $Id) {
            return $line
        }
    }
}

function Call-Tool([int]$Id, [string]$Name, $Arguments) {
    Send @{ jsonrpc = "2.0"; id = $Id; method = "tools/call"; params = @{ name = $Name; arguments = $Arguments } }
    $line = Receive $Id
    Write-Host "[$Name] $line"
    return $line
}

try {
    Send @{
        jsonrpc = "2.0"; id = 1; method = "initialize"
        params = @{
            protocolVersion = "2024-11-05"
            capabilities    = @{}
            clientInfo      = @{ name = "hands-elevated-verify"; version = "1.0.0" }
        }
    }
    Write-Host "[initialize] $(Receive 1)"
    Send @{ jsonrpc = "2.0"; method = "notifications/initialized" }

    $status = Call-Tool 2 "elevation_status" @{}
    if ($Window) {
        Call-Tool 3 "activate" @{ window = $Window } | Out-Null
        Start-Sleep -Milliseconds 500
    } elseif (-not $NoPrompt) {
        Write-Host ""
        Write-Host "type refuses unless the foreground window has a focused editable field."
        Write-Host "Focus the ELEVATED target's text field now, then press Enter."
        Read-Host | Out-Null
    }
    if ($StopFirst) {
        Call-Tool 4 "stop" @{} | Out-Null
    }
    Call-Tool 5 "type" @{ text = $Nonce } | Out-Null
    Write-Host ""
    Write-Host "Sent nonce $Nonce. This script does not decide the outcome."
    Write-Host "ok:true / a tool result / observe text does NOT pass: read the nonce back from the elevated target."
} finally {
    try { $writer.Close() } catch {}
    if (-not $bridge.WaitForExit(3000)) {
        # Only the bridge child this script started.
        Stop-Process -Id $bridge.Id -Force -ErrorAction SilentlyContinue
    }
}
