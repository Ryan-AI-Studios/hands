//! Semantic UIA actions behind the Hands policy pipeline.
//!
//! `ok:true` means the pattern call returned success — never proof the
//! application did what the user wanted. `verified:true` requires a matching
//! read-back. A semantic outcome is never retried and never downgraded to
//! physical input.
//!
//! The full gate order is:
//! `session → validate → resolve → yielded → blocked → require hwnd → high-IL
//! → ensure_installed → gate_semantic → worker checks → challenge note →
//! bounded dispatch → read-back → envelope`.
//! Two divergences from `click_inner` are deliberate: the semantic path does
//! not inherit `refuse_if_outside_client`, and it passes `None` as the High-IL
//! probe point (step 6 already requires a hwnd, and `None` keeps any coordinate
//! fallback out of this path).

use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use windows::Win32::UI::Accessibility::{
    ExpandCollapseState, ExpandCollapseState_Collapsed, ExpandCollapseState_Expanded,
    ExpandCollapseState_LeafNode, ExpandCollapseState_PartiallyExpanded, IUIAutomationElement,
    IUIAutomationExpandCollapsePattern, IUIAutomationInvokePattern,
    IUIAutomationSelectionItemPattern, IUIAutomationTogglePattern, IUIAutomationValuePattern,
    ToggleState, ToggleState_Indeterminate, ToggleState_Off, ToggleState_On,
    UIA_ExpandCollapsePatternId, UIA_InvokePatternId, UIA_PATTERN_ID, UIA_SelectionItemPatternId,
    UIA_TogglePatternId, UIA_ValuePatternId,
};
use windows::core::BSTR;

use crate::actuate::{self, ActuateEnvelope, ActuateRequest, ActuateTarget};
use crate::challenge;
use crate::error::HandsError;
use crate::fence::{self, FenceInfo};
use crate::foreground;
use crate::input;
use crate::logs::{self, LogFence, LogTarget};
use crate::space::{ensure_dpi, virtual_screen};
use crate::target::{ResolvedTarget, Target};
use crate::uia;

pub const DEFAULT_TIMEOUT_MS: u64 = 3000;
pub const MIN_TIMEOUT_MS: u64 = 500;
pub const TIMEOUT_ENV: &str = "HANDS_UIA_ACTION_TIMEOUT_MS";

const VERIFY_READBACK: &str = "state_readback";
const VERIFY_UNAVAILABLE: &str = "unavailable";

// ---------------------------------------------------------------- vocabulary

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticAction {
    Invoke,
    Toggle,
    Expand,
    Collapse,
    Select,
    SetValue,
}

impl SemanticAction {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "invoke" => Some(Self::Invoke),
            "toggle" => Some(Self::Toggle),
            "expand" => Some(Self::Expand),
            "collapse" => Some(Self::Collapse),
            "select" => Some(Self::Select),
            "set-value" | "set_value" => Some(Self::SetValue),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Invoke => "invoke",
            Self::Toggle => "toggle",
            Self::Expand => "expand",
            Self::Collapse => "collapse",
            Self::Select => "select",
            Self::SetValue => "set-value",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Delivered,
    Undelivered,
    Unknown,
}

impl Delivery {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Delivered => "delivered",
            Self::Undelivered => "undelivered",
            Self::Unknown => "unknown",
        }
    }
}

/// Flattened into `ActuateEnvelope`; `None` keeps every non-semantic envelope
/// byte-identical.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticInfo {
    pub action: String,
    pub delivery: Delivery,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<String>,
    #[serde(default)]
    pub foreground_changed: bool,
    #[serde(default)]
    pub pointer_moved: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct SemanticError {
    pub code: &'static str,
}

#[derive(Debug, Clone)]
pub(crate) struct SemanticRequest {
    pub element_id: String,
    pub action: SemanticAction,
    pub value: Option<String>,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub(crate) struct SemanticResult {
    #[allow(dead_code)] // read by the envelope-mapping tests only
    pub dispatched: bool,
    pub delivery: Delivery,
    pub verified: bool,
    pub verify: &'static str,
    #[allow(dead_code)] // kept for parity with the plan; never serialized
    pub state_after: Option<String>,
    pub foreground_changed: bool,
    pub pointer_moved: bool,
    pub error: Option<SemanticError>,
}

impl SemanticResult {
    fn refusal(code: &'static str) -> Self {
        Self {
            dispatched: false,
            delivery: Delivery::Undelivered,
            verified: false,
            verify: VERIFY_UNAVAILABLE,
            state_after: None,
            foreground_changed: false,
            pointer_moved: false,
            error: Some(SemanticError { code }),
        }
    }

    fn unknown(code: &'static str) -> Self {
        Self {
            dispatched: false,
            delivery: Delivery::Unknown,
            verified: false,
            verify: VERIFY_UNAVAILABLE,
            state_after: None,
            foreground_changed: false,
            pointer_moved: false,
            error: Some(SemanticError { code }),
        }
    }
}

pub(crate) trait SemanticBackend {
    fn dispatch(&self, resolved: &ResolvedTarget, req: &SemanticRequest) -> SemanticResult;
}

// -------------------------------------------------------------- pure seams

/// Map a raw `u32` HRESULT (always `err.code().0 as u32`, never an
/// `Error` object or its internal sentinel) to the closed vocabulary.
pub(crate) fn classify_hresult(code: u32) -> (&'static str, Delivery) {
    match code {
        0x8004_0201 => ("stale_target", Delivery::Undelivered), // UIA_E_ELEMENTNOTAVAILABLE
        0x8004_0200 => ("disabled", Delivery::Undelivered),     // UIA_E_ELEMENTNOTENABLED
        0x8004_0204 => ("unsupported_pattern", Delivery::Undelivered), // UIA_E_NOTSUPPORTED
        0x8013_1509 => ("invalid_operation", Delivery::Undelivered), // UIA_E_INVALIDOPERATION
        0x8013_1505 => ("provider_timeout", Delivery::Unknown), // UIA_E_TIMEOUT
        0x8007_0005 => ("access_denied", Delivery::Undelivered), // E_ACCESSDENIED
        // A null pattern from `GetCurrentPatternAs` surfaces as
        // `Error::empty()`, whose `code()` is 0.
        0 => ("unsupported_pattern", Delivery::Undelivered),
        // Unclassified: the call failed, the effect is unproven.
        _ => ("invalid_operation", Delivery::Unknown),
    }
}

/// Toggle decision: `Ok(None)` means "already in the requested state".
pub(crate) fn decide_toggle(
    before: ToggleState,
    desired: Option<&str>,
) -> Result<Option<ToggleState>, &'static str> {
    if before == ToggleState_Indeterminate {
        return Err("invalid_operation");
    }
    let Some(desired) = desired else {
        let next = if before == ToggleState_On {
            ToggleState_Off
        } else {
            ToggleState_On
        };
        return Ok(Some(next));
    };
    let want_on = parse_toggle_desired(desired).ok_or("invalid_operation")?;
    if (before == ToggleState_On) == want_on {
        return Ok(None);
    }
    Ok(Some(if want_on {
        ToggleState_On
    } else {
        ToggleState_Off
    }))
}

/// Expand/collapse decision: `Ok(true)` means "already in the requested state".
pub(crate) fn decide_expand(
    state: ExpandCollapseState,
    action: SemanticAction,
) -> Result<bool, &'static str> {
    if state == ExpandCollapseState_LeafNode {
        return Err("unsupported_pattern");
    }
    if state == ExpandCollapseState_PartiallyExpanded {
        return Err("invalid_operation");
    }
    match action {
        SemanticAction::Expand => Ok(state == ExpandCollapseState_Expanded),
        SemanticAction::Collapse => Ok(state == ExpandCollapseState_Collapsed),
        _ => Err("invalid_operation"),
    }
}

