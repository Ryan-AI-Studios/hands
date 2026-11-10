<#
.SYNOPSIS
Drive one elevated-shell qualification case through the medium-IL MCP bridge.

.DESCRIPTION
This is the only place a full 0126 case is driven. Every step is an MCP request over
`hands mcp --bridge \\.\pipe\hands-elev`: a medium local `hands observe/click/type` cannot
resolve an elevated target. The script performs the MCP handshake, requires
`elevation_status` = role pipe-desk AND privileged true before any actuation, refuses to
drive a case from a non-medium shell, types exactly one fixed template, and writes
`evidence.json` with the 0126 verdict vocabulary.

Honesty: `ok:true`, `foregrounded:true`, a SendInput count and a tool result are NOT
evidence. A console/terminal cell is `OBSERVED_EFFECT` only when the target-created
artifact holds the exact nonce AND carries a High mandatory label. A stop cell needs the
owner to read the partial-prefix screenshot; the script records it as
`DELIVERED_UNVERIFIED` with an explicit owner promotion note. A lifecycle disconnect is
`effect:"unknown"`, never "nothing was sent".

Safety: never registers a task, never runs `schtasks /Run`, never raises UAC, never reads
a secret, never sends a persist/session allow. Bridge and the one stop child are spawned
through System.Diagnostics.Process with UseShellExecute = $false (no shell-execute/runas
path). The only process this script terminates is the bridge child it spawned itself
(lifecycle disconnect): never the task, the High server, or any other hands.exe.

.PARAMETER Case
console | terminal | stop | lifecycle. `stop` must be the last case of a session.

.PARAMETER Window
Target selector: pid, unique exact title, unique substring, or hwnd:<hex>. Required for
every driving case; the script never guesses a window.

.PARAMETER Nonce
`^[A-Za-z0-9-]{8,64}$`; default ELEVQ-<8 hex>. It names the artifact the target creates.

.PARAMETER EvidenceDir
Directory for evidence.json and the screenshots. Required outside -Inspect.

.PARAMETER Exe
hands.exe used for the bridge and the stop child.
Default: %ProgramFiles%\Hands\hands.exe.

.PARAMETER GrantOnceOnFence
Allow one `confirm` after a fence: mode=once, the exact returned domain and category.
Never persist, never session.

.PARAMETER Inspect
Print one JSON object describing the contract. No bridge, no window, no actuation.
#>

[CmdletBinding()]
param(
    [ValidateSet("console", "terminal", "stop", "lifecycle")]
    [string]$Case = "console",
    [string]$Window,
    [ValidatePattern('^[A-Za-z0-9-]{8,64}$')]
    [string]$Nonce = ("ELEVQ-" + [Guid]::NewGuid().ToString("N").Substring(0, 8)),
    [string]$EvidenceDir,
    [string]$Exe = (Join-Path $env:ProgramFiles "Hands\hands.exe"),
    [switch]$GrantOnceOnFence,
    [switch]$Inspect
)

$ErrorActionPreference = "Stop"

$pipeName = '\\.\pipe\hands-elev'
$mediumLabelSid = "S-1-16-8192"
$highLabelSid = "S-1-16-12288"
$verdictTokens = @(
    "OBSERVED_EFFECT",
    "DELIVERED_UNVERIFIED",
    "REFUSED_EXPECTED",
    "UNSUPPORTED",
    "FAILED",
    "NOT_RUN",
    "RECORDED"
)
$evidenceFields = @(
    "case", "client_il", "bridge_status", "target", "steps", "artifact",
    "verdict", "limitations", "generated_at", "build"
)
$confirmPolicy = "once-only: only after a tool returns a fence, only with -GrantOnceOnFence, mode=once, the exact returned domain and category, never persist, never session"
$fixedTemplate = 'Write-Output {0} | Out-File -Encoding ascii "$env:TEMP\hands-qual-{0}.txt"'
# Fixed in-flight stop filler: <= 32 chars and '#'-prefixed so an accidental Enter is inert.
# > 32 chars would take TypePath::Clipboard and skip the per-code-unit lease::poll().
$stopFiller = "#ELEVQ-" + $Nonce.Substring($Nonce.Length - 8)
if ($stopFiller.Length -gt 32) {
    throw "stop filler must stay on the Unicode type path: > 32 chars takes TypePath::Clipboard, which skips the per-code-unit lease::poll() an in-flight stop needs; got $($stopFiller.Length) chars"
}

