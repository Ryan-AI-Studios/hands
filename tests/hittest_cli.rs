//! CLI/schema/source locks for 0128 `hittest` and `click --require-interactable`.
//! No live Chrome drive.

use std::process::Command;

fn hands() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hands"))
}

#[test]
fn hittest_help_names_targets_reasons_and_honesty() {
    let output = hands()
        .args(["hittest", "--help"])
        .output()
        .expect("run hands hittest --help");
    assert!(output.status.success(), "help must exit 0");
    let text = String::from_utf8_lossy(&output.stdout);
    for needle in [
        "--element-id",
        "--grid",
        "--x",
        "--y",
        "--samples",
        "interactable:null",
        "top frame",
        "No input",
        "native-window occlusion",
    ] {
        assert!(
            text.contains(needle),
            "hittest help missing {needle}:\n{text}"
        );
    }
}

#[test]
fn hittest_rejects_two_target_selectors() {
    let output = hands()
        .args(["hittest", "--element-id", "chr:0", "--x", "1", "--y", "2"])
        .output()
        .expect("run hands hittest");
    assert!(
        !output.status.success(),
        "mutually exclusive selectors must fail"
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("exactly one of"), "{text}");
}

#[test]
fn hittest_rejects_uia_element_mode() {
    let output = hands()
        .args(["hittest", "--element-id", "uia:1.2"])
        .output()
        .expect("run hands hittest");
    assert!(!output.status.success(), "uia: is not a DOM target");
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(text.contains("interactable_unavailable"), "{text}");
}

#[test]
fn hittest_dispatches_without_input_main_or_lease() {
    let src = include_str!("../src/main.rs");
    let dispatch = src.find("Command::Hittest {").expect("hittest dispatch");
    let fallback = src.find("other => {").expect("input_main fallback");
    assert!(
        dispatch < fallback,
        "hittest must dispatch like Observe/ElevationStatus, not input_main"
    );
    assert!(
        src.contains("| Command::Hittest { .. }"),
        "hittest must be unreachable inside input_main"
    );
    let start = src.find("fn hittest_main").expect("hittest_main");
    let end = src[start..]
        .find("fn native_host_manifest_main")
        .map(|i| start + i)
        .expect("next fn");
    let body = &src[start..end];
    assert!(!body.contains("lease::install"), "{body}");
    assert!(!body.contains("ensure_dpi"), "{body}");
}

#[test]
fn click_help_advertises_require_interactable() {
    let output = hands()
        .args(["click", "--help"])
        .output()
        .expect("run hands click --help");
    assert!(output.status.success(), "help must exit 0");
    let text = String::from_utf8_lossy(&output.stdout);
    for needle in [
        "--require-interactable",
        "chr:/sref1:c only",
        "interactable_checked",
    ] {
        assert!(
            text.contains(needle),
            "click help missing {needle}:\n{text}"
        );
    }
}

#[test]
fn hittest_envelope_schema_is_documented() {
    let src = include_str!("../src/hittest.rs");
    for needle in [
        "pub struct HittestEnvelope",
        "pub struct Assessment",
        "pub struct AtPoint",
        "pub enum AssessmentConfidence",
        "interactable: Option<bool>",
    ] {
        assert!(src.contains(needle), "hittest.rs missing {needle}");
    }
}
