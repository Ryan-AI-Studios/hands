//! CLI surface for `hands uia-action`. No desktop dependency: the JSON shape
//! test builds an envelope directly, and the run test only requires that the
//! command emits a semantic envelope (resolved or refused) rather than crashing.

use std::process::Command;

fn hands() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hands"))
}

#[test]
fn uia_action_help_lists_actions() {
    let output = hands()
        .args(["uia-action", "--help"])
        .output()
        .expect("run hands uia-action --help");
    assert!(output.status.success(), "help must exit 0");
    let text = String::from_utf8_lossy(&output.stdout);
    for needle in ["--element-id", "--action", "--value", "--timeout-ms"] {
        assert!(text.contains(needle), "help missing {needle}:\n{text}");
    }
    assert!(
        text.contains("set-value") || text.contains("set_value"),
        "help must name the actions:\n{text}"
    );
}

#[test]
fn uia_action_unknown_action_exits_nonzero() {
    let output = hands()
        .args(["uia-action", "--element-id", "uia:1.2", "--action", "press"])
        .output()
        .expect("run hands uia-action");
    assert!(!output.status.success(), "unknown action must exit nonzero");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown uia action"),
        "stderr must name the action:\n{stderr}"
    );
}

#[test]
fn uia_action_set_value_without_value_fails() {
    let output = hands()
        .args([
            "uia-action",
            "--element-id",
            "uia:1.2",
            "--action",
            "set-value",
        ])
        .output()
        .expect("run hands uia-action");
    assert!(!output.status.success(), "missing value must exit nonzero");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("requires --value"),
        "stderr must explain the value:\n{stderr}"
    );
}

#[test]
fn uia_action_rejects_non_uia_target() {
    let output = hands()
        .args(["uia-action", "--element-id", "chr:3", "--action", "invoke"])
        .output()
        .expect("run hands uia-action");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("must start with uia:"),
        "stderr must refuse other ids:\n{stderr}"
    );
}

#[test]
fn uia_action_json_envelope_has_flat_semantic_fields() {
    // Build the envelope the CLI prints and assert the flattened wire shape.
    let mut env = hands::ActuateEnvelope {
        session_id: "cli-shape".into(),
        ok: true,
        frozen: false,
        target: hands::actuate::ActuateTarget {
            kind: "element".into(),
            id: Some("uia:1.2".into()),
            x: 10,
            y: 20,
        },
        retried: false,
        settled: false,
        foregrounded: false,
        error: None,
        fence: None,
        challenge: None,
        roi: None,
        miss: None,
        navigated: false,
        attempt: None,
        cooldown_ms: None,
        loop_suspected: false,
        guidance: None,
        verified: false,
        semantic: Some(hands::uia_actions::SemanticInfo {
            action: "toggle".into(),
            delivery: hands::uia_actions::Delivery::Delivered,
            verify: Some("state_readback".into()),
            foreground_changed: false,
            pointer_moved: false,
        }),
    };
    env.ok = true;
    let json = hands::actuate::serialize_envelope(&env).expect("serialize");
    let value: serde_json::Value = serde_json::from_str(&json).expect("json");
    assert_eq!(value["action"], "toggle");
    assert_eq!(value["delivery"], "delivered");
    assert_eq!(value["verify"], "state_readback");
    assert_eq!(value["foreground_changed"], false);
    assert_eq!(value["pointer_moved"], false);
    assert!(value.get("semantic").is_none(), "wire shape must stay flat");
    // Non-semantic envelopes carry none of these keys.
    let plain = hands::actuate::ActuateEnvelope {
        semantic: None,
        ..env
    };
    let plain_json = hands::actuate::serialize_envelope(&plain).expect("serialize");
    assert!(!plain_json.contains("\"action\""), "{plain_json}");
    assert!(!plain_json.contains("\"delivery\""), "{plain_json}");
}