if ($Inspect) {
    # One JSON object. No bridge, no window, no input, no filesystem write.
    $inspectEnvelope = [ordered]@{
        case              = $Case
        window            = $Window
        nonce             = $Nonce
        actuation         = $false
        transport         = "mcp --bridge $pipeName"
        fixed_template    = ($fixedTemplate -f $Nonce)
        stop_filler       = $stopFiller
        stop_filler_len   = $stopFiller.Length
        stop_filler_path  = "unicode"
        confirm_policy    = $confirmPolicy
        medium_gate       = $mediumLabelSid
        privilege_gate    = 'role == pipe-desk && privileged == true'
        evidence_fields   = $evidenceFields
        verdict_tokens    = $verdictTokens
        owner_cells       = "O1-O12 stay owner-only: no task registration, no UAC, no elevated input from the agent"
    }
    Write-Output ($inspectEnvelope | ConvertTo-Json -Compress -Depth 8)
    exit 0
}

if (-not $Window) {
    [Console]::Error.WriteLine("-Window is required; this script does not guess an elevated target.")
    exit 1
}
if (-not $EvidenceDir) {
    [Console]::Error.WriteLine("-EvidenceDir is required outside -Inspect.")
    exit 1
}
if (-not (Test-Path -LiteralPath $Exe)) {
    [Console]::Error.WriteLine("Exe not found: $Exe")
    exit 1
}

function Test-Prop($Object, [string]$Name) {
    if ($null -eq $Object) { return $false }
    if ($Object -is [System.Collections.IDictionary]) { return $Object.Contains($Name) }
    return ($Object.PSObject.Properties.Name -contains $Name)
}

function Get-Prop($Object, [string]$Name) {
    if (-not (Test-Prop $Object $Name)) { return $null }
    if ($Object -is [System.Collections.IDictionary]) { return $Object[$Name] }
    return $Object.$Name
}

function Get-ClientIntegrity {
    # The label line is localized; the SID is not. Never read client_il from the bridge.
    $text = (& whoami.exe /groups 2>$null | Out-String)
    if ($text -match "S-1-16-16384") { return "system" }
    if ($text -match $highLabelSid) { return "high" }
    if ($text -match $mediumLabelSid) { return "medium" }
    if ($text -match "S-1-16-4096") { return "low" }
    return "unknown"
}

function Get-MandatoryLabel([string]$Path) {
    # Prefer a SID-based read (SDDL carries the SID even on a localized OS). A label read that
    # cannot be resolved returns "unknown", which never passes as High (fail closed).
    try {
        $sddl = (Get-Acl -LiteralPath $Path -Audit).Sddl
        if ($sddl -match $highLabelSid) { return "high" }
        if ($sddl -match $mediumLabelSid) { return "medium" }
        if ($sddl -match "S-1-16-4096") { return "low" }
    } catch {
        # SACL read can be denied; fall through to icacls (label ACEs are world-readable).
    }
    $text = (& icacls.exe $Path 2>$null | Out-String)
    $line = ($text -split "`r?`n" | Where-Object { $_ -match "Mandatory Label" } | Select-Object -First 1)
    if ($line -match "High") { return "high" }
    if ($line -match "Medium") { return "medium" }
    if ($line -match "Low") { return "low" }
    return "unknown"
}

function Save-Evidence($Evidence) {
    if (-not (Test-Path -LiteralPath $EvidenceDir)) {
        New-Item -ItemType Directory -Path $EvidenceDir -Force | Out-Null
    }
    $path = Join-Path $EvidenceDir "evidence.json"
    $json = $Evidence | ConvertTo-Json -Depth 12
    $utf8 = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($path, $json, $utf8)
    Write-Host "EVIDENCE: $path"
    return $path
}

function Start-OwnBridge {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = (Resolve-Path -LiteralPath $Exe).Path
    $psi.Arguments = "mcp --bridge $pipeName"
    # No shell-execute path, so there is no runas/elevation route.
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $utf8 = New-Object System.Text.UTF8Encoding($false)
    $psi.StandardOutputEncoding = $utf8
    $process = [System.Diagnostics.Process]::Start($psi)
    $writer = New-Object System.IO.StreamWriter($process.StandardInput.BaseStream, $utf8)
    return [pscustomobject]@{ Process = $process; Writer = $writer; Pid = $process.Id }
}

function Send-McpLine($Bridge, $Message) {
    $Bridge.Writer.WriteLine(($Message | ConvertTo-Json -Compress -Depth 12))
    $Bridge.Writer.Flush()
}

function Receive-McpId($Bridge, [int]$Id, [int]$TimeoutMs = 15000) {
    while ($true) {
        $task = $Bridge.Process.StandardOutput.ReadLineAsync()
        if (-not $task.Wait($TimeoutMs)) {
            throw "timeout waiting for response id=$Id (bridge exited=$($Bridge.Process.HasExited))"
        }
        $line = $task.Result
        if ($null -eq $line) {
            throw "bridge closed stdout while waiting for id=$Id (server disconnect or bridge kill)"
        }
        $parsed = $line | ConvertFrom-Json
        if ((Test-Prop $parsed "id") -and $parsed.id -eq $Id) {
            return $parsed
        }
    }
}

