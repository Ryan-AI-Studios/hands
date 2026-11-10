//! CLI surface for the 0130 `changes` poll. Only the no-request paths run here: the live host is
//! never driven from CI.

use std::process::Command;

fn hands() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hands"))
}

#[test]
fn changes_help_lists_every_flag() {
    let output = hands()
        .args(["changes", "--help"])
        .output()
        .expect("run hands changes --help");
    assert!(output.status.success(), "help must exit 0");
    let text = String::from_utf8_lossy(&output.stdout);
    for flag in ["--since", "--detail", "--session-id", "--timing"] {
        assert!(text.contains(flag), "help must name {flag}");
    }
}

#[test]
fn malformed_since_is_ok_false_and_exits_nonzero_without_a_request() {
    let output = hands()
        .args(["changes", "--since", "not-a-token"])
        .output()
        .expect("run hands changes --since bad");
    assert!(!output.status.success(), "a usage refusal exits nonzero");
    let text = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(text.trim()).expect("one JSON line");
    assert_eq!(value["ok"], serde_json::json!(false));
    assert_eq!(value["schema"], serde_json::json!("hands.changes/v1"));
    assert!(
        value["error"]
            .as_str()
            .is_some_and(|e| e.starts_with("malformed --since")),
        "{value}"
    );
    assert!(
        value.get("resync").is_none(),
        "a usage error is never a resync"
    );
    assert!(value.get("elements").is_none());
    assert!(value.get("screenshot_path").is_none());
}