/// Step-3 resolution failures map onto the closed vocabulary by the prose the
/// live resolvers emit (`uia.rs` for a dead RuntimeId, `target.rs` for
/// geometry). The drift test keeps those prefixes honest.
pub(crate) fn resolve_code(err: &HandsError) -> &'static str {
    let message = err.tool_message();
    if message.starts_with("no UIA element with RuntimeId") {
        "stale_target"
    } else {
        // Zero-area / off-screen-centre / any other resolution failure.
        "target_unavailable"
    }
}

pub(crate) fn parse_toggle_desired(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "checked" => Some(true),
        "off" | "false" | "unchecked" => Some(false),
        _ => None,
    }
}

fn role_admits_set_value(role: &str) -> bool {
    ["Edit", "Document", "ComboBox", "combo box"]
        .iter()
        .any(|name| role.eq_ignore_ascii_case(name))
}

fn is_no_evidence(err: &HandsError) -> bool {
    err.tool_message().starts_with("no element evidence")
}

// --------------------------------------------------------------- validation

fn validate_value(
    action: SemanticAction,
    value: Option<&str>,
) -> Result<Option<String>, HandsError> {
    match action {
        SemanticAction::SetValue => {
            let value =
                value.ok_or_else(|| HandsError::Input("set-value requires --value".into()))?;
            if value.contains('\0') {
                return Err(HandsError::Input("value must not contain NUL".into()));
            }
            Ok(Some(value.to_string()))
        }
        SemanticAction::Toggle => match value {
            None => Ok(None),
            Some(raw) => {
                if raw.contains('\0') {
                    return Err(HandsError::Input("value must not contain NUL".into()));
                }
                if parse_toggle_desired(raw).is_none() {
                    return Err(HandsError::Input(
                        "--value for toggle must be one of on|off|true|false|checked|unchecked"
                            .into(),
                    ));
                }
                Ok(Some(raw.to_string()))
            }
        },
        other => {
            if value.is_some() {
                return Err(HandsError::Input(format!(
                    "--value is not valid for {}",
                    other.as_str()
                )));
            }
            Ok(None)
        }
    }
}

fn timeout_from(request: &ActuateRequest) -> Duration {
    let millis = request
        .timeout_ms
        .or_else(|| {
            std::env::var(TIMEOUT_ENV)
                .ok()
                .and_then(|raw| raw.trim().parse::<u64>().ok())
        })
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .max(MIN_TIMEOUT_MS);
    Duration::from_millis(millis)
}

// ---------------------------------------------------------------- envelopes

fn base_envelope(
    session_id: &str,
    target: ActuateTarget,
    action: SemanticAction,
) -> ActuateEnvelope {
    ActuateEnvelope {
        session_id: session_id.to_string(),
        ok: false,
        frozen: false,
        target,
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
        semantic: Some(SemanticInfo {
            action: action.as_str().to_string(),
            delivery: Delivery::Undelivered,
            verify: None,
            foreground_changed: false,
            pointer_moved: false,
        }),
    }
}

fn unresolved_target(element_id: &str) -> ActuateTarget {
    ActuateTarget {
        kind: "element".to_string(),
        id: Some(element_id.to_string()),
        x: 0,
        y: 0,
    }
}

fn refusal(
    session_id: &str,
    target: ActuateTarget,
    action: SemanticAction,
    code: &str,
    fence_info: Option<FenceInfo>,
) -> ActuateEnvelope {
    let mut env = base_envelope(session_id, target, action);
    env.error = Some(code.to_string());
    env.fence = fence_info;
    env
}

/// Enrich a refusal returned by a shared `actuate` helper: nothing was read
/// back, so `verified` is false, and the semantic block says undelivered while
/// the helper's own error/fence/challenge prose survives untouched.
fn mark_refused(mut env: ActuateEnvelope, action: SemanticAction) -> ActuateEnvelope {
    env.verified = false;
    env.semantic = Some(SemanticInfo {
        action: action.as_str().to_string(),
        delivery: Delivery::Undelivered,
        verify: None,
        foreground_changed: false,
        pointer_moved: false,
    });
    env
}

fn yield_refusal(
    session_id: &str,
    target: ActuateTarget,
    action: SemanticAction,
) -> ActuateEnvelope {
    let mut env = base_envelope(session_id, target, action);
    env.frozen = crate::lease::is_frozen();
    env.error = Some(challenge::YIELD_ERROR.to_string());
    env.challenge = Some(challenge::snapshot());
    env
}

fn dispatched_envelope(
    session_id: &str,
    target: ActuateTarget,
    action: SemanticAction,
    result: SemanticResult,
) -> ActuateEnvelope {
    let mut env = base_envelope(session_id, target, action);
    env.semantic = Some(SemanticInfo {
        action: action.as_str().to_string(),
        delivery: result.delivery,
        verify: Some(result.verify.to_string()),
        foreground_changed: result.foreground_changed,
        pointer_moved: result.pointer_moved,
    });
    match result.error {
        Some(err) => {
            env.ok = false;
            env.error = Some(err.code.to_string());
            env.verified = false;
        }
        None => {
            env.ok = true;
            env.verified = result.verified;
        }
    }
    env
}

// ------------------------------------------------------- policy environment

/// The live gates. Tests implement this to exercise the pipeline without COM
/// or a desktop.
pub(crate) trait PolicyEnv {
    fn resolve(&self, element_id: &str) -> Result<ResolvedTarget, HandsError>;
    fn yielded(&self, session_id: &str) -> bool;
    fn blocked(
        &self,
        session_id: &str,
        target: ActuateTarget,
    ) -> Result<Option<ActuateEnvelope>, HandsError>;
    fn high_il(
        &self,
        session_id: &str,
        target: &ActuateTarget,
        hwnd: Option<isize>,
    ) -> Result<Option<ActuateEnvelope>, HandsError>;
    fn fence(
        &self,
        session_id: &str,
        resolved: &ResolvedTarget,
    ) -> Result<Option<FenceInfo>, HandsError>;
}

struct LiveEnv;

impl PolicyEnv for LiveEnv {
    fn resolve(&self, element_id: &str) -> Result<ResolvedTarget, HandsError> {
        let space = ensure_dpi().and_then(|_| virtual_screen())?;
        Target::parse(Some(element_id), None, None, None)?.resolve(space)
    }

    fn yielded(&self, _session_id: &str) -> bool {
        challenge::yielded()
    }

    fn blocked(
        &self,
        session_id: &str,
        target: ActuateTarget,
    ) -> Result<Option<ActuateEnvelope>, HandsError> {
        actuate::refuse_if_blocked(session_id, target)
    }

    fn high_il(
        &self,
        session_id: &str,
        target: &ActuateTarget,
        hwnd: Option<isize>,
    ) -> Result<Option<ActuateEnvelope>, HandsError> {
        actuate::refuse_if_high_il(session_id, target, hwnd, None)
    }

    fn fence(
        &self,
        session_id: &str,
        resolved: &ResolvedTarget,
    ) -> Result<Option<FenceInfo>, HandsError> {
        fence::ensure_installed();
        fence::gate_semantic(session_id, resolved)
    }
}

// ---------------------------------------------------------------- pipeline

/// Shared CLI/MCP entry point.
pub fn uia_action(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    let env = LiveEnv;
    let backend = LiveBackend;
    let mut out = run(&env, &backend, &req)?;
    // Every `Ok` exit settles the session cooldown exactly once (physical
    // parity: `click` wraps `click_inner` in `after_actuate`).
    actuate::settle_cooldown("uia-action", &mut out);
    log_outcome(&out, &req);
    Ok(out)
}