function Stop-OwnBridge($Bridge) {
    if ($null -eq $Bridge) { return }
    try { $Bridge.Writer.Close() } catch { }
    try {
        if (-not $Bridge.Process.HasExited) {
            if (-not $Bridge.Process.WaitForExit(3000)) {
                # The only allowed kill: the bridge child this script spawned itself.
                # Never the task, the High server, or another hands.exe.
                Stop-Process -Id $Bridge.Pid -Force -ErrorAction SilentlyContinue
            }
        }
    } catch {
        # The process handle may already be gone; no other process is ever terminated.
    }
}

function Get-Payload($Response) {
    $result = Get-Prop $Response "result"
    if ($null -eq $result) { throw "tool call returned no result" }
    $content = Get-Prop $result "content"
    if ($null -eq $content -or @($content).Count -lt 1) { throw "tool call returned no content" }
    $text = Get-Prop (@($content)[0]) "text"
    if ([string]::IsNullOrWhiteSpace($text)) { throw "tool result text was empty" }
    try {
        return ($text | ConvertFrom-Json)
    } catch {
        # A transport-level tool error is text, not the envelope. Keep it honest.
        return [pscustomobject]@{ ok = $false; error = $text }
    }
}

function New-BridgeCounter {
    return [pscustomobject]@{ Next = 2 }
}

function Invoke-Tool($Bridge, $Counter, [string]$Name, $Arguments) {
    $id = $Counter.Next
    $Counter.Next = $id + 1
    Send-McpLine $Bridge @{
        jsonrpc = "2.0"
        id      = $id
        method  = "tools/call"
        params  = @{ name = $Name; arguments = $Arguments }
    }
    $response = Receive-McpId $Bridge $id
    return [pscustomobject]@{ Id = $id; Response = $response; Payload = (Get-Payload $response) }
}

function Connect-PrivilegedBridge($Bridge, $Counter) {
    Send-McpLine $Bridge @{
        jsonrpc = "2.0"
        id      = 1
        method  = "initialize"
        params  = @{
            protocolVersion = "2024-11-05"
            capabilities    = @{}
            clientInfo      = @{ name = "hands-elevated-qualify"; version = "1.0.0" }
        }
    }
    $null = Receive-McpId $Bridge 1
    Send-McpLine $Bridge @{ jsonrpc = "2.0"; method = "notifications/initialized" }
    $status = Invoke-Tool $Bridge $Counter "elevation_status" @{}
    $payload = $status.Payload
    if ((Get-Prop $payload "role") -ne "pipe-desk" -or (Get-Prop $payload "privileged") -ne $true) {
        throw "refusing to actuate: elevation_status must be role=pipe-desk and privileged=true (got $($payload | ConvertTo-Json -Compress)); never falling back to a local desk"
    }
    return $payload
}

function Get-StepRecord([string]$Name, $Tool) {
    # Redacted: no typed text, no operator paths beyond the artifact.
    $payload = $Tool.Payload
    $record = [ordered]@{
        step          = $Name
        ok            = Get-Prop $payload "ok"
        frozen        = Get-Prop $payload "frozen"
        verified      = Get-Prop $payload "verified"
        post_dispatch = Get-Prop $payload "post_dispatch"
        delivery      = Get-Prop $payload "delivery"
        error         = Get-Prop $payload "error"
        refusal       = Get-Prop $payload "refusal"
        fence         = $null
    }
    $fence = Get-Prop $payload "fence"
    if ($null -ne $fence) {
        $record.fence = [ordered]@{
            domain   = Get-Prop $fence "domain"
            category = Get-Prop $fence "category"
            name     = Get-Prop $fence "name"
        }
    }
    return $record
}

function Invoke-WithOptionalOnceConfirm($Bridge, $Counter, [string]$Name, $Arguments, $Steps) {
    $first = Invoke-Tool $Bridge $Counter $Name $Arguments
    $record = Get-StepRecord $Name $first
    $Steps.Add($record) | Out-Null
    $fence = Get-Prop $first.Payload "fence"
    if ($null -eq $fence) {
        # A free pass is recorded as fence:none, never fabricated as a fence.
        $record.fence = "none"
        return [pscustomobject]@{ Payload = $first.Payload; Confirmed = $false }
    }
    if (-not $GrantOnceOnFence) {
        return [pscustomobject]@{ Payload = $first.Payload; Confirmed = $false }
    }
    $domain = Get-Prop $fence "domain"
    $category = Get-Prop $fence "category"
    if ([string]::IsNullOrWhiteSpace($domain) -or [string]::IsNullOrWhiteSpace($category)) {
        throw "fence did not name a domain and category; refusing to confirm"
    }
    Write-Host "FENCE: domain=$domain category=$category -> granting once (explicit switch)"
    $grant = Invoke-Tool $Bridge $Counter "confirm" @{
        domain   = $domain
        category = $category
        mode     = "once"
    }
    Write-Host "GRANT: $($grant.Payload | ConvertTo-Json -Compress)"
    $retry = Invoke-Tool $Bridge $Counter $Name $Arguments
    $Steps.Add((Get-StepRecord "$Name+after-once-confirm" $retry)) | Out-Null
    return [pscustomobject]@{ Payload = $retry.Payload; Confirmed = $true }
}

