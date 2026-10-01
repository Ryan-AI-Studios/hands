//! Default cargo PE must launch and stay `uiAccess=false`.

use std::process::Command;

fn pe_contains_utf8_or_utf16(bytes: &[u8], needle: &str) -> bool {
    if bytes.windows(needle.len()).any(|w| w == needle.as_bytes()) {
        return true;
    }
    let utf16: Vec<u8> = needle
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    bytes.windows(utf16.len()).any(|w| w == utf16.as_slice())
}

fn count_needle(bytes: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() {
        return 0;
    }
    bytes.windows(needle.len()).filter(|w| *w == needle).count()
}

fn count_rt_manifest(bytes: &[u8]) -> usize {
    let utf8 = count_needle(bytes, b"requestedExecutionLevel");
    if utf8 > 0 {
        return utf8;
    }
    let utf16: Vec<u8> = "requestedExecutionLevel"
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    count_needle(bytes, &utf16)
}

#[test]
fn count_rt_manifest_counts_repeats_not_boolean() {
    let once = b"xxrequestedExecutionLevelyy";
    let twice = b"requestedExecutionLevel--requestedExecutionLevel";
    assert_eq!(count_rt_manifest(once), 1);
    assert_eq!(count_rt_manifest(twice), 2);
}

#[test]
fn default_cli_elevation_status_launches_and_pe_is_not_uiaccess() {
    let exe = env!("CARGO_BIN_EXE_hands");
    let bytes = std::fs::read(exe).expect("read default PE");
    assert!(
        !pe_contains_utf8_or_utf16(&bytes, r#"uiAccess="true""#),
        "default cargo PE must not declare uiAccess=true"
    );
    assert!(
        !pe_contains_utf8_or_utf16(&bytes, "uiAccess='true'"),
        "default cargo PE must not declare uiAccess=true"
    );
    assert!(
        pe_contains_utf8_or_utf16(&bytes, "asInvoker")
            || pe_contains_utf8_or_utf16(&bytes, "asInvoker"),
        "default PE should still embed asInvoker"
    );
    let manifests = count_rt_manifest(&bytes);
    assert!(
        manifests >= 1,
        "default PE must embed requestedExecutionLevel (count={manifests})"
    );

    let out = Command::new(exe)
        .arg("elevation-status")
        .output()
        .expect("spawn elevation-status");
    assert!(
        out.status.success(),
        "default elevation-status must launch: stderr {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("status json");
    assert!(v.get("exe_path").is_some(), "{stdout}");
    assert!(v.get("integrity").is_some(), "{stdout}");
    assert!(v.get("integrity_rid").is_some(), "{stdout}");
    assert_eq!(
        v.get("uiaccess_granted").and_then(|x| x.as_bool()),
        Some(false)
    );
    assert_eq!(
        v.get("secure_location").and_then(|x| x.as_bool()),
        Some(false)
    );
}

#[test]
fn type_focus_lock_isolates_lease_and_high_il() {
    let root = env!("CARGO_MANIFEST_DIR");
    let src = std::fs::read_to_string(format!("{root}\\src\\actuate.rs")).unwrap();
    let start = src.find("fn type_focus_lock()").expect("type_focus_lock");
    let rest = &src[start..];
    let end = rest.find("fn fg_10()").unwrap_or(rest.len());
    let body = &rest[..end];
    assert!(
        body.contains("lease::TEST_LOCK"),
        "type_focus_lock must take lease::TEST_LOCK:\n{body}"
    );
    assert!(
        body.contains("elevation::TEST_LOCK"),
        "type_focus_lock must take elevation::TEST_LOCK:\n{body}"
    );
    assert!(
        body.contains("set_high_il_hook(Some(|_| false))"),
        "non-High-IL type tests must hook high_il=false:\n{body}"
    );
}

#[test]
fn activate_high_il_tests_exist() {
    let root = env!("CARGO_MANIFEST_DIR");
    let src = std::fs::read_to_string(format!("{root}\\src\\actuate.rs")).unwrap();
    assert!(
        src.contains("fn activate_high_il_ungranted_is_named_refuse()"),
        "activate High-IL ungranted refuse must be locked"
    );
    assert!(
        src.contains("fn activate_high_il_granted_raises()"),
        "activate High-IL granted raise must be locked"
    );
}

#[test]
fn click_high_il_test_hooks_foreground_hwnd() {
    let root = env!("CARGO_MANIFEST_DIR");
    let src = std::fs::read_to_string(format!("{root}\\src\\actuate.rs")).unwrap();
    let needle = "fn click_high_il_ungranted_is_named_refuse_not_elementfrompoint()";
    let start = src.find(needle).expect(needle);
    let rest = &src[start..];
    let end = rest.find("fn yield_machine()").unwrap_or(rest.len());
    let body = &rest[..end];
    assert!(
        body.contains("set_foreground_hwnd_hook"),
        "pixel-click High-IL test must hook FG so unattended NULL cannot skip gate_high_il"
    );
}

#[test]
fn provision_scripts_parse_and_reuse_cert_subject() {
    let root = env!("CARGO_MANIFEST_DIR");
    for name in ["uiaaccess-provision.ps1", "uiaaccess-verify.ps1"] {
        let path = format!("{root}\\scripts\\{name}");
        let src = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("read {path}"));
        if name.contains("provision") {
            assert!(src.contains("CN=HelpingHands UIAccess"));
            assert!(
                src.contains("NonAdmin") || src.contains("non-admin") || src.contains("Non-admin")
            );
            assert!(src.contains("target\\uiaccess"));
            assert!(
                src.contains("Do not rewrite native-host JSON")
                    || src.contains("Do not rewrite native-host")
            );
            assert!(
                src.contains("non-elevated"),
                "provision must warn verify from a non-elevated console"
            );
            assert!(
                src.contains("unsigned") && src.contains("740"),
                "provision must warn that target\\uiaccess PE is unsigned"
            );
        } else {
            assert!(src.contains("--session-id"));
            assert!(src.contains("confirm --domain desktop --category elevated"));
            assert!(src.contains("--mode session"));
            assert!(!src.contains("--mode once"));
            assert!(src.contains("non-elevated"));
            assert!(src.contains("ElementFromPoint"));
            assert!(src.contains(".ok") || src.contains("ok -eq"));
            assert!(
                src.contains("nonzero -ClickX") || src.contains("requires nonzero"),
                "verify must throw when -ClickX/-ClickY are omitted"
            );
            assert!(
                src.contains("Start-Sleep"),
                "verify must dwell so the owner can focus the elevated edit"
            );
            assert!(
                !src.contains("Pass -ClickX"),
                "verify must not silently skip the click half"
            );
        }
        let cmd = format!(
            "$t=$null; $e=$null; [void][System.Management.Automation.Language.Parser]::ParseFile('{}', [ref]$t, [ref]$e); if ($e) {{ $e | ForEach-Object {{ $_.ToString() }}; exit 1 }}",
            path.replace('\'', "''")
        );
        let status = Command::new("powershell")
            .args(["-NoProfile", "-Command", &cmd])
            .status()
            .expect("powershell");
        assert!(status.success(), "parse {name}");
    }
}
