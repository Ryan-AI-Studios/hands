//! 0127 DoD-7 (automated half): runs `scripts/hands-rollout-drill.ps1` in the serial gate.
//! The drill works only inside a temp directory it creates; it never touches Program Files,
//! never registers or starts a task, and never runs a listener. This test checks the hashes
//! it reports and that its temp directory is gone afterwards.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn rollout_drill_swaps_and_restores_with_recorded_hashes() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("hands-rollout-drill.ps1");
    let out = Command::new("pwsh")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .output()
        .expect("pwsh");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "drill failed: stdout={stdout} stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("drill json");

    assert_eq!(json["schema"], "hands.rollout-drill/v1");
    assert_eq!(json["ok"], true, "{json}");
    let previous = json["previous_sha256"].as_str().expect("previous hash");
    let candidate = json["candidate_sha256"].as_str().expect("candidate hash");
    assert_ne!(previous, candidate, "the drill needs two distinct images");
    assert_eq!(
        json["installed_after_swap_sha256"], candidate,
        "swap installs the candidate"
    );
    assert_eq!(
        json["installed_after_rollback_sha256"], previous,
        "rollback restores the previous image"
    );
    assert_eq!(
        json["refused_bad_stage"], true,
        "a wrong-hash stage must be refused"
    );
    assert_eq!(
        json["temp_removed"], true,
        "the drill removes its own temp directory"
    );

    let temp_dir = json["temp_dir"].as_str().expect("temp dir");
    assert!(
        std::path::Path::new(temp_dir).starts_with(std::env::temp_dir()),
        "the drill ran outside the temp folder: {temp_dir}"
    );
    assert!(
        !std::path::Path::new(temp_dir).exists(),
        "temp dir left behind: {temp_dir}"
    );
}
