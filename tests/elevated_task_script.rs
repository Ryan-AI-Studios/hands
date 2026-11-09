//! Spec 0120 §3.6 #16. Runs `hands-elevated-task.ps1 -Inspect` only: nothing is
//! registered, copied, or elevated, and the real `HandsElevated` task is never touched.

use std::path::PathBuf;
use std::process::Command;

fn script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("hands-elevated-task.ps1")
}

fn inspect(trigger: &str) -> (serde_json::Value, String) {
    let exe = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(script_path())
        .arg("-Exe")
        .arg(&exe)
        .args(["-Trigger", trigger, "-Inspect"])
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
    (serde_json::from_str(json_line).unwrap(), stdout)
}

#[test]
fn on_demand_definition_has_no_trigger_and_highest_interactive_principal() {
    let (def, stdout) = inspect("OnDemand");
    assert_eq!(def["TaskName"], "HandsElevated");
    assert_eq!(def["TriggerCount"], 0);
    assert_eq!(def["LogonType"], "Interactive");
    assert_eq!(def["RunLevel"], "Highest");
    assert_eq!(def["ExecutionTimeLimit"], "PT0S");
    assert_eq!(def["MultipleInstances"], "IgnoreNew");
    assert_eq!(def["Arguments"], r"serve --pipe \\.\pipe\hands-elev");
    // The action is the admin-only installed copy, never the source tree.
    let execute = def["Execute"].as_str().unwrap().to_ascii_lowercase();
    assert!(execute.ends_with(r"\hands\hands.exe"), "{execute}");
    assert!(execute.starts_with(&std::env::var("ProgramFiles").unwrap().to_ascii_lowercase()));
    assert!(stdout.contains("STANDING ELEVATION PATH"));
    assert!(stdout.contains("INTERACTIVE LOGON SESSION"));
    assert!(stdout.contains("Unregister-ScheduledTask -TaskName 'HandsElevated' -Confirm:$false"));
    assert!(stdout.contains("schtasks /End /TN HandsElevated"));
}

#[test]
fn at_logon_is_an_explicit_opt_in_with_one_trigger() {
    let (def, _) = inspect("AtLogOn");
    assert_eq!(def["TriggerCount"], 1);
}

#[test]
fn script_never_self_elevates_and_enforces_admin_only_exe() {
    let src = std::fs::read_to_string(script_path()).unwrap();
    assert!(!src.contains("-Verb RunAs"));
    assert!(!src.contains("Start-Process"));
    assert!(src.contains("Get-WritableByUntrusted"));
    assert!(src.contains("Resolve-Path -LiteralPath"));
}
