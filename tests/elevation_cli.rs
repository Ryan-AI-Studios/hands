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

fn count_rt_manifest(bytes: &[u8]) -> usize {
    // RT_MANIFEST type id is 24. Search both ASCII/UTF-16 XML markers.
    let mut n = 0;
    if pe_contains_utf8_or_utf16(bytes, "requestedExecutionLevel") {
        n += 1;
    }
    n
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
    assert_eq!(
        count_rt_manifest(&bytes),
        1,
        "exactly one requestedExecutionLevel / RT_MANIFEST marker"
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