function Get-ObserveEnvelope($Bridge, $Counter, [string]$Name, [hashtable]$Extra, $Steps, [string]$ScreenshotTag) {
    $arguments = @{ window = $Window }
    foreach ($key in $Extra.Keys) { $arguments[$key] = $Extra[$key] }
    $tool = Invoke-Tool $Bridge $Counter "observe" $arguments
    $payload = $tool.Payload
    $record = [ordered]@{
        step           = $Name
        ok             = Get-Prop $payload "ok"
        target_window  = Get-Prop $payload "target_window"
        elements_total = Get-Prop $payload "elements_total"
        error          = Get-Prop $payload "error"
    }
    $preview = Get-Prop $payload "fg_preview_path"
    if ($null -eq $preview) { $preview = Get-Prop $payload "screenshot_path" }
    if ($preview) {
        $record.preview = $preview
    }
    $Steps.Add($record) | Out-Null
    if ($preview -and $ScreenshotTag) {
        Write-Host "SCREENSHOT[$ScreenshotTag]: $preview"
    }
    return $payload
}

function Get-TargetIdentity($ObservePayload) {
    $target = Get-Prop $ObservePayload "target_window"
    if ($null -eq $target) { return $null }
    $pidValue = Get-Prop $target "pid"
    $exePath = $null
    try {
        $proc = Get-Process -Id $pidValue -ErrorAction Stop
        $exePath = $proc.Path
    } catch {
        $exePath = $null
    }
    return [ordered]@{
        pid   = $pidValue
        hwnd  = Get-Prop $target "hwnd"
        class = Get-Prop $target "class"
        title = Get-Prop $target "title"
        exe   = $exePath
    }
}

function Get-FirstClickableRef($ObservePayload) {
    $elements = Get-Prop $ObservePayload "elements"
    foreach ($element in @($elements)) {
        $ref = Get-Prop $element "ref"
        if (-not $ref) { $ref = Get-Prop $element "id" }
        if ($ref) { return [pscustomobject]@{ Ref = $ref; Role = (Get-Prop $element "role") } }
    }
    return $null
}

function Get-ArtifactState([string]$Path, [string]$Expected) {
    $state = [ordered]@{
        path            = $Path
        expected        = $Expected
        exists          = $false
        actual          = $null
        sha256          = $null
        mandatory_label = "missing"
        content_matches = $false
    }
    if (Test-Path -LiteralPath $Path) {
        $state.exists = $true
        $raw = (Get-Content -LiteralPath $Path -Raw)
        $state.actual = if ($null -eq $raw) { "" } else { $raw.Trim() }
        $state.sha256 = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
        $state.mandatory_label = Get-MandatoryLabel $Path
        $state.content_matches = ($state.actual -eq $Expected)
    }
    return $state
}

# --- medium-shell gate -----------------------------------------------------------------
# client_il comes from this script's own label line, never from the bridge.
$clientIl = Get-ClientIntegrity
if ($clientIl -ne "medium") {
    $refusal = [ordered]@{
        case             = $Case
        client_il        = $clientIl
        bridge_status    = $null
        target           = $null
        steps            = @()
        artifact         = $null
        verdict          = "NOT_RUN"
        limitations      = @("driving shell must be medium ($mediumLabelSid); got $clientIl; the script refuses to drive from a non-medium shell")
        generated_at     = (Get-Date).ToString("o")
        build            = $null
    }
    Write-Host "REFUSED: driving shell integrity is $clientIl; every driving case requires medium ($mediumLabelSid)."
    $null = Save-Evidence $refusal
    exit 2
}

# --- read-only build identity ----------------------------------------------------------
$exeItem = Get-Item -LiteralPath $Exe
$build = [ordered]@{
    exe     = $exeItem.FullName
    sha256  = (Get-FileHash -LiteralPath $Exe -Algorithm SHA256).Hash
    written = $exeItem.LastWriteTime.ToString("o")
}

