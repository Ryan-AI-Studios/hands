//! CLI surface and fixture-mode shape for the 0125 `find` tool. No live drive, no extension.

use std::path::PathBuf;
use std::process::Command;

fn hands() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hands"))
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

const CASES_REF: &str = "sref1:c.c.0.7a1b.feedface.0";

#[test]
fn find_help_lists_every_flag() {
    let output = hands()
        .args(["find", "--help"])
        .output()
        .expect("run hands find --help");
    assert!(output.status.success(), "help must exit 0");
    let text = String::from_utf8_lossy(&output.stdout);
    for flag in [
        "--within",
        "--window",
        "--ref",
        "--role",
        "--text",
        "--rect",
        "--rect-css",
        "--subtree",
        "--depth",
        "--limit",
        "--offset",
        "--no-caps",
        "--assess",
        "--budget-ms",
        "--max-nodes",
        "--timeout-ms",
        "--session-id",
    ] {
        assert!(text.contains(flag), "find help missing {flag}:\n{text}");
    }
    for needle in ["page_complete", "snapshot", "read-only"] {
        assert!(text.contains(needle), "find help missing {needle}");
    }
}

#[test]
fn bad_filter_exits_one_with_invalid_filter() {
    let output = hands()
        .args(["find", "--within", CASES_REF, "--rect", "1,2,3,4"])
        .output()
        .expect("run hands find");
    assert_eq!(output.status.code(), Some(1));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("invalid_filter"), "{text}");
}

#[test]
fn within_and_ref_together_are_refused() {
    let output = hands()
        .args(["find", "--within", CASES_REF, "--ref", CASES_REF])
        .output()
        .expect("run hands find");
    assert_eq!(output.status.code(), Some(1));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("invalid_scope"), "{text}");
}

#[test]
fn fixture_list_query_prints_the_documented_shape() {
    let output = hands()
        .env("HANDS_CHROME_SNAPSHOT", fixture("find-cases.json"))
        .args([
            "find",
            "--within",
            CASES_REF,
            "--role",
            "button",
            "--session-id",
            "s-find-cli",
        ])
        .output()
        .expect("run hands find");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("find envelope json");
    assert_eq!(json["ok"], true);
    assert_eq!(json["scope"]["kind"], "chrome_snapshot");
    assert_eq!(json["scope"]["doc"], "7a1b");
    assert_eq!(json["coverage"]["scope_source"], "fixture");
    assert_eq!(json["coverage"]["page_complete"], false);
    assert_eq!(json["matches_total"], 3);
    assert_eq!(json["matches_returned"], 3);
    assert_eq!(json["matches_truncated"], false);
    assert_eq!(json["paging"], "snapshot");
    let matches = json["matches"].as_array().expect("matches");
    assert_eq!(matches.len(), 3);
    assert_eq!(matches[0]["ref"], "sref1:c.c.0.7a1b.feedface.0");
    assert_eq!(matches[0]["idx"], 0);
    assert_eq!(matches[0]["role"], "Button");
    assert_eq!(matches[0]["key_ok"], true);
    // The one authorized wire shape: `ref`, no compact alias.
    assert!(matches[0].get("r").is_none());
    // Chrome list mode never carries a physical rect.
    assert!(matches[0].get("rect").is_none());
}

#[test]
fn zero_matches_never_claim_page_absence() {
    let output = hands()
        .env("HANDS_CHROME_SNAPSHOT", fixture("find-cases.json"))
        .args(["find", "--within", CASES_REF, "--text", "zzz-nothing-here"])
        .output()
        .expect("run hands find");
    assert!(output.status.success());
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("find envelope json");
    assert_eq!(json["matches_total"], 0);
    assert_eq!(json["coverage"]["page_complete"], false);
    assert!(json["coverage"].get("reason").is_none());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(!text.contains("absent"), "{text}");
}
