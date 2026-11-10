//! Spec 0126 §3.1 / DoD-1. Pins the elevated-shell qualification harness by source scan
//! plus a no-side-effect `-Inspect` run. No bridge is spawned, no window is resolved, no
//! input is sent, and the real `HandsElevated` task is never touched.

use std::path::PathBuf;
use std::process::Command;

fn script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("hands-elevated-qualify.ps1")
}

fn source() -> String {
    std::fs::read_to_string(script_path()).expect("read hands-elevated-qualify.ps1")
}

/// Comment-free source so documentation (`<# … #>` and `#` lines) cannot satisfy or break
/// a lock. The locks are about executable statements.
fn code_only() -> String {
    let src = source();
    let mut out = String::new();
    let mut in_block = false;
    for line in src.lines() {
        let trimmed = line.trim_start();
        if in_block {
            if trimmed.contains("#>") {
                in_block = false;
            }
            continue;
        }
        if trimmed.starts_with("<#") {
            in_block = !trimmed.contains("#>");
            continue;
        }
        if trimmed.starts_with('#') {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn squeezed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn inspect(
    case: &str,
    window: &str,
    evidence_dir: &std::path::Path,
) -> (serde_json::Value, String) {
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(script_path())
        .args(["-Inspect", "-Case", case, "-Window", window])
        .arg("-EvidenceDir")
        .arg(evidence_dir)
        .output()
        .expect("powershell.exe");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let json_line = stdout
        .lines()
        .find(|l| l.starts_with('{'))
        .expect("inspect json line");
    (
        serde_json::from_str(json_line).expect("inspect json"),
        stdout,
    )
}

fn temp_evidence_dir() -> PathBuf {
    std::env::temp_dir().join(format!("hands-qual-inspect-{}", uuid::Uuid::new_v4()))
}

#[test]
fn qualify_inspect_reports_a_fixed_template_and_no_actuation() {
    let dir = temp_evidence_dir();
    let (json, stdout) = inspect("console", "hwnd:1a2b", &dir);
    assert_eq!(json["case"], "console");
    assert_eq!(json["window"], "hwnd:1a2b");
    assert_eq!(json["actuation"], false);
    assert_eq!(json["transport"], r"mcp --bridge \\.\pipe\hands-elev");
    // Exactly one fixed command template; the nonce is the only variable.
    let template = json["fixed_template"].as_str().expect("template");
    assert!(template.contains("Out-File -Encoding ascii"), "{template}");
    assert!(template.contains("hands-qual-"), "{template}");
    let nonce = json["nonce"].as_str().expect("nonce");
    assert_eq!(template.matches(nonce).count(), 2, "{template}");
    assert!(
        stdout.contains("\"actuation\":false"),
        "-Inspect must declare no actuation:\n{stdout}"
    );
    // -Inspect has no side effect: it never creates the evidence directory.
    assert!(
        !dir.exists(),
        "-Inspect must not create the evidence dir: {}",
        dir.display()
    );
}

#[test]
fn qualify_requires_pipe_desk_and_privileged_before_actuation() {
    let code = code_only();
    assert!(
        code.contains("-ne \"pipe-desk\""),
        "the harness must refuse a role other than pipe-desk"
    );
    assert!(
        code.contains("-ne $true"),
        "the harness must require privileged == true"
    );
    assert!(
        code.contains("never falling back to a local desk"),
        "the refusal must name the no-local-desk guarantee"
    );
    let gate = code
        .find("Connect-PrivilegedBridge $bridge $counter")
        .expect("privilege gate call");
    let observe = code
        .find("Get-ObserveEnvelope $bridge $counter")
        .expect("observe call");
    assert!(
        gate < observe,
        "the privilege gate must run before any observe/actuation"
    );
    // The gate itself reads elevation_status through the bridge, not a CLI status call.
    let connect = code
        .find("function Connect-PrivilegedBridge")
        .expect("gate definition");
    let body = &code[connect..gate];
    assert!(
        body.contains("Invoke-Tool $Bridge $Counter \"elevation_status\""),
        "the gate must use the MCP elevation_status tool:\n{body}"
    );
}

#[test]
fn qualify_confirm_is_once_only_and_fence_scoped() {
    let code = squeezed(&code_only());
    assert!(
        code.contains("mode = \"once\""),
        "confirm must send mode=once"
    );
    assert!(
        !code.contains("mode = \"persist\""),
        "the harness must never send a persist allow"
    );
    assert!(
        !code.contains("mode = \"session\""),
        "the harness must never send a session allow"
    );
    assert!(
        code.contains("if (-not $GrantOnceOnFence)"),
        "confirm must be behind the explicit -GrantOnceOnFence switch"
    );
    assert!(
        code.contains("domain = $domain category = $category"),
        "confirm must reuse the exact fence domain and category:\n{code}"
    );
    assert!(
        code.contains("fence did not name a domain and category"),
        "an unnamed fence must refuse to confirm"
    );
    assert!(
        code.contains("fence = \"none\""),
        "a free pass must be recorded as fence:none, never fabricated"
    );
}

#[test]
fn qualify_has_no_free_text_or_elevation_parameters() {
    let code = squeezed(&code_only());
    for banned in ["-Text ", "-Command ", "-Script ", "$Template", "$Text"] {
        assert!(
            !code.contains(banned),
            "the harness must not accept operator-supplied text: {banned}"
        );
    }
    assert!(
        code.contains("[ValidatePattern('^[A-Za-z0-9-]{8,64}$')]"),
        "the nonce charset must be pinned"
    );
    assert!(
        code.contains("$fixedTemplate -f $Nonce"),
        "the typed command must come from the fixed template"
    );
    for forbidden_param in ["RunAs", "Verb"] {
        assert!(
            !code.contains(forbidden_param),
            "no elevation parameter/mode may exist: {forbidden_param}"
        );
    }
}

#[test]
fn qualify_evidence_uses_the_seven_verdict_tokens() {
    let dir = temp_evidence_dir();
    let (json, _) = inspect("lifecycle", "hwnd:1", &dir);
    let tokens: Vec<&str> = json["verdict_tokens"]
        .as_array()
        .expect("verdict_tokens")
        .iter()
        .map(|v| v.as_str().expect("token"))
        .collect();
    assert_eq!(
        tokens,
        vec![
            "OBSERVED_EFFECT",
            "DELIVERED_UNVERIFIED",
            "REFUSED_EXPECTED",
            "UNSUPPORTED",
            "FAILED",
            "NOT_RUN",
            "RECORDED",
        ],
        "the seven §2.4 verdict tokens must be exactly these"
    );
    let fields: Vec<&str> = json["evidence_fields"]
        .as_array()
        .expect("evidence_fields")
        .iter()
        .map(|v| v.as_str().expect("field"))
        .collect();
    for required in [
        "case",
        "client_il",
        "bridge_status",
        "target",
        "steps",
        "artifact",
        "verdict",
        "limitations",
        "build",
    ] {
        assert!(
            fields.contains(&required),
            "evidence must record {required}: {fields:?}"
        );
    }
}

#[test]
fn qualify_never_persists_or_registers_or_reads_secrets() {
    let code = squeezed(&code_only());
    for banned in [
        "Register-ScheduledTask",
        "Unregister-ScheduledTask",
        "schtasks",
        "Start-Process",
        "-Verb RunAs",
        "Read-Host",
        "SecureString",
        "ConvertTo-SecureString",
        "Remove-Item",
        "Invoke-Expression",
        "mode = \"persist\"",
        "mode = \"session\"",
    ] {
        assert!(
            !code.contains(banned),
            "the harness must not contain the executable token {banned}"
        );
    }
    assert!(
        code.contains("UseShellExecute = $false"),
        "the bridge and stop child must be spawned without shell-execute"
    );
    assert!(
        code.contains("[System.Diagnostics.Process]::Start"),
        "the bridge must be a direct Process.Start"
    );
}

#[test]
fn qualify_artifact_must_be_high_labelled_to_pass() {
    let code = squeezed(&code_only());
    assert!(
        code.contains("$artifact.content_matches -and $artifact.mandatory_label -eq \"high\""),
        "OBSERVED_EFFECT must require the exact nonce AND a High mandatory label:\n{code}"
    );
    let high_gate = code
        .find("$artifact.content_matches -and $artifact.mandatory_label -eq \"high\"")
        .expect("high gate");
    let effect = code
        .find("$evidence.verdict = \"OBSERVED_EFFECT\"")
        .expect("OBSERVED_EFFECT assignment");
    assert!(
        high_gate < effect,
        "the OBSERVED_EFFECT assignment must follow the High-label gate"
    );
    assert!(
        code.contains("not High: the target was not proven elevated"),
        "a matching artifact with a non-High label must fail, not pass"
    );
    assert!(
        code.contains("ok:true is not effect"),
        "a tool result alone must never be effect evidence"
    );
}

#[test]
fn qualify_stop_filler_stays_on_the_unicode_type_path() {
    let dir = temp_evidence_dir();
    let (json, _) = inspect("stop", "hwnd:1", &dir);
    let filler = json["stop_filler"].as_str().expect("stop_filler");
    assert!(filler.starts_with('#'), "filler must be inert: {filler}");
    assert!(
        filler.len() <= 32,
        "TYPE_UNICODE_MAX = 32; a longer filler pastes via the clipboard: {filler} ({} chars)",
        filler.len()
    );
    assert!(
        filler.len() >= 8,
        "filler must carry a nonce-shaped tail: {filler}"
    );
    let template = json["fixed_template"].as_str().expect("template");
    assert!(
        filler.len() < template.len(),
        "the stop filler must not be the long command template"
    );
    assert_eq!(json["stop_filler_len"], filler.len());
    assert_eq!(json["stop_filler_path"], "unicode");
    let code = code_only();
    assert!(
        code.contains("TypePath::Clipboard"),
        "the source must name why >32 chars is forbidden"
    );
    assert!(
        code.contains("-gt 32"),
        "the harness must hard-fail a filler longer than 32 chars"
    );
}

#[test]
fn qualify_refuses_a_non_medium_driving_shell() {
    let code = code_only();
    assert!(
        code.contains("$mediumLabelSid = \"S-1-16-8192\""),
        "the medium label SID must be pinned"
    );
    assert!(
        code.contains("if ($clientIl -ne \"medium\")"),
        "every driving case must refuse a non-medium shell"
    );
    assert!(
        code.contains("exit 2"),
        "the refusal must be a hard, distinguishable exit"
    );
    let gate = code
        .find("if ($clientIl -ne \"medium\")")
        .expect("medium gate");
    let spawn = code
        .find("$bridge = Start-OwnBridge")
        .expect("bridge spawn");
    assert!(
        gate < spawn,
        "the medium-shell gate must refuse before the bridge is spawned"
    );
    assert!(
        code.contains("client_il = Get-ClientIntegrity")
            || code.contains("$clientIl = Get-ClientIntegrity"),
        "client_il must come from the script's own label line"
    );
    assert!(
        code.contains("whoami.exe /groups"),
        "the label line must be read from whoami /groups"
    );
}

#[test]
fn qualify_kills_only_its_own_bridge_child() {
    let code = code_only();
    assert_eq!(
        code.matches("Stop-Process").count(),
        1,
        "exactly one termination site is allowed (the scoped kill carve-out)"
    );
    assert!(
        code.contains("Stop-Process -Id $Bridge.Pid -Force"),
        "the only kill must target the bridge child this script spawned"
    );
    assert!(!code.contains("Stop-Process -Name"), "never kill by name");
    assert!(
        !code.contains("Kill("),
        "no other process kill path may exist"
    );
    // The carve-out is documentation at the kill site, so it is checked in the raw source.
    let raw = source();
    assert!(
        raw.contains("Only the bridge child this script spawned itself")
            || raw.contains("the bridge child this script spawned itself"),
        "the carve-out must be stated at the kill site"
    );
    assert!(
        code.contains("$Bridge.Pid = $process.Id") || code.contains("Pid = $process.Id"),
        "the harness must record the pid it spawned"
    );
}