$evidence = [ordered]@{
    case          = $Case
    client_il     = $clientIl
    nonce         = $Nonce
    window        = $Window
    bridge_status = $null
    target        = $null
    steps         = (New-Object System.Collections.ArrayList)
    artifact      = $null
    verdict       = "NOT_RUN"
    limitations   = @()
    generated_at  = (Get-Date).ToString("o")
    build         = $build
}

$artifactPath = Join-Path $env:TEMP ("hands-qual-{0}.txt" -f $Nonce)
$templateText = $fixedTemplate -f $Nonce
Write-Host "CASE   : $Case"
Write-Host "TARGET : $Window"
Write-Host "TEMPLATE: $templateText"
Write-Host "FILLER : $stopFiller (len=$($stopFiller.Length), unicode path)"

$bridge = $null
$counter = $null
try {
    # --- lifecycle preflight (read-only, no state change) ------------------------------
    if ($Case -eq "lifecycle") {
        $task = $null
        try { $task = Get-ScheduledTask -TaskName HandsElevated -ErrorAction Stop } catch { $task = $null }
        $preflight = [ordered]@{
            step       = "lifecycle-preflight"
            task_state = if ($null -ne $task) { "$($task.State)" } else { "absent" }
            triggers   = if ($null -ne $task) { @($task.Triggers).Count } else { $null }
            principal  = if ($null -ne $task) { "$($task.Principal.LogonType)/$($task.Principal.RunLevel)" } else { $null }
            action     = if ($null -ne $task) { @($task.Actions | ForEach-Object { "$($_.Execute) $($_.Arguments)" }) } else { @() }
            exe_sha256 = $build.sha256
            pipe       = (Test-Path -LiteralPath $pipeName)
            server_windows = @(Get-Process hands -ErrorAction SilentlyContinue |
                Where-Object { $_.MainWindowTitle } |
                ForEach-Object { [ordered]@{ pid = $_.Id; title = $_.MainWindowTitle } })
        }
        $evidence.steps.Add($preflight) | Out-Null
        Write-Host "PREFLIGHT: $($preflight | ConvertTo-Json -Compress -Depth 6)"
    }

    # --- bridge + handshake + privilege gate ------------------------------------------
    $bridge = Start-OwnBridge
    $counter = New-BridgeCounter
    Write-Host "BRIDGE: pid=$($bridge.Pid) (this script owns it)"
    try {
        $status = Connect-PrivilegedBridge $bridge $counter
    } catch {
        # Absent server (owner stopped the task) is a designed cell, not a harness crash:
        # the bridge must fail loud and must never start a local desk.
        $stderrText = ""
        try {
            if (-not $bridge.Process.HasExited) { $null = $bridge.Process.WaitForExit(3000) }
            $stderrText = $bridge.Process.StandardError.ReadToEnd()
        } catch { }
        $evidence.steps.Add([ordered]@{
                step               = "bridge-preflight"
                transport          = "failed"
                stderr             = $stderrText
                local_desk_started = $false
            }) | Out-Null
        Stop-OwnBridge $bridge
        $bridge = $null
        if ($stderrText -match "privileged pipe not available" -or $stderrText -match "not starting a local desk server") {
            $evidence.verdict = "REFUSED_EXPECTED"
            $evidence.limitations += "the privileged pipe is absent; the bridge failed loud and no local desk started (owner: start the HandsElevated task, then re-run)"
            $script:abortRecorded = $true
            throw [System.OperationCanceledException]::new("absent-server cell recorded")
        }
        throw
    }
    $evidence.bridge_status = $status
    $evidence.steps.Add([ordered]@{ step = "elevation_status"; payload = $status }) | Out-Null
    Write-Host "STATUS: $($status | ConvertTo-Json -Compress)"

    # --- first observe: resolve the target, record identity ---------------------------
    $observe = Get-ObserveEnvelope $bridge $counter "observe-target" @{} $evidence.steps "target"
    $evidence.target = Get-TargetIdentity $observe
    if ($null -eq $evidence.target) {
        throw "observe did not resolve -Window $Window; refusing to actuate"
    }
    Write-Host "TARGET-ID: $($evidence.target | ConvertTo-Json -Compress)"

    switch ($Case) {
        "console" {
            # activate -> click (or a recorded UNSUPPORTED click cell) -> type -> preview -> enter -> artifact
            $activate = Invoke-Tool $bridge $counter "activate" @{ window = $Window }
            $evidence.steps.Add((Get-StepRecord "activate" $activate)) | Out-Null
            if (-not (Get-Prop $activate.Payload "foregrounded")) {
                $evidence.limitations += "activate did not report foregrounded:true; the click/type cells cannot be evidence"
            }
            Start-Sleep -Milliseconds 400
            # Human-read evidence for the activate cell (O3): the target must be in front.
            $null = Get-ObserveEnvelope $bridge $counter "observe-post-activate" @{ fg_preview = $true } $evidence.steps "post-activate"

            $clickable = Get-FirstClickableRef $observe
            if ($null -eq $clickable) {
                $evidence.steps.Add([ordered]@{
                        step     = "click"
                        verdict  = "UNSUPPORTED"
                        error    = "observe resolved no element with a hittable center in the console"
                    }) | Out-Null
            } else {
                $click = Invoke-Tool $bridge $counter "click" @{ element_id = $clickable.Ref }
                $evidence.steps.Add((Get-StepRecord "click" $click)) | Out-Null
                Start-Sleep -Milliseconds 200
            }

            $type = Invoke-WithOptionalOnceConfirm $bridge $counter "type" @{ text = $templateText } $evidence.steps
            if (-not (Get-Prop $type.Payload "ok")) {
                $evidence.verdict = "FAILED"
                $evidence.limitations += "type refused: $(Get-Prop $type.Payload 'error')"
                break
            }

            $null = Get-ObserveEnvelope $bridge $counter "observe-pre-submit" @{ fg_preview = $true } $evidence.steps "pre-submit"

            $enter = Invoke-WithOptionalOnceConfirm $bridge $counter "key" @{ name = "enter" } $evidence.steps
            if (-not (Get-Prop $enter.Payload "ok")) {
                $evidence.verdict = "FAILED"
                $evidence.limitations += "key enter refused: $(Get-Prop $enter.Payload 'error')"
                break
            }
            Start-Sleep -Milliseconds 800

            $artifact = Get-ArtifactState $artifactPath $Nonce
            $evidence.artifact = $artifact
            if ($artifact.exists) {
                Write-Host "ARTIFACT: $($artifact.path) sha256=$($artifact.sha256) label=$($artifact.mandatory_label)"
                Write-Host "CLEANUP: the artifact is High-labelled; delete it from an elevated shell (a medium process cannot)"
            }
            if ($artifact.exists -and $artifact.content_matches -and $artifact.mandatory_label -eq "high") {
                # Target-created artifact with the exact nonce and a High label: genuine effect.
                $evidence.verdict = "OBSERVED_EFFECT"
            } elseif ($artifact.exists -and $artifact.content_matches) {
                $evidence.verdict = "FAILED"
                $evidence.limitations += "artifact matched but its mandatory label is '$($artifact.mandatory_label)', not High: the target was not proven elevated"
            } else {
                $evidence.verdict = "FAILED"
                $evidence.limitations += "artifact absent or wrong content after Enter; ok:true is not effect"
            }
        }
        "terminal" {
            $activate = Invoke-Tool $bridge $counter "activate" @{ window = $Window }
            $evidence.steps.Add((Get-StepRecord "activate" $activate)) | Out-Null
            Start-Sleep -Milliseconds 400
            $null = Get-ObserveEnvelope $bridge $counter "observe-post-activate" @{ fg_preview = $true } $evidence.steps "post-activate"

            $clickable = Get-FirstClickableRef $observe
            if ($null -ne $clickable) {
                $click = Invoke-Tool $bridge $counter "click" @{ element_id = $clickable.Ref }
                $evidence.steps.Add((Get-StepRecord "click" $click)) | Out-Null
                Start-Sleep -Milliseconds 200
            }

            $type = Invoke-WithOptionalOnceConfirm $bridge $counter "type" @{ text = $templateText } $evidence.steps
            if (Get-Prop $type.Payload "ok") {
                $null = Get-ObserveEnvelope $bridge $counter "observe-pre-submit" @{ fg_preview = $true } $evidence.steps "pre-submit"
                $enter = Invoke-WithOptionalOnceConfirm $bridge $counter "key" @{ name = "enter" } $evidence.steps
                Start-Sleep -Milliseconds 800
                $artifact = Get-ArtifactState $artifactPath $Nonce
                $evidence.artifact = $artifact
                if ($artifact.exists -and $artifact.content_matches -and $artifact.mandatory_label -eq "high") {
                    $evidence.verdict = "OBSERVED_EFFECT"
                } else {
                    $evidence.verdict = "DELIVERED_UNVERIFIED"
                    $evidence.limitations += "type was delivered but no High-labelled artifact with the exact nonce was read back"
                }
            } else {
                $errorText = "$(Get-Prop $type.Payload 'error')"
                $evidence.verdict = "UNSUPPORTED"
                $evidence.limitations += "Windows Terminal input is not qualified on this head: $errorText"
                $evidence.steps.Add([ordered]@{
                        step          = "terminal-observed-role"
                        target_class  = $evidence.target.class
                        target_title  = $evidence.target.title
                        element_role  = if ($null -ne $clickable) { $clickable.Role } else { $null }
                        refusal       = $errorText
                    }) | Out-Null
            }
        }
        "stop" {
            # The desk must be idle >= 2 s so a prior case's fresh stop cannot leak in.
            $deskLog = Join-Path $env:LOCALAPPDATA "hands\logs\desk.jsonl"
            $deskStopRowsBefore = 0
            if (Test-Path -LiteralPath $deskLog) {
                $deskStopRowsBefore = @(Get-Content -LiteralPath $deskLog -Tail 50 | Where-Object { $_ -match '"stop"' }).Count
            }
            Start-Sleep -Milliseconds 2200
            $typeId = $counter.Next
            $counter.Next = $typeId + 1
            # Write the type request WITHOUT waiting: the stop must land inside the send window,
            # not on process-start luck.
            Send-McpLine $bridge @{
                jsonrpc = "2.0"
                id      = $typeId
                method  = "tools/call"
                params  = @{ name = "type"; arguments = @{ text = $stopFiller } }
            }
            Start-Sleep -Milliseconds 150

            $stopPsi = New-Object System.Diagnostics.ProcessStartInfo
            $stopPsi.FileName = (Resolve-Path -LiteralPath $Exe).Path
            $stopPsi.Arguments = "stop"
            $stopPsi.UseShellExecute = $false
            $stopPsi.CreateNoWindow = $true
            $stopPsi.RedirectStandardOutput = $true
            $stopPsi.RedirectStandardError = $true
            $stopProcess = [System.Diagnostics.Process]::Start($stopPsi)
            $stopOut = $stopProcess.StandardOutput.ReadToEnd()
            $stopProcess.WaitForExit(15000) | Out-Null
            # This child is our own stop sender: not the server, the task, or another hands.exe.
            Write-Host "STOP-CLI: pid=$($stopProcess.Id) exit=$($stopProcess.ExitCode) out=$stopOut"

            $typeResponse = Receive-McpId $bridge $typeId 20000
            $payload = Get-Payload $typeResponse
            $record = Get-StepRecord "type(in-flight)" ([pscustomobject]@{ Payload = $payload })
            $record.stop_cli = [ordered]@{
                pid        = $stopProcess.Id
                exit_code  = $stopProcess.ExitCode
                client_il  = $clientIl
            }
            $evidence.steps.Add($record) | Out-Null
            Start-Sleep -Milliseconds 300
            $null = Get-ObserveEnvelope $bridge $counter "observe-after-stop" @{ fg_preview = $true } $evidence.steps "after-stop"

            $frozen = Get-Prop $payload "frozen"
            $delivery = Get-Prop $payload "delivery"
            $postDispatch = Get-Prop $payload "post_dispatch"
            $stopRows = @()
            if (Test-Path -LiteralPath $deskLog) {
                $stopRows = @(Get-Content -LiteralPath $deskLog -Tail 50 | Where-Object { $_ -match '"stop"' })
            }
            $stopRowDelta = $stopRows.Count - $deskStopRowsBefore
            $evidence.steps.Add([ordered]@{
                    step              = "high-desk-stop-rows"
                    before            = $deskStopRowsBefore
                    after             = $stopRows.Count
                    delta             = $stopRowDelta
                    tail_redacted     = $stopRows.Count -gt 0
                }) | Out-Null
            if ($stopRowDelta -lt 1) {
                $evidence.limitations += "no new High-desk stop row was observed after the stop request; record it as evidence context, not an abort"
            }

            if ($frozen -eq $true -and ($delivery -eq "unknown" -or $postDispatch -eq $true)) {
                # The frozen envelope plus a High-desk stop row is mechanism evidence; the strict
                # partial prefix needs the owner's human-read screenshot before this cell can be
                # promoted to OBSERVED_EFFECT.
                $evidence.verdict = "DELIVERED_UNVERIFIED"
                $evidence.limitations += "owner must read the after-stop PNG for a strict partial prefix of '$stopFiller' and promote this cell; the script never self-certifies pixels"
            } elseif ($frozen -eq $true) {
                $evidence.verdict = "DELIVERED_UNVERIFIED"
                $evidence.limitations += "frozen envelope without post_dispatch/delivery:unknown; record this as unverified, not an abort"
            } else {
                $evidence.verdict = "NOT_RUN"
                $evidence.limitations += "timing miss: the stop landed before dispatch or after completion; retry up to 3 attempts and never count it as an abort"
            }
            Write-Host "STOP-RESULT: frozen=$frozen delivery=$delivery post_dispatch=$postDispatch stop_rows=$($stopRows.Count)"
        }
        "lifecycle" {
            # Focus the dedicated target first so the in-flight request is a real type attempt,
            # not an immediate pre-dispatch refusal.
            $activate = Invoke-Tool $bridge $counter "activate" @{ window = $Window }
            $evidence.steps.Add((Get-StepRecord "activate" $activate)) | Out-Null
            Start-Sleep -Milliseconds 300
            # Disconnect mid-type: terminate ONLY the bridge child this script spawned.
            $typeId = $counter.Next
            $counter.Next = $typeId + 1
            Send-McpLine $bridge @{
                jsonrpc = "2.0"
                id      = $typeId
                method  = "tools/call"
                params  = @{ name = "type"; arguments = @{ text = $stopFiller } }
            }
            Start-Sleep -Milliseconds 200
            $ownPid = $bridge.Pid
            Stop-OwnBridge $bridge
            $transportError = $null
            try {
                $null = Receive-McpId $bridge $typeId 5000
            } catch {
                $transportError = "$($_.Exception.Message)"
            }
            $evidence.steps.Add([ordered]@{
                    step            = "disconnect-mid-type"
                    bridge_pid      = $ownPid
                    transport       = if ($transportError) { "failed" } else { "unexpectedly-answered" }
                    transport_error = $transportError
                    effect          = "unknown"
                }) | Out-Null
            $bridge = $null

            # Disconnect after Enter: a fresh bridge child, key enter, then kill that same child.
            $second = Start-OwnBridge
            $bridge = $second
            $counter = New-BridgeCounter
            $null = Connect-PrivilegedBridge $bridge $counter
            $enterId = $counter.Next
            $counter.Next = $enterId + 1
            Send-McpLine $bridge @{
                jsonrpc = "2.0"
                id      = $enterId
                method  = "tools/call"
                params  = @{ name = "key"; arguments = @{ name = "enter" } }
            }
            Start-Sleep -Milliseconds 150
            $ownPid2 = $bridge.Pid
            Stop-OwnBridge $bridge
            $transportError2 = $null
            try {
                $null = Receive-McpId $bridge $enterId 5000
            } catch {
                $transportError2 = "$($_.Exception.Message)"
            }
            $evidence.steps.Add([ordered]@{
                    step            = "disconnect-after-enter"
                    bridge_pid      = $ownPid2
                    transport       = if ($transportError2) { "failed" } else { "unexpectedly-answered" }
                    transport_error = $transportError2
                    effect          = "unknown"
                }) | Out-Null
            $bridge = $null

            # Owner-only cells: no task start/registration/revoke from this script.
            $evidence.steps.Add([ordered]@{
                    step   = "reconnect"
                    verdict = "NOT_RUN"
                    reason = "owner state change: stop and start the HandsElevated task from an elevated shell (the plan names the exact commands), then re-run -Case lifecycle for a bridge + elevation_status cell"
                    effect = "unknown"
                }) | Out-Null
            $evidence.steps.Add([ordered]@{
                    step   = "revoke"
                    verdict = "NOT_RUN"
                    reason = "owner consent + elevated shell required: unregister the HandsElevated task (the plan names the exact command); then the bridge must fail loud with no local desk and no listener"
                    effect = "unknown"
                }) | Out-Null

            $failed = @($evidence.steps | Where-Object { (Test-Prop $_ "transport") -and $_.transport -eq "failed" })
            $answered = @($evidence.steps | Where-Object { (Test-Prop $_ "transport") -and $_.transport -eq "unexpectedly-answered" })
            if ($answered.Count -gt 0) {
                # The response was already buffered when the bridge died: a timing miss, not a
                # product failure and never a pass.
                $evidence.verdict = "NOT_RUN"
                $evidence.limitations += "a disconnect cell answered before the kill (timing miss); retry the cell instead of counting it"
            } elseif ($failed.Count -ge 2) {
                # Transport failure with an unknown effect is the DoD-7 outcome, not a success claim.
                $evidence.verdict = "DELIVERED_UNVERIFIED"
            } else {
                $evidence.verdict = "FAILED"
                $evidence.limitations += "a disconnect cell did not reach a bridge child; no transport failure was observed"
            }
        }
    }
} catch {
    if (-not $script:abortRecorded) {
        $evidence.limitations += "harness error: $($_.Exception.Message)"
        if ($evidence.verdict -eq "NOT_RUN") { $evidence.verdict = "FAILED" }
    }
} finally {
    if ($null -ne $bridge) { Stop-OwnBridge $bridge }
    $evidence.limitations += "standing elevation path: any process as this user can start the Highest/Interactive HandsElevated task with no UAC prompt; this script did not register, start or revoke the task"
    $null = Save-Evidence $evidence
}

Write-Host "VERDICT: $($evidence.verdict)"
if ($evidence.verdict -in @("FAILED", "NOT_RUN")) { exit 1 }
exit 0
