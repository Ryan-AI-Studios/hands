//! CLI help/schema for the 0129 transform vocabulary. No live drive.

use std::process::Command;

fn hands() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hands"))
}

#[test]
fn observe_help_names_transform_block() {
    let output = hands()
        .args(["observe", "--help"])
        .output()
        .expect("run hands observe --help");
    assert!(output.status.success(), "help must exit 0");
    let text = String::from_utf8_lossy(&output.stdout);
    for needle in ["transform", "valid:false", "observe_id", "page_viewport"] {
        assert!(
            text.contains(needle),
            "observe help missing {needle}:\n{text}"
        );
    }
}

#[test]
fn click_help_names_point_contract_and_verified_meanings() {
    let output = hands()
        .args(["click", "--help"])
        .output()
        .expect("run hands click --help");
    assert!(output.status.success(), "help must exit 0");
    let text = String::from_utf8_lossy(&output.stdout);
    for needle in [
        "PointContract",
        "physical:null",
        "verified",
        "dispatch delivered",
    ] {
        assert!(
            text.contains(needle),
            "click help missing {needle}:\n{text}"
        );
    }
}

#[test]
fn hover_and_scroll_help_name_the_same_guard() {
    for (cmd, needle) in [
        ("hover", "validated point contract"),
        ("scroll", "same point contract"),
    ] {
        let output = hands()
            .args([cmd, "--help"])
            .output()
            .expect("run hands help");
        assert!(output.status.success(), "{cmd} help must exit 0");
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            text.contains(needle),
            "{cmd} help missing {needle}:\n{text}"
        );
    }
}
