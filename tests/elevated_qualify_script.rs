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
    // The target must set the High label itself (F-B1): a plain elevated write is not labelled.
    assert!(
        template.contains("icacls") && template.contains("/setintegritylevel High"),
        "the fixed template must set the High label from the target: {template}"
    );
    let nonce = json["nonce"].as_str().expect("nonce");
    assert_eq!(template.matches(nonce).count(), 3, "{template}");
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
        "Set-ScheduledTask",
        "Enable-ScheduledTask",
        "Disable-ScheduledTask",
        "Start-ScheduledTask",
        "Stop-ScheduledTask",
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
    // Case-insensitive, so `runas` and the `start` process alias cannot slip in either (F-l7).
    let lower = code.to_ascii_lowercase();
    for banned in ["runas", "start-process"] {
        assert!(
            !lower.contains(banned),
            "banned (case-insensitive): {banned}"
        );
    }
    for line in code.lines() {
        let trimmed = line.trim_start().to_ascii_lowercase();
        assert!(
            !trimmed.starts_with("start "),
            "the `start` alias must not be used as a command: {line}"
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

/// Mandatory-label RID of a path (S-1-16-<rid>), read from the SACL label ACE. This is the reader
/// the harness needs: an unlabelled file returns `None`, never a guessed High.
fn label_rid(path: &std::path::Path) -> Option<u32> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{ERROR_SUCCESS, HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows::Win32::Security::{
        ACL, GetAce, GetSecurityDescriptorSacl, GetSidSubAuthority, GetSidSubAuthorityCount,
        LABEL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SYSTEM_MANDATORY_LABEL_ACE,
    };

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    let mut sacl: *mut ACL = std::ptr::null_mut();
    let status = unsafe {
        GetNamedSecurityInfoW(
            windows::core::PCWSTR(wide.as_ptr()),
            SE_FILE_OBJECT,
            LABEL_SECURITY_INFORMATION,
            None,
            None,
            None,
            Some(&raw mut sacl),
            &raw mut sd,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let mut present = windows::core::BOOL::default();
    let mut defaulted = windows::core::BOOL::default();
    let mut label_acl: *mut ACL = std::ptr::null_mut();
    let mut rid = None;
    if unsafe {
        GetSecurityDescriptorSacl(sd, &raw mut present, &raw mut label_acl, &raw mut defaulted)
    }
    .is_ok()
        && present.as_bool()
        && !label_acl.is_null()
    {
        let mut ace = std::ptr::null_mut();
        if unsafe { GetAce(label_acl, 0, &raw mut ace) }.is_ok() {
            let label = unsafe { &*ace.cast::<SYSTEM_MANDATORY_LABEL_ACE>() };
            let sid = PSID(std::ptr::from_ref(&label.SidStart).cast_mut().cast());
            unsafe {
                let count = *GetSidSubAuthorityCount(sid);
                if count > 0 {
                    rid = Some(*GetSidSubAuthority(sid, u32::from(count) - 1));
                }
            }
        }
    }
    unsafe {
        let _ = LocalFree(Some(HLOCAL(sd.0)));
    }
    rid
}

/// Every input call must be preceded (within `window` chars) by a foreground/target bind.
fn assert_bound_before(code: &str, call: &str, guard: &str, window: usize) {
    let mut search_from = 0;
    while let Some(found) = code[search_from..].find(call) {
        let at = search_from + found;
        let before = &code[..at];
        let guard_at = before
            .rfind(guard)
            .unwrap_or_else(|| panic!("{call} at {at} has no preceding {guard}"));
        assert!(
            at - guard_at <= window,
            "{call} at {at} is not bound: last {guard} at {guard_at}"
        );
        search_from = at + call.len();
    }
}

#[test]
fn high_label_set_step_is_present_and_the_reader_reads_it() {
    // F-B1: a plain elevated write is NOT High-labelled, so the target's fixed template must set the
    // label itself, and the harness must read that label back rather than guess.
    let dir = temp_evidence_dir();
    let (json, _) = inspect("console", "hwnd:1", &dir);
    let template = json["fixed_template"].as_str().expect("template");
    let write_at = template.find("Out-File").expect("write step");
    let label_at = template
        .find("/setintegritylevel High")
        .expect("label-set step in the fixed template");
    assert!(
        write_at < label_at,
        "the target must write the artifact then set its High label: {template}"
    );

    let raw = source();
    assert!(
        raw.contains("GetNamedSecurityInfoW") && raw.contains("MandatoryLabelSid"),
        "the label reader must read the SACL label RID, not parse a guess"
    );
    assert!(
        raw.contains("LABEL_SECURITY_INFORMATION = 0x10"),
        "the reader must name the label security information class"
    );

    // The reader itself: a plain temp file has no explicit label, and the same set step the
    // template uses makes the reader return the real High RID *when the caller can raise a label*.
    let path = std::env::temp_dir().join(format!("hands-label-{}.txt", uuid::Uuid::new_v4()));
    std::fs::write(&path, b"PROBE").expect("write probe");
    assert_eq!(
        label_rid(&path),
        None,
        "a plain write must not read as High (this is the F-B1 fact)"
    );
    let il = hands::elevation::process_integrity_rid().expect("process integrity");
    if il >= hands::elevation::HIGH_IL_RID {
        let out = Command::new("icacls")
            .arg(&path)
            .args(["/setintegritylevel", "High"])
            .output()
            .expect("icacls");
        assert!(
            out.status.success(),
            "icacls set High failed from a High token: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            label_rid(&path),
            Some(0x3000),
            "the reader must read the High label the template sets"
        );
    } else {
        // N2: raising a label above the caller's IL needs SeRelabelPrivilege, which a UAC-filtered
        // token does not hold. The plain-write half and the template-presence locks above stay
        // unconditional; CI (windows-latest) and the owner's High shell exercise the raise.
        eprintln!(
            "skipping the High label raise: caller IL 0x{il:x} < 0x{:x} (needs SeRelabelPrivilege); \
             the template-presence and plain-write locks still ran",
            hands::elevation::HIGH_IL_RID
        );
        assert_eq!(label_rid(&path), None, "the plain write stays unlabelled");
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn qualify_binds_the_foreground_target_before_every_input() {
    let code = code_only();
    assert!(
        code.contains("function Assert-ForegroundTarget"),
        "the harness must re-resolve the target before input"
    );
    assert!(
        code.contains("target-binding:"),
        "a binding mismatch must be a named failure"
    );
    assert!(
        code.contains("throw \"activate-not-foregrounded:"),
        "a failed activate must refuse, not continue"
    );
    // Every type/key dispatch (tool or direct line) is bound within the same block.
    assert_bound_before(
        &code,
        "Invoke-WithOptionalOnceConfirm $bridge $counter \"type\"",
        "Assert-ForegroundTarget $bridge",
        400,
    );
    assert_bound_before(
        &code,
        "Invoke-WithOptionalOnceConfirm $bridge $counter \"key\"",
        "Assert-ForegroundTarget $bridge",
        400,
    );
    assert_bound_before(
        &code,
        "params  = @{ name = \"type\"; arguments = @{ text = $stopFiller } }",
        "Assert-ForegroundTarget $bridge",
        400,
    );
    assert_bound_before(
        &code,
        "params  = @{ name = \"key\"; arguments = @{ name = \"enter\" } }",
        "Assert-ForegroundTarget $bridge",
        400,
    );
    // The stop case must activate + click before the in-flight type (F-M2).
    let stop = code
        .split("\"stop\" {")
        .nth(1)
        .expect("stop case")
        .split("\"lifecycle\" {")
        .next()
        .expect("stop case end");
    assert!(stop.contains("\"activate\""), "stop must activate");
    assert!(
        stop.contains("Invoke-Tool $bridge $counter \"click\""),
        "stop must click the target before typing"
    );
}

#[test]
fn qualify_stop_and_lifecycle_cells_are_bounded_and_named() {
    let code = squeezed(&code_only());
    // F-l4/N3: honour HANDS_LOGS_DIR, record the real desk-side gap, enforce last-case.
    assert!(code.contains("function Get-DeskLogPath"));
    assert!(code.contains("$env:HANDS_LOGS_DIR"));
    assert!(code.contains("since_last_action_ms = $sinceLastAction"));
    assert!(code.contains("desk_log_gap_ms = $deskLogGap"));
    assert!(code.contains("precondition_ms = 2000"));
    assert!(code.contains("the desk log changed $deskLogGap ms ago"));
    assert!(code.contains("stop-case-completed.json"));
    assert!(
        code.contains("stop must be the last case of a session"),
        "the harness must refuse a case after stop"
    );
    // O-A: a pre-existing artifact at the nonce path can never be this run's effect.
    assert!(code.contains("$evidence.artifact_preexisting = $artifactAtStart"));
    assert!(code.contains("artifact-preexisting:"));
    // F-l2: the lifecycle disconnect is the named own-bridge pid kill.
    assert!(code.contains("disconnect_method = \"own-bridge-pid-kill\""));
    assert!(code.contains("Stop-OwnBridge $bridge -KillNow"));
    // N4: an unknown-effect disconnect carries effect:"unknown" (mapping recorded in spec §2.4).
    assert!(code.contains("disconnect effect is UNKNOWN"));
    assert!(code.contains("$evidence.effect = \"unknown\""));
    // F-m1 / F-l3: a frozen-only (pre-dispatch) refusal is NOT_RUN, not DELIVERED_UNVERIFIED.
    let pre = code
        .find("the stop landed before dispatch (frozen without post_dispatch/delivery:unknown)")
        .expect("pre-dispatch timing-miss branch");
    let prior = &code[pre - 400.min(pre)..pre];
    assert!(
        prior.contains("$evidence.verdict = \"NOT_RUN\""),
        "a pre-dispatch frozen refusal must map to NOT_RUN"
    );
    // A non-pass verdict exits non-zero.
    assert!(
        code.contains("$passVerdicts = @(\"OBSERVED_EFFECT\", \"REFUSED_EXPECTED\", \"UNSUPPORTED\", \"RECORDED\")"),
        "only positive tokens may exit zero"
    );
    assert!(code.contains("if ($evidence.verdict -notin $passVerdicts) { exit 1 }"));
    // F-m2: only the 0115 focus gate maps to UNSUPPORTED in the terminal case.
    let gate = code
        .find("focused control is not an edit, combo, or document")
        .expect("focus-gate guard");
    let unsupported = code[gate..]
        .find("$evidence.verdict = \"UNSUPPORTED\"")
        .expect("UNSUPPORTED assignment");
    assert!(
        unsupported <= 400,
        "UNSUPPORTED must stay inside the focus-gate guard"
    );
    // F-l1: the reader prefers the SACL reader and falls back to the label text.
    assert!(code.contains("[HandsMandatoryLabel]::MandatoryLabelSid"));
    assert!(code.contains("Mandatory Label"));
}

#[test]
fn qualify_absent_server_pass_is_lifecycle_only() {
    // N1: the absent-server bridge refusal is a DoD-7 (`-Case lifecycle`) cell. Every other case
    // must record NOT_RUN (non-pass, exit 1), never the pass token REFUSED_EXPECTED.
    let code = squeezed(&code_only());
    let pass_token = code
        .find("$evidence.verdict = \"REFUSED_EXPECTED\"")
        .expect("absent-server pass token");
    let before = &code[pass_token.saturating_sub(400)..pass_token];
    assert!(
        before.contains("if ($Case -eq \"lifecycle\")"),
        "REFUSED_EXPECTED must be gated on -Case lifecycle:\n{before}"
    );
    let after = &code[pass_token..(pass_token + 500).min(code.len())];
    assert!(
        after.contains("$evidence.verdict = \"NOT_RUN\""),
        "a non-lifecycle absent-server must record NOT_RUN:\n{after}"
    );
    assert!(
        !code.contains("\"REFUSED_EXPECTED\", \"UNSUPPORTED\", \"RECORDED\", \"NOT_RUN\""),
        "NOT_RUN must never join the pass tokens"
    );
}