fn log_outcome(env: &ActuateEnvelope, req: &ActuateRequest) {
    let Some(info) = env.semantic.as_ref() else {
        return;
    };
    let action = info.action.clone();
    let delivery = info.delivery.as_str();
    let len = if action == "set-value" {
        req.value.as_ref().map(|value| value.len())
    } else {
        None
    };
    let target = LogTarget {
        kind: env.target.kind.clone(),
        id: env.target.id.clone(),
        x: env.target.x,
        y: env.target.y,
    };
    let fence = env.fence.as_ref().map(|fence| LogFence {
        domain: fence.domain.clone(),
        category: fence.category.clone(),
        name: fence.name.clone(),
        role: fence.role.clone(),
    });
    let _ = logs::record_uia_action(
        &env.session_id,
        &action,
        delivery,
        env.verified,
        len,
        env.ok,
        env.error.as_deref(),
        Some(target),
        fence,
    );
}

fn run(
    env: &dyn PolicyEnv,
    backend: &dyn SemanticBackend,
    req: &ActuateRequest,
) -> Result<ActuateEnvelope, HandsError> {
    // 1. session
    let session_id = actuate::session(req)?;

    // 2. validate id + action + value (transport errors, no envelope)
    let element_id = req
        .element_id
        .clone()
        .ok_or_else(|| HandsError::Input("element id must start with uia:".into()))?;
    if !element_id.starts_with("uia:") {
        return Err(HandsError::Input("element id must start with uia:".into()));
    }
    let action_name = req
        .action
        .as_deref()
        .ok_or_else(|| HandsError::Input("missing uia action".into()))?;
    let action = SemanticAction::parse(action_name)
        .ok_or_else(|| HandsError::Input(format!("unknown uia action '{action_name}'")))?;
    let value = validate_value(action, req.value.as_deref())?;
    let timeout = timeout_from(req);

    // 3. resolve (pre-existing unbounded STA join; timeout bounds dispatch only)
    let resolved = match env.resolve(&element_id) {
        Ok(resolved) => resolved,
        Err(err) => {
            let code = resolve_code(&err);
            return Ok(refusal(
                &session_id,
                unresolved_target(&element_id),
                action,
                code,
                None,
            ));
        }
    };
    let info = ActuateTarget {
        kind: resolved.kind.to_string(),
        id: resolved.id.clone(),
        x: resolved.x,
        y: resolved.y,
    };

    // 4. challenge yield
    if env.yielded(&session_id) {
        return Ok(yield_refusal(&session_id, info, action));
    }

    // 5. freeze / cooldown
    if let Some(env_out) = env.blocked(&session_id, info.clone())? {
        return Ok(mark_refused(env_out, action));
    }

    // 6. hwnd required (otherwise High-IL and window text read the foreground)
    let Some(hwnd) = resolved.hwnd else {
        return Ok(refusal(&session_id, info, action, "no_target_hwnd", None));
    };

    // 7. High-IL (None probe point: no coordinate fallback)
    if let Some(env_out) = env.high_il(&session_id, &info, Some(hwnd))? {
        return Ok(mark_refused(env_out, action));
    }

    // 8. fence on target evidence only
    match env.fence(&session_id, &resolved) {
        Ok(None) => {}
        Ok(Some(fence_info)) => {
            let env_out = actuate::refuse_fence(session_id.clone(), info, fence_info)?;
            return Ok(mark_refused(env_out, action));
        }
        Err(err) if is_no_evidence(&err) => {
            return Ok(refusal(
                &session_id,
                info,
                action,
                "no_element_evidence",
                None,
            ));
        }
        Err(err) => {
            return Ok(refusal(
                &session_id,
                info,
                action,
                &err.tool_message(),
                None,
            ));
        }
    }

    // 9-10. worker checks + bounded dispatch + read-back
    let request = SemanticRequest {
        element_id: element_id.clone(),
        action,
        value,
        timeout,
    };
    let result = backend.dispatch(&resolved, &request);
    Ok(dispatched_envelope(&session_id, info, action, result))
}

// ------------------------------------------------------------- live worker

pub(crate) struct LiveBackend;

impl SemanticBackend for LiveBackend {
    fn dispatch(&self, resolved: &ResolvedTarget, req: &SemanticRequest) -> SemanticResult {
        let want = match crate::target::parse_runtime_id(&req.element_id) {
            Ok(want) => want,
            Err(err) => return SemanticResult::refusal(resolve_code(&err)),
        };
        let action = req.action;
        let value = req.value.clone();
        let outer_hwnd = resolved.hwnd;
        bounded_dispatch(req.timeout, move || {
            worker_run(want, action, value, outer_hwnd)
        })
    }
}

/// Spawn the STA worker and wait at most `timeout`. Timeout and worker loss are
/// `unknown` (the effect is unproven) and are never retried. A timed-out worker
/// is detached, not aborted: the call may still take effect.
fn bounded_dispatch<F>(timeout: Duration, work: F) -> SemanticResult
where
    F: FnOnce() -> SemanticResult + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("hands-uia-action".to_string())
        .spawn(move || {
            let _ = tx.send(work());
        });
    if spawned.is_err() {
        return SemanticResult::unknown("worker_lost");
    }
    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => SemanticResult::unknown("provider_timeout"),
        Err(mpsc::RecvTimeoutError::Disconnected) => SemanticResult::unknown("worker_lost"),
    }
}

fn worker_run(
    want: Vec<i32>,
    action: SemanticAction,
    value: Option<String>,
    outer_hwnd: Option<isize>,
) -> SemanticResult {
    let _sta = match uia::StaGuard::enter() {
        Ok(guard) => guard,
        Err(err) => return SemanticResult::refusal(resolve_code(&err)),
    };
    let automation = match uia::create_automation() {
        Ok(automation) => automation,
        Err(err) => return SemanticResult::refusal(resolve_code(&err)),
    };
    let com = match uia::resolve_on(&automation, &want) {
        Ok(com) => com,
        Err(err) => return SemanticResult::refusal(resolve_code(&err)),
    };
    let inner = com.resolved.hwnd;
    if let Err(code) = hwnd_matches(outer_hwnd, inner) {
        return SemanticResult::refusal(code);
    }

    // Shared pre-checks (element properties).
    if let Err(result) = check_element(&com.element) {
        return result;
    }
    if action == SemanticAction::SetValue && !role_admits_set_value(&com.resolved.role) {
        return SemanticResult::refusal("invalid_role");
    }

    let before = sample_side_effects();
    let outcome = match action {
        SemanticAction::Invoke => invoke(&com.element),
        SemanticAction::Toggle => toggle(&com.element, value.as_deref()),
        SemanticAction::Expand | SemanticAction::Collapse => expand_collapse(&com.element, action),
        SemanticAction::Select => select(&com.element),
        SemanticAction::SetValue => match value {
            Some(text) => set_value(&com.element, &text),
            None => SemanticResult::refusal("invalid_operation"),
        },
    };
    let after = sample_side_effects();
    let mut outcome = outcome;
    let (fg_before, pt_before) = before;
    let (fg_after, pt_after) = after;
    outcome.foreground_changed = fg_before != fg_after;
    outcome.pointer_moved = pt_before != pt_after;
    outcome
}

fn hwnd_matches(outer: Option<isize>, inner: Option<isize>) -> Result<(), &'static str> {
    match (outer, inner) {
        (Some(outer), Some(inner)) if outer == inner => Ok(()),
        (None, Some(_)) => Ok(()),
        _ => Err("stale_target"),
    }
}

fn check_element(element: &IUIAutomationElement) -> Result<(), SemanticResult> {
    let password = unsafe { element.CurrentIsPassword() }.map_err(com_error)?;
    if password.as_bool() {
        return Err(SemanticResult::refusal("password"));
    }
    let enabled = unsafe { element.CurrentIsEnabled() }.map_err(com_error)?;
    if !enabled.as_bool() {
        return Err(SemanticResult::refusal("disabled"));
    }
    let offscreen = unsafe { element.CurrentIsOffscreen() }.map_err(com_error)?;
    if offscreen.as_bool() {
        return Err(SemanticResult::refusal("offscreen"));
    }
    Ok(())
}

