//! 0127 DoD-1 / DoD-3 / DoD-4 / DoD-5: locks for the desktop qualification scripts.
//!
//! Source scans run on non-comment code and are scoped per script. The behavioural checks run
//! the read-only probe and the matrix runner with `-PlanOnly` (no cargo, no desk).

use std::path::PathBuf;
use std::process::Command;

fn scripts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts")
}

fn read(name: &str) -> String {
    std::fs::read_to_string(scripts_dir().join(name)).unwrap_or_else(|e| panic!("read {name}: {e}"))
}

/// Non-comment code only: `<# … #>` blocks and `#` lines are documentation.
fn code_only(src: &str) -> String {
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

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hands-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn run_ps(script: &str, args: &[&str]) -> std::process::Output {
    Command::new("pwsh")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(scripts_dir().join(script))
        .args(args)
        .output()
        .expect("pwsh")
}

/// The four scripts that may appear in the 0127 scan set, with the 0126 script included.
const SCAN_SET: [&str; 4] = [
    "hands-deploy-probe.ps1",
    "hands-qualify-desktop.ps1",
    "hands-rollout-drill.ps1",
    "hands-elevated-qualify.ps1",
];

#[test]
fn no_task_registration_elevation_or_policy_change_in_scan_set() {
    let forbidden = [
        "-Verb RunAs",
        "Set-ExecutionPolicy",
        "Register-ScheduledTask",
        "Unregister-ScheduledTask",
        "schtasks /Create",
        "schtasks /Change",
        "schtasks /End",
        "schtasks /Delete",
        "schtasks.exe /Create",
        "schtasks.exe /Change",
        "schtasks.exe /End",
        "schtasks.exe /Delete",
    ];
    for script in SCAN_SET {
        let code = code_only(&read(script));
        for needle in forbidden {
            assert!(
                !code.contains(needle),
                "{script} must not contain `{needle}` outside comments"
            );
        }
    }
}

#[test]
fn stop_process_is_forbidden_except_the_one_bridge_kill() {
    for script in [
        "hands-deploy-probe.ps1",
        "hands-qualify-desktop.ps1",
        "hands-rollout-drill.ps1",
    ] {
        assert!(
            !code_only(&read(script)).contains("Stop-Process"),
            "{script} must never call Stop-Process"
        );
    }
    // 0126 O4 carve-out: the exact line, exactly once. A second occurrence fails.
    let code = code_only(&read("hands-elevated-qualify.ps1"));
    let exact = "Stop-Process -Id $Bridge.Pid -Force -ErrorAction SilentlyContinue";
    assert_eq!(
        code.matches("Stop-Process").count(),
        1,
        "hands-elevated-qualify.ps1 may name Stop-Process exactly once"
    );
    assert!(
        code.contains(exact),
        "the one allowed line must be the bridge kill"
    );
}

#[test]
fn probe_is_read_only_apart_from_its_out_file() {
    let code = code_only(&read("hands-deploy-probe.ps1"));
    for needle in [
        "Set-",
        "New-ItemProperty",
        "Remove-Item",
        "Copy-Item",
        "Move-Item",
        "Start-Process",
        "Start-ScheduledTask",
        "reg add",
        "reg delete",
        "[System.IO.File]::Replace",
        "Stop-Process",
        "Register-ScheduledTask",
    ] {
        assert!(!code.contains(needle), "probe must not contain `{needle}`");
    }
    assert_eq!(
        code.matches("[System.IO.File]::WriteAllText($Out").count(),
        1,
        "the probe's one permitted write is the -Out file"
    );
}

#[test]
fn drill_is_confined_to_its_own_temp_directory() {
    let src = read("hands-rollout-drill.ps1");
    let code = code_only(&src);
    for needle in [
        "Program Files",
        "ProgramFiles",
        "schtasks",
        "Start-Process",
        "Stop-Process",
        "TcpListener",
        "HttpListener",
        "Listen(",
    ] {
        assert!(!code.contains(needle), "drill must not contain `{needle}`");
    }
    assert!(
        code.contains("GetTempPath()"),
        "the drill must root itself in the temp folder"
    );
}

#[test]
fn no_listener_in_any_new_script() {
    for script in [
        "hands-deploy-probe.ps1",
        "hands-qualify-desktop.ps1",
        "hands-rollout-drill.ps1",
    ] {
        let code = code_only(&read(script));
        for needle in [
            "TcpListener",
            "HttpListener",
            "System.Net.Sockets",
            "Listen(",
        ] {
            assert!(
                !code.contains(needle),
                "{script} must not open a listener (`{needle}`)"
            );
        }
    }
}

#[test]
fn qualify_runner_declares_exactly_gate_and_matrix_and_runs_serial_tests() {
    let code = code_only(&read("hands-qualify-desktop.ps1"));
    assert!(
        code.contains(r#"[ValidateSet("gate", "matrix")]"#),
        "-Mode must be exactly gate and matrix"
    );
    assert!(
        code.contains(r#""--test-threads=1""#),
        "the gate must pass --test-threads=1 to cargo test"
    );
}

#[test]
fn matrix_plan_marks_high_il_dynamic_arms_untested_with_the_reason() {
    let dir = temp_dir("qual-high");
    let out = run_ps(
        "hands-qualify-desktop.ps1",
        &[
            "-Mode",
            "matrix",
            "-Out",
            dir.to_str().unwrap(),
            "-ElevationOverride",
            "high",
            "-PlanOnly",
            "-Rows",
            "Q01",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("matrix.json")).expect("matrix.json"),
    )
    .expect("matrix json");
    let records = json["records"].as_array().expect("records");
    let dynamic = records
        .iter()
        .find(|r| r["cell"].as_str().unwrap_or("").contains("restricted pipe"))
        .expect("Q01 dynamic arm record");
    assert_eq!(dynamic["row"], "Q01");
    assert_eq!(dynamic["status"], "UNTESTED", "{dynamic}");
    assert!(
        dynamic["evidence"]
            .as_str()
            .unwrap_or("")
            .contains("High IL: squat/GENERIC_WRITE arms need a medium caller"),
        "{dynamic}"
    );
    assert_eq!(json["elevation"]["privileged"], true);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn matrix_never_records_a_row_as_pass() {
    let dir = temp_dir("qual-nopass");
    let out = run_ps(
        "hands-qualify-desktop.ps1",
        &[
            "-Mode",
            "matrix",
            "-Out",
            dir.to_str().unwrap(),
            "-ElevationOverride",
            "medium",
            "-PlanOnly",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("matrix.json")).expect("matrix.json"),
    )
    .expect("matrix json");
    let records = json["records"].as_array().expect("records");
    assert!(!records.is_empty());
    for r in records {
        assert_ne!(r["status"], "PASS", "matrix must never claim PASS: {r}");
    }
    // Every row Q01-Q19 is present (the matrix is complete, even where N/A or UNTESTED).
    for n in 1..=19 {
        let row = format!("Q{n:02}");
        assert!(
            records.iter().any(|r| r["row"] == row.as_str()),
            "row {row} missing from matrix.json"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn probe_writes_exactly_one_file_and_reports_the_five_facts() {
    let dir = temp_dir("probe");
    let out_file = dir.join("probe.json");
    let out = run_ps(
        "hands-deploy-probe.ps1",
        &["-Out", out_file.to_str().unwrap()],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let entries: Vec<_> = std::fs::read_dir(&dir).expect("read dir").collect();
    assert_eq!(entries.len(), 1, "the probe may write only its -Out file");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out_file).expect("probe.json"))
            .expect("probe json");
    assert_eq!(json["read_only"], true);
    assert_eq!(
        json["run_version"], false,
        "default probe starts no process for --version"
    );
    assert!(json["installed"].is_object(), "installed facts");
    assert!(json["dev"].is_object(), "dev facts");
    assert!(json["task"].is_object(), "task facts");
    assert!(json["processes"].is_array(), "process facts");
    assert!(json["native_host"].is_object(), "native host facts");
    assert!(json["desk_mutex_present"].is_boolean());
    assert!(json["pipe_present"].is_boolean());
    let _ = std::fs::remove_dir_all(&dir);
}
