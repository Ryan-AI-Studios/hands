//! Runs the executable extension harness (DoD-3/DoD-6 behavioural evidence).
//!
//! The harness needs Node. CI (`windows-latest`) has it; a local machine without Node skips
//! honestly rather than pretending the extension behaviour was executed.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn extension_node_harness_passes_when_node_is_present() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let harness = root.join("extension").join("tests").join("harness.js");
    assert!(harness.is_file(), "missing harness: {}", harness.display());

    let output = match Command::new("node").arg(&harness).output() {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("node not found; extension harness skipped (owner/CI gate)");
            return;
        }
        Err(err) => panic!("run node extension harness: {err}"),
    };
    assert!(
        output.status.success(),
        "extension harness failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("extension harness: ok"),
        "unexpected harness output: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}