fn com_error(err: windows::core::Error) -> SemanticResult {
    let (code, delivery) = classify_hresult(err.code().0 as u32);
    SemanticResult {
        dispatched: true,
        delivery,
        verified: false,
        verify: VERIFY_UNAVAILABLE,
        state_after: None,
        foreground_changed: false,
        pointer_moved: false,
        error: Some(SemanticError { code }),
    }
}

fn acquire<T: windows::core::Interface>(
    element: &IUIAutomationElement,
    pattern_id: UIA_PATTERN_ID,
) -> Result<T, SemanticResult> {
    match unsafe { element.GetCurrentPatternAs::<T>(pattern_id) } {
        Ok(pattern) => Ok(pattern),
        // A null pattern surfaces as Error::empty(); `code()` maps it to 0.
        Err(err) => Err(com_error(err)),
    }
}

fn sample_side_effects() -> (Option<isize>, Option<(i32, i32)>) {
    (foreground::foreground_hwnd(), input::cursor_pos().ok())
}

fn delivered_readback(verified: bool, state: Option<String>) -> SemanticResult {
    SemanticResult {
        dispatched: true,
        delivery: if verified {
            Delivery::Delivered
        } else {
            Delivery::Unknown
        },
        verified,
        verify: VERIFY_READBACK,
        state_after: state,
        foreground_changed: false,
        pointer_moved: false,
        error: None,
    }
}

fn skipped() -> SemanticResult {
    SemanticResult {
        dispatched: false,
        delivery: Delivery::Delivered,
        verified: true,
        verify: VERIFY_READBACK,
        state_after: None,
        foreground_changed: false,
        pointer_moved: false,
        error: None,
    }
}

fn invoke(element: &IUIAutomationElement) -> SemanticResult {
    let pattern: IUIAutomationInvokePattern = match acquire(element, UIA_InvokePatternId) {
        Ok(pattern) => pattern,
        Err(result) => return result,
    };
    challenge::note_actuation_if_proceeding(false);
    if let Err(err) = unsafe { pattern.Invoke() } {
        return com_error(err);
    }
    SemanticResult {
        dispatched: true,
        delivery: Delivery::Delivered,
        verified: false,
        verify: VERIFY_UNAVAILABLE,
        state_after: None,
        foreground_changed: false,
        pointer_moved: false,
        error: None,
    }
}

fn toggle(element: &IUIAutomationElement, desired: Option<&str>) -> SemanticResult {
    let pattern: IUIAutomationTogglePattern = match acquire(element, UIA_TogglePatternId) {
        Ok(pattern) => pattern,
        Err(result) => return result,
    };
    let before = match unsafe { pattern.CurrentToggleState() } {
        Ok(state) => state,
        Err(err) => return com_error(err),
    };
    let intended = match decide_toggle(before, desired) {
        Ok(Some(intended)) => intended,
        Ok(None) => return skipped(),
        Err(code) => return SemanticResult::refusal(code),
    };
    challenge::note_actuation_if_proceeding(false);
    if let Err(err) = unsafe { pattern.Toggle() } {
        return com_error(err);
    }
    let after = match unsafe { pattern.CurrentToggleState() } {
        Ok(state) => state,
        Err(err) => return com_error(err),
    };
    delivered_readback(
        after == intended,
        Some(toggle_state_name(after).to_string()),
    )
}

fn expand_collapse(element: &IUIAutomationElement, action: SemanticAction) -> SemanticResult {
    let pattern: IUIAutomationExpandCollapsePattern =
        match acquire(element, UIA_ExpandCollapsePatternId) {
            Ok(pattern) => pattern,
            Err(result) => return result,
        };
    let before = match unsafe { pattern.CurrentExpandCollapseState() } {
        Ok(state) => state,
        Err(err) => return com_error(err),
    };
    match decide_expand(before, action) {
        Ok(true) => return skipped(),
        Ok(false) => {}
        Err(code) => return SemanticResult::refusal(code),
    }
    challenge::note_actuation_if_proceeding(false);
    let dispatched = match action {
        SemanticAction::Expand => unsafe { pattern.Expand() },
        SemanticAction::Collapse => unsafe { pattern.Collapse() },
        _ => return SemanticResult::refusal("invalid_operation"),
    };
    if let Err(err) = dispatched {
        return com_error(err);
    }
    let after = match unsafe { pattern.CurrentExpandCollapseState() } {
        Ok(state) => state,
        Err(err) => return com_error(err),
    };
    let expected = if action == SemanticAction::Expand {
        ExpandCollapseState_Expanded
    } else {
        ExpandCollapseState_Collapsed
    };
    delivered_readback(
        after == expected,
        Some(expand_state_name(after).to_string()),
    )
}

fn select(element: &IUIAutomationElement) -> SemanticResult {
    let pattern: IUIAutomationSelectionItemPattern =
        match acquire(element, UIA_SelectionItemPatternId) {
            Ok(pattern) => pattern,
            Err(result) => return result,
        };
    let selected = match unsafe { pattern.CurrentIsSelected() } {
        Ok(selected) => selected,
        Err(err) => return com_error(err),
    };
    if selected.as_bool() {
        return skipped();
    }
    challenge::note_actuation_if_proceeding(false);
    if let Err(err) = unsafe { pattern.Select() } {
        return com_error(err);
    }
    let after = match unsafe { pattern.CurrentIsSelected() } {
        Ok(selected) => selected,
        Err(err) => return com_error(err),
    };
    delivered_readback(after.as_bool(), None)
}

fn set_value(element: &IUIAutomationElement, text: &str) -> SemanticResult {
    let pattern: IUIAutomationValuePattern = match acquire(element, UIA_ValuePatternId) {
        Ok(pattern) => pattern,
        Err(result) => return result,
    };
    let read_only = match unsafe { pattern.CurrentIsReadOnly() } {
        Ok(read_only) => read_only,
        Err(err) => return com_error(err),
    };
    if read_only.as_bool() {
        return SemanticResult::refusal("read_only");
    }
    let value = BSTR::from(text);
    challenge::note_actuation_if_proceeding(false);
    if let Err(err) = unsafe { pattern.SetValue(&value) } {
        return com_error(err);
    }
    let after = match unsafe { pattern.CurrentValue() } {
        Ok(after) => after,
        Err(err) => return com_error(err),
    };
    // `state_after` is never set for value writes and never logged.
    delivered_readback(after == text, None)
}

fn toggle_state_name(state: ToggleState) -> &'static str {
    if state == ToggleState_On {
        "on"
    } else if state == ToggleState_Off {
        "off"
    } else {
        "indeterminate"
    }
}

fn expand_state_name(state: ExpandCollapseState) -> &'static str {
    if state == ExpandCollapseState_Expanded {
        "expanded"
    } else if state == ExpandCollapseState_Collapsed {
        "collapsed"
    } else if state == ExpandCollapseState_PartiallyExpanded {
        "partially_expanded"
    } else {
        "leaf"
    }
}

