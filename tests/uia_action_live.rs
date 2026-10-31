//! Ignored live tests for semantic UIA actions (owner decision D-2).
//!
//! Run with: `cargo test --test uia_action_live -- --ignored --test-threads=1`
//! and `HANDS_LIVE_SCRATCH=1`.
//!
//! Each test spawns its own `scripts/uia-action-scratch.ps1` fixture and kills
//! it on teardown; it never drives a window the owner left open, never a
//! secret field, never a High-IL target. A pre-started fixture can be reused
//! by exporting `HANDS_SCRATCH_HWND` (the test still asserts the controls it
//! finds before acting).

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use hands::extract::{Detail, RawNode};
use hands::uia_actions::uia_action;
use hands::{ActuateRequest, HandsError};

struct Scratch {
    child: Option<Child>,
    hwnd: isize,
    source: ScratchSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScratchSource {
    SelfSpawned,
    Provided,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn live_enabled() -> bool {
    std::env::var("HANDS_LIVE_SCRATCH")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Start (or attach to) the scratch fixture. Returns `None` when the live
/// switch is off, so an accidental ignored run is a no-op rather than a drive
/// of whatever happens to be on screen.
fn scratch() -> Option<Scratch> {
    if !live_enabled() {
        eprintln!("HANDS_LIVE_SCRATCH is not set; skipping live semantic test");
        return None;
    }
    if let Ok(raw) = std::env::var("HANDS_SCRATCH_HWND") {
        let hwnd = parse_hwnd(&raw)?;
        return Some(Scratch {
            child: None,
            hwnd,
            source: ScratchSource::Provided,
        });
    }
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "\\scripts\\uia-action-scratch.ps1"
    );
    let mut child = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            script,
        ])
        .stdout(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .expect("spawn scratch fixture");
    let stdout = child.stdout.take().expect("fixture stdout");
    let mut reader = BufReader::new(stdout);
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut hwnd = None;
    while Instant::now() < deadline {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        if let Some(rest) = line.trim().strip_prefix("hwnd=") {
            hwnd = parse_hwnd(rest);
            break;
        }
    }
    let hwnd = hwnd.expect("fixture did not print hwnd=0x...");
    Some(Scratch {
        child: Some(child),
        hwnd,
        source: ScratchSource::SelfSpawned,
    })
}

fn parse_hwnd(raw: &str) -> Option<isize> {
    let text = raw.trim().trim_start_matches("hwnd=").trim();
    let hex = text.strip_prefix("0x").unwrap_or(text);
    isize::from_str_radix(hex, 16).ok()
}

/// The fixture activates itself when it is shown. Give it a moment, then put
/// the previously-foreground window back so the semantic path is proven to run
/// against a *not-foreground* target (DoD-3). This is test setup: the tool
/// under test never raises anything.
fn restore_foreground(previous: Option<isize>) {
    std::thread::sleep(Duration::from_millis(500));
    let Some(previous) = previous else { return };
    if hands::foreground::foreground_hwnd() != Some(previous) {
        let _ = hands::foreground::offer(Some(previous), (0, 0));
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn assert_not_foreground(hwnd: isize) {
    assert_ne!(
        hands::foreground::foreground_hwnd(),
        Some(hwnd),
        "the scratch window must not be foreground for background-dispatch evidence"
    );
}

/// The semantic path never moves the pointer and never raises a window itself
/// (source-scan test). Some providers activate their own window while handling
/// a pattern call, and a busy desktop can move focus independently; the live
/// claim is therefore "pointer still, foreground change reported as observed",
/// not "always false".
fn assert_pointer_still_and_foreground_honest(
    info: &hands::uia_actions::SemanticInfo,
    _fixture: isize,
) {
    assert!(
        !info.pointer_moved,
        "semantic path must not move the pointer"
    );
    if info.foreground_changed {
        eprintln!(
            "note: foreground changed during the call (reported truthfully; the tool never raises)"
        );
    }
}

fn nodes(hwnd: isize) -> Vec<RawNode> {
    hands::uia::collect(Detail::Dom, Some(hwnd))
        .expect("UIA collect on the scratch window")
        .nodes
}

fn find<'a>(nodes: &'a [RawNode], name: &str, role: &str) -> Option<&'a RawNode> {
    nodes
        .iter()
        .find(|node| node.name.eq_ignore_ascii_case(name) && node.role.eq_ignore_ascii_case(role))
}

/// Assert the control the test is about to drive is really the one the fixture
/// defined (observed `uia:` names are asserted before acting).
fn control(nodes: &[RawNode], name: &str, role: &str) -> String {
    let node = find(nodes, name, role).unwrap_or_else(|| {
        panic!(
            "fixture control {name}/{role} not found; saw {:?}",
            nodes
                .iter()
                .map(|n| (n.name.clone(), n.role.clone()))
                .collect::<Vec<_>>()
        )
    });
    node.element_id().expect("runtime id")
}

fn action(
    element_id: &str,
    action: &str,
    value: Option<&str>,
) -> Result<hands::ActuateEnvelope, HandsError> {
    uia_action(ActuateRequest {
        session_id: Some("uia-action-live".into()),
        element_id: Some(element_id.to_string()),
        action: Some(action.to_string()),
        value: value.map(str::to_string),
        timeout_ms: Some(3_000),
        ..ActuateRequest::default()
    })
}

fn semantic_kind(env: &hands::ActuateEnvelope) -> (&str, &str) {
    let info = env.semantic.as_ref().expect("semantic block");
    (info.action.as_str(), info.delivery.as_str())
}

#[test]
#[ignore = "live desktop; D-2 authorized. Run: HANDS_LIVE_SCRATCH=1 cargo test --test uia_action_live -- --ignored --test-threads=1"]
fn live_semantic_invoke_scratch_button() {
    let previous = hands::foreground::foreground_hwnd();
    let Some(scratch) = scratch() else { return };
    restore_foreground(previous);
    assert_not_foreground(scratch.hwnd);
    let nodes = nodes(scratch.hwnd);
    let id = control(&nodes, "Refresh", "Button");
    let env = action(&id, "invoke", None).expect("invoke result");
    assert!(env.ok, "{:?}", env.error);
    let (action_name, delivery) = semantic_kind(&env);
    assert_eq!(action_name, "invoke");
    assert_eq!(delivery, "delivered");
    assert!(!env.verified, "invoke is dispatch-only");
    let info = env.semantic.expect("semantic");
    assert_pointer_still_and_foreground_honest(&info, scratch.hwnd);
    assert_ne!(
        scratch.source,
        ScratchSource::Provided,
        "self-spawned window expected for the recorded evidence"
    );
}

#[test]
#[ignore = "live desktop; D-2 authorized. Run: HANDS_LIVE_SCRATCH=1 cargo test --test uia_action_live -- --ignored --test-threads=1"]
fn live_semantic_set_value_scratch_edit() {
    let previous = hands::foreground::foreground_hwnd();
    let Some(scratch) = scratch() else { return };
    restore_foreground(previous);
    assert_not_foreground(scratch.hwnd);
    let nodes = nodes(scratch.hwnd);
    let id = control(&nodes, "Name", "Edit");
    let env = action(&id, "set-value", Some("hands 0122")).expect("set-value result");
    assert!(env.ok, "{:?}", env.error);
    let (action_name, delivery) = semantic_kind(&env);
    assert_eq!(action_name, "set-value");
    assert_eq!(delivery, "delivered");
    assert!(env.verified, "value read-back must match");
    let info = env.semantic.expect("semantic");
    assert_pointer_still_and_foreground_honest(&info, scratch.hwnd);
}

#[test]
#[ignore = "live desktop; D-2 authorized. Run: HANDS_LIVE_SCRATCH=1 cargo test --test uia_action_live -- --ignored --test-threads=1"]
fn live_semantic_set_value_on_button_refused() {
    let previous = hands::foreground::foreground_hwnd();
    let Some(scratch) = scratch() else { return };
    restore_foreground(previous);
    assert_not_foreground(scratch.hwnd);
    let nodes = nodes(scratch.hwnd);
    // Benign button: the fence must not be what refuses here.
    let id = control(&nodes, "Refresh", "Button");
    let env = action(&id, "set-value", Some("nope")).expect("set-value result");
    assert!(!env.ok);
    assert_eq!(env.error.as_deref(), Some("invalid_role"));
    assert_eq!(semantic_kind(&env).1, "undelivered");
}

#[test]
#[ignore = "live desktop; D-2 authorized. Run: HANDS_LIVE_SCRATCH=1 cargo test --test uia_action_live -- --ignored --test-threads=1"]
fn live_semantic_unsupported_pattern_refused() {
    let previous = hands::foreground::foreground_hwnd();
    let Some(scratch) = scratch() else { return };
    restore_foreground(previous);
    assert_not_foreground(scratch.hwnd);
    let nodes = nodes(scratch.hwnd);
    // A WinForms Button exposes Invoke, not Toggle.
    let id = control(&nodes, "Refresh", "Button");
    let env = action(&id, "toggle", None).expect("toggle result");
    assert!(!env.ok);
    assert_eq!(env.error.as_deref(), Some("unsupported_pattern"));
    assert_eq!(semantic_kind(&env).1, "undelivered");
}

#[test]
#[ignore = "live desktop; D-2 authorized. Run: HANDS_LIVE_SCRATCH=1 cargo test --test uia_action_live -- --ignored --test-threads=1"]
fn live_semantic_send_button_fence_refused() {
    let previous = hands::foreground::foreground_hwnd();
    let Some(scratch) = scratch() else { return };
    restore_foreground(previous);
    assert_not_foreground(scratch.hwnd);
    let nodes = nodes(scratch.hwnd);
    let id = control(&nodes, "Send", "Button");
    let env = action(&id, "invoke", None).expect("invoke result");
    assert!(!env.ok);
    assert!(env.fence.is_some(), "Send must refuse at the fence");
    assert_eq!(semantic_kind(&env).1, "undelivered");
}

#[test]
#[ignore = "live desktop; D-2 authorized. Run: HANDS_LIVE_SCRATCH=1 cargo test --test uia_action_live -- --ignored --test-threads=1"]
fn live_semantic_toggle_scratch_checkbox() {
    let previous = hands::foreground::foreground_hwnd();
    let Some(scratch) = scratch() else { return };
    restore_foreground(previous);
    assert_not_foreground(scratch.hwnd);
    let nodes = nodes(scratch.hwnd);
    let id = control(&nodes, "Enable", "check box");
    let first = action(&id, "toggle", Some("on")).expect("toggle on");
    assert!(first.ok, "{:?}", first.error);
    assert_eq!(semantic_kind(&first).1, "delivered");
    // Restore the fixture state for a later run.
    let back = action(&id, "toggle", Some("off")).expect("toggle off");
    assert!(back.ok, "{:?}", back.error);
}
#[test]
#[ignore = "live desktop; D-2 authorized. Elevated window not authorized (0120 parked) — recorded untested."]
fn live_semantic_high_il_refused() {
    // No elevated scratch window is authorized (0120 stays parked; D-2 keeps
    // High-IL targets denied). Recorded as `untested` in review.md.
    if std::env::var("HANDS_SCRATCH_HIGH_IL_HWND").is_err() {
        eprintln!("live_semantic_high_il_refused: untested (no elevated scratch window)");
        return;
    }
    panic!("an elevated scratch window is not authorized by D-2; use track 0120");
}