// ------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space::Rect;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeEnv {
        resolved: Mutex<Option<Result<ResolvedTarget, HandsError>>>,
        yielded: bool,
        blocked: bool,
        high_il: bool,
        fence: Option<FenceInfo>,
        fence_err: Option<HandsError>,
        fence_calls: AtomicUsize,
    }

    impl FakeEnv {
        fn ok() -> Self {
            Self {
                resolved: Mutex::new(Some(Ok(resolved_target("Send", "button", Some(0x1234))))),
                yielded: false,
                blocked: false,
                high_il: false,
                fence: None,
                fence_err: None,
                fence_calls: AtomicUsize::new(0),
            }
        }
    }

    impl PolicyEnv for FakeEnv {
        fn resolve(&self, _element_id: &str) -> Result<ResolvedTarget, HandsError> {
            self.resolved
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| Ok(resolved_target("Refresh", "button", Some(0x1234))))
        }

        fn yielded(&self, _session_id: &str) -> bool {
            self.yielded
        }

        fn blocked(
            &self,
            session_id: &str,
            target: ActuateTarget,
        ) -> Result<Option<ActuateEnvelope>, HandsError> {
            if !self.blocked {
                return Ok(None);
            }
            let mut env = base_envelope(session_id, target, SemanticAction::Invoke);
            env.ok = false;
            env.frozen = true;
            env.error = Some("desk lease frozen (physical input or Pause/Break)".into());
            Ok(Some(env))
        }

        fn high_il(
            &self,
            session_id: &str,
            target: &ActuateTarget,
            _hwnd: Option<isize>,
        ) -> Result<Option<ActuateEnvelope>, HandsError> {
            if !self.high_il {
                return Ok(None);
            }
            let mut env = base_envelope(session_id, target.clone(), SemanticAction::Invoke);
            env.error = Some("high-IL target refused".into());
            Ok(Some(env))
        }

        fn fence(
            &self,
            _session_id: &str,
            _resolved: &ResolvedTarget,
        ) -> Result<Option<FenceInfo>, HandsError> {
            self.fence_calls.fetch_add(1, Ordering::SeqCst);
            if let Some(err) = &self.fence_err {
                return Err(clone_error(err));
            }
            Ok(self.fence.clone())
        }
    }

    fn clone_error(err: &HandsError) -> HandsError {
        match err {
            HandsError::Target(message) => HandsError::Target(message.clone()),
            HandsError::Uia(message) => HandsError::Uia(message.clone()),
            HandsError::Input(message) => HandsError::Input(message.clone()),
            other => HandsError::Uia(other.tool_message()),
        }
    }

    fn resolved_target(name: &str, role: &str, hwnd: Option<isize>) -> ResolvedTarget {
        ResolvedTarget {
            target: Target::ElementId("uia:1.2".into()),
            kind: "element",
            id: Some("uia:1.2".into()),
            x: 40,
            y: 50,
            rect: Rect {
                x: 10,
                y: 20,
                w: 60,
                h: 20,
            },
            hwnd,
            name: name.into(),
            role: role.into(),
        }
    }

    #[derive(Default)]
    struct FakeBackend {
        result: Mutex<Option<SemanticResult>>,
        calls: AtomicUsize,
        last_value: Mutex<Option<String>>,
    }

    impl FakeBackend {
        fn with(result: SemanticResult) -> Self {
            Self {
                result: Mutex::new(Some(result)),
                calls: AtomicUsize::new(0),
                last_value: Mutex::new(None),
            }
        }
    }

    impl SemanticBackend for FakeBackend {
        fn dispatch(&self, _resolved: &ResolvedTarget, req: &SemanticRequest) -> SemanticResult {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.last_value.lock().unwrap() = req.value.clone();
            self.result
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| delivered_readback(true, None))
        }
    }

    fn request(action: &str, value: Option<&str>) -> ActuateRequest {
        ActuateRequest {
            session_id: Some("uia-action-test".into()),
            element_id: Some("uia:1.2".into()),
            action: Some(action.into()),
            value: value.map(str::to_string),
            timeout_ms: Some(500),
            ..ActuateRequest::default()
        }
    }

    fn run_with(env: &FakeEnv, backend: &FakeBackend, req: &ActuateRequest) -> ActuateEnvelope {
        run(env, backend, req).expect("pipeline result")
    }

    #[test]
    fn semantic_requires_uia_element_id() {
        let env = FakeEnv::ok();
        let backend = FakeBackend::default();
        let mut req = request("invoke", None);
        req.element_id = Some("chr:0".into());
        let err = run(&env, &backend, &req).expect_err("must refuse");
        assert!(err.tool_message().contains("must start with uia:"), "{err}");
        let mut req = request("invoke", None);
        req.element_id = None;
        let err = run(&env, &backend, &req).expect_err("must refuse");
        assert!(err.tool_message().contains("must start with uia:"), "{err}");
    }

    #[test]
    fn semantic_rejects_grid_and_pixel_ids() {
        let env = FakeEnv::ok();
        let backend = FakeBackend::default();
        for id in ["g:0:0", "12,34"] {
            let mut req = request("invoke", None);
            req.element_id = Some(id.into());
            let err = run(&env, &backend, &req).expect_err("must refuse");
            assert!(err.tool_message().contains("must start with uia:"), "{err}");
        }
    }

    #[test]
    fn semantic_rejects_unknown_action() {
        let env = FakeEnv::ok();
        let backend = FakeBackend::default();
        let err = run(&env, &backend, &request("press", None)).expect_err("must refuse");
        assert!(err.tool_message().contains("unknown uia action"), "{err}");
    }

    #[test]
    fn semantic_set_value_requires_value() {
        let env = FakeEnv::ok();
        let backend = FakeBackend::default();
        let err = run(&env, &backend, &request("set-value", None)).expect_err("must refuse");
        assert!(err.tool_message().contains("requires --value"), "{err}");
    }

    #[test]
    fn semantic_rejects_value_for_other_actions() {
        let env = FakeEnv::ok();
        let backend = FakeBackend::default();
        for action in ["invoke", "expand", "collapse", "select"] {
            let err = run(&env, &backend, &request(action, Some("x"))).expect_err("must refuse");
            assert!(
                err.tool_message().contains("is not valid for"),
                "{action}: {err}"
            );
        }
    }

    #[test]
    fn semantic_rejects_nul_value() {
        let env = FakeEnv::ok();
        let backend = FakeBackend::default();
        let err = run(&env, &backend, &request("set-value", Some("a\0b"))).expect_err("refuse");
        assert!(err.tool_message().contains("NUL"), "{err}");
    }

    #[test]
    fn semantic_parse_accepts_kebab_and_snake() {
        assert_eq!(
            SemanticAction::parse("set-value"),
            Some(SemanticAction::SetValue)
        );
        assert_eq!(
            SemanticAction::parse("SET_VALUE"),
            Some(SemanticAction::SetValue)
        );
        assert_eq!(
            SemanticAction::parse("toggle"),
            Some(SemanticAction::Toggle)
        );
        assert_eq!(
            SemanticAction::parse("expand"),
            Some(SemanticAction::Expand)
        );
        assert_eq!(
            SemanticAction::parse("collapse"),
            Some(SemanticAction::Collapse)
        );
        assert_eq!(
            SemanticAction::parse("select"),
            Some(SemanticAction::Select)
        );
        assert_eq!(
            SemanticAction::parse("invoke"),
            Some(SemanticAction::Invoke)
        );
        assert_eq!(SemanticAction::parse("press"), None);
    }

    #[test]
    fn semantic_requires_target_hwnd() {
        let mut env = FakeEnv::ok();
        env.resolved = Mutex::new(Some(Ok(resolved_target("Send", "button", None))));
        let backend = FakeBackend::default();
        let env_out = run_with(&env, &backend, &request("invoke", None));
        assert!(!env_out.ok);
        assert_eq!(env_out.error.as_deref(), Some("no_target_hwnd"));
        assert_eq!(
            env_out.semantic.expect("semantic").delivery,
            Delivery::Undelivered
        );
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn semantic_refuses_before_dispatch_when_frozen() {
        let mut env = FakeEnv::ok();
        env.blocked = true;
        let backend = FakeBackend::default();
        let env_out = run_with(&env, &backend, &request("invoke", None));
        assert!(!env_out.ok);
        assert!(env_out.frozen);
        assert_eq!(
            env_out.error.as_deref(),
            Some("desk lease frozen (physical input or Pause/Break)")
        );
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn semantic_honors_challenge_yield_before_dispatch() {
        let mut env = FakeEnv::ok();
        env.yielded = true;
        let backend = FakeBackend::default();
        let env_out = run_with(&env, &backend, &request("invoke", None));
        assert!(!env_out.ok);
        assert_eq!(
            env_out.semantic.expect("semantic").delivery,
            Delivery::Undelivered
        );
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn semantic_refusal_envelope_carries_action_and_undelivered() {
        let fence = FenceInfo {
            domain: "desktop".into(),
            category: "Messages".into(),
            name: "Send".into(),
            role: "button".into(),
            modes: vec![],
        };
        let mut env = FakeEnv::ok();
        env.fence = Some(fence);
        let backend = FakeBackend::default();
        let env_out = run_with(&env, &backend, &request("invoke", None));
        let info = env_out.semantic.expect("semantic");
        assert_eq!(info.action, "invoke");
        assert_eq!(info.delivery, Delivery::Undelivered);
        assert!(env_out.fence.is_some());
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn semantic_refusal_is_not_verified() {
        // Both shared helpers hardcode verified:true; mark_refused must win.
        let mut env = FakeEnv::ok();
        env.blocked = true;
        let backend = FakeBackend::default();
        let env_out = run_with(&env, &backend, &request("toggle", None));
        assert!(!env_out.verified);
        assert!(!env_out.ok);
    }

    #[test]
    fn semantic_high_il_is_fail_closed() {
        let mut env = FakeEnv::ok();
        env.high_il = true;
        let backend = FakeBackend::default();
        let env_out = run_with(&env, &backend, &request("invoke", None));
        assert!(!env_out.ok);
        assert_eq!(
            env_out.semantic.expect("semantic").delivery,
            Delivery::Undelivered
        );
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn semantic_unnamed_target_refused_before_dispatch() {
        let mut env = FakeEnv::ok();
        env.fence_err = Some(HandsError::Target(
            "no element evidence: resolved UIA element has no name after the 8-ancestor walk"
                .into(),
        ));
        let backend = FakeBackend::default();
        let env_out = run_with(&env, &backend, &request("invoke", None));
        assert_eq!(env_out.error.as_deref(), Some("no_element_evidence"));
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn semantic_fence_is_re_evaluated_per_call() {
        let env = FakeEnv::ok();
        let backend = FakeBackend::default();
        run_with(&env, &backend, &request("invoke", None));
        run_with(&env, &backend, &request("invoke", None));
        assert_eq!(env.fence_calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn semantic_notes_actuation_only_when_dispatching() {
        // Each mutating action notes the challenge actuation exactly once, only
        // after the last state read / skip decision and immediately before the
        // mutating pattern call. A dropped, moved or duplicated note fails here.
        let src = include_str!("uia_actions.rs");
        let cases: [(&str, &str, Option<&str>); 5] = [
            ("fn invoke(", ".Invoke()", None),
            ("fn toggle(", ".Toggle()", Some("CurrentToggleState")),
            (
                "fn expand_collapse(",
                ".Expand()",
                Some("CurrentExpandCollapseState"),
            ),
            ("fn select(", ".Select()", Some("CurrentIsSelected")),
            ("fn set_value(", ".SetValue(", Some("CurrentIsReadOnly")),
        ];
        for (signature, dispatch, last_read) in cases {
            let start = src.find(signature).unwrap_or_else(|| panic!("{signature}"));
            let end = src[start..]
                .find("\n}\n")
                .map(|offset| start + offset)
                .unwrap_or_else(|| panic!("{signature} end"));
            let body = &src[start..end];
            let notes = body
                .matches("challenge::note_actuation_if_proceeding(false)")
                .count();
            assert_eq!(notes, 1, "{signature} must note exactly once:\n{body}");
            let note = body
                .find("challenge::note_actuation_if_proceeding(false)")
                .expect("note");
            let call = body
                .find(dispatch)
                .unwrap_or_else(|| panic!("{signature} missing {dispatch}"));
            assert!(note < call, "{signature}: note must precede {dispatch}");
            if let Some(read) = last_read {
                assert!(
                    body[..note].contains(read),
                    "{signature}: a {read} read must precede the note"
                );
            }
            // No note may precede the skip decision.
            if let Some(skip) = body.find("skipped()") {
                assert!(
                    skip < note,
                    "{signature}: a skip must return before the note"
                );
            }
        }
    }

    #[test]
    fn semantic_source_order_resolve_yielded_blocked_high_il_ensure_installed_gate() {
        let src = include_str!("uia_actions.rs");
        let start = src.find("fn run(").expect("run");
        let end = src[start..]
            .find("\n}\n")
            .map(|offset| start + offset)
            .expect("run end");
        let body = &src[start..end];
        let order = [
            "env.resolve",
            "env.yielded",
            "env.blocked",
            "resolved.hwnd",
            "env.high_il",
            "env.fence",
            "backend.dispatch",
        ];
        let mut cursor = 0usize;
        for needle in order {
            let found = body[cursor..]
                .find(needle)
                .unwrap_or_else(|| panic!("missing {needle} in order"));
            cursor += found;
        }
        // `ensure_installed` runs inside the live fence gate, which the pipeline
        // reaches before `gate_semantic`; pin that composition too.
        let env_start = src.find("impl PolicyEnv for LiveEnv").expect("LiveEnv");
        let env_body = &src[env_start..env_start + 2_500];
        let fence_fn = env_body.find("fn fence(").expect("LiveEnv::fence");
        let ensure = env_body[fence_fn..]
            .find("fence::ensure_installed()")
            .expect("ensure_installed before the gate");
        let gate = env_body[fence_fn..]
            .find("fence::gate_semantic(")
            .expect("gate_semantic call");
        assert!(
            ensure < gate,
            "ensure_installed must run before gate_semantic"
        );
    }

    #[test]
    fn semantic_never_calls_send_input_or_set_foreground() {
        let src = include_str!("uia_actions.rs");
        let start = src
            .find("// ------------------------------------------------------------- live worker")
            .expect("live worker marker");
        let end = src
            .find("// ------------------------------------------------------------------- tests")
            .expect("tests marker");
        let worker = &src[start..end];
        for banned in [
            "SendInput",
            "SetForegroundWindow",
            "SetCursorPos",
            "ElementFromPoint",
            "uia::hit_test",
            "uia::focused",
        ] {
            assert!(!worker.contains(banned), "worker must not use {banned}");
        }
    }

    #[test]
    fn semantic_invoke_reports_dispatch_only() {
        let env = FakeEnv::ok();
        let backend = FakeBackend::with(SemanticResult {
            dispatched: true,
            delivery: Delivery::Delivered,
            verified: false,
            verify: VERIFY_UNAVAILABLE,
            state_after: None,
            foreground_changed: false,
            pointer_moved: false,
            error: None,
        });
        let env_out = run_with(&env, &backend, &request("invoke", None));
        let info = env_out.semantic.expect("semantic");
        assert!(env_out.ok);
        assert!(!env_out.verified);
        assert_eq!(info.delivery, Delivery::Delivered);
        assert_eq!(info.verify.as_deref(), Some(VERIFY_UNAVAILABLE));
    }

    #[test]
    fn semantic_toggle_flip_samples_state_before() {
        // Toggle with no value flips: On -> Off, Off -> On, Indeterminate refuses.
        assert_eq!(
            decide_toggle(ToggleState_On, None),
            Ok(Some(ToggleState_Off))
        );
        assert_eq!(
            decide_toggle(ToggleState_Off, None),
            Ok(Some(ToggleState_On))
        );
        assert_eq!(
            decide_toggle(ToggleState_Indeterminate, None),
            Err("invalid_operation")
        );
    }

    #[test]
    fn semantic_toggle_desired_state_skips_when_already() {
        assert_eq!(decide_toggle(ToggleState_On, Some("on")), Ok(None));
        assert_eq!(decide_toggle(ToggleState_Off, Some("false")), Ok(None));
        assert_eq!(
            decide_toggle(ToggleState_Off, Some("checked")),
            Ok(Some(ToggleState_On))
        );
        assert_eq!(
            decide_toggle(ToggleState_On, Some("unchecked")),
            Ok(Some(ToggleState_Off))
        );
        assert_eq!(
            decide_toggle(ToggleState_On, Some("maybe")),
            Err("invalid_operation")
        );
    }

    #[test]
    fn semantic_toggle_indeterminate_refused() {
        let env = FakeEnv::ok();
        let backend = FakeBackend::with(SemanticResult::refusal("invalid_operation"));
        let env_out = run_with(&env, &backend, &request("toggle", None));
        assert!(!env_out.ok);
        assert_eq!(env_out.error.as_deref(), Some("invalid_operation"));
    }

    #[test]
    fn semantic_toggle_readback_mismatch_is_unknown() {
        let env = FakeEnv::ok();
        let backend = FakeBackend::with(delivered_readback(false, Some("off".into())));
        let env_out = run_with(&env, &backend, &request("toggle", Some("on")));
        let info = env_out.semantic.expect("semantic");
        assert!(env_out.ok);
        assert!(!env_out.verified);
        assert_eq!(info.delivery, Delivery::Unknown);
        assert_eq!(info.verify.as_deref(), Some(VERIFY_READBACK));
    }

    #[test]
    fn semantic_expand_skips_when_expanded() {
        assert_eq!(
            decide_expand(ExpandCollapseState_Expanded, SemanticAction::Expand),
            Ok(true)
        );
        assert_eq!(
            decide_expand(ExpandCollapseState_Collapsed, SemanticAction::Expand),
            Ok(false)
        );
    }

    #[test]
    fn semantic_collapse_skips_when_collapsed() {
        assert_eq!(
            decide_expand(ExpandCollapseState_Collapsed, SemanticAction::Collapse),
            Ok(true)
        );
        assert_eq!(
            decide_expand(ExpandCollapseState_Expanded, SemanticAction::Collapse),
            Ok(false)
        );
    }

    #[test]
    fn semantic_expand_leaf_node_refused() {
        assert_eq!(
            decide_expand(ExpandCollapseState_LeafNode, SemanticAction::Expand),
            Err("unsupported_pattern")
        );
        assert_eq!(
            decide_expand(
                ExpandCollapseState_PartiallyExpanded,
                SemanticAction::Collapse
            ),
            Err("invalid_operation")
        );
    }

    #[test]
    fn semantic_select_skips_when_selected() {
        let env = FakeEnv::ok();
        let backend = FakeBackend::with(skipped());
        let env_out = run_with(&env, &backend, &request("select", None));
        let info = env_out.semantic.expect("semantic");
        assert!(env_out.ok);
        assert!(env_out.verified);
        assert_eq!(info.delivery, Delivery::Delivered);
        assert_eq!(info.verify.as_deref(), Some(VERIFY_READBACK));
    }

    #[test]
    fn semantic_expand_collapse_report_state() {
        assert_eq!(expand_state_name(ExpandCollapseState_Expanded), "expanded");
        assert_eq!(
            expand_state_name(ExpandCollapseState_Collapsed),
            "collapsed"
        );
        assert_eq!(
            expand_state_name(ExpandCollapseState_PartiallyExpanded),
            "partially_expanded"
        );
        assert_eq!(expand_state_name(ExpandCollapseState_LeafNode), "leaf");
    }

    #[test]
    fn semantic_select_reports_selected() {
        let env = FakeEnv::ok();
        let backend = FakeBackend::with(delivered_readback(true, None));
        let env_out = run_with(&env, &backend, &request("select", None));
        assert!(env_out.ok);
        assert!(env_out.verified);
    }

    #[test]
    fn semantic_set_value_refuses_read_only() {
        let env = FakeEnv::ok();
        let backend = FakeBackend::with(SemanticResult::refusal("read_only"));
        let env_out = run_with(&env, &backend, &request("set-value", Some("new name")));
        assert!(!env_out.ok);
        assert_eq!(env_out.error.as_deref(), Some("read_only"));
        assert_eq!(
            env_out.semantic.expect("semantic").delivery,
            Delivery::Undelivered
        );
    }

    #[test]
    fn semantic_set_value_refuses_password_field() {
        let env = FakeEnv::ok();
        let backend = FakeBackend::with(SemanticResult::refusal("password"));
        let env_out = run_with(&env, &backend, &request("set-value", Some("secret")));
        assert_eq!(env_out.error.as_deref(), Some("password"));
        assert!(!env_out.ok);
    }

    #[test]
    fn semantic_set_value_admits_edit_document_combo() {
        for role in ["Edit", "Document", "ComboBox", "combo box", "edit"] {
            assert!(role_admits_set_value(role), "{role} must be admitted");
        }
        for role in ["Button", "Text", "Hyperlink", "button"] {
            assert!(!role_admits_set_value(role), "{role} must be refused");
        }
    }

    #[test]
    fn semantic_set_value_refuses_invalid_role() {
        let env = FakeEnv::ok();
        let backend = FakeBackend::with(SemanticResult::refusal("invalid_role"));
        let env_out = run_with(&env, &backend, &request("set-value", Some("x")));
        assert_eq!(env_out.error.as_deref(), Some("invalid_role"));
    }

    #[test]
    fn semantic_no_role_gate_on_invoke() {
        // The role gate is worker-side and set-value only: an Invoke on a
        // button must reach the backend, not refuse as invalid_role.
        let env = FakeEnv::ok();
        let backend = FakeBackend::with(delivered_readback(true, None));
        let env_out = run_with(&env, &backend, &request("invoke", None));
        assert!(env_out.ok, "{:?}", env_out.error);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn semantic_refuses_disabled_property() {
        let env = FakeEnv::ok();
        let backend = FakeBackend::with(SemanticResult::refusal("disabled"));
        let env_out = run_with(&env, &backend, &request("invoke", None));
        assert_eq!(env_out.error.as_deref(), Some("disabled"));
    }

    #[test]
    fn semantic_refuses_offscreen_property() {
        let env = FakeEnv::ok();
        let backend = FakeBackend::with(SemanticResult::refusal("offscreen"));
        let env_out = run_with(&env, &backend, &request("invoke", None));
        assert_eq!(env_out.error.as_deref(), Some("offscreen"));
    }

    #[test]
    fn semantic_set_value_omits_state_after_and_logs_len_only() {
        let env = FakeEnv::ok();
        let backend = FakeBackend::with(delivered_readback(true, None));
        let env_out = run_with(&env, &backend, &request("set-value", Some("hello")));
        let json = serde_json::to_string(&env_out).expect("json");
        assert!(!json.contains("hello"), "{json}");
        assert!(!json.contains("state_after"), "{json}");
        let meta = logs::ActionMeta {
            action: "set-value".into(),
            delivery: "delivered".into(),
            verified: true,
            len: Some(5),
        };
        let meta_json = serde_json::to_string(&meta).expect("json");
        assert!(meta_json.contains("\"len\":5"), "{meta_json}");
        assert!(!meta_json.contains("hello"), "{meta_json}");
    }

    #[test]
    fn semantic_provider_timeout_is_unknown_and_not_retried() {
        let result = bounded_dispatch(Duration::from_millis(20), || {
            std::thread::sleep(Duration::from_millis(200));
            delivered_readback(true, None)
        });
        assert_eq!(result.delivery, Delivery::Unknown);
        assert_eq!(result.error.expect("error").code, "provider_timeout");
        assert!(!result.dispatched);
    }

    #[test]
    fn semantic_worker_lost_is_unknown() {
        let result = bounded_dispatch(Duration::from_millis(500), || {
            panic!("worker panicked before dispatch")
        });
        assert_eq!(result.delivery, Delivery::Unknown);
        assert_eq!(result.error.expect("error").code, "worker_lost");
    }

    #[test]
    fn semantic_stale_runtime_id_reports_stale_target() {
        let err = HandsError::Target(
            "no UIA element with RuntimeId 42.1; RuntimeId does not survive navigation".into(),
        );
        assert_eq!(resolve_code(&err), "stale_target");
        let err = HandsError::Target("element uia:1.2 has zero-area bounding rect".into());
        assert_eq!(resolve_code(&err), "target_unavailable");
        let err = HandsError::Target("element uia:1.2 center is outside the virtual screen".into());
        assert_eq!(resolve_code(&err), "target_unavailable");
    }

    #[test]
    fn semantic_unsupported_pattern_reports_unsupported() {
        assert_eq!(
            classify_hresult(0x8004_0204),
            ("unsupported_pattern", Delivery::Undelivered)
        );
        assert_eq!(
            classify_hresult(0),
            ("unsupported_pattern", Delivery::Undelivered)
        );
    }

    #[test]
    fn semantic_empty_error_object_goes_through_code() {
        let err = windows::core::Error::empty();
        assert_eq!(err.code().0 as u32, 0);
        assert_eq!(
            classify_hresult(err.code().0 as u32),
            ("unsupported_pattern", Delivery::Undelivered)
        );
    }

    #[test]
    fn semantic_hwnd_mismatch_reports_stale_target() {
        assert_eq!(hwnd_matches(Some(1), Some(1)), Ok(()));
        assert_eq!(hwnd_matches(Some(1), Some(2)), Err("stale_target"));
        assert_eq!(hwnd_matches(Some(1), None), Err("stale_target"));
        assert_eq!(hwnd_matches(None, Some(1)), Ok(()));
    }

    #[test]
    fn semantic_foreground_change_is_reported_not_reactivated() {
        let env = FakeEnv::ok();
        let mut result = delivered_readback(true, None);
        result.foreground_changed = true;
        let backend = FakeBackend::with(result);
        let env_out = run_with(&env, &backend, &request("invoke", None));
        assert!(env_out.semantic.expect("semantic").foreground_changed);
    }

    #[test]
    fn semantic_pointer_move_is_reported() {
        let env = FakeEnv::ok();
        let mut result = delivered_readback(true, None);
        result.pointer_moved = true;
        let backend = FakeBackend::with(result);
        let env_out = run_with(&env, &backend, &request("toggle", None));
        assert!(env_out.semantic.expect("semantic").pointer_moved);
    }

    #[test]
    fn semantic_set_value_ignores_focused_edit_leaf_0115() {
        // The semantic path targets an explicit element; it never consults the
        // focused leaf the way `type` does.
        let src = include_str!("uia_actions.rs");
        let start = src.find("fn run(").expect("run");
        let end = src
            .find("// ------------------------------------------------------------- live worker")
            .expect("worker");
        assert!(!src[start..end].contains("focused"));
    }

    #[test]
    fn semantic_resolve_failure_envelope_is_undelivered_with_code() {
        let mut env = FakeEnv::ok();
        env.resolved = Mutex::new(Some(Err(HandsError::Target(
            "no UIA element with RuntimeId 42.1".into(),
        ))));
        let backend = FakeBackend::default();
        let env_out = run_with(&env, &backend, &request("invoke", None));
        assert!(!env_out.ok);
        assert_eq!(env_out.error.as_deref(), Some("stale_target"));
        assert_eq!(
            env_out.semantic.expect("semantic").delivery,
            Delivery::Undelivered
        );
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn semantic_resolve_failure_preserves_requested_identity() {
        let env = FakeEnv {
            resolved: Mutex::new(Some(Err(HandsError::Target(
                "no UIA element with RuntimeId 42.1".into(),
            )))),
            ..FakeEnv::ok()
        };
        let backend = FakeBackend::default();
        let env_out = run_with(&env, &backend, &request("invoke", None));
        assert_eq!(env_out.target.kind, "element");
        assert_eq!(env_out.target.id.as_deref(), Some("uia:1.2"));
        assert_eq!((env_out.target.x, env_out.target.y), (0, 0));
    }

    #[test]
    fn semantic_envelope_roundtrips_through_deserialize() {
        let env = FakeEnv::ok();
        let backend = FakeBackend::with(delivered_readback(true, None));
        let env_out = run_with(&env, &backend, &request("toggle", None));
        let json = serde_json::to_string(&env_out).expect("json");
        let back: ActuateEnvelope = serde_json::from_str(&json).expect("deserialize");
        let info = back.semantic.expect("semantic");
        assert_eq!(info.action, "toggle");
        assert_eq!(info.delivery, Delivery::Delivered);
    }

    #[test]
    fn semantic_refusal_preserves_yield_and_frozen_desk_errors() {
        let mut env = FakeEnv::ok();
        env.yielded = true;
        let backend = FakeBackend::default();
        let env_out = run_with(&env, &backend, &request("invoke", None));
        // Prose survives: mark_refused only adds semantic metadata + verified=false.
        let error = env_out.error.clone().expect("error prose");
        assert!(error.contains("challenge"), "{error}");
        assert!(!env_out.verified);
    }

    #[test]
    fn semantic_resolve_failure_stamps_cooldown() {
        let mut env = FakeEnv::ok();
        env.resolved = Mutex::new(Some(Err(HandsError::Target(
            "no UIA element with RuntimeId 42.1".into(),
        ))));
        let backend = FakeBackend::default();
        let mut out = run_with(&env, &backend, &request("invoke", None));
        actuate::settle_cooldown("uia-action", &mut out);
        assert!(out.attempt.is_some(), "resolve failure must stamp attempt");
        assert!(!out.ok);
    }

    #[test]
    fn semantic_refusal_stamps_cooldown_and_attempt() {
        let mut env = FakeEnv::ok();
        env.fence = Some(FenceInfo {
            domain: "desktop".into(),
            category: "Messages".into(),
            name: "Send".into(),
            role: "button".into(),
            modes: vec![],
        });
        let backend = FakeBackend::default();
        let mut out = run_with(&env, &backend, &request("invoke", None));
        actuate::settle_cooldown("uia-action", &mut out);
        assert!(out.attempt.is_some());
        assert!(!out.ok);
    }

    #[test]
    fn semantic_success_clears_cooldown() {
        let env = FakeEnv::ok();
        let backend = FakeBackend::with(delivered_readback(true, None));
        let mut out = run_with(&env, &backend, &request("invoke", None));
        actuate::settle_cooldown("uia-action", &mut out);
        assert!(out.ok);
        assert!(out.attempt.is_none());
        assert!(!out.loop_suspected);
    }

    #[test]
    fn semantic_timeout_config_clamps_env_and_flag() {
        let low = ActuateRequest {
            timeout_ms: Some(10),
            ..ActuateRequest::default()
        };
        assert_eq!(timeout_from(&low), Duration::from_millis(MIN_TIMEOUT_MS));
        let high = ActuateRequest {
            timeout_ms: Some(12_000),
            ..ActuateRequest::default()
        };
        assert_eq!(timeout_from(&high), Duration::from_millis(12_000));
    }

    #[test]
    fn semantic_timeout_env_clamps_to_minimum() {
        // The env is process-global; no other test reads it (requests set
        // timeout_ms explicitly), and the gates run serial.
        unsafe { std::env::set_var(TIMEOUT_ENV, "0") };
        let req = ActuateRequest::default();
        assert_eq!(timeout_from(&req), Duration::from_millis(MIN_TIMEOUT_MS));
        unsafe { std::env::set_var(TIMEOUT_ENV, "9000") };
        assert_eq!(timeout_from(&req), Duration::from_millis(9_000));
        unsafe { std::env::remove_var(TIMEOUT_ENV) };
        assert_eq!(
            timeout_from(&req),
            Duration::from_millis(DEFAULT_TIMEOUT_MS)
        );
    }
}
