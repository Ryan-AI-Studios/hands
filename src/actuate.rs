//! High-level click / hover / type / key / scroll / wait_settle / stop.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::bezier::Rng;
use crate::challenge::{self, ChallengeInfo, YIELD_ERROR};
use crate::chrome;
use crate::cooldown::{self, Snapshot};
use crate::error::HandsError;
use crate::extract::ControlKind;
use crate::fence::{self, FenceInfo};
use crate::foreground;
use crate::identity::{IdentityReport, IdentityState, SnapshotRef};
use crate::input;
use crate::lease;
use crate::logs::{self, LogFence, LogTarget};
use crate::observe::ENVELOPE_MAX_BYTES;
use crate::point::{
    PointContract, PointSource, TransformConfidence, TransformMethod, contract_for, map_rect,
};
use crate::session::resolve_session_id_from_os;
use crate::settle;
use crate::space::{Rect, Space, ensure_dpi, virtual_screen};
use crate::target::{ResolvedTarget, Target, TargetOutcome};
use crate::uia;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActuateTarget {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActuateEnvelope {
    pub session_id: String,
    pub ok: bool,
    pub frozen: bool,
    pub target: ActuateTarget,
    pub retried: bool,
    pub settled: bool,
    pub foregrounded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fence: Option<FenceInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge: Option<ChallengeInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roi: Option<Rect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub miss: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub navigated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cooldown_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub loop_suspected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guidance: Option<String>,
    /// Full point contract for `chr:` and explicit targets. Absent on pre-contract refusals.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub point: Option<crate::point::PointContract>,
    #[serde(default = "verified_default", skip_serializing_if = "verified_is_true")]
    pub verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assessment: Option<crate::hittest::Assessment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interactable_checked: Option<bool>,
    /// Snapshot-bound identity evidence/refusal. Absent for grid/pixel and pre-resolve fails.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<IdentityReport>,
    /// True only when a first physical dispatch may already have been sent. 0124 must not
    /// treat `identity` alone as "nothing sent" when this is set.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub post_dispatch: bool,
    #[serde(flatten, default)]
    pub semantic: Option<crate::uia_actions::SemanticInfo>,
}

#[allow(dead_code)]
fn verified_default() -> bool {
    true
}

#[allow(dead_code)]
fn verified_is_true(v: &bool) -> bool {
    *v
}

#[derive(Debug, Clone, Default)]
pub struct ActuateRequest {
    pub session_id: Option<String>,
    pub element_id: Option<String>,
    pub grid: Option<String>,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub w: Option<i32>,
    pub h: Option<i32>,
    pub text: Option<String>,
    pub name: Option<String>,
    pub dy: Option<i32>,
    pub dx: Option<i32>,
    pub blind: bool,
    pub require_interactable: bool,
    pub action: Option<String>,
    pub value: Option<String>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActivateWindow {
    pub pid: u32,
    pub title: String,
    pub hwnd: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActivateEnvelope {
    pub session_id: String,
    pub ok: bool,
    pub foregrounded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<ActivateWindow>,
    #[serde(default)]
    pub frozen: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge: Option<ChallengeInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cooldown_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub loop_suspected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guidance: Option<String>,
}

static LAST_TARGET: Mutex<Option<Rect>> = Mutex::new(None);

fn remember_target(rect: Rect) {
    if let Ok(mut slot) = LAST_TARGET.lock() {
        *slot = Some(rect);
    }
}

/// Process-local last click/hover rect. Standalone `wait_settle` does not read this
/// (bare default is the foreground window). Kept so `remember_target` stays the writer.
#[allow(dead_code)]
fn last_target() -> Option<Rect> {
    LAST_TARGET.lock().ok().and_then(|g| *g)
}

pub fn finalize_envelope(envelope: ActuateEnvelope) -> Result<ActuateEnvelope, HandsError> {
    let json = serialize_envelope(&envelope)?;
    if json.len() > ENVELOPE_MAX_BYTES {
        return Err(HandsError::Input(format!(
            "actuate envelope is {} bytes (hard max {ENVELOPE_MAX_BYTES})",
            json.len()
        )));
    }
    Ok(envelope)
}

pub fn serialize_envelope(envelope: &ActuateEnvelope) -> Result<String, HandsError> {
    serde_json::to_string(envelope)
        .map_err(|err| HandsError::Input(format!("envelope serialize: {err}")))
}

pub fn serialize_activate(envelope: &ActivateEnvelope) -> Result<String, HandsError> {
    serde_json::to_string(envelope)
        .map_err(|err| HandsError::Input(format!("activate envelope: {err}")))
}

struct ActivateHooks {
    inventory: fn() -> Vec<foreground::TitledWindow>,
    offer: fn(Option<isize>, (i32, i32)) -> bool,
    foreground: fn() -> Option<isize>,
    is_live: fn(isize) -> bool,
}

impl ActivateHooks {
    fn live() -> Self {
        Self {
            inventory: foreground::titled_windows,
            offer: foreground::offer,
            foreground: foreground::foreground_hwnd,
            is_live: foreground::is_live_hwnd,
        }
    }
}

fn classify_activate_reason(
    hwnd: isize,
    fg: Option<isize>,
    is_live: fn(isize) -> bool,
) -> &'static str {
    if !is_live(hwnd) {
        "stale_hwnd"
    } else if fg.is_none() {
        "no_foreground_window"
    } else {
        "os_refused"
    }
}

fn stamp_actuate(env: &mut ActuateEnvelope, snap: Snapshot) {
    env.attempt = (snap.attempt > 0).then_some(snap.attempt);
    env.cooldown_ms = snap.cooldown_ms;
    env.loop_suspected = snap.loop_suspected;
    env.guidance = cooldown::guidance(env.frozen, snap);
}

fn stamp_activate(env: &mut ActivateEnvelope, snap: Snapshot) {
    env.attempt = (snap.attempt > 0).then_some(snap.attempt);
    env.cooldown_ms = snap.cooldown_ms;
    env.loop_suspected = snap.loop_suspected;
    env.guidance = cooldown::guidance(env.frozen, snap);
}

pub(crate) fn refuse_if_blocked(
    session_id: &str,
    target: ActuateTarget,
) -> Result<Option<ActuateEnvelope>, HandsError> {
    let frozen = lease::is_frozen();
    if !frozen && !cooldown::is_cooling(session_id) {
        return Ok(None);
    }
    let error = if frozen {
        Some("desk lease frozen (physical input or Pause/Break)".into())
    } else {
        Some("session cooling — honor cooldown_ms; do not retry yet".into())
    };
    finalize_envelope(ActuateEnvelope {
        session_id: session_id.into(),
        ok: false,
        frozen,
        target,
        retried: false,
        settled: false,
        foregrounded: false,
        error,
        fence: None,
        challenge: None,
        roi: None,
        miss: None,
        navigated: false,
        attempt: None,
        cooldown_ms: None,
        loop_suspected: false,
        guidance: None,
        point: None,
        verified: true,
        assessment: None,
        interactable_checked: None,
        semantic: None,
        identity: None,
        post_dispatch: false,
    })
    .map(Some)
}

fn finish_activate(mut envelope: ActivateEnvelope) -> Result<ActivateEnvelope, HandsError> {
    logs::ensure_installed();
    logs::remember_session(&envelope.session_id);
    if envelope.ok && envelope.foregrounded {
        cooldown::note_success(&envelope.session_id);
    } else if !envelope.ok {
        let snap = cooldown::note_rejection(&envelope.session_id);
        stamp_activate(&mut envelope, snap);
    }
    let _ = logs::record_actuate(
        &envelope.session_id,
        "activate",
        envelope.ok,
        envelope.error.as_deref(),
        None,
        None,
        None,
        None,
        false,
    );
    Ok(envelope)
}

pub fn activate(
    session_id: Option<String>,
    window: String,
) -> Result<ActivateEnvelope, HandsError> {
    activate_with(session_id, window, ActivateHooks::live())
}

fn activate_with(
    session_id: Option<String>,
    window: String,
    hooks: ActivateHooks,
) -> Result<ActivateEnvelope, HandsError> {
    let session_id = resolve_session_id_from_os(session_id.as_deref());
    logs::check_write_id(&session_id)?;
    if challenge::yielded() {
        return finish_activate(ActivateEnvelope {
            session_id,
            ok: false,
            foregrounded: false,
            window: None,
            frozen: lease::is_frozen(),
            error: Some(YIELD_ERROR.into()),
            reason: None,
            challenge: Some(challenge::snapshot()),
            attempt: None,
            cooldown_ms: None,
            loop_suspected: false,
            guidance: None,
        });
    }
    let frozen = lease::is_frozen();
    if frozen || cooldown::is_cooling(&session_id) {
        return finish_activate(ActivateEnvelope {
            session_id,
            ok: false,
            foregrounded: false,
            window: None,
            frozen,
            error: Some(if frozen {
                "desk lease frozen (physical input or Pause/Break)".into()
            } else {
                "session cooling — honor cooldown_ms; do not retry yet".into()
            }),
            reason: None,
            challenge: None,
            attempt: None,
            cooldown_ms: None,
            loop_suspected: false,
            guidance: None,
        });
    }
    let inventory = (hooks.inventory)();
    let hit = match crate::observe::resolve_window(&window, &inventory) {
        Ok(w) => w,
        Err(err) => {
            return finish_activate(ActivateEnvelope {
                session_id,
                ok: false,
                foregrounded: false,
                window: None,
                frozen: false,
                error: Some(err.tool_message()),
                reason: None,
                challenge: None,
                attempt: None,
                cooldown_ms: None,
                loop_suspected: false,
                guidance: None,
            });
        }
    };
    match crate::elevation::high_il_reach(Some(hit.hwnd)) {
        Ok(crate::elevation::HighIlReach::Allow) => {}
        Ok(crate::elevation::HighIlReach::ConfirmElevated) => {}
        Ok(crate::elevation::HighIlReach::Refuse) => {
            return finish_activate(ActivateEnvelope {
                session_id,
                ok: false,
                foregrounded: false,
                window: None,
                frozen: false,
                error: Some(crate::elevation::UNGRANTED_MSG.into()),
                reason: None,
                challenge: None,
                attempt: None,
                cooldown_ms: None,
                loop_suspected: false,
                guidance: None,
            });
        }
        Err(e) => {
            return finish_activate(ActivateEnvelope {
                session_id,
                ok: false,
                foregrounded: false,
                window: None,
                frozen: false,
                error: Some(e.tool_message()),
                reason: None,
                challenge: None,
                attempt: None,
                cooldown_ms: None,
                loop_suspected: false,
                guidance: None,
            });
        }
    };
    challenge::note_actuation();
    let center = match hit.rect {
        Some(r) => (r.x + r.w / 2, r.y + r.h / 2),
        None => (0, 0),
    };
    let _ = (hooks.offer)(Some(hit.hwnd), center);
    let fg = (hooks.foreground)();
    let foregrounded = fg == Some(hit.hwnd);
    let reason = if foregrounded {
        None
    } else {
        Some(classify_activate_reason(hit.hwnd, fg, hooks.is_live).into())
    };
    finish_activate(ActivateEnvelope {
        session_id,
        ok: true,
        foregrounded,
        window: Some(ActivateWindow {
            pid: hit.pid,
            title: hit.title,
            hwnd: foreground::format_hwnd(hit.hwnd),
        }),
        frozen: false,
        error: None,
        reason,
        challenge: None,
        attempt: None,
        cooldown_ms: None,
        loop_suspected: false,
        guidance: None,
    })
}

#[allow(clippy::too_many_arguments)]
fn base(
    session_id: String,
    target: ActuateTarget,
    ok: bool,
    frozen: bool,
    retried: bool,
    settled: bool,
    foregrounded: bool,
    error: Option<String>,
) -> Result<ActuateEnvelope, HandsError> {
    finalize_envelope(ActuateEnvelope {
        session_id,
        ok,
        frozen,
        target,
        retried,
        settled,
        foregrounded,
        error,
        fence: None,
        challenge: None,
        roi: None,
        miss: None,
        navigated: false,
        attempt: None,
        cooldown_ms: None,
        loop_suspected: false,
        guidance: None,
        point: None,
        verified: true,
        assessment: None,
        interactable_checked: None,
        semantic: None,
        identity: None,
        post_dispatch: false,
    })
}

fn refuse_yield(session_id: String, target: ActuateTarget) -> Result<ActuateEnvelope, HandsError> {
    finalize_envelope(ActuateEnvelope {
        session_id,
        ok: false,
        frozen: lease::is_frozen(),
        target,
        retried: false,
        settled: false,
        foregrounded: false,
        error: Some(YIELD_ERROR.into()),
        fence: None,
        challenge: Some(challenge::snapshot()),
        roi: None,
        miss: None,
        navigated: false,
        attempt: None,
        cooldown_ms: None,
        loop_suspected: false,
        guidance: None,
        point: None,
        verified: true,
        assessment: None,
        interactable_checked: None,
        semantic: None,
        identity: None,
        post_dispatch: false,
    })
}

pub(crate) fn refuse_if_yielded(
    session_id: &str,
    target: ActuateTarget,
) -> Result<Option<ActuateEnvelope>, HandsError> {
    if challenge::yielded() {
        Ok(Some(refuse_yield(session_id.to_string(), target)?))
    } else {
        Ok(None)
    }
}

fn refuse_if_outside_client(
    session_id: &str,
    info: &ActuateTarget,
    resolved: &crate::target::ResolvedTarget,
) -> Result<Option<ActuateEnvelope>, HandsError> {
    let fg = foreground::foreground_hwnd();
    let Some(intended) = resolved.hwnd.and_then(foreground::root_hwnd).or(fg) else {
        return fail(
            session_id.to_string(),
            info.clone(),
            HandsError::Target(
                "no foreground window to validate the point against; activate a window first"
                    .into(),
            ),
            false,
            false,
            false,
        )
        .map(Some);
    };
    if foreground::point_in_allowed_client(resolved.x, resolved.y, intended, resolved.hwnd, None) {
        return Ok(None);
    }
    let client = foreground::client_rect(intended).unwrap_or(Rect {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    });
    let title = foreground::title(Some(intended));
    let hwnd = foreground::format_hwnd(intended);
    fail(
        session_id.to_string(),
        info.clone(),
        HandsError::Target(format!(
            "point ({},{}) is outside the client rect of window {title:?} hwnd:{hwnd} client=({},{},{},{}); pick a target whose click center is inside that client or an owned popup, or activate the intended window first",
            resolved.x, resolved.y, client.x, client.y, client.w, client.h
        )),
        false,
        false,
        false,
    )
    .map(Some)
}

pub(crate) fn refuse_if_high_il(
    session_id: &str,
    info: &ActuateTarget,
    hwnd: Option<isize>,
    point: Option<(i32, i32)>,
) -> Result<Option<ActuateEnvelope>, HandsError> {
    let probe = crate::elevation::probe_target(hwnd, point);
    match fence::gate_high_il(session_id, probe) {
        Ok(None) => Ok(None),
        Ok(Some(fence_info)) => {
            refuse_fence(session_id.to_string(), info.clone(), fence_info).map(Some)
        }
        Err(err) => fail(
            session_id.to_string(),
            info.clone(),
            err,
            false,
            false,
            false,
        )
        .map(Some),
    }
}

pub(crate) fn refuse_fence(
    session_id: String,
    target: ActuateTarget,
    fence: FenceInfo,
) -> Result<ActuateEnvelope, HandsError> {
    finalize_envelope(ActuateEnvelope {
        session_id,
        ok: false,
        frozen: false,
        target,
        retried: false,
        settled: false,
        foregrounded: false,
        error: None,
        fence: Some(fence),
        challenge: None,
        roi: None,
        miss: None,
        navigated: false,
        attempt: None,
        cooldown_ms: None,
        loop_suspected: false,
        guidance: None,
        point: None,
        verified: true,
        assessment: None,
        interactable_checked: None,
        semantic: None,
        identity: None,
        post_dispatch: false,
    })
}

fn fail(
    session_id: String,
    target: ActuateTarget,
    err: HandsError,
    foregrounded: bool,
    retried: bool,
    settled: bool,
) -> Result<ActuateEnvelope, HandsError> {
    let frozen = matches!(err, HandsError::Lease(_)) || lease::is_frozen();
    base(
        session_id,
        target,
        false,
        frozen,
        retried,
        settled,
        foregrounded,
        Some(err.tool_message()),
    )
}

/// Failure after a point contract existed (dispatch/attempt path). `point` is retained.
#[allow(clippy::too_many_arguments)]
fn fail_with_point(
    session_id: String,
    target: ActuateTarget,
    err: HandsError,
    foregrounded: bool,
    retried: bool,
    settled: bool,
    point: Option<PointContract>,
) -> Result<ActuateEnvelope, HandsError> {
    let mut env = fail(session_id, target, err, foregrounded, retried, settled)?;
    env.point = point;
    finalize_envelope(env)
}

/// Failure after dispatch may have sent input (for example a post-dispatch freeze): keep the
/// contract but serialize `verified:false` so consumers do not read `ok:false` as "nothing sent".
#[allow(clippy::too_many_arguments)]
fn fail_with_point_unverified(
    session_id: String,
    target: ActuateTarget,
    err: HandsError,
    foregrounded: bool,
    retried: bool,
    settled: bool,
    point: Option<PointContract>,
) -> Result<ActuateEnvelope, HandsError> {
    let mut env = fail(session_id, target, err, foregrounded, retried, settled)?;
    env.point = point;
    env.verified = false;
    finalize_envelope(env)
}

fn none_target() -> ActuateTarget {
    let (x, y) = input::cursor_pos().unwrap_or((0, 0));
    ActuateTarget {
        kind: "none".into(),
        id: None,
        x,
        y,
    }
}

fn resolved_info(kind: &str, id: Option<String>, x: i32, y: i32) -> ActuateTarget {
    ActuateTarget {
        kind: kind.into(),
        id,
        x,
        y,
    }
}

fn raw_session(req: &ActuateRequest) -> String {
    resolve_session_id_from_os(req.session_id.as_deref())
}

pub(crate) fn session(req: &ActuateRequest) -> Result<String, HandsError> {
    logs::ensure_installed();
    let id = raw_session(req);
    logs::check_write_id(&id)?;
    logs::remember_session(&id);
    Ok(id)
}

fn log_target(target: &ActuateTarget) -> LogTarget {
    LogTarget {
        kind: target.kind.clone(),
        id: target.id.clone(),
        x: target.x,
        y: target.y,
    }
}

fn log_fence(fence: &FenceInfo) -> LogFence {
    LogFence {
        domain: fence.domain.clone(),
        category: fence.category.clone(),
        name: fence.name.clone(),
        role: fence.role.clone(),
    }
}

fn counts_rejection(tool: &str) -> bool {
    matches!(
        tool,
        "click" | "hover" | "type" | "key" | "scroll" | "uia-action"
    )
}

/// Session-cooldown bookkeeping shared by the physical tools and `uia-action`.
///
/// `ok` counts as success (it means the call was accepted, not that the effect was
/// verified); anything else is a rejection that arms `loop_suspected` / `cooldown_ms`.
pub(crate) fn settle_cooldown(tool: &str, env: &mut ActuateEnvelope) {
    if !counts_rejection(tool) {
        return;
    }
    if env.ok {
        cooldown::note_success(&env.session_id);
        return;
    }
    if let Some(report) = env.identity.as_ref() {
        // DoD-5 exemption: only stale/ambiguous refs and the three binding preconditions
        // skipped a send. Every other refusal (including an exact-state transform refusal)
        // arms the session cooldown exactly as `origin/main` did.
        let exempt_state = matches!(
            report.state,
            IdentityState::Stale | IdentityState::Ambiguous
        );
        let binding_precondition = report.state == IdentityState::Unknown
            && matches!(
                report.reason.as_deref(),
                Some("tab_not_active" | "window_not_focused" | "tab_hidden")
            );
        if exempt_state || binding_precondition {
            return;
        }
        // `binding_changed_after_dispatch` is intentionally NOT exempt: the first click was
        // already sent, so an unverified dispatched action counts toward loop protection.
    }
    let snap = cooldown::note_rejection(&env.session_id);
    stamp_actuate(env, snap);
}

fn after_actuate(
    tool: &str,
    result: Result<ActuateEnvelope, HandsError>,
    type_len: Option<usize>,
    key: Option<&str>,
    blind: bool,
) -> Result<ActuateEnvelope, HandsError> {
    logs::ensure_installed();
    let result = match result {
        Ok(mut env) => {
            settle_cooldown(tool, &mut env);
            Ok(env)
        }
        Err(err) => Err(err),
    };
    if let Ok(env) = &result {
        logs::remember_session(&env.session_id);
        let _ = logs::record_actuate(
            &env.session_id,
            tool,
            env.ok,
            env.error.as_deref(),
            Some(log_target(&env.target)),
            env.fence.as_ref().map(log_fence),
            type_len,
            key,
            blind,
        );
    }
    result
}

fn resolve_req(req: &ActuateRequest, space: Space) -> Result<TargetOutcome, HandsError> {
    let target = Target::parse(req.element_id.as_deref(), req.grid.as_deref(), req.x, req.y)?;
    target.resolve_with_opts(
        space,
        foreground::foreground_hwnd(),
        req.require_interactable,
    )
}

/// Transform-invalid refusal: `ok:false`, `verified:false`, full `point` contract, no
/// `offer`, no `SendInput`, and no fence prompt (this runs before `ensure_installed`).
pub(crate) fn refuse_transform(
    session_id: String,
    target: ActuateTarget,
    contract: PointContract,
    identity: Option<IdentityReport>,
) -> Result<ActuateEnvelope, HandsError> {
    // Spec §3.7: the identity resolves, but the 0129 transform refuses. Keep `state` as the
    // resolved identity (exact/legacy) and put the named transform reason in both `error` and
    // `identity.reason`; the envelope is `ok:false` + `verified:false` +
    // `point.transform.valid=false`, so 0124 must gate on those, not on `identity.state` alone.
    let reason = contract
        .transform
        .reason
        .clone()
        .unwrap_or_else(|| "transform is not valid".into());
    let identity = identity.map(|report| {
        // Closed post-transform identity token; the 0129 detail stays in top-level `error`.
        report.with_reason("transform_invalid")
    });
    finalize_envelope(ActuateEnvelope {
        session_id,
        ok: false,
        frozen: lease::is_frozen(),
        target,
        retried: false,
        settled: false,
        foregrounded: false,
        error: Some(reason),
        fence: None,
        challenge: None,
        roi: None,
        miss: None,
        navigated: false,
        attempt: None,
        cooldown_ms: None,
        loop_suspected: false,
        guidance: None,
        point: Some(contract),
        verified: false,
        assessment: None,
        interactable_checked: None,
        identity,
        post_dispatch: false,
        semantic: None,
    })
}

/// Identity refusal: nothing was sent. `error` carries the closed reason token, `identity`
/// carries state/binding/hint, and no point/verified key is fabricated. `target.x/y = 0`
/// is documented as "not a point"; 0124 must not use it.
pub(crate) fn refuse_identity(
    session_id: String,
    mut target: ActuateTarget,
    report: IdentityReport,
) -> Result<ActuateEnvelope, HandsError> {
    target.x = 0;
    target.y = 0;
    if let Some(r) = report.r#ref.clone() {
        target.id = Some(r);
    }
    let error = report
        .reason
        .clone()
        .unwrap_or_else(|| report.state_token().to_string());
    let mut env = base(
        session_id,
        target,
        false,
        lease::is_frozen(),
        false,
        false,
        false,
        Some(error),
    )?;
    env.identity = Some(report);
    finalize_envelope(env)
}

fn unresolved_info(target: &Target, contract: &PointContract) -> ActuateTarget {
    let id = match target {
        Target::ElementId(id) => Some(id.clone()),
        Target::Scoped(r) => Some(r.to_string()),
        Target::Grid { col, row } => Some(format!("g:{col}:{row}")),
        Target::Pixel { .. } => None,
    };
    let (x, y) = contract.physical.map(|p| (p.x, p.y)).unwrap_or((0, 0));
    ActuateTarget {
        kind: "element".into(),
        id,
        x,
        y,
    }
}

fn identity_info(target: &Target, report: &IdentityReport) -> ActuateTarget {
    ActuateTarget {
        kind: "element".into(),
        id: report.r#ref.clone().or_else(|| match target {
            Target::ElementId(id) => Some(id.clone()),
            Target::Scoped(r) => Some(r.to_string()),
            _ => None,
        }),
        x: 0,
        y: 0,
    }
}

/// Final live re-map: live Win32 geometry + recorded page metrics, before the outside-client
/// guard and any side effect. Move-only updates the point; a size/basis/DPI/monitor change
/// returns a `stale_viewport` refusal.
fn final_live_remap(
    session_id: &str,
    info: &mut ActuateTarget,
    resolved: &mut ResolvedTarget,
) -> Result<Option<ActuateEnvelope>, HandsError> {
    let Some(contract) = resolved.contract.as_ref() else {
        // No contract at all: fail closed rather than dispatching at the resolve-time point.
        let bad = crate::point::PointContract {
            source: PointSource::Chr,
            css: None,
            physical: None,
            transform: crate::point::TransformReport::invalid(
                "window_mismatch: target has no point contract to re-map",
                None,
            ),
        };
        return refuse_transform(
            session_id.to_string(),
            info.clone(),
            bad,
            resolved.identity.clone(),
        )
        .map(Some);
    };
    if contract.source != PointSource::Chr {
        return Ok(None);
    }
    // Fail closed: a `chr:` contract without its recorded CSS point or map cannot be re-mapped.
    let (Some(css), Some(map)) = (resolved.css_point, resolved.map.as_ref()) else {
        let mut bad = contract.clone();
        bad.physical = None;
        bad.transform.valid = false;
        bad.transform.method = TransformMethod::None;
        bad.transform.confidence = TransformConfidence::None;
        bad.transform.reason =
            Some("stale_viewport: chr: target has no recorded CSS point or map".into());
        return refuse_transform(
            session_id.to_string(),
            info.clone(),
            bad,
            resolved.identity.clone(),
        )
        .map(Some);
    };
    let Some(hwnd) = resolved.hwnd else {
        let mut bad = contract.clone();
        bad.physical = None;
        bad.transform.valid = false;
        bad.transform.method = TransformMethod::None;
        bad.transform.confidence = TransformConfidence::None;
        bad.transform.reason = Some("window_mismatch: chr: target has no Chrome HWND".into());
        return refuse_transform(
            session_id.to_string(),
            info.clone(),
            bad,
            resolved.identity.clone(),
        )
        .map(Some);
    };
    match chrome::remap_chr(map, hwnd) {
        Ok(new_map) => {
            let new_contract = contract_for(&new_map, css);
            let Some(p) = new_contract.physical else {
                let mut bad = new_contract;
                bad.physical = None;
                bad.transform.valid = false;
                bad.transform.method = TransformMethod::None;
                bad.transform.confidence = TransformConfidence::None;
                bad.transform.reason = Some("stale_viewport: point left the viewport".into());
                return refuse_transform(
                    session_id.to_string(),
                    info.clone(),
                    bad,
                    resolved.identity.clone(),
                )
                .map(Some);
            };
            let rect = resolved
                .css_rect
                .and_then(|r| map_rect(&new_contract, r).ok())
                .unwrap_or(resolved.rect);
            resolved.x = p.x;
            resolved.y = p.y;
            resolved.rect = rect;
            resolved.contract = Some(new_contract);
            resolved.map = Some(new_map);
            info.x = p.x;
            info.y = p.y;
            if let Some(env) = scoped_binding_refusal(session_id, info, &resolved.target)? {
                return Ok(Some(env));
            }
            Ok(None)
        }
        Err(reason) => {
            let mut bad = contract.clone();
            bad.physical = None;
            bad.transform.valid = false;
            bad.transform.method = TransformMethod::None;
            bad.transform.confidence = TransformConfidence::None;
            bad.transform.reason = Some(reason);
            refuse_transform(
                session_id.to_string(),
                info.clone(),
                bad,
                resolved.identity.clone(),
            )
            .map(Some)
        }
    }
}

fn point_for_dispatch(resolved: &ResolvedTarget) -> Option<PointContract> {
    resolved.contract.clone()
}

/// Final binding gate for scoped Chrome refs. Used after the 0129 remap and again immediately
/// before `left_click` (narrowing the resolve→dispatch residual; still not a content read).
fn scoped_binding_refusal(
    session_id: &str,
    info: &ActuateTarget,
    target: &Target,
) -> Result<Option<ActuateEnvelope>, HandsError> {
    let Some(report) = scoped_binding_report(target)? else {
        return Ok(None);
    };
    refuse_identity(session_id.to_string(), info.clone(), report).map(Some)
}

fn scoped_binding_report(target: &Target) -> Result<Option<IdentityReport>, HandsError> {
    let Target::Scoped(
        r @ SnapshotRef::Chrome {
            tab,
            frame,
            doc,
            snap,
            ..
        },
    ) = target
    else {
        return Ok(None);
    };
    // `evidence.window` is corroboration only: a window drag/move keeps the tab-id binding
    // valid (spec §3.3), so only active/focused gate the dispatch.
    match chrome::verify_binding(*tab) {
        Ok(evidence) if evidence.active && evidence.focused => {}
        Ok(evidence) => {
            let reason = if !evidence.active {
                "tab_not_active"
            } else {
                "window_not_focused"
            };
            return Ok(Some(IdentityReport::chrome_failure(
                r,
                IdentityState::Unknown,
                reason,
            )));
        }
        Err(report) => return Ok(Some(report)),
    }
    if *frame > 0 {
        return match chrome::verify_frame_binding(*tab, *frame, doc, snap) {
            Ok(evidence) if evidence.doc == *doc => Ok(None),
            Ok(_) => Ok(Some(IdentityReport::chrome_failure(
                r,
                IdentityState::Stale,
                "frame_document_changed",
            ))),
            Err(report) => Ok(Some(report)),
        };
    }
    Ok(None)
}

/// Pre-move frame re-read: a frame that was removed or navigated refuses before the pointer
/// moves. This is the distinct `frame_binding` op, never the top-frame `binding` branch.
fn frame_binding_refusal(
    session_id: &str,
    info: &ActuateTarget,
    target: &Target,
) -> Result<Option<ActuateEnvelope>, HandsError> {
    let Target::Scoped(
        r @ SnapshotRef::Chrome {
            tab,
            frame,
            doc,
            snap,
            ..
        },
    ) = target
    else {
        return Ok(None);
    };
    if *frame == 0 {
        return Ok(None);
    }
    match chrome::verify_frame_binding(*tab, *frame, doc, snap) {
        Ok(evidence) if evidence.doc == *doc => Ok(None),
        Ok(_) => refuse_identity(
            session_id.to_string(),
            info.clone(),
            IdentityReport::chrome_failure(r, IdentityState::Stale, "frame_document_changed"),
        )
        .map(Some),
        Err(report) => refuse_identity(session_id.to_string(), info.clone(), report).map(Some),
    }
}

/// The first click was already sent, so a binding change before the retry must not be reported
/// as a pre-dispatch "nothing sent" refusal. Keep the point and serialize `verified:false`.
fn post_dispatch_binding_refusal(
    session_id: String,
    info: ActuateTarget,
    resolved: &ResolvedTarget,
    settled: bool,
    foregrounded: bool,
    report: IdentityReport,
) -> Result<ActuateEnvelope, HandsError> {
    let mut report = report;
    report = report.with_reason("binding_changed_after_dispatch");
    let reason = report
        .reason
        .clone()
        .unwrap_or_else(|| report.state_token().to_string());
    let mut env = base(
        session_id,
        info,
        false,
        lease::is_frozen(),
        false,
        settled,
        foregrounded,
        Some(reason),
    )?;
    env.point = point_for_dispatch(resolved);
    env.verified = false;
    env.post_dispatch = true;
    env.identity = Some(report);
    finalize_envelope(env)
}

pub fn click(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    let blind = req.blind;
    let require_interactable = req.require_interactable;
    let result = click_inner(req).map(|mut env| {
        if require_interactable && env.interactable_checked.is_none() {
            // Positive marker on every envelope from a binary that understood the flag.
            env.interactable_checked = Some(true);
        }
        env
    });
    after_actuate("click", result, None, None, blind)
}

fn click_miss(same: bool, focus_lost: bool) -> Option<&'static str> {
    if focus_lost {
        Some("focus_lost")
    } else if same {
        Some("no_change")
    } else {
        None
    }
}

const CAPTION_POLL_BUDGET: Duration = Duration::from_millis(300);

#[derive(Debug)]
struct ClickEffect {
    navigated: bool,
    miss: Option<&'static str>,
}

/// Chrome caption change or interstitial title. Empty→empty is not navigation.
fn caption_nav(chrome: bool, before: &str, after: &str) -> bool {
    if !chrome {
        return false;
    }
    if settle::title_blocks_settled(after) {
        return true;
    }
    after != before && !(before.is_empty() && after.is_empty())
}

fn poll_caption_nav(chrome: bool, before: &str, hwnd: Option<isize>) -> bool {
    if !chrome {
        return false;
    }
    let deadline = Instant::now() + CAPTION_POLL_BUDGET;
    while Instant::now() < deadline {
        std::thread::sleep(settle::FRAME_GAP);
        if caption_nav(chrome, before, &foreground::title(hwnd)) {
            return true;
        }
    }
    false
}

fn url_nav(before: Option<&str>, after: Option<&str>) -> bool {
    match (before, after) {
        (Some(a), Some(b)) => a != b,
        _ => false,
    }
}

fn click_effect_after_settle(
    hwnd: Option<isize>,
    title_before: &str,
    url_before: Option<&str>,
    same: bool,
    focus_lost: bool,
    allow_poll: bool,
) -> ClickEffect {
    let chrome = foreground::target_is_chrome(hwnd);
    let after = foreground::title(hwnd);
    let mut navigated = caption_nav(chrome, title_before, &after);
    if !navigated && !chrome {
        navigated = url_nav(url_before, uia::page_url(hwnd).as_deref());
    }
    if !navigated && allow_poll && same && chrome {
        navigated = poll_caption_nav(chrome, title_before, hwnd);
    }
    if navigated {
        ClickEffect {
            navigated: true,
            miss: None,
        }
    } else {
        ClickEffect {
            navigated: false,
            miss: click_miss(same, focus_lost),
        }
    }
}

fn should_retry_click(effect: &ClickEffect) -> bool {
    effect.miss.is_some() && !effect.navigated
}

fn click_inner(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    let session_id = match session(&req) {
        Ok(id) => id,
        Err(err) => return fail(raw_session(&req), none_target(), err, false, false, false),
    };
    let space = match ensure_dpi().and_then(|_| virtual_screen()) {
        Ok(s) => s,
        Err(err) => return fail(session_id, none_target(), err, false, false, false),
    };
    if req.blind && req.element_id.is_some() {
        return fail(
            session_id,
            none_target(),
            HandsError::Input(
                "blind click does not support --element-id; provide --x/--y or --grid".into(),
            ),
            false,
            false,
            false,
        );
    }
    if req.require_interactable && req.blind {
        return fail(
            session_id,
            none_target(),
            HandsError::Input("interactable_unavailable: click on chr: or sref1:c only".into()),
            false,
            false,
            false,
        );
    }
    let outcome = match resolve_req(&req, space) {
        Ok(r) => r,
        Err(err) => return fail(session_id, none_target(), err, false, false, false),
    };
    let (mut resolved, contract_refusal, identity_refusal, mut info, guard_hwnd, guard_point) =
        match outcome {
            TargetOutcome::Resolved(r) => {
                let info = resolved_info(r.kind, r.id.clone(), r.x, r.y);
                (Some(*r), None, None, info, None, None)
            }
            TargetOutcome::Unresolved {
                target,
                contract,
                hwnd,
                assessment: _,
                identity,
            } => {
                let info = unresolved_info(&target, &contract);
                let point = contract.physical.map(|p| (p.x, p.y));
                (None, Some((contract, identity)), None, info, hwnd, point)
            }
            TargetOutcome::Identity { target, report } => {
                let info = identity_info(&target, &report);
                (None, None, Some(report), info, None, None)
            }
        };
    let guard_hwnd = guard_hwnd.or_else(|| resolved.as_ref().and_then(|r| r.hwnd));
    let guard_point = guard_point.or_else(|| resolved.as_ref().map(|r| (r.x, r.y)));
    if let Some(env) = refuse_if_yielded(&session_id, info.clone())? {
        return Ok(env);
    }
    if let Some(env) = refuse_if_blocked(&session_id, info.clone())? {
        return Ok(env);
    }
    if let Some(env) = refuse_if_high_il(&session_id, &info, guard_hwnd, guard_point)? {
        return Ok(env);
    }
    if let Some(report) = identity_refusal {
        return refuse_identity(session_id, info, report);
    }
    if let Some((contract, identity)) = contract_refusal {
        return refuse_transform(session_id, info, *contract, identity);
    }
    if req.require_interactable {
        let resolved_ref = resolved.as_ref().expect("resolved outcome");
        if let Some(env) =
            crate::hittest::guard_require_interactable(&session_id, &info, resolved_ref)?
        {
            return Ok(env);
        }
    }
    let resolved_ref = resolved.as_mut().expect("resolved outcome");
    fence::ensure_installed();
    if req.blind {
        match fence::gate_blind(&session_id) {
            Ok(None) => {}
            Ok(Some(info_fence)) => return refuse_fence(session_id, info, info_fence),
            Err(err) => return fail(session_id, info, err, false, false, false),
        }
    } else {
        match fence::gate_click(&session_id, resolved_ref) {
            Ok(None) => {}
            Ok(Some(info_fence)) => return refuse_fence(session_id, info, info_fence),
            Err(err) => return fail(session_id, info, err, false, false, false),
        }
    }
    if !req.blind
        && let Some(env) = final_live_remap(&session_id, &mut info, resolved_ref)?
    {
        return Ok(env);
    }
    let resolved = resolved.expect("resolved outcome");
    if let Some(env) = refuse_if_outside_client(&session_id, &info, &resolved)? {
        return Ok(env);
    }
    if req.blind {
        return dispatch_blind_click(session_id, info, resolved, space);
    }
    let title_before = foreground::title(resolved.hwnd);
    let chrome = foreground::target_is_chrome(resolved.hwnd);
    let url_before = if chrome {
        None
    } else {
        uia::page_url(resolved.hwnd)
    };
    challenge::note_actuation_if_proceeding(false);
    remember_target(resolved.rect);
    let mut rng = Rng::from_time();
    let roi = settle::default_roi(space, Some(resolved.rect), (resolved.x, resolved.y));
    if let Some(env) = frame_binding_refusal(&session_id, &info, &resolved.target)? {
        return Ok(env);
    }
    let foregrounded = foreground::offer(resolved.hwnd, (resolved.x, resolved.y));
    if let Err(err) = input::move_to(space, resolved.x, resolved.y, &mut rng) {
        return fail_with_point(
            session_id,
            info,
            err,
            foregrounded,
            false,
            false,
            point_for_dispatch(&resolved),
        );
    }
    let snapshot = match crate::capture::capture_roi(space, roi) {
        Ok(f) => f,
        Err(err) => {
            return fail_with_point(
                session_id,
                info,
                err,
                foregrounded,
                false,
                false,
                point_for_dispatch(&resolved),
            );
        }
    };
    // Narrow R7: re-probe tabs/windows immediately before the click. A tab switch during
    // offer/move/capture is still refused with no input; the probe→click microseconds remain
    // the named residual.
    if let Some(env) = scoped_binding_refusal(&session_id, &info, &resolved.target)? {
        return Ok(env);
    }
    if let Err(err) = input::left_click(&mut rng) {
        return fail_with_point_unverified(
            session_id,
            info,
            err,
            foregrounded,
            false,
            false,
            point_for_dispatch(&resolved),
        );
    }
    let (mut settled, after) = match settle::wait_settle(space, roi) {
        Ok(v) => v,
        Err(err) => {
            return fail_with_point_unverified(
                session_id,
                info,
                err,
                foregrounded,
                false,
                false,
                point_for_dispatch(&resolved),
            );
        }
    };
    let same = settle::roi_unchanged(&snapshot, &after);
    let focus_lost = foregrounded
        && resolved.hwnd.is_some()
        && !foreground::same_top_level(resolved.hwnd, foreground::foreground_hwnd());
    let mut effect = click_effect_after_settle(
        resolved.hwnd,
        &title_before,
        url_before.as_deref(),
        same,
        focus_lost,
        true,
    );
    let mut retried = false;
    if should_retry_click(&effect) && lease::poll().is_ok() {
        retried = true;
        if focus_lost {
            let _ = foreground::offer(resolved.hwnd, (resolved.x, resolved.y));
        }
        // A tab switch during the first click/settle must not re-click the foreground tab.
        // The first click already went out, so this is a post-dispatch refusal.
        if let Some(env) = scoped_binding_refusal(&session_id, &info, &resolved.target)? {
            let report = env.identity.expect("binding refusal has identity");
            return post_dispatch_binding_refusal(
                session_id,
                info,
                &resolved,
                settled,
                foregrounded,
                report,
            );
        }
        if let Err(err) = input::left_click(&mut rng) {
            return fail_with_point_unverified(
                session_id,
                info,
                err,
                foregrounded,
                true,
                settled,
                point_for_dispatch(&resolved),
            );
        }
        let (settled2, after2) = match settle::wait_settle(space, roi) {
            Ok(v) => v,
            Err(err) => {
                return fail_with_point_unverified(
                    session_id,
                    info,
                    err,
                    foregrounded,
                    true,
                    settled,
                    point_for_dispatch(&resolved),
                );
            }
        };
        settled = settled2;
        let same = settle::roi_unchanged(&snapshot, &after2);
        let focus_lost = foregrounded
            && resolved.hwnd.is_some()
            && !foreground::same_top_level(resolved.hwnd, foreground::foreground_hwnd());
        effect = click_effect_after_settle(
            resolved.hwnd,
            &title_before,
            url_before.as_deref(),
            same,
            focus_lost,
            false,
        );
    }
    if lease::is_frozen() {
        let mut env = base(
            session_id,
            info,
            false,
            true,
            retried,
            settled,
            foregrounded,
            Some("desk lease frozen (physical input or Pause/Break)".into()),
        )?;
        env.point = point_for_dispatch(&resolved);
        // The click may already have landed at the now-frozen desk.
        env.verified = false;
        return finalize_envelope(env);
    }
    let mut env = base(
        session_id,
        info,
        true,
        false,
        retried,
        settled,
        foregrounded,
        None,
    )?;
    env.point = point_for_dispatch(&resolved);
    env.miss = effect.miss.map(str::to_string);
    env.navigated = effect.navigated;
    env.assessment = resolved.assessment.clone();
    env.identity = resolved.identity.clone();
    finalize_envelope(env)
}

fn dispatch_blind_click(
    session_id: String,
    info: ActuateTarget,
    resolved: crate::target::ResolvedTarget,
    space: Space,
) -> Result<ActuateEnvelope, HandsError> {
    challenge::note_actuation_if_proceeding(false);
    remember_target(resolved.rect);
    let point = Some(crate::point::explicit_contract(
        PointSource::Blind,
        crate::point::PhysicalPoint {
            x: resolved.x,
            y: resolved.y,
        },
    ));
    let mut rng = Rng::from_time();
    let foregrounded = foreground::offer(resolved.hwnd, (resolved.x, resolved.y));
    if let Err(err) = input::move_to(space, resolved.x, resolved.y, &mut rng) {
        return fail_with_point(session_id, info, err, foregrounded, false, false, point);
    }
    if let Err(err) = input::left_click(&mut rng) {
        return fail_with_point_unverified(
            session_id,
            info,
            err,
            foregrounded,
            false,
            false,
            point,
        );
    }
    if lease::is_frozen() {
        let mut env = base(
            session_id,
            info,
            false,
            true,
            false,
            false,
            foregrounded,
            Some("desk lease frozen (physical input or Pause/Break)".into()),
        )?;
        env.point = point;
        // The click may already have landed at an unverified blind target.
        env.verified = false;
        return finalize_envelope(env);
    }
    let mut env = base(
        session_id,
        info,
        true,
        false,
        false,
        false,
        foregrounded,
        None,
    )?;
    env.verified = false;
    env.point = Some(crate::point::explicit_contract(
        PointSource::Blind,
        crate::point::PhysicalPoint {
            x: resolved.x,
            y: resolved.y,
        },
    ));
    finalize_envelope(env)
}

pub fn hover(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    let blind = req.blind;
    after_actuate("hover", hover_inner(req), None, None, blind)
}

fn hover_inner(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    let session_id = match session(&req) {
        Ok(id) => id,
        Err(err) => return fail(raw_session(&req), none_target(), err, false, false, false),
    };
    if req.require_interactable {
        return fail(
            session_id,
            none_target(),
            HandsError::Input("interactable_unavailable: click on chr: or sref1:c only".into()),
            false,
            false,
            false,
        );
    }
    if req.blind {
        return fail(
            session_id,
            none_target(),
            HandsError::Input("blind is click/type only".into()),
            false,
            false,
            false,
        );
    }
    let space = match ensure_dpi().and_then(|_| virtual_screen()) {
        Ok(s) => s,
        Err(err) => return fail(session_id, none_target(), err, false, false, false),
    };
    let outcome = match resolve_req(&req, space) {
        Ok(r) => r,
        Err(err) => return fail(session_id, none_target(), err, false, false, false),
    };
    let (mut resolved, contract_refusal, identity_refusal, mut info, guard_hwnd, guard_point) =
        match outcome {
            TargetOutcome::Resolved(r) => {
                let info = resolved_info(r.kind, r.id.clone(), r.x, r.y);
                (Some(*r), None, None, info, None, None)
            }
            TargetOutcome::Unresolved {
                target,
                contract,
                hwnd,
                assessment: _,
                identity,
            } => {
                let info = unresolved_info(&target, &contract);
                let point = contract.physical.map(|p| (p.x, p.y));
                (None, Some((contract, identity)), None, info, hwnd, point)
            }
            TargetOutcome::Identity { target, report } => {
                let info = identity_info(&target, &report);
                (None, None, Some(report), info, None, None)
            }
        };
    let guard_hwnd = guard_hwnd.or_else(|| resolved.as_ref().and_then(|r| r.hwnd));
    let guard_point = guard_point.or_else(|| resolved.as_ref().map(|r| (r.x, r.y)));
    if let Some(env) = refuse_if_yielded(&session_id, info.clone())? {
        return Ok(env);
    }
    if let Some(env) = refuse_if_blocked(&session_id, info.clone())? {
        return Ok(env);
    }
    if let Some(env) = refuse_if_high_il(&session_id, &info, guard_hwnd, guard_point)? {
        return Ok(env);
    }
    if let Some(report) = identity_refusal {
        return refuse_identity(session_id, info, report);
    }
    if let Some((contract, identity)) = contract_refusal {
        return refuse_transform(session_id, info, *contract, identity);
    }
    let resolved_ref = resolved.as_mut().expect("resolved outcome");
    if let Some(env) = final_live_remap(&session_id, &mut info, resolved_ref)? {
        return Ok(env);
    }
    let resolved = resolved.expect("resolved outcome");
    if let Some(env) = refuse_if_outside_client(&session_id, &info, &resolved)? {
        return Ok(env);
    }
    remember_target(resolved.rect);
    let mut rng = Rng::from_time();
    let foregrounded = foreground::offer(resolved.hwnd, (resolved.x, resolved.y));
    // Re-probe after `offer` and immediately before the pointer move; a tab switch here must
    // not land the pointer in the foreground tab (B1).
    if let Some(env) = scoped_binding_refusal(&session_id, &info, &resolved.target)? {
        return Ok(env);
    }
    if let Err(err) = input::move_to(space, resolved.x, resolved.y, &mut rng) {
        return fail_with_point(
            session_id,
            info,
            err,
            foregrounded,
            false,
            false,
            point_for_dispatch(&resolved),
        );
    }
    if let Err(err) = hover_dwell() {
        return fail_with_point_unverified(
            session_id,
            info,
            err,
            foregrounded,
            false,
            false,
            point_for_dispatch(&resolved),
        );
    }
    let mut env = base(
        session_id,
        info,
        true,
        false,
        false,
        false,
        foregrounded,
        None,
    )?;
    env.point = point_for_dispatch(&resolved);
    env.identity = resolved.identity.clone();
    finalize_envelope(env)
}

pub fn type_text(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    let type_len = req.text.as_ref().map(|t| t.chars().count());
    let blind = req.blind;
    after_actuate("type", type_text_inner(req), type_len, None, blind)
}

fn type_text_inner(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    let session_id = match session(&req) {
        Ok(id) => id,
        Err(err) => return fail(raw_session(&req), none_target(), err, false, false, false),
    };
    let info = none_target();
    if let Some(env) = refuse_if_yielded(&session_id, info.clone())? {
        return Ok(env);
    }
    let Some(text) = req.text.as_deref() else {
        return fail(
            session_id,
            info,
            HandsError::Input("type requires --text".into()),
            false,
            false,
            false,
        );
    };
    if text.contains('\n') || text.contains('\r') {
        return fail(
            session_id,
            info,
            HandsError::Fence("type cannot contain newline; use key enter to submit".into()),
            false,
            false,
            false,
        );
    }
    if let Some(env) = refuse_if_blocked(&session_id, info.clone())? {
        return Ok(env);
    }
    if let Some(env) = refuse_if_high_il(&session_id, &info, None, None)? {
        return Ok(env);
    }
    if !req.blind {
        if let Some(env) = refuse_unless_focused_editable(&session_id, &info)? {
            return Ok(env);
        }
    } else {
        match fence::gate_blind(&session_id) {
            Ok(None) => {}
            Ok(Some(info_fence)) => return refuse_fence(session_id, info, info_fence),
            Err(err) => return fail(session_id, info, err, false, false, false),
        }
    }
    if let Err(err) = ensure_dpi() {
        return fail(session_id, info, err, false, false, false);
    }
    challenge::note_actuation();
    match input::type_text(text) {
        Ok(_) => {
            let mut env = base(session_id, info, true, false, false, false, true, None)?;
            if req.blind {
                env.verified = false;
            }
            Ok(env)
        }
        Err(err) => fail(session_id, info, err, false, false, false),
    }
}

fn refuse_unless_focused_editable(
    session_id: &str,
    info: &ActuateTarget,
) -> Result<Option<ActuateEnvelope>, HandsError> {
    let Some(fg) = foreground::foreground_hwnd() else {
        return fail(
            session_id.to_string(),
            info.clone(),
            HandsError::Target(
                "focused control is not in the foreground window; activate the intended window and click an editable field first"
                    .into(),
            ),
            false,
            false,
            false,
        )
        .map(Some);
    };
    let leaf = match uia::focused_leaf() {
        Ok(leaf) => leaf,
        Err(_) => {
            return fail(
                session_id.to_string(),
                info.clone(),
                HandsError::Target(
                    "no focused edit, combo, or document in the foreground window; click an editable field first"
                        .into(),
                ),
                false,
                false,
                false,
            )
            .map(Some);
        }
    };
    if !matches!(
        leaf.kind,
        ControlKind::Edit | ControlKind::ComboBox | ControlKind::Document
    ) {
        return fail(
            session_id.to_string(),
            info.clone(),
            HandsError::Target(
                "focused control is not an edit, combo, or document; click an editable field first"
                    .into(),
            ),
            false,
            false,
            false,
        )
        .map(Some);
    }
    let Some(hwnd) = leaf.hwnd else {
        return fail(
            session_id.to_string(),
            info.clone(),
            HandsError::Target(
                "focused control is not in the foreground window; activate the intended window and click an editable field first"
                    .into(),
            ),
            false,
            false,
            false,
        )
        .map(Some);
    };
    if !foreground::related_to_root(hwnd, fg) {
        return fail(
            session_id.to_string(),
            info.clone(),
            HandsError::Target(
                "focused control is not in the foreground window; activate the intended window and click an editable field first"
                    .into(),
            ),
            false,
            false,
            false,
        )
        .map(Some);
    }
    Ok(None)
}

pub fn key(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    let name = req.name.clone();
    after_actuate("key", key_inner(req), None, name.as_deref(), false)
}

fn key_inner(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    let session_id = match session(&req) {
        Ok(id) => id,
        Err(err) => return fail(raw_session(&req), none_target(), err, false, false, false),
    };
    let info = none_target();
    let Some(name) = req.name.as_deref() else {
        return fail(
            session_id,
            info,
            HandsError::Input("key requires --name".into()),
            false,
            false,
            false,
        );
    };
    if let Some(env) = refuse_if_yielded(&session_id, info.clone())? {
        return Ok(env);
    }
    if let Some(env) = refuse_if_blocked(&session_id, info.clone())? {
        return Ok(env);
    }
    if let Some(env) = refuse_if_high_il(&session_id, &info, None, None)? {
        return Ok(env);
    }
    if input::is_enter_key(name) {
        fence::ensure_installed();
        match fence::gate_enter(&session_id) {
            Ok(None) => {}
            Ok(Some(info_fence)) => return refuse_fence(session_id, info, info_fence),
            Err(err) => return fail(session_id, info, err, false, false, false),
        }
    }
    if let Err(err) = ensure_dpi() {
        return fail(session_id, info, err, false, false, false);
    }
    challenge::note_actuation();
    match input::named_key(name) {
        Ok(()) => base(session_id, info, true, false, false, false, false, None),
        Err(err) => fail(session_id, info, err, false, false, false),
    }
}

pub fn scroll(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    after_actuate("scroll", scroll_inner(req), None, None, false)
}

fn scroll_inner(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    let session_id = match session(&req) {
        Ok(id) => id,
        Err(err) => return fail(raw_session(&req), none_target(), err, false, false, false),
    };
    let Some(dy) = req.dy else {
        return fail(
            session_id,
            none_target(),
            HandsError::Input("scroll requires --dy".into()),
            false,
            false,
            false,
        );
    };
    let has_target =
        req.element_id.is_some() || req.grid.is_some() || req.x.is_some() || req.y.is_some();
    let foregrounded;
    let mut info = none_target();
    let mut point = None;
    if let Some(env) = refuse_if_yielded(&session_id, info.clone())? {
        return Ok(env);
    }
    if let Some(env) = refuse_if_blocked(&session_id, info.clone())? {
        return Ok(env);
    }
    if !has_target && let Some(env) = refuse_if_high_il(&session_id, &info, None, None)? {
        return Ok(env);
    }
    if has_target {
        match hover_inner(ActuateRequest {
            session_id: Some(session_id.clone()),
            element_id: req.element_id.clone(),
            grid: req.grid.clone(),
            x: req.x,
            y: req.y,
            ..ActuateRequest::default()
        }) {
            Ok(env) => {
                if !env.ok {
                    return Ok(env);
                }
                foregrounded = env.foregrounded;
                info = env.target;
                point = env.point;
            }
            Err(err) => return fail(session_id, info, err, false, false, false),
        }
    } else {
        let space = match ensure_dpi().and_then(|_| virtual_screen()) {
            Ok(s) => s,
            Err(err) => return fail(session_id, info, err, false, false, false),
        };
        let Some(fg) = foreground::foreground_hwnd() else {
            return fail(
                session_id,
                info,
                HandsError::Target(
                    "scroll has no foreground window; activate a window first".into(),
                ),
                false,
                false,
                false,
            );
        };
        let Some(client) = foreground::client_rect(fg) else {
            return fail(
                session_id,
                info,
                HandsError::Target(
                    "scroll has no foreground client rect; activate a window first".into(),
                ),
                false,
                false,
                false,
            );
        };
        let (cx, cy) = client.center();
        info = resolved_info("pixel", None, cx, cy);
        remember_target(client);
        let mut rng = Rng::from_time();
        foregrounded = foreground::offer(Some(fg), (cx, cy));
        if let Err(err) = input::move_to(space, cx, cy, &mut rng) {
            return fail(session_id, info, err, foregrounded, false, false);
        }
    }
    // Re-probe after the hover dwell and immediately before the wheel; a tab switch during
    // hover must not send the wheel into the foreground tab (B1).
    if let Ok(target) = Target::parse(req.element_id.as_deref(), req.grid.as_deref(), req.x, req.y)
        && let Some(env) = scoped_binding_refusal(&session_id, &info, &target)?
    {
        return Ok(env);
    }
    challenge::note_actuation();
    match input::scroll_wheel(dy, req.dx) {
        Ok(()) => {
            let mut env = base(
                session_id,
                info,
                true,
                false,
                false,
                false,
                foregrounded,
                None,
            )?;
            env.point = point;
            finalize_envelope(env)
        }
        Err(err) => {
            fail_with_point_unverified(session_id, info, err, foregrounded, false, false, point)
        }
    }
}

pub fn wait_settle(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    after_actuate("wait_settle", wait_settle_inner(req), None, None, false)
}

fn wait_settle_inner(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    let session_id = match session(&req) {
        Ok(id) => id,
        Err(err) => return fail(raw_session(&req), none_target(), err, false, false, false),
    };
    let space = match ensure_dpi().and_then(|_| virtual_screen()) {
        Ok(s) => s,
        Err(err) => return fail(session_id, none_target(), err, false, false, false),
    };
    let explicit = match explicit_roi(&req) {
        Ok(v) => v,
        Err(err) => return fail(session_id, none_target(), err, false, false, false),
    };
    let (roi, title_hwnd, origin) = match explicit {
        Some(raw) => (space.clip_rect(raw), None, (raw.x, raw.y)),
        None => {
            let hwnd = foreground::foreground_hwnd();
            let fg = hwnd.and_then(foreground::window_rect);
            match settle::default_wait_roi(space, fg) {
                Ok(r) => (r, hwnd, (r.x, r.y)),
                Err(err) => {
                    return fail(session_id, none_target(), err, false, false, false);
                }
            }
        }
    };
    let info = ActuateTarget {
        kind: "roi".into(),
        id: None,
        x: origin.0,
        y: origin.1,
    };
    let with_roi = |env: ActuateEnvelope| {
        let mut env = env;
        env.roi = Some(roi);
        finalize_envelope(env)
    };
    match settle::wait_settle(space, roi) {
        Ok((settled, _)) => {
            let caption = foreground::title(title_hwnd);
            let settled = if settle::title_blocks_settled(&caption) {
                false
            } else {
                settled
            };
            with_roi(base(
                session_id, info, true, false, false, settled, false, None,
            )?)
        }
        Err(err) => with_roi(fail(session_id, info, err, false, false, false)?),
    }
}

fn explicit_roi(req: &ActuateRequest) -> Result<Option<Rect>, HandsError> {
    match (req.x, req.y, req.w, req.h) {
        (None, None, None, None) => Ok(None),
        (Some(x), Some(y), Some(w), Some(h)) => Ok(Some(Rect { x, y, w, h })),
        _ => Err(HandsError::Settle(
            "wait_settle ROI requires all of --x --y --w --h".into(),
        )),
    }
}

pub const HOVER_DWELL_MS: u64 = 100;
pub const HOVER_DWELL_SLICE_MS: u64 = 10;

pub fn hover_dwell_slice_count() -> u64 {
    HOVER_DWELL_MS.div_ceil(HOVER_DWELL_SLICE_MS)
}

#[cfg(test)]
type HoverDwellHook = fn() -> Result<(), HandsError>;

#[cfg(test)]
thread_local! {
    static HOVER_DWELL_HOOK: std::cell::RefCell<Option<HoverDwellHook>> =
        const { std::cell::RefCell::new(None) };
}

fn hover_dwell() -> Result<(), HandsError> {
    #[cfg(test)]
    {
        if let Some(hook) = HOVER_DWELL_HOOK.with(|c| *c.borrow()) {
            return hook();
        }
    }
    let mut left = HOVER_DWELL_MS;
    while left > 0 {
        let step = left.min(HOVER_DWELL_SLICE_MS);
        std::thread::sleep(std::time::Duration::from_millis(step));
        left -= step;
        lease::poll()?;
    }
    Ok(())
}

/// MCP `stop` — posts a desk-wide request, then freezes this process.
pub fn stop(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    after_actuate("stop", stop_inner(req), None, None, false)
}

fn stop_inner(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    stop_shared(req)
}

/// CLI `stop` without installing hooks — posts the same desk-wide request.
pub fn stop_cli_noop(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    after_actuate("stop", stop_cli_noop_inner(req), None, None, false)
}

fn stop_cli_noop_inner(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    stop_shared(req)
}

fn stop_shared(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    let session_id = match session(&req) {
        Ok(id) => id,
        Err(err) => return fail(raw_session(&req), none_target(), err, false, false, false),
    };
    logs::ensure_installed();
    fence::ensure_installed();
    let write_err = lease::request_stop().err();
    lease::freeze_now_with(lease::FreezeCause::Stop);
    // Do not read is_frozen here — it re-ingests the file we just wrote and fires
    // Pause/Stop listeners a second time. Envelope frozen is the freeze we just performed.
    let frozen = true;
    let (ok, error) = match write_err {
        Some(err) => (false, Some(err.tool_message())),
        None => (true, None),
    };
    base(
        session_id,
        none_target(),
        ok,
        frozen,
        false,
        false,
        false,
        error,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::point::{CssPoint, CssRect};
    use windows::Win32::UI::Input::KeyboardAndMouse::INPUT;

    thread_local! {
        static SEND_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    fn count_identity_sends(_: &[INPUT]) -> Result<(), HandsError> {
        SEND_COUNT.with(|c| c.set(c.get() + 1));
        Ok(())
    }

    fn sent_count() -> usize {
        SEND_COUNT.with(|c| c.get())
    }

    #[test]
    fn oversized_session_id_is_tool_error() {
        let env = ActuateEnvelope {
            session_id: "s".repeat(20_000),
            ok: true,
            frozen: false,
            target: ActuateTarget {
                kind: "none".into(),
                id: None,
                x: 0,
                y: 0,
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
            point: None,
            verified: true,
            assessment: None,
            interactable_checked: None,
            semantic: None,
            identity: None,
            post_dispatch: false,
        };
        let err = finalize_envelope(env).expect_err("must not emit oversize");
        assert!(err.to_string().contains("16384"), "{err}");
    }

    fn sample_target() -> ActuateTarget {
        ActuateTarget {
            kind: "roi".into(),
            id: None,
            x: 10,
            y: 20,
        }
    }

    #[test]
    fn wait_settle_envelope_includes_roi() {
        let env = ActuateEnvelope {
            session_id: "s".into(),
            ok: true,
            frozen: false,
            target: sample_target(),
            retried: false,
            settled: true,
            foregrounded: false,
            error: None,
            fence: None,
            challenge: None,
            roi: Some(Rect {
                x: 10,
                y: 20,
                w: 800,
                h: 600,
            }),
            miss: None,
            navigated: false,
            attempt: None,
            cooldown_ms: None,
            loop_suspected: false,
            guidance: None,
            point: None,
            verified: true,
            assessment: None,
            interactable_checked: None,
            semantic: None,
            identity: None,
            post_dispatch: false,
        };
        let json = serialize_envelope(&env).expect("json");
        assert!(json.contains("\"roi\""), "{json}");
        assert!(json.contains("\"w\":800"), "{json}");
        assert!(json.contains("\"h\":600"), "{json}");
        assert!(json.contains("\"x\":10"), "{json}");
        assert!(json.contains("\"y\":20"), "{json}");
        assert!(!json.contains("\"miss\""), "{json}");
    }

    #[test]
    fn click_envelope_omits_roi() {
        let env = ActuateEnvelope {
            session_id: "s".into(),
            ok: true,
            frozen: false,
            target: ActuateTarget {
                kind: "element".into(),
                id: Some("uia:1".into()),
                x: 40,
                y: 50,
            },
            retried: false,
            settled: true,
            foregrounded: true,
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
            point: None,
            verified: true,
            assessment: None,
            interactable_checked: None,
            semantic: None,
            identity: None,
            post_dispatch: false,
        };
        let json = serialize_envelope(&env).expect("json");
        assert!(!json.contains("\"roi\""), "{json}");
        assert!(!json.contains("\"miss\""), "{json}");
        assert!(!json.contains("\"navigated\""), "{json}");
        assert!(!json.contains("\"verified\""), "{json}");
    }

    #[test]
    fn click_envelope_includes_navigated() {
        let env = ActuateEnvelope {
            session_id: "s".into(),
            ok: true,
            frozen: false,
            target: ActuateTarget {
                kind: "element".into(),
                id: Some("chr:9".into()),
                x: 40,
                y: 50,
            },
            retried: false,
            settled: true,
            foregrounded: true,
            error: None,
            fence: None,
            challenge: None,
            roi: None,
            miss: None,
            navigated: true,
            attempt: None,
            cooldown_ms: None,
            loop_suspected: false,
            guidance: None,
            point: None,
            verified: true,
            assessment: None,
            interactable_checked: None,
            semantic: None,
            identity: None,
            post_dispatch: false,
        };
        let json = serialize_envelope(&env).expect("json");
        assert!(json.contains("\"navigated\":true"), "{json}");
        assert!(!json.contains("\"miss\""), "{json}");
    }

    #[test]
    fn click_envelope_includes_miss_no_change() {
        let env = ActuateEnvelope {
            session_id: "s".into(),
            ok: true,
            frozen: false,
            target: ActuateTarget {
                kind: "element".into(),
                id: Some("uia:1".into()),
                x: 40,
                y: 50,
            },
            retried: true,
            settled: true,
            foregrounded: true,
            error: None,
            fence: None,
            challenge: None,
            roi: None,
            miss: Some("no_change".into()),
            navigated: false,
            attempt: None,
            cooldown_ms: None,
            loop_suspected: false,
            guidance: None,
            point: None,
            verified: true,
            assessment: None,
            interactable_checked: None,
            semantic: None,
            identity: None,
            post_dispatch: false,
        };
        let json = serialize_envelope(&env).expect("json");
        assert!(json.contains("\"miss\":\"no_change\""), "{json}");
    }

    #[test]
    fn hover_envelope_omits_miss() {
        let env = ActuateEnvelope {
            session_id: "s".into(),
            ok: true,
            frozen: false,
            target: ActuateTarget {
                kind: "element".into(),
                id: Some("chr:0".into()),
                x: 12,
                y: 34,
            },
            retried: false,
            settled: false,
            foregrounded: true,
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
            point: None,
            verified: true,
            assessment: None,
            interactable_checked: None,
            semantic: None,
            identity: None,
            post_dispatch: false,
        };
        let json = serialize_envelope(&env).expect("json");
        assert!(!json.contains("\"miss\""), "{json}");
        assert!(!json.contains("\"roi\""), "{json}");
    }

    #[test]
    fn click_miss_table() {
        assert_eq!(click_miss(false, false), None);
        assert_eq!(click_miss(true, false), Some("no_change"));
        assert_eq!(click_miss(false, true), Some("focus_lost"));
        assert_eq!(click_miss(true, true), Some("focus_lost"));
    }

    thread_local! {
        static TITLE_TICK: std::cell::RefCell<u32> = const { std::cell::RefCell::new(0) };
    }

    struct CaptionHooks;

    impl Drop for CaptionHooks {
        fn drop(&mut self) {
            crate::foreground::set_window_title_hook(None);
            crate::foreground::set_chrome_target_hook(None);
            crate::uia::set_page_url_hook(None);
            TITLE_TICK.with(|t| *t.borrow_mut() = 0);
        }
    }

    fn chrome_yes(_: Option<isize>) -> bool {
        true
    }

    fn chrome_no(_: Option<isize>) -> bool {
        false
    }

    fn title_story(_: Option<isize>) -> String {
        "Apple Stock Nears Record High. Why $400 Could Be Next.".into()
    }

    fn title_full_chart(_: Option<isize>) -> String {
        "AAPL - Apple Stock Price".into()
    }

    fn title_star_untitled(_: Option<isize>) -> String {
        "*Untitled".into()
    }

    fn title_just_a_moment(_: Option<isize>) -> String {
        "Just a moment...".into()
    }

    fn title_a_then_story(_: Option<isize>) -> String {
        TITLE_TICK.with(|t| {
            let n = *t.borrow();
            *t.borrow_mut() = n.saturating_add(1);
            if n == 0 {
                "AAPL - Apple Stock Price".into()
            } else {
                "Apple Stock Nears Record High. Why $400 Could Be Next.".into()
            }
        })
    }

    fn predicted_left_clicks(effect: &ClickEffect) -> u32 {
        if should_retry_click(effect) { 2 } else { 1 }
    }

    #[test]
    fn chrome_caption_change_is_navigated_no_retry() {
        let _hooks = CaptionHooks;
        crate::foreground::set_chrome_target_hook(Some(chrome_yes));
        crate::foreground::set_window_title_hook(Some(title_story));
        let effect =
            click_effect_after_settle(None, "AAPL - Apple Stock Price", None, true, false, true);
        assert!(effect.navigated, "{effect:?}");
        assert_eq!(effect.miss, None);
        assert!(!should_retry_click(&effect));
        assert_eq!(predicted_left_clicks(&effect), 1);
    }

    #[test]
    fn same_caption_same_roi_is_no_change_one_retry() {
        let _hooks = CaptionHooks;
        crate::foreground::set_chrome_target_hook(Some(chrome_yes));
        crate::foreground::set_window_title_hook(Some(title_full_chart));
        let effect =
            click_effect_after_settle(None, "AAPL - Apple Stock Price", None, true, false, false);
        assert!(!effect.navigated, "{effect:?}");
        assert_eq!(effect.miss, Some("no_change"));
        assert!(should_retry_click(&effect));
        assert_eq!(predicted_left_clicks(&effect), 2);
    }

    #[test]
    fn focus_lost_same_caption_is_not_navigated() {
        let _hooks = CaptionHooks;
        crate::foreground::set_chrome_target_hook(Some(chrome_yes));
        crate::foreground::set_window_title_hook(Some(title_full_chart));
        let effect =
            click_effect_after_settle(None, "AAPL - Apple Stock Price", None, true, true, false);
        assert!(!effect.navigated, "{effect:?}");
        assert_eq!(effect.miss, Some("focus_lost"));
        assert!(should_retry_click(&effect));
    }

    #[test]
    fn non_chrome_title_change_is_not_navigated() {
        let _hooks = CaptionHooks;
        crate::foreground::set_chrome_target_hook(Some(chrome_no));
        crate::foreground::set_window_title_hook(Some(title_star_untitled));
        let effect = click_effect_after_settle(None, "Untitled", None, true, false, true);
        assert!(!effect.navigated, "{effect:?}");
        assert_eq!(effect.miss, Some("no_change"));
    }

    #[test]
    fn loading_caption_is_navigated_no_retry() {
        let _hooks = CaptionHooks;
        crate::foreground::set_chrome_target_hook(Some(chrome_yes));
        crate::foreground::set_window_title_hook(Some(title_just_a_moment));
        let effect =
            click_effect_after_settle(None, "AAPL - Apple Stock Price", None, true, false, true);
        assert!(effect.navigated, "{effect:?}");
        assert_eq!(effect.miss, None);
        assert!(!should_retry_click(&effect));
        assert_eq!(predicted_left_clicks(&effect), 1);
    }

    #[test]
    fn poll_detects_late_chrome_caption() {
        let _hooks = CaptionHooks;
        crate::foreground::set_chrome_target_hook(Some(chrome_yes));
        crate::foreground::set_window_title_hook(Some(title_a_then_story));
        let effect =
            click_effect_after_settle(None, "AAPL - Apple Stock Price", None, true, false, true);
        assert!(effect.navigated, "{effect:?}");
        assert_eq!(effect.miss, None);
        assert!(!should_retry_click(&effect));
    }

    #[test]
    fn caption_nav_table() {
        assert!(caption_nav(true, "A", "B"));
        assert!(!caption_nav(true, "A", "A"));
        assert!(!caption_nav(true, "", ""));
        assert!(caption_nav(true, "", "B"));
        assert!(!caption_nav(false, "Untitled", "*Untitled"));
        assert!(caption_nav(true, "A", "Just a moment..."));
        assert!(caption_nav(true, "Just a moment...", "Just a moment..."));
        assert!(url_nav(
            Some("http://tauri.localhost/a"),
            Some("http://tauri.localhost/b")
        ));
        assert!(!url_nav(
            Some("http://tauri.localhost/a"),
            Some("http://tauri.localhost/a")
        ));
        assert!(!url_nav(None, Some("http://tauri.localhost/a")));
        assert!(!url_nav(Some("http://tauri.localhost/a"), None));
        assert!(!url_nav(None, None));
    }

    fn url_after_b(_: Option<isize>) -> Option<String> {
        Some("http://tauri.localhost/matters/b".into())
    }

    #[test]
    fn non_chrome_page_url_change_is_navigated_no_retry() {
        let _hooks = CaptionHooks;
        crate::foreground::set_chrome_target_hook(Some(chrome_no));
        crate::foreground::set_window_title_hook(Some(|_| "Dedupe Desk".into()));
        crate::uia::set_page_url_hook(Some(url_after_b));
        let effect = click_effect_after_settle(
            None,
            "Dedupe Desk",
            Some("http://tauri.localhost/matters/a"),
            true,
            false,
            true,
        );
        assert!(effect.navigated, "{effect:?}");
        assert_eq!(effect.miss, None);
        assert!(!should_retry_click(&effect));
        assert_eq!(predicted_left_clicks(&effect), 1);
    }

    #[test]
    fn chrome_ignores_page_url_change() {
        let _hooks = CaptionHooks;
        crate::foreground::set_chrome_target_hook(Some(chrome_yes));
        crate::foreground::set_window_title_hook(Some(title_full_chart));
        crate::uia::set_page_url_hook(Some(url_after_b));
        let effect = click_effect_after_settle(
            None,
            "AAPL - Apple Stock Price",
            Some("http://tauri.localhost/matters/a"),
            true,
            false,
            false,
        );
        assert!(!effect.navigated, "{effect:?}");
        assert_eq!(effect.miss, Some("no_change"));
        assert!(should_retry_click(&effect));
    }

    #[test]
    fn click_caption_latency_p50_within_frame_gap() {
        let _hooks = CaptionHooks;
        crate::foreground::set_chrome_target_hook(Some(chrome_yes));
        crate::foreground::set_window_title_hook(Some(title_full_chart));
        let mut no_nav = Vec::new();
        let mut immediate_nav = Vec::new();
        for _ in 0..10 {
            let t0 = Instant::now();
            let effect = click_effect_after_settle(
                None,
                "AAPL - Apple Stock Price",
                None,
                false,
                false,
                false,
            );
            no_nav.push(t0.elapsed());
            assert!(!effect.navigated, "{effect:?}");
            crate::foreground::set_window_title_hook(Some(title_story));
            let t1 = Instant::now();
            let effect = click_effect_after_settle(
                None,
                "AAPL - Apple Stock Price",
                None,
                false,
                false,
                false,
            );
            immediate_nav.push(t1.elapsed());
            assert!(effect.navigated, "{effect:?}");
            crate::foreground::set_window_title_hook(Some(title_full_chart));
        }
        no_nav.sort();
        immediate_nav.sort();
        let p50 = |xs: &[Duration]| xs[xs.len() / 2];
        let p95 = |xs: &[Duration]| xs[(xs.len() * 95) / 100];
        assert!(
            p50(&no_nav) <= settle::FRAME_GAP,
            "no-nav p50 {:?} exceeds one FRAME_GAP",
            p50(&no_nav)
        );
        assert!(
            p50(&immediate_nav) <= settle::FRAME_GAP,
            "immediate-nav p50 {:?} exceeds one FRAME_GAP",
            p50(&immediate_nav)
        );
        let _ = (p95(&no_nav), p95(&immediate_nav));
    }

    fn pixel_resolved(x: i32, y: i32) -> crate::target::ResolvedTarget {
        crate::target::ResolvedTarget {
            target: Target::Pixel { x, y },
            kind: "pixel",
            id: None,
            x,
            y,
            rect: Rect { x, y, w: 1, h: 1 },
            hwnd: None,
            name: String::new(),
            role: String::new(),
            css_point: None,
            css_rect: None,
            contract: None,
            map: None,
            assessment: None,
            identity: None,
            uia_runtime_id: None,
            uia_expected_fp: None,
        }
    }

    fn hook_client_800x600(_: isize) -> Option<Rect> {
        Some(Rect {
            x: 0,
            y: 0,
            w: 800,
            h: 600,
        })
    }

    #[test]
    fn refuse_if_outside_client_ok_false_does_not_offer() {
        crate::foreground::set_client_rect_hook(Some(hook_client_800x600));
        let _ = crate::foreground::take_offer_calls();
        let resolved = pixel_resolved(10, 1037);
        let info = resolved_info("pixel", None, 10, 1037);
        let env = refuse_if_outside_client("s-0105", &info, &resolved)
            .expect("guard result")
            .expect("named refusal");
        crate::foreground::set_client_rect_hook(None);
        assert!(!env.ok, "{env:?}");
        assert!(
            env.error.as_deref().is_some_and(
                |e| e.contains("outside the client rect") || e.contains("no foreground window")
            ),
            "{:?}",
            env.error
        );
        assert!(
            crate::foreground::take_offer_calls().is_empty(),
            "offer must not run on an out-of-client refuse"
        );
    }

    #[test]
    fn refuse_if_outside_client_allows_inside() {
        crate::foreground::set_client_rect_hook(Some(hook_client_800x600));
        let resolved = pixel_resolved(10, 10);
        let info = resolved_info("pixel", None, 10, 10);
        let out = refuse_if_outside_client("s-0105-in", &info, &resolved).expect("guard result");
        crate::foreground::set_client_rect_hook(None);
        if crate::foreground::foreground_hwnd().is_some() {
            assert!(out.is_none(), "{out:?}");
        }
    }

    #[test]
    fn outside_client_refuse_counts_rejection_not_success() {
        let _g = crate::cooldown::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::cooldown::reset_for_test();
        crate::foreground::set_client_rect_hook(Some(hook_client_800x600));
        let resolved = pixel_resolved(10, 1037);
        let info = resolved_info("pixel", None, 10, 1037);
        let env = refuse_if_outside_client("s-0105-rej", &info, &resolved)
            .expect("guard result")
            .expect("named refusal");
        crate::foreground::set_client_rect_hook(None);
        assert!(!env.ok, "{env:?}");
        let out = after_actuate("click", Ok(env), None, None, false).expect("after");
        assert!(!out.ok, "{out:?}");
        let snap = crate::cooldown::snapshot("s-0105-rej");
        assert_eq!(snap.attempt, 1, "{snap:?}");
        crate::cooldown::reset_for_test();
    }

    #[test]
    fn click_inner_snapshots_after_offer_and_move() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn click_inner").expect("click_inner");
        let rest = &src[start..];
        let end = rest.find("pub fn hover").expect("hover follows");
        let body = &rest[..end];
        let offer = body.find("foreground::offer").expect("offer");
        let move_to = body.find("input::move_to").expect("move_to");
        let capture = body.find("capture_roi").expect("capture_roi");
        let click = body.find("left_click").expect("left_click");
        let refuse = body.find("refuse_if_yielded").expect("refuse");
        let remember = body.find("remember_target").expect("remember");
        assert!(
            capture > offer && capture > move_to && capture < click,
            "first capture_roi must be after offer and move_to and before first left_click:\n{body}"
        );
        let blocked = body.find("refuse_if_blocked").expect("refuse_if_blocked");
        let client = body
            .find("refuse_if_outside_client")
            .expect("refuse_if_outside_client");
        assert!(
            refuse < remember && remember < move_to,
            "click_inner must refuse yield before remember_target before move_to:\n{body}"
        );
        assert!(
            blocked < offer && client < offer && client < remember,
            "click_inner must refuse frozen/cooling and out-of-client before offer/remember:\n{body}"
        );
        assert!(
            body.contains("default_roi"),
            "click must use default_roi:\n{body}"
        );
        assert!(
            !body.contains("default_wait_roi"),
            "click must not use default_wait_roi:\n{body}"
        );
        assert!(
            !body.contains("preprocess::"),
            "click snapshot must not preprocess:\n{body}"
        );
        let title = body.find("foreground::title").expect("title before click");
        let effect = body
            .find("click_effect_after_settle")
            .expect("click_effect_after_settle");
        assert!(
            client < title && title < click && click < effect,
            "title_before must be after the client guard and before first left_click; effect before retry:\n{body}"
        );
        assert!(
            !body.contains("try_snapshot")
                && !body.contains("try_resolve")
                && !body.contains("note_last_url"),
            "click_inner must not snapshot, resolve, or note_last_url:\n{body}"
        );
        assert!(
            body.contains("page_url"),
            "click_inner must probe page_url on non-Chrome:\n{body}"
        );
        let retry = body.find("should_retry_click").expect("should_retry_click");
        let second = body.rfind("left_click").expect("retry left_click");
        assert!(
            effect < retry && retry < second,
            "retry left_click must follow should_retry_click after the first effect:\n{body}"
        );
    }

    #[test]
    fn mcp_click_help_mentions_miss() {
        let mcp = include_str!("mcp.rs");
        let start = mcp
            .find("Bézier-move and left-click")
            .expect("mcp click description");
        let rest = &mcp[start..];
        let end = rest.find("fn click").unwrap_or(rest.len().min(500));
        let click_desc = &rest[..end];
        assert!(
            click_desc.contains("miss") || click_desc.contains("no_change"),
            "mcp click description should mention miss or no_change:\n{click_desc}"
        );
        assert!(
            click_desc.contains("navigated"),
            "mcp click description should mention navigated:\n{click_desc}"
        );
    }

    #[test]
    fn wait_settle_inner_default_does_not_use_last_target_or_cursor() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn wait_settle_inner").expect("wait_settle_inner");
        let rest = &src[start..];
        let end = rest
            .find("\nfn explicit_roi")
            .expect("explicit_roi follows");
        let body = &rest[..end];
        assert!(
            !body.contains("last_target("),
            "default wait_settle must not call last_target:\n{body}"
        );
        assert!(
            !body.contains("default_roi("),
            "default wait_settle must not call settle::default_roi:\n{body}"
        );
        assert!(
            !body.contains("cursor_pos("),
            "default wait_settle must not fall back to cursor:\n{body}"
        );
        assert!(
            body.contains("default_wait_roi"),
            "default wait_settle must use default_wait_roi:\n{body}"
        );
        assert!(
            body.contains("title_blocks_settled"),
            "standalone wait_settle must apply title honesty:\n{body}"
        );
        assert!(
            !body.contains("miss:") && !body.contains("env.miss") && !body.contains("click_miss"),
            "standalone wait_settle must not assign miss:\n{body}"
        );
    }

    #[test]
    fn type_trailing_newline_is_tool_error() {
        let _g = crate::challenge::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::challenge::reset_for_test();
        let env = type_text(ActuateRequest {
            session_id: Some("t-newline".into()),
            text: Some("hello\n".into()),
            ..ActuateRequest::default()
        })
        .unwrap();
        assert!(!env.ok);
        assert!(env.fence.is_none());
        let err = env.error.unwrap_or_default();
        assert!(err.contains("newline"), "{err}");
        assert!(err.contains("key enter"), "{err}");
    }

    #[test]
    fn type_cr_is_tool_error() {
        let _g = crate::challenge::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::challenge::reset_for_test();
        let env = type_text(ActuateRequest {
            session_id: Some("t-cr".into()),
            text: Some("hello\r".into()),
            ..ActuateRequest::default()
        })
        .unwrap();
        assert!(!env.ok);
        assert!(env.error.unwrap_or_default().contains("newline"));
    }

    struct TypeFocusGuard;
    impl Drop for TypeFocusGuard {
        fn drop(&mut self) {
            crate::elevation::set_high_il_hook(None);
            crate::elevation::set_caller_rid_hook(None);
            crate::uia::set_focused_leaf_hook(None);
            crate::foreground::set_foreground_hwnd_hook(None);
            crate::input::set_send_inputs_hook(None);
        }
    }

    fn type_focus_lock() -> (
        TypeFocusGuard,
        std::sync::MutexGuard<'static, ()>,
        std::sync::MutexGuard<'static, ()>,
        std::sync::MutexGuard<'static, ()>,
    ) {
        let lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let elev = crate::elevation::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let challenge = crate::challenge::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        lease::reset_for_test();
        crate::challenge::reset_for_test();
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| false));
        (TypeFocusGuard, lease, elev, challenge)
    }

    fn fg_10() -> Option<isize> {
        Some(0x10)
    }
    fn fg_none() -> Option<isize> {
        None
    }
    fn leaf_err() -> Result<crate::uia::FocusedLeaf, HandsError> {
        Err(HandsError::Uia("GetFocusedElement: not available".into()))
    }
    fn leaf_button_fg() -> Result<crate::uia::FocusedLeaf, HandsError> {
        Ok(crate::uia::FocusedLeaf {
            kind: ControlKind::Button,
            hwnd: Some(0x10),
        })
    }
    fn leaf_edit_bg() -> Result<crate::uia::FocusedLeaf, HandsError> {
        Ok(crate::uia::FocusedLeaf {
            kind: ControlKind::Edit,
            hwnd: Some(0x11),
        })
    }
    fn leaf_edit_fg() -> Result<crate::uia::FocusedLeaf, HandsError> {
        Ok(crate::uia::FocusedLeaf {
            kind: ControlKind::Edit,
            hwnd: Some(0x10),
        })
    }
    fn leaf_combo_fg() -> Result<crate::uia::FocusedLeaf, HandsError> {
        Ok(crate::uia::FocusedLeaf {
            kind: ControlKind::ComboBox,
            hwnd: Some(0x10),
        })
    }
    fn leaf_doc_fg() -> Result<crate::uia::FocusedLeaf, HandsError> {
        Ok(crate::uia::FocusedLeaf {
            kind: ControlKind::Document,
            hwnd: Some(0x10),
        })
    }
    fn leaf_edit_windowless() -> Result<crate::uia::FocusedLeaf, HandsError> {
        Ok(crate::uia::FocusedLeaf {
            kind: ControlKind::Edit,
            hwnd: None,
        })
    }
    fn panic_sends(
        _: &[windows::Win32::UI::Input::KeyboardAndMouse::INPUT],
    ) -> Result<(), HandsError> {
        panic!("SendInput must not run on type focus refuse");
    }

    thread_local! {
        static TYPE_SENDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    fn count_sends(
        _: &[windows::Win32::UI::Input::KeyboardAndMouse::INPUT],
    ) -> Result<(), HandsError> {
        TYPE_SENDS.with(|c| c.set(c.get() + 1));
        Ok(())
    }

    fn type_hi(session: &str) -> ActuateEnvelope {
        type_text(ActuateRequest {
            session_id: Some(session.into()),
            text: Some("hi".into()),
            ..ActuateRequest::default()
        })
        .unwrap()
    }

    #[test]
    fn type_no_focus_refuses_without_send() {
        let _g = type_focus_lock();
        crate::foreground::set_foreground_hwnd_hook(Some(fg_10));
        crate::uia::set_focused_leaf_hook(Some(leaf_err));
        crate::input::set_send_inputs_hook(Some(panic_sends));
        let env = type_hi("s-act-0115-nofocus");
        assert!(!env.ok, "{env:?}");
        assert!(!env.foregrounded);
        let err = env.error.unwrap_or_default();
        assert!(err.contains("no focused edit, combo, or document"), "{err}");
        assert!(err.contains("click an editable field first"), "{err}");
        let snap = crate::cooldown::snapshot("s-act-0115-nofocus");
        assert_eq!(snap.attempt, 1, "{snap:?}");
    }

    #[test]
    fn type_button_focus_refuses_without_send() {
        let _g = type_focus_lock();
        crate::foreground::set_foreground_hwnd_hook(Some(fg_10));
        crate::uia::set_focused_leaf_hook(Some(leaf_button_fg));
        crate::input::set_send_inputs_hook(Some(panic_sends));
        let env = type_hi("s-act-0115-button");
        assert!(!env.ok, "{env:?}");
        let err = env.error.unwrap_or_default();
        assert!(err.contains("not an edit, combo, or document"), "{err}");
    }

    #[test]
    fn type_edit_outside_fg_refuses_without_send() {
        let _g = type_focus_lock();
        crate::foreground::set_foreground_hwnd_hook(Some(fg_10));
        crate::uia::set_focused_leaf_hook(Some(leaf_edit_bg));
        crate::input::set_send_inputs_hook(Some(panic_sends));
        let env = type_hi("s-act-0115-bg");
        assert!(!env.ok, "{env:?}");
        let err = env.error.unwrap_or_default();
        assert!(err.contains("not in the foreground window"), "{err}");
    }

    #[test]
    fn type_windowless_edit_refuses_without_send() {
        let _g = type_focus_lock();
        crate::foreground::set_foreground_hwnd_hook(Some(fg_10));
        crate::uia::set_focused_leaf_hook(Some(leaf_edit_windowless));
        crate::input::set_send_inputs_hook(Some(panic_sends));
        let env = type_hi("s-act-0115-nowin");
        assert!(!env.ok, "{env:?}");
        let err = env.error.unwrap_or_default();
        assert!(err.contains("not in the foreground window"), "{err}");
    }

    #[test]
    fn type_no_foreground_window_refuses_without_send() {
        let _g = type_focus_lock();
        crate::foreground::set_foreground_hwnd_hook(Some(fg_none));
        crate::uia::set_focused_leaf_hook(Some(leaf_edit_fg));
        crate::input::set_send_inputs_hook(Some(panic_sends));
        let env = type_hi("s-act-0115-nofg");
        assert!(!env.ok, "{env:?}");
        let err = env.error.unwrap_or_default();
        assert!(err.contains("not in the foreground window"), "{err}");
    }

    fn type_editable_ok(session: &str, leaf: fn() -> Result<crate::uia::FocusedLeaf, HandsError>) {
        let _g = type_focus_lock();
        crate::foreground::set_foreground_hwnd_hook(Some(fg_10));
        crate::uia::set_focused_leaf_hook(Some(leaf));
        TYPE_SENDS.with(|c| c.set(0));
        crate::input::set_send_inputs_hook(Some(count_sends));
        let env = type_hi(session);
        assert!(env.ok, "{env:?}");
        assert!(env.foregrounded, "{env:?}");
        assert!(TYPE_SENDS.with(|c| c.get()) > 0, "{env:?}");
    }

    #[test]
    fn type_edit_in_fg_delivers_and_foregrounded() {
        type_editable_ok("s-act-0115-edit", leaf_edit_fg);
    }

    #[test]
    fn type_combobox_in_fg_delivers() {
        type_editable_ok("s-act-0115-combo", leaf_combo_fg);
    }

    #[test]
    fn type_document_in_fg_delivers() {
        type_editable_ok("s-act-0115-doc", leaf_doc_fg);
    }

    #[test]
    fn type_text_inner_guards_before_note_and_send() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn type_text_inner").expect("type_text_inner");
        let rest = &src[start..];
        let end = rest.find("pub fn key").expect("key follows");
        let body = &rest[..end];
        assert!(
            !body.contains("uia::focused("),
            "type guard must not call uia::focused():\n{body}"
        );
        assert!(
            body.contains("focused_leaf"),
            "type guard must call focused_leaf:\n{body}"
        );
        let probe = body
            .find("refuse_if_high_il")
            .expect("type_text_inner must consult High-IL probe");
        let guard = body
            .find("refuse_unless_focused_editable")
            .expect("refuse_unless_focused_editable");
        let dpi = body.find("ensure_dpi").expect("ensure_dpi");
        let note = body
            .find("challenge::note_actuation")
            .expect("note_actuation");
        let send = body.find("input::type_text").expect("input::type_text");
        assert!(
            probe < guard && guard < dpi && dpi < note && note < send,
            "High-IL probe before focus guard/dpi/note/send:\n{body}"
        );
        let fence = include_str!("fence.rs");
        assert!(
            fence.contains("uia::focused(") || fence.contains("crate::uia::focused("),
            "fence must still call focused()"
        );
    }

    #[test]
    fn inners_refuse_blocked_before_high_il() {
        let src = include_str!("actuate.rs");
        for (fn_name, follow) in [
            ("fn click_inner", "pub fn hover"),
            ("fn hover_inner", "pub fn type_text"),
            ("fn type_text_inner", "pub fn key"),
            ("fn key_inner", "pub fn scroll"),
            ("fn scroll_inner", "pub fn wait_settle"),
        ] {
            let start = src.find(fn_name).unwrap_or_else(|| panic!("{fn_name}"));
            let rest = &src[start..];
            let end = rest
                .find(follow)
                .unwrap_or_else(|| panic!("{follow} after {fn_name}"));
            let body = &rest[..end];
            let blocked = body
                .find("refuse_if_blocked")
                .unwrap_or_else(|| panic!("{fn_name} refuse_if_blocked"));
            let probe = body
                .find("refuse_if_high_il")
                .unwrap_or_else(|| panic!("{fn_name} refuse_if_high_il"));
            assert!(
                blocked < probe,
                "{fn_name} must refuse_if_blocked before refuse_if_high_il:\n{body}"
            );
        }
        let fail_fn = src.find("fn fail(").expect("fn fail");
        let fail_end = src[fail_fn..]
            .find("\nfn ")
            .map(|i| fail_fn + i)
            .unwrap_or(src.len());
        let fail_src = &src[fail_fn..fail_end];
        assert!(
            fail_src.contains("lease::is_frozen()"),
            "fail() must preserve desk freeze:\n{fail_src}"
        );
    }

    #[test]
    fn click_inner_probes_high_il_before_gate_click() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn click_inner").expect("click_inner");
        let rest = &src[start..];
        let end = rest.find("pub fn hover").expect("hover follows");
        let body = &rest[..end];
        let probe = body.find("refuse_if_high_il").expect("High-IL probe");
        let gate = body.find("gate_click").expect("gate_click");
        assert!(
            probe < gate,
            "High-IL probe must run before UIA hit_test via gate_click:\n{body}"
        );
    }

    struct ElevationGuard;
    impl Drop for ElevationGuard {
        fn drop(&mut self) {
            crate::elevation::set_high_il_hook(None);
            crate::elevation::set_caller_rid_hook(None);
            crate::elevation::set_uiaccess_hook(None);
            crate::input::set_send_inputs_hook(None);
            crate::foreground::set_foreground_hwnd_hook(None);
            crate::foreground::set_client_rect_hook(None);
            crate::uia::set_focused_leaf_hook(None);
            crate::uia::set_hit_test_hook(None);
            crate::capture::set_capture_roi_hook(None);
            HOVER_DWELL_HOOK.with(|c| *c.borrow_mut() = None);
        }
    }

    fn elevation_lock() -> (
        ElevationGuard,
        std::sync::MutexGuard<'static, ()>,
        std::sync::MutexGuard<'static, ()>,
    ) {
        let elev = crate::elevation::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let challenge = crate::challenge::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::challenge::reset_for_test();
        (ElevationGuard, elev, challenge)
    }

    fn key_named(session: &str, name: &str) -> ActuateEnvelope {
        key(ActuateRequest {
            session_id: Some(session.into()),
            name: Some(name.into()),
            ..ActuateRequest::default()
        })
        .unwrap()
    }

    #[test]
    fn type_and_key_high_il_ungranted_refuse_without_send() {
        let _g = elevation_lock();
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| true));
        crate::elevation::set_uiaccess_hook(Some(|| false));
        crate::foreground::set_foreground_hwnd_hook(Some(fg_10));
        crate::input::set_send_inputs_hook(Some(panic_sends));
        let typed = type_hi("s-0119-type-ungranted");
        assert!(!typed.ok, "{typed:?}");
        let err = typed.error.unwrap_or_default();
        assert!(err.contains("UIAccess"), "{err}");
        assert!(
            err.contains(r"hands serve --pipe \\.\pipe\hands-elev"),
            "{err}"
        );
        let keyed = key_named("s-0119-key-ungranted", "tab");
        assert!(!keyed.ok, "{keyed:?}");
        let kerr = keyed.error.unwrap_or_default();
        assert!(kerr.contains("UIAccess"), "{kerr}");
    }

    #[test]
    fn high_il_granted_without_allow_is_elevated_fence() {
        let _g = elevation_lock();
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| true));
        crate::elevation::set_uiaccess_hook(Some(|| true));
        crate::foreground::set_foreground_hwnd_hook(Some(fg_10));
        crate::input::set_send_inputs_hook(Some(panic_sends));
        crate::allows::with_test_env(|| {
            let env = type_hi("s-0119-type-fence");
            assert!(!env.ok, "{env:?}");
            let fence = env.fence.as_ref().expect("Elevated fence");
            assert_eq!(fence.category, "elevated");
            assert_eq!(fence.domain, "desktop");
            assert!(env.error.is_none(), "{env:?}");
        });
    }

    #[test]
    fn high_il_granted_with_allow_types() {
        let _g = elevation_lock();
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| true));
        crate::elevation::set_uiaccess_hook(Some(|| true));
        crate::foreground::set_foreground_hwnd_hook(Some(fg_10));
        crate::uia::set_focused_leaf_hook(Some(leaf_edit_fg));
        TYPE_SENDS.with(|c| c.set(0));
        crate::input::set_send_inputs_hook(Some(count_sends));
        crate::allows::with_test_env(|| {
            crate::allows::grant(
                "s-0119-type-allow",
                "desktop",
                crate::classify::Category::Elevated,
                crate::allows::AllowMode::Session,
            )
            .unwrap();
            let env = type_hi("s-0119-type-allow");
            assert!(env.ok, "{env:?}");
            assert!(TYPE_SENDS.with(|c| c.get()) > 0, "{env:?}");
        });
    }

    #[test]
    fn click_high_il_ungranted_is_named_refuse_not_elementfrompoint() {
        let _g = elevation_lock();
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| true));
        crate::elevation::set_uiaccess_hook(Some(|| false));
        crate::foreground::set_foreground_hwnd_hook(Some(fg_10));
        crate::input::set_send_inputs_hook(Some(panic_sends));
        let env = click(ActuateRequest {
            session_id: Some("s-0119-click-ungranted".into()),
            x: Some(10),
            y: Some(10),
            ..ActuateRequest::default()
        })
        .unwrap();
        assert!(!env.ok, "{env:?}");
        let err = env.error.unwrap_or_default();
        assert!(err.contains("UIAccess"), "{err}");
        assert!(
            !err.contains("ElementFromPoint"),
            "probe must run before UIA hit_test:\n{err}"
        );
    }

    fn yield_machine() {
        let hit = crate::challenge::DetectHit {
            present: true,
            kind: Some(crate::challenge::ChallengeKind::Recaptcha),
            reason: Some("i'm not a robot".into()),
        };
        crate::challenge::apply_observe("s-act", hit.clone());
        crate::challenge::note_actuation();
        crate::challenge::apply_observe("s-act", hit.clone());
        crate::challenge::note_actuation();
        crate::challenge::apply_observe("s-act", hit);
        assert!(crate::challenge::yielded());
    }

    #[test]
    fn yielded_click_refuses_without_sendinput() {
        let _g = crate::challenge::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::challenge::reset_for_test();
        yield_machine();
        let env = refuse_if_yielded(
            "s-act",
            ActuateTarget {
                kind: "pixel".into(),
                id: None,
                x: 1,
                y: 1,
            },
        )
        .unwrap()
        .expect("yield refuse");
        assert!(!env.ok);
        assert_eq!(env.error.as_deref(), Some(YIELD_ERROR));
        assert!(env.challenge.as_ref().is_some_and(|c| c.yielded));
        crate::challenge::reset_for_test();
    }

    fn distinctive_seed_rect() -> Rect {
        Rect {
            x: 12_345,
            y: 54_321,
            w: 77,
            h: 88,
        }
    }

    fn in_space_pixel(space: Space) -> (i32, i32) {
        let x = space.origin_x + space.width / 2;
        let y = space.origin_y + space.height / 2;
        assert!(
            space.contains_point(x, y),
            "pixel ({x},{y}) must be inside virtual screen {space:?}"
        );
        (x, y)
    }

    fn yield_pixel_req(x: i32, y: i32) -> ActuateRequest {
        ActuateRequest {
            session_id: Some("s-act".into()),
            x: Some(x),
            y: Some(y),
            ..ActuateRequest::default()
        }
    }

    #[test]
    fn yielded_hover_does_not_update_last_target() {
        let _g = crate::challenge::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::challenge::reset_for_test();
        with_stop_env(|| {
            let space = ensure_dpi()
                .and_then(|_| virtual_screen())
                .expect("virtual_screen");
            let seed = distinctive_seed_rect();
            remember_target(seed);
            yield_machine();
            let (x, y) = in_space_pixel(space);
            let env = hover(yield_pixel_req(x, y)).expect("hover envelope");
            assert!(!env.ok, "{env:?}");
            assert_eq!(env.error.as_deref(), Some(YIELD_ERROR));
            assert!(env.challenge.as_ref().is_some_and(|c| c.yielded));
            assert_eq!(last_target(), Some(seed));
        });
        crate::challenge::reset_for_test();
    }

    #[test]
    fn yielded_click_does_not_update_last_target() {
        let _g = crate::challenge::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::challenge::reset_for_test();
        with_stop_env(|| {
            let space = ensure_dpi()
                .and_then(|_| virtual_screen())
                .expect("virtual_screen");
            let seed = distinctive_seed_rect();
            remember_target(seed);
            yield_machine();
            let (x, y) = in_space_pixel(space);
            let env = click(yield_pixel_req(x, y)).expect("click envelope");
            assert!(!env.ok, "{env:?}");
            assert_eq!(env.error.as_deref(), Some(YIELD_ERROR));
            assert!(env.challenge.as_ref().is_some_and(|c| c.yielded));
            assert_eq!(last_target(), Some(seed));
        });
        crate::challenge::reset_for_test();
    }

    #[test]
    fn stop_shared_does_not_re_ingest_via_is_frozen() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn stop_shared").expect("stop_shared");
        let rest = &src[start..];
        let end = rest.find("#[cfg(test)]").expect("tests follow stop_shared");
        let body = &rest[..end];
        assert!(
            !body.contains("#[cfg(test)]"),
            "stop_shared slice must not include tests:\n{body}"
        );
        assert!(
            body.contains("let frozen = true"),
            "stop_shared must set frozen true after local freeze:\n{body}"
        );
        assert!(
            !body.contains("let frozen = lease::is_frozen()"),
            "stop_shared must not re-ingest via is_frozen:\n{body}"
        );
    }

    #[test]
    fn stop_shared_notifies_stop_once_with_ingest_on() {
        let _lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _elev = crate::elevation::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        lease::reset_for_test();
        let _hooks = crate::elevation::HookGuard;
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| false));
        with_stop_env(|| {
            with_stop_request_path(|_| {
                lease::reset_for_test();
                logs::reinstall_for_test();
                let _ingest = lease::enable_stop_ingest_for_test();
                fence::reinstall_for_test();
                let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
                let slot = seen.clone();
                lease::subscribe(move |cause| {
                    slot.lock().unwrap_or_else(|e| e.into_inner()).push(cause);
                });
                let desk_before = logs::read_tail("desk", None)
                    .map(|d| d.events.iter().filter(|e| e.kind == "stop").count())
                    .unwrap_or(0);
                let env = stop_cli_noop(ActuateRequest {
                    session_id: Some("s-stop-once".into()),
                    ..ActuateRequest::default()
                })
                .unwrap();
                assert!(env.ok, "{env:?}");
                assert!(env.frozen);
                assert_eq!(
                    *seen.lock().unwrap(),
                    vec![lease::FreezeCause::Stop],
                    "same-process stop must notify FreezeCause::Stop once"
                );
                let session = logs::read_tail("s-stop-once", None).unwrap();
                let stops: Vec<_> = session.events.iter().filter(|e| e.kind == "stop").collect();
                assert_eq!(stops.len(), 1, "{session:?}");
                assert_eq!(stops[0].tool.as_deref(), Some("stop"));
                let desk = logs::read_tail("desk", None).unwrap();
                let desk_stops: Vec<_> = desk.events.iter().filter(|e| e.kind == "stop").collect();
                assert_eq!(
                    desk_stops.len(),
                    desk_before + 1,
                    "this stop must append one desk stop; before={desk_before} desk={desk:?}"
                );
                assert_eq!(desk_stops.last().and_then(|e| e.tool.as_deref()), None);
            });
        });
        crate::elevation::set_high_il_hook(None);
        crate::elevation::set_caller_rid_hook(None);
        lease::reset_for_test();
    }

    #[test]
    fn hover_inner_remembers_after_yield_before_move() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn hover_inner").expect("hover_inner");
        let rest = &src[start..];
        let end = rest.find("pub fn type_text").expect("type_text follows");
        let body = &rest[..end];
        assert!(
            !body.contains("#[cfg(test)]"),
            "hover_inner slice must not include tests:\n{body}"
        );
        let refuse = body.find("refuse_if_yielded").expect("refuse");
        let blocked = body.find("refuse_if_blocked").expect("refuse_if_blocked");
        let remember = body.find("remember_target").expect("remember");
        let move_to = body.find("input::move_to").expect("move_to");
        let offer = body.find("foreground::offer").expect("offer");
        let client = body
            .find("refuse_if_outside_client")
            .expect("refuse_if_outside_client");
        assert!(
            refuse < remember && remember < move_to,
            "hover_inner must refuse yield before remember_target before move_to:\n{body}"
        );
        assert!(
            blocked < offer && client < offer && client < remember,
            "hover_inner must refuse frozen/cooling and out-of-client before offer/remember:\n{body}"
        );
        assert!(
            !body.contains("note_actuation"),
            "hover must not count as an actuation attempt:\n{body}"
        );
    }

    #[test]
    fn scroll_inner_untargeted_moves_to_fg_client() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn scroll_inner").expect("scroll_inner");
        let rest = &src[start..];
        let end = rest
            .find("pub fn wait_settle")
            .expect("wait_settle follows");
        let body = &rest[..end];
        assert!(
            body.contains("client_rect") && body.contains("move_to") && body.contains("center()"),
            "untargeted scroll must move to the FG client centre:\n{body}"
        );
        assert!(
            body.contains("no foreground window") || body.contains("no foreground client"),
            "untargeted scroll must name a missing-window refusal:\n{body}"
        );
        assert!(
            body.contains("hover_inner") && body.contains("req.x") && body.contains("req.y"),
            "targeted scroll must keep --x/--y hover-first:\n{body}"
        );
    }

    #[test]
    fn hover_dwell_is_ten_slices() {
        assert_eq!(HOVER_DWELL_MS, 100);
        assert_eq!(HOVER_DWELL_SLICE_MS, 10);
        assert_eq!(hover_dwell_slice_count(), 10);
    }

    fn with_stop_env<T>(f: impl FnOnce() -> T) -> T {
        crate::allows::with_test_env(|| crate::logs::with_test_env(f))
    }

    fn with_stop_request_path<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        let dir = std::env::temp_dir().join(format!("hands-actuate-stop-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stop-request.json");
        let prev = std::env::var_os("HANDS_STOP_REQUEST_PATH");
        unsafe {
            std::env::set_var("HANDS_STOP_REQUEST_PATH", &path);
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&path)));
        match prev {
            Some(v) => unsafe { std::env::set_var("HANDS_STOP_REQUEST_PATH", v) },
            None => unsafe { std::env::remove_var("HANDS_STOP_REQUEST_PATH") },
        }
        let _ = std::fs::remove_dir_all(&dir);
        match result {
            Ok(v) => v,
            Err(p) => std::panic::resume_unwind(p),
        }
    }

    #[test]
    fn cli_and_mcp_stop_post_file_and_share_success_envelope() {
        let _g = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        lease::reset_for_test();
        with_stop_env(|| {
            with_stop_request_path(|path| {
                let cli = stop_cli_noop(ActuateRequest {
                    session_id: Some("s-stop-cli".into()),
                    ..ActuateRequest::default()
                })
                .unwrap();
                assert!(cli.ok, "{cli:?}");
                assert!(cli.frozen);
                assert!(cli.error.is_none());
                assert!(path.exists(), "CLI stop must write the request file");
                let body = std::fs::read_to_string(path).unwrap();
                assert!(body.contains("hands.stop/v1"), "{body}");
                assert!(!body.contains("no-op"), "{body}");

                lease::reset_for_test();
                let mcp = stop(ActuateRequest {
                    session_id: Some("s-stop-mcp".into()),
                    ..ActuateRequest::default()
                })
                .unwrap();
                assert!(mcp.ok, "{mcp:?}");
                assert!(mcp.frozen);
                assert!(mcp.error.is_none());
            });
        });
        lease::reset_for_test();
    }

    #[test]
    fn stop_write_failure_still_freezes_and_names_path() {
        let _g = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        lease::reset_for_test();
        with_stop_env(|| {
            let dir = std::env::temp_dir()
                .join(format!("hands-actuate-stop-fail-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let blocker = dir.join("blocker");
            std::fs::write(&blocker, b"x").unwrap();
            let bad = blocker.join("stop-request.json");
            let prev = std::env::var_os("HANDS_STOP_REQUEST_PATH");
            unsafe {
                std::env::set_var("HANDS_STOP_REQUEST_PATH", &bad);
            }
            let env = stop_cli_noop(ActuateRequest {
                session_id: Some("s-stop-fail".into()),
                ..ActuateRequest::default()
            })
            .unwrap();
            match prev {
                Some(v) => unsafe { std::env::set_var("HANDS_STOP_REQUEST_PATH", v) },
                None => unsafe { std::env::remove_var("HANDS_STOP_REQUEST_PATH") },
            }
            let _ = std::fs::remove_dir_all(&dir);
            lease::reset_for_test();
            assert!(!env.ok, "{env:?}");
            assert!(env.frozen);
            let err = env.error.unwrap_or_default();
            assert!(
                err.contains(&bad.display().to_string()),
                "error must name the path, got {err}"
            );
        });
        lease::reset_for_test();
    }

    #[test]
    fn stop_sources_drop_noop_wording() {
        let actuate = include_str!("actuate.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production actuate.rs");
        let readme = include_str!("../README.md");
        let agents = include_str!("../AGENTS.md");
        let main = include_str!("main.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production main.rs");
        for blob in [actuate, readme, agents, main] {
            assert!(
                !blob.contains("no-op unless an MCP lease"),
                "leftover no-op-unless string"
            );
            assert!(
                !blob.contains("no-op after the process exits"),
                "leftover no-op-after string"
            );
            assert!(
                !blob.contains("no live MCP lease; Pause/Break still works"),
                "leftover CLI no-op string"
            );
        }
    }

    #[test]
    fn fence_refused_click_leaves_actuation_flag_clear() {
        let _g = crate::challenge::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::challenge::reset_for_test();
        let hit = crate::challenge::DetectHit {
            present: true,
            kind: Some(crate::challenge::ChallengeKind::Recaptcha),
            reason: Some("i'm not a robot".into()),
        };
        crate::challenge::apply_observe("s-act", hit.clone());
        crate::challenge::note_actuation_if_proceeding(true);
        crate::challenge::apply_observe("s-act", hit);
        assert_eq!(crate::challenge::snapshot().attempts, 0);
        assert!(!crate::challenge::snapshot().yielded);
        crate::challenge::reset_for_test();
    }

    fn gated_buy_hit(_: i32, _: i32) -> Result<crate::uia::HitElement, HandsError> {
        Ok(crate::uia::HitElement {
            name: "Buy".into(),
            role: "button".into(),
            hwnd: Some(0x10),
            value: None,
        })
    }

    #[test]
    fn confirm_gated_click_returns_fence_envelope_before_dispatch() {
        let _g = elevation_lock();
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| false));
        crate::foreground::set_foreground_hwnd_hook(Some(fg_10));
        crate::foreground::set_client_rect_hook(Some(hook_client_800x600));
        crate::uia::set_hit_test_hook(Some(gated_buy_hit));
        crate::input::set_send_inputs_hook(Some(panic_sends));
        lease::reset_for_test();
        let _ = crate::foreground::take_offer_calls();
        crate::allows::with_test_env(|| {
            let space = ensure_dpi()
                .and_then(|_| virtual_screen())
                .expect("virtual_screen");
            let (x, y) = in_space_pixel(space);
            let env = click(ActuateRequest {
                session_id: Some("s-0129-confirm".into()),
                x: Some(x),
                y: Some(y),
                ..ActuateRequest::default()
            })
            .expect("click envelope");
            assert!(!env.ok, "{env:?}");
            assert!(!env.settled, "{env:?}");
            assert!(!env.retried, "{env:?}");
            let fence = env.fence.as_ref().expect("confirm fence envelope");
            assert_eq!(fence.category, "money", "{env:?}");
            assert!(env.error.is_none(), "{env:?}");
            assert!(
                env.point.is_none(),
                "confirm envelope must precede dispatch/remap: {env:?}"
            );
            assert!(
                crate::foreground::take_offer_calls().is_empty(),
                "no offer before confirm"
            );
        });
    }

    fn sample_titled(hwnd: isize, pid: u32, title: &str) -> crate::foreground::TitledWindow {
        crate::foreground::TitledWindow {
            hwnd,
            pid,
            title: title.into(),
            class: "Chrome_WidgetWin_1".into(),
            iconic: false,
            zoomed: false,
            rect: Some(Rect {
                x: 10,
                y: 20,
                w: 800,
                h: 600,
            }),
        }
    }

    struct ActivateFixtureGuard;
    impl Drop for ActivateFixtureGuard {
        fn drop(&mut self) {
            crate::elevation::set_high_il_hook(None);
            crate::elevation::set_caller_rid_hook(None);
            crate::elevation::set_uiaccess_hook(None);
            crate::foreground::set_titled_windows_hook(None);
        }
    }

    fn activate_fixture_lock() -> (
        ActivateFixtureGuard,
        std::sync::MutexGuard<'static, ()>,
        std::sync::MutexGuard<'static, ()>,
    ) {
        let lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let elev = crate::elevation::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        lease::reset_for_test();
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| false));
        crate::elevation::set_uiaccess_hook(Some(|| false));
        (ActivateFixtureGuard, lease, elev)
    }

    #[test]
    fn activate_hwnd_offers_and_reports_foregrounded() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static OFFERS: AtomicUsize = AtomicUsize::new(0);
        fn offer_ok(hwnd: Option<isize>, _: (i32, i32)) -> bool {
            OFFERS.fetch_add(1, Ordering::SeqCst);
            hwnd == Some(0x11)
        }
        fn fg_ok() -> Option<isize> {
            Some(0x11)
        }
        let _g = activate_fixture_lock();
        crate::foreground::set_titled_windows_hook(Some(vec![
            sample_titled(0x11, 99, "Chrome A"),
            sample_titled(0x22, 99, "Chrome B"),
        ]));
        OFFERS.store(0, Ordering::SeqCst);
        let env = activate_with(
            Some("s-act-ok".into()),
            "hwnd:11".into(),
            ActivateHooks {
                inventory: crate::foreground::titled_windows,
                offer: offer_ok,
                foreground: fg_ok,
                is_live: |_| true,
            },
        )
        .expect("activate");
        assert!(env.ok, "{env:?}");
        assert!(env.foregrounded);
        assert_eq!(env.reason, None);
        assert_eq!(OFFERS.load(Ordering::SeqCst), 1);
        assert_eq!(env.window.as_ref().map(|w| w.hwnd.as_str()), Some("11"));
        assert_eq!(env.window.as_ref().map(|w| w.pid), Some(99));
    }

    #[test]
    fn activate_ambiguous_pid_and_stale_hwnd_do_not_offer() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static OFFERS: AtomicUsize = AtomicUsize::new(0);
        fn offer_count(_: Option<isize>, _: (i32, i32)) -> bool {
            OFFERS.fetch_add(1, Ordering::SeqCst);
            true
        }
        let _g = activate_fixture_lock();
        crate::foreground::set_titled_windows_hook(Some(vec![
            sample_titled(0x11, 99, "Chrome A"),
            sample_titled(0x22, 99, "Chrome B"),
        ]));
        OFFERS.store(0, Ordering::SeqCst);
        let many = activate_with(
            Some("s-act-many".into()),
            "99".into(),
            ActivateHooks {
                inventory: crate::foreground::titled_windows,
                offer: offer_count,
                foreground: || None,
                is_live: |_| true,
            },
        )
        .expect("ambiguous");
        assert!(!many.ok, "{many:?}");
        assert!(
            many.error
                .as_deref()
                .is_some_and(|e| e.contains("multiple windows")),
            "{many:?}"
        );
        let stale = activate_with(
            Some("s-act-stale".into()),
            "hwnd:dead".into(),
            ActivateHooks {
                inventory: crate::foreground::titled_windows,
                offer: offer_count,
                foreground: || None,
                is_live: |_| true,
            },
        )
        .expect("stale");
        assert!(!stale.ok, "{stale:?}");
        assert!(
            stale
                .error
                .as_deref()
                .is_some_and(|e| e.contains("stale hwnd")),
            "{stale:?}"
        );
        assert_eq!(OFFERS.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn activate_exact_title_open_beats_substring_opencode_tab() {
        // HITL 2026-09-24: activate Open must pick the #32770 titled Open, not a
        // Chrome tab whose title contains "opencode". App copy is comments only.
        use std::sync::atomic::{AtomicUsize, Ordering};
        static OFFERS: AtomicUsize = AtomicUsize::new(0);
        fn offer_ok(hwnd: Option<isize>, _: (i32, i32)) -> bool {
            OFFERS.fetch_add(1, Ordering::SeqCst);
            hwnd == Some(0xc1cca)
        }
        fn fg_ok() -> Option<isize> {
            Some(0xc1cca)
        }
        let _g = activate_fixture_lock();
        let chrome = {
            let mut w = sample_titled(
                0x1429ca,
                5784,
                "does opencode use agents.md … Google Chrome",
            );
            w.class = "Chrome_WidgetWin_1".into();
            w
        };
        let dialog = {
            let mut w = sample_titled(0xc1cca, 30360, "Open");
            w.class = "#32770".into();
            w
        };
        crate::foreground::set_titled_windows_hook(Some(vec![chrome, dialog]));
        OFFERS.store(0, Ordering::SeqCst);
        let env = activate_with(
            Some("s-act-0114-open".into()),
            "Open".into(),
            ActivateHooks {
                inventory: crate::foreground::titled_windows,
                offer: offer_ok,
                foreground: fg_ok,
                is_live: |_| true,
            },
        )
        .expect("activate");
        assert!(env.ok, "{env:?}");
        assert!(env.foregrounded);
        assert_eq!(env.reason, None);
        assert_eq!(OFFERS.load(Ordering::SeqCst), 1);
        assert_eq!(env.window.as_ref().map(|w| w.hwnd.as_str()), Some("c1cca"));
        assert_eq!(env.window.as_ref().map(|w| w.pid), Some(30360));
        assert_eq!(env.window.as_ref().map(|w| w.title.as_str()), Some("Open"));
    }

    #[test]
    fn yielded_activate_does_not_offer() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static OFFERS: AtomicUsize = AtomicUsize::new(0);
        fn offer_count(_: Option<isize>, _: (i32, i32)) -> bool {
            OFFERS.fetch_add(1, Ordering::SeqCst);
            true
        }
        let _fix = activate_fixture_lock();
        let _g = crate::challenge::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::challenge::reset_for_test();
        crate::foreground::set_titled_windows_hook(Some(vec![sample_titled(0x11, 7, "Chrome")]));
        OFFERS.store(0, Ordering::SeqCst);
        yield_machine();
        let env = activate_with(
            Some("s-act".into()),
            "hwnd:11".into(),
            ActivateHooks {
                inventory: crate::foreground::titled_windows,
                offer: offer_count,
                foreground: || Some(0x11),
                is_live: |_| true,
            },
        )
        .expect("yield envelope");
        crate::challenge::reset_for_test();
        assert!(!env.ok, "{env:?}");
        assert_eq!(env.error.as_deref(), Some(YIELD_ERROR));
        assert_eq!(OFFERS.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn activate_with_refuses_frozen_or_cooling_before_offer() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn activate_with").expect("activate_with");
        let rest = &src[start..];
        let end = rest.find("\nfn base(").expect("base follows");
        let body = &rest[..end];
        assert!(
            !body.contains("#[cfg(test)]"),
            "activate_with slice must not include tests:\n{body}"
        );
        let cooling = body.find("cooldown::is_cooling").expect("cooling check");
        let offer = body.find("(hooks.offer)").expect("offer");
        assert!(
            cooling < offer,
            "activate_with must refuse frozen/cooling before offer:\n{body}"
        );
        assert!(
            body.contains("lease::is_frozen"),
            "activate_with must check lease freeze:\n{body}"
        );
    }

    #[test]
    fn frozen_click_third_has_fields_fourth_skips_offer() {
        let _lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cool = crate::cooldown::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _ch = crate::challenge::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _elev = crate::elevation::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _elev_hooks = ElevationGuard;
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| false));
        lease::reset_for_test();
        crate::cooldown::reset_for_test();
        crate::challenge::reset_for_test();
        with_stop_env(|| {
            lease::freeze_now_with(lease::FreezeCause::Physical);
            let space = ensure_dpi()
                .and_then(|_| virtual_screen())
                .expect("virtual_screen");
            let (x, y) = in_space_pixel(space);
            let req = ActuateRequest {
                session_id: Some("s-cool-click".into()),
                x: Some(x),
                y: Some(y),
                ..ActuateRequest::default()
            };
            let first = click(req.clone()).expect("first");
            assert!(!first.ok, "{first:?}");
            assert!(first.frozen);
            assert_eq!(first.attempt, Some(1));
            assert!(first.cooldown_ms.is_none());
            let second = click(req.clone()).expect("second");
            assert_eq!(second.attempt, Some(2));
            let third = click(req.clone()).expect("third");
            assert_eq!(third.attempt, Some(3));
            assert!(third.loop_suspected);
            assert!(third.cooldown_ms.is_some(), "{third:?}");
            assert_eq!(
                third.guidance.as_deref(),
                Some(crate::cooldown::GUIDANCE_FROZEN)
            );
            let fourth = click(req).expect("fourth");
            assert!(!fourth.ok, "{fourth:?}");
            assert!(fourth.frozen);
            assert!(fourth.loop_suspected);
            assert!(fourth.cooldown_ms.is_some());
            assert_eq!(fourth.attempt, Some(4));
        });
        lease::reset_for_test();
        crate::cooldown::reset_for_test();
        crate::challenge::reset_for_test();
    }

    #[test]
    fn frozen_activate_does_not_offer() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static OFFERS: AtomicUsize = AtomicUsize::new(0);
        fn offer_count(_: Option<isize>, _: (i32, i32)) -> bool {
            OFFERS.fetch_add(1, Ordering::SeqCst);
            true
        }
        let _lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cool = crate::cooldown::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _ch = crate::challenge::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        lease::reset_for_test();
        crate::cooldown::reset_for_test();
        crate::challenge::reset_for_test();
        crate::foreground::set_titled_windows_hook(Some(vec![sample_titled(0x11, 7, "Chrome")]));
        OFFERS.store(0, Ordering::SeqCst);
        lease::freeze_now_with(lease::FreezeCause::Physical);
        let env = activate_with(
            Some("s-cool-act".into()),
            "hwnd:11".into(),
            ActivateHooks {
                inventory: crate::foreground::titled_windows,
                offer: offer_count,
                foreground: || Some(0x11),
                is_live: |_| true,
            },
        )
        .expect("frozen envelope");
        crate::foreground::set_titled_windows_hook(None);
        lease::reset_for_test();
        crate::cooldown::reset_for_test();
        crate::challenge::reset_for_test();
        assert!(!env.ok, "{env:?}");
        assert!(env.frozen);
        assert_eq!(env.attempt, Some(1));
        assert_eq!(
            env.guidance.as_deref(),
            Some(crate::cooldown::GUIDANCE_FROZEN)
        );
        assert_eq!(OFFERS.load(Ordering::SeqCst), 0);
    }

    fn live_always(_: isize) -> bool {
        true
    }

    fn live_never(_: isize) -> bool {
        false
    }

    fn offer_false(_: Option<isize>, _: (i32, i32)) -> bool {
        false
    }

    #[test]
    fn classify_activate_reason_table() {
        assert_eq!(
            classify_activate_reason(0x11, None, live_never),
            "stale_hwnd"
        );
        assert_eq!(
            classify_activate_reason(0x11, None, live_always),
            "no_foreground_window"
        );
        assert_eq!(
            classify_activate_reason(0x11, Some(0x22), live_always),
            "os_refused"
        );
    }

    #[test]
    fn activate_already_fg_is_foregrounded_even_when_offer_fails() {
        let _g = activate_fixture_lock();
        crate::foreground::set_titled_windows_hook(Some(vec![sample_titled(0x11, 7, "Chrome")]));
        let env = activate_with(
            Some("s-act-already-fg".into()),
            "hwnd:11".into(),
            ActivateHooks {
                inventory: crate::foreground::titled_windows,
                offer: offer_false,
                foreground: || Some(0x11),
                is_live: live_always,
            },
        )
        .expect("activate");
        assert!(env.ok, "{env:?}");
        assert!(env.foregrounded, "{env:?}");
        assert_eq!(env.reason, None);
        assert_eq!(env.error, None);
    }

    #[test]
    fn activate_offer_fail_no_fg_is_no_foreground_window() {
        let _g = activate_fixture_lock();
        crate::foreground::set_titled_windows_hook(Some(vec![sample_titled(0x11, 7, "Chrome")]));
        let env = activate_with(
            Some("s-act-no-fg".into()),
            "hwnd:11".into(),
            ActivateHooks {
                inventory: crate::foreground::titled_windows,
                offer: offer_false,
                foreground: || None,
                is_live: live_always,
            },
        )
        .expect("activate");
        assert!(env.ok, "{env:?}");
        assert!(!env.foregrounded, "{env:?}");
        assert_eq!(env.reason.as_deref(), Some("no_foreground_window"));
        assert_eq!(env.error, None);
    }

    #[test]
    fn activate_offer_fail_other_fg_is_os_refused() {
        let _g = activate_fixture_lock();
        crate::foreground::set_titled_windows_hook(Some(vec![sample_titled(0x11, 7, "Chrome")]));
        let env = activate_with(
            Some("s-act-os-refused".into()),
            "hwnd:11".into(),
            ActivateHooks {
                inventory: crate::foreground::titled_windows,
                offer: offer_false,
                foreground: || Some(0x22),
                is_live: live_always,
            },
        )
        .expect("activate");
        assert!(env.ok, "{env:?}");
        assert!(!env.foregrounded, "{env:?}");
        assert_eq!(env.reason.as_deref(), Some("os_refused"));
        assert_eq!(env.error, None);
    }

    #[test]
    fn activate_dead_hwnd_after_resolve_is_stale_hwnd() {
        let _g = activate_fixture_lock();
        crate::foreground::set_titled_windows_hook(Some(vec![sample_titled(0x11, 7, "Chrome")]));
        let env = activate_with(
            Some("s-act-dead".into()),
            "hwnd:11".into(),
            ActivateHooks {
                inventory: crate::foreground::titled_windows,
                offer: offer_false,
                foreground: || Some(0x22),
                is_live: live_never,
            },
        )
        .expect("activate");
        assert!(env.ok, "{env:?}");
        assert!(!env.foregrounded, "{env:?}");
        assert_eq!(env.reason.as_deref(), Some("stale_hwnd"));
        assert_eq!(env.error, None);
    }

    #[test]
    fn activate_high_il_ungranted_is_named_refuse() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static OFFERS: AtomicUsize = AtomicUsize::new(0);
        fn offer_count(_: Option<isize>, _: (i32, i32)) -> bool {
            OFFERS.fetch_add(1, Ordering::SeqCst);
            true
        }
        let _lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g = elevation_lock();
        lease::reset_for_test();
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| true));
        crate::elevation::set_uiaccess_hook(Some(|| false));
        crate::foreground::set_titled_windows_hook(Some(vec![sample_titled(0x11, 7, "Elevated")]));
        OFFERS.store(0, Ordering::SeqCst);
        let env = activate_with(
            Some("s-0119-act-ungranted".into()),
            "hwnd:11".into(),
            ActivateHooks {
                inventory: crate::foreground::titled_windows,
                offer: offer_count,
                foreground: || Some(0x11),
                is_live: live_always,
            },
        )
        .expect("activate");
        crate::foreground::set_titled_windows_hook(None);
        assert!(!env.ok, "{env:?}");
        let err = env.error.unwrap_or_default();
        assert!(err.contains("UIAccess"), "{err}");
        assert_eq!(OFFERS.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn activate_high_il_granted_raises() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static OFFERS: AtomicUsize = AtomicUsize::new(0);
        fn offer_ok(hwnd: Option<isize>, _: (i32, i32)) -> bool {
            OFFERS.fetch_add(1, Ordering::SeqCst);
            hwnd == Some(0x11)
        }
        let _lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g = elevation_lock();
        lease::reset_for_test();
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| true));
        crate::elevation::set_uiaccess_hook(Some(|| true));
        crate::foreground::set_titled_windows_hook(Some(vec![sample_titled(0x11, 7, "Elevated")]));
        OFFERS.store(0, Ordering::SeqCst);
        let env = activate_with(
            Some("s-0119-act-granted".into()),
            "hwnd:11".into(),
            ActivateHooks {
                inventory: crate::foreground::titled_windows,
                offer: offer_ok,
                foreground: || Some(0x11),
                is_live: live_always,
            },
        )
        .expect("activate");
        crate::foreground::set_titled_windows_hook(None);
        assert!(env.ok, "{env:?}");
        assert!(env.foregrounded, "{env:?}");
        assert_eq!(OFFERS.load(Ordering::SeqCst), 1);
        assert_eq!(env.error, None);
    }

    #[test]
    fn activate_envelope_omits_reason_when_none() {
        let env = ActivateEnvelope {
            session_id: "s".into(),
            ok: true,
            foregrounded: true,
            window: None,
            frozen: false,
            error: None,
            reason: None,
            challenge: None,
            attempt: None,
            cooldown_ms: None,
            loop_suspected: false,
            guidance: None,
        };
        let json = serialize_activate(&env).expect("json");
        assert!(!json.contains("\"reason\""), "{json}");
        assert!(!json.contains("\"error\""), "{json}");
    }

    #[test]
    fn activate_envelope_includes_reason() {
        let env = ActivateEnvelope {
            session_id: "s".into(),
            ok: true,
            foregrounded: false,
            window: None,
            frozen: false,
            error: None,
            reason: Some("os_refused".into()),
            challenge: None,
            attempt: None,
            cooldown_ms: None,
            loop_suspected: false,
            guidance: None,
        };
        let json = serialize_activate(&env).expect("json");
        assert!(json.contains("\"reason\":\"os_refused\""), "{json}");
        assert!(!json.contains("\"error\""), "{json}");
    }

    fn panic_hit_test(_: i32, _: i32) -> Result<crate::uia::HitElement, HandsError> {
        panic!("hit_test must not run on a blind click");
    }

    fn hook_client_virtual(_: isize) -> Option<Rect> {
        let space = virtual_screen().ok()?;
        Some(Rect {
            x: space.origin_x,
            y: space.origin_y,
            w: space.width,
            h: space.height,
        })
    }

    fn grant_blind(session: &str) {
        crate::allows::grant(
            session,
            "desktop",
            crate::classify::Category::Blind,
            crate::allows::AllowMode::Session,
        )
        .unwrap();
    }

    fn blind_pixel_req(session: &str, x: i32, y: i32) -> ActuateRequest {
        ActuateRequest {
            session_id: Some(session.into()),
            x: Some(x),
            y: Some(y),
            blind: true,
            ..ActuateRequest::default()
        }
    }

    #[test]
    fn blind_click_dispatches_without_hit_test_and_verified_false() {
        let _g = elevation_lock();
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| false));
        crate::foreground::set_foreground_hwnd_hook(Some(fg_10));
        crate::foreground::set_client_rect_hook(Some(hook_client_virtual));
        crate::uia::set_hit_test_hook(Some(panic_hit_test));
        TYPE_SENDS.with(|c| c.set(0));
        crate::input::set_send_inputs_hook(Some(count_sends));
        crate::allows::with_test_env(|| {
            let space = ensure_dpi()
                .and_then(|_| virtual_screen())
                .expect("virtual_screen");
            let (x, y) = in_space_pixel(space);
            grant_blind("s-0121-click");
            let env = click(blind_pixel_req("s-0121-click", x, y)).expect("click");
            assert!(env.ok, "{env:?}");
            assert!(!env.verified, "{env:?}");
            assert!(!env.settled, "{env:?}");
            assert!(!env.retried, "{env:?}");
            assert!(env.miss.is_none(), "{env:?}");
            let json = serialize_envelope(&env).expect("json");
            assert!(json.contains("\"verified\":false"), "{json}");
            assert!(!json.contains("\"miss\""), "{json}");
            assert!(TYPE_SENDS.with(|c| c.get()) > 0, "{env:?}");
        });
    }

    #[test]
    fn flag_off_real_blind_click_has_no_hittest_fields() {
        let _g = elevation_lock();
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| false));
        crate::foreground::set_foreground_hwnd_hook(Some(fg_10));
        crate::foreground::set_client_rect_hook(Some(hook_client_virtual));
        crate::uia::set_hit_test_hook(Some(panic_hit_test));
        crate::input::set_send_inputs_hook(Some(count_sends));
        crate::allows::with_test_env(|| {
            let space = ensure_dpi()
                .and_then(|_| virtual_screen())
                .expect("virtual_screen");
            let (x, y) = in_space_pixel(space);
            grant_blind("s-0128-flagoff");
            let env = click(blind_pixel_req("s-0128-flagoff", x, y)).expect("click");
            assert!(env.ok, "{env:?}");
            assert!(env.assessment.is_none(), "{env:?}");
            assert!(env.interactable_checked.is_none(), "{env:?}");
            let json = serialize_envelope(&env).expect("json");
            assert!(!json.contains("\"assessment\""), "{json}");
            assert!(!json.contains("interactable_checked"), "{json}");
        });
    }

    #[test]
    fn require_interactable_blocked_refuses_runtime_seam_before_offer() {
        let _g = elevation_lock();
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| false));
        crate::foreground::set_foreground_hwnd_hook(Some(fg_10));
        crate::foreground::set_client_rect_hook(Some(hook_client_virtual));
        crate::input::set_send_inputs_hook(Some(panic_sends));
        let _ = crate::foreground::take_offer_calls();
        let map = act_remap_map(100, 80);
        let css_rect = crate::point::CssRect {
            left: 100.0,
            top: 50.0,
            width: 200.0,
            height: 40.0,
        };
        let css = css_rect.center();
        let contract = crate::point::contract_for(&map, css);
        let rect = crate::point::map_rect(&contract, css_rect).expect("rect");
        let mut assessment: crate::hittest::Assessment = serde_json::from_str(include_str!(
            "../tests/fixtures/hittest/replies/sticky-header.json"
        ))
        .expect("assessment");
        assessment.selected_point = css;
        assessment.target_rect_css = css_rect;
        assessment.interactable = Some(false);
        assessment.reason = Some("blocked".into());
        chrome::set_resolve_stub(Some(chrome::ChromeHit {
            id: "chr:0".into(),
            name: "Sign in".into(),
            role: "Button".into(),
            rect,
            css_rect,
            css_point: css,
            contract,
            hwnd: Some(0x10),
            map: Some(map),
            assessment: Some(assessment),
            identity: None,
        }));
        let env = click(ActuateRequest {
            session_id: Some("s-0128-seam".into()),
            element_id: Some("chr:0".into()),
            require_interactable: true,
            ..ActuateRequest::default()
        })
        .expect("click");
        chrome::set_resolve_stub(None);
        assert!(!env.ok, "{env:?}");
        assert_eq!(env.error.as_deref(), Some("interactable_blocked: blocked"));
        assert_eq!(env.interactable_checked, Some(true));
        assert!(
            crate::foreground::take_offer_calls().is_empty(),
            "blocked refusal must not offer/focus"
        );
    }

    #[test]
    fn blind_click_element_id_refuses_before_resolve() {
        let _g = elevation_lock();
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| false));
        crate::input::set_send_inputs_hook(Some(panic_sends));
        crate::uia::set_hit_test_hook(Some(panic_hit_test));
        for id in ["uia:1", "chr:0", "chr:42"] {
            let env = click(ActuateRequest {
                session_id: Some("s-0121-eid".into()),
                element_id: Some(id.into()),
                x: Some(10),
                y: Some(10),
                blind: true,
                ..ActuateRequest::default()
            })
            .expect("envelope");
            assert!(!env.ok, "{id} {env:?}");
            assert!(env.verified, "{id} {env:?}");
            let err = env.error.clone().unwrap_or_default();
            assert!(
                err.contains("blind click does not support --element-id"),
                "{id} {err}"
            );
            let json = serialize_envelope(&env).expect("json");
            assert!(!json.contains("\"verified\""), "{json}");
        }
    }

    #[test]
    fn blind_type_skips_edit_leaf_and_verified_false() {
        let _g = elevation_lock();
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| false));
        crate::foreground::set_foreground_hwnd_hook(Some(fg_10));
        crate::uia::set_focused_leaf_hook(Some(leaf_button_fg));
        TYPE_SENDS.with(|c| c.set(0));
        crate::input::set_send_inputs_hook(Some(count_sends));
        crate::allows::with_test_env(|| {
            grant_blind("s-0121-type");
            let env = type_text(ActuateRequest {
                session_id: Some("s-0121-type".into()),
                text: Some("hi".into()),
                blind: true,
                ..ActuateRequest::default()
            })
            .expect("type");
            assert!(env.ok, "{env:?}");
            assert!(!env.verified, "{env:?}");
            let json = serialize_envelope(&env).expect("json");
            assert!(json.contains("\"verified\":false"), "{json}");
            assert!(TYPE_SENDS.with(|c| c.get()) > 0, "{env:?}");
        });
    }

    #[test]
    fn blind_type_newline_still_refuses() {
        let _g = elevation_lock();
        crate::input::set_send_inputs_hook(Some(panic_sends));
        let env = type_text(ActuateRequest {
            session_id: Some("s-0121-nl".into()),
            text: Some("hi\n".into()),
            blind: true,
            ..ActuateRequest::default()
        })
        .expect("type");
        assert!(!env.ok, "{env:?}");
        assert!(env.verified, "{env:?}");
        let err = env.error.clone().unwrap_or_default();
        assert!(err.contains("newline"), "{err}");
        let json = serialize_envelope(&env).expect("json");
        assert!(!json.contains("\"verified\""), "{json}");
    }

    #[test]
    fn blind_click_and_type_high_il_ungranted_refuse() {
        let _g = elevation_lock();
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| true));
        crate::elevation::set_uiaccess_hook(Some(|| false));
        crate::foreground::set_foreground_hwnd_hook(Some(fg_10));
        crate::input::set_send_inputs_hook(Some(panic_sends));
        crate::uia::set_hit_test_hook(Some(panic_hit_test));
        let clicked = click(ActuateRequest {
            session_id: Some("s-0121-hil-click".into()),
            x: Some(10),
            y: Some(10),
            blind: true,
            ..ActuateRequest::default()
        })
        .expect("click");
        assert!(!clicked.ok, "{clicked:?}");
        assert!(clicked.verified, "{clicked:?}");
        let err = clicked.error.clone().unwrap_or_default();
        assert!(err.contains("UIAccess"), "{err}");
        let typed = type_text(ActuateRequest {
            session_id: Some("s-0121-hil-type".into()),
            text: Some("hi".into()),
            blind: true,
            ..ActuateRequest::default()
        })
        .expect("type");
        assert!(!typed.ok, "{typed:?}");
        assert!(typed.verified, "{typed:?}");
        let json = serialize_envelope(&clicked).expect("json");
        assert!(!json.contains("\"verified\""), "{json}");
    }

    #[test]
    fn hover_blind_refuses() {
        let _g = elevation_lock();
        crate::input::set_send_inputs_hook(Some(panic_sends));
        let env = hover(ActuateRequest {
            session_id: Some("s-0121-hover".into()),
            x: Some(10),
            y: Some(10),
            blind: true,
            ..ActuateRequest::default()
        })
        .expect("hover");
        assert!(!env.ok, "{env:?}");
        let err = env.error.unwrap_or_default();
        assert!(err.contains("click/type only"), "{err}");
    }

    #[test]
    fn blind_click_honors_frozen() {
        let _lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cool = crate::cooldown::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _ch = crate::challenge::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _elev = crate::elevation::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _elev_hooks = ElevationGuard;
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| false));
        crate::input::set_send_inputs_hook(Some(panic_sends));
        crate::uia::set_hit_test_hook(Some(panic_hit_test));
        lease::reset_for_test();
        crate::cooldown::reset_for_test();
        crate::challenge::reset_for_test();
        lease::freeze_now_with(lease::FreezeCause::Physical);
        let env = click(ActuateRequest {
            session_id: Some("s-0121-frozen".into()),
            x: Some(10),
            y: Some(10),
            blind: true,
            ..ActuateRequest::default()
        })
        .expect("click");
        assert!(!env.ok, "{env:?}");
        assert!(env.frozen, "{env:?}");
        lease::reset_for_test();
    }

    fn freeze_on_click_release(
        inputs: &[windows::Win32::UI::Input::KeyboardAndMouse::INPUT],
    ) -> Result<(), HandsError> {
        // Freeze on the button-up so `left_click` still returns Ok and the post-dispatch
        // frozen branch (not the pre-dispatch move/error branch) is exercised.
        if inputs.iter().any(|input| {
            unsafe { input.Anonymous.mi.dwFlags }
                .contains(windows::Win32::UI::Input::KeyboardAndMouse::MOUSEEVENTF_LEFTUP)
        }) {
            lease::freeze_now_with(lease::FreezeCause::Physical);
        }
        Ok(())
    }

    #[test]
    fn blind_click_frozen_after_dispatch_keeps_point() {
        let _lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g = elevation_lock();
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| false));
        crate::foreground::set_foreground_hwnd_hook(Some(fg_10));
        crate::foreground::set_client_rect_hook(Some(hook_client_virtual));
        crate::uia::set_hit_test_hook(Some(panic_hit_test));
        crate::input::set_send_inputs_hook(Some(freeze_on_click_release));
        lease::reset_for_test();
        crate::allows::with_test_env(|| {
            let space = ensure_dpi()
                .and_then(|_| virtual_screen())
                .expect("virtual_screen");
            let (x, y) = in_space_pixel(space);
            grant_blind("s-0121-frozen-point");
            let env = click(blind_pixel_req("s-0121-frozen-point", x, y)).expect("click");
            assert!(!env.ok, "{env:?}");
            assert!(env.frozen, "{env:?}");
            assert!(
                !env.verified,
                "post-dispatch freeze must be unverified: {env:?}"
            );
            let point = env
                .point
                .as_ref()
                .expect("point on frozen dispatch envelope");
            assert_eq!(point.source, crate::point::PointSource::Blind);
            assert_eq!(point.physical, Some(crate::point::PhysicalPoint { x, y }));
            assert_eq!(
                point.transform.method,
                crate::point::TransformMethod::Explicit
            );
            assert!(point.transform.valid);
            let json = serialize_envelope(&env).expect("json");
            assert!(json.contains("\"point\""), "{json}");
            assert!(json.contains("\"source\":\"blind\""), "{json}");
            assert!(json.contains("\"verified\":false"), "{json}");
        });
        lease::reset_for_test();
    }

    fn free_hit(_: i32, _: i32) -> Result<crate::uia::HitElement, HandsError> {
        Ok(crate::uia::HitElement {
            name: String::new(),
            role: "Other".into(),
            hwnd: Some(0x10),
            value: None,
        })
    }

    fn fake_capture(space: Space, rect: Rect) -> Result<crate::capture::RoiFrame, HandsError> {
        let clipped = space.clip_rect(rect);
        let w = clipped.w.max(1);
        let h = clipped.h.max(1);
        Ok(crate::capture::RoiFrame {
            width: w,
            height: h,
            pixels: vec![0; (w * h * 4) as usize],
        })
    }

    #[test]
    fn non_blind_click_frozen_after_dispatch_is_unverified() {
        let _lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g = elevation_lock();
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| false));
        crate::foreground::set_foreground_hwnd_hook(Some(fg_10));
        crate::foreground::set_client_rect_hook(Some(hook_client_virtual));
        crate::uia::set_hit_test_hook(Some(free_hit));
        crate::input::set_send_inputs_hook(Some(freeze_on_click_release));
        crate::capture::set_capture_roi_hook(Some(fake_capture));
        lease::reset_for_test();
        crate::cooldown::reset_for_test();
        crate::allows::with_test_env(|| {
            let space = ensure_dpi()
                .and_then(|_| virtual_screen())
                .expect("virtual_screen");
            let (x, y) = in_space_pixel(space);
            let env = click(ActuateRequest {
                session_id: Some("s-0129-nonblind-frozen".into()),
                x: Some(x),
                y: Some(y),
                ..ActuateRequest::default()
            })
            .expect("click");
            assert!(!env.ok, "{env:?}");
            assert!(env.frozen, "{env:?}");
            assert!(
                !env.verified,
                "post-dispatch freeze must be unverified: {env:?}"
            );
            let point = env.point.as_ref().expect("point contract");
            assert_eq!(point.source, crate::point::PointSource::Pixel);
            assert_eq!(point.physical, Some(crate::point::PhysicalPoint { x, y }));
            let json = serialize_envelope(&env).expect("json");
            assert!(json.contains("\"verified\":false"), "{json}");
            assert!(json.contains("\"point\""), "{json}");
        });
        lease::reset_for_test();
    }

    fn freeze_on_wheel(
        inputs: &[windows::Win32::UI::Input::KeyboardAndMouse::INPUT],
    ) -> Result<(), HandsError> {
        if inputs.iter().any(|input| {
            unsafe { input.Anonymous.mi.dwFlags }
                .contains(windows::Win32::UI::Input::KeyboardAndMouse::MOUSEEVENTF_WHEEL)
        }) {
            lease::freeze_now_with(lease::FreezeCause::Physical);
        }
        Ok(())
    }

    fn noop_sends(
        _: &[windows::Win32::UI::Input::KeyboardAndMouse::INPUT],
    ) -> Result<(), HandsError> {
        Ok(())
    }

    fn dwell_fails() -> Result<(), HandsError> {
        Err(HandsError::Input("hover dwell failed".into()))
    }

    #[test]
    fn non_blind_scroll_frozen_after_dispatch_is_unverified() {
        let _lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g = elevation_lock();
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| false));
        crate::foreground::set_foreground_hwnd_hook(Some(fg_10));
        crate::foreground::set_client_rect_hook(Some(hook_client_virtual));
        crate::input::set_send_inputs_hook(Some(freeze_on_wheel));
        lease::reset_for_test();
        crate::cooldown::reset_for_test();
        let env = scroll(ActuateRequest {
            session_id: Some("s-0129-nonblind-scroll".into()),
            dy: Some(1),
            ..ActuateRequest::default()
        })
        .expect("scroll");
        assert!(!env.ok, "{env:?}");
        assert!(
            !env.verified,
            "post-dispatch scroll must be unverified: {env:?}"
        );
        let json = serialize_envelope(&env).expect("json");
        assert!(json.contains("\"verified\":false"), "{json}");
        lease::reset_for_test();
    }

    #[test]
    fn non_blind_hover_failure_after_dispatch_is_unverified() {
        let _lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g = elevation_lock();
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| false));
        crate::foreground::set_foreground_hwnd_hook(Some(fg_10));
        crate::foreground::set_client_rect_hook(Some(hook_client_virtual));
        crate::input::set_send_inputs_hook(Some(noop_sends));
        HOVER_DWELL_HOOK.with(|c| *c.borrow_mut() = Some(dwell_fails));
        lease::reset_for_test();
        crate::cooldown::reset_for_test();
        crate::allows::with_test_env(|| {
            let space = ensure_dpi()
                .and_then(|_| virtual_screen())
                .expect("virtual_screen");
            let (x, y) = in_space_pixel(space);
            let env = hover(ActuateRequest {
                session_id: Some("s-0129-nonblind-hover".into()),
                x: Some(x),
                y: Some(y),
                ..ActuateRequest::default()
            })
            .expect("hover");
            assert!(!env.ok, "{env:?}");
            assert!(
                !env.verified,
                "post-dispatch hover must be unverified: {env:?}"
            );
            let point = env.point.as_ref().expect("point contract");
            assert_eq!(point.source, crate::point::PointSource::Pixel);
            let json = serialize_envelope(&env).expect("json");
            assert!(json.contains("\"verified\":false"), "{json}");
        });
        lease::reset_for_test();
    }

    #[test]
    fn blind_paths_consult_req_blind_before_guards() {
        let src = include_str!("actuate.rs");
        let click_start = src.find("fn click_inner").expect("click_inner");
        let click_end = click_start + src[click_start..].find("pub fn hover").expect("hover");
        let click_body = &src[click_start..click_end];
        let eid = click_body
            .find("req.blind && req.element_id.is_some()")
            .expect("element_id refuse");
        let resolve = click_body.find("resolve_req").expect("resolve_req");
        let probe = click_body
            .find("refuse_if_high_il")
            .expect("refuse_if_high_il");
        let gate_blind = click_body.find("gate_blind").expect("gate_blind");
        let gate_click = click_body.find("gate_click").expect("gate_click");
        assert!(
            eid < resolve && probe < gate_blind && probe < gate_click,
            "blind element_id before resolve; High-IL before gates:\n{click_body}"
        );
        assert!(
            !click_body[..eid].contains("resolve_req"),
            "resolve_req must not appear before the blind element_id refuse"
        );
        let type_start = src.find("fn type_text_inner").expect("type_text_inner");
        let type_end = type_start + src[type_start..].find("pub fn key").expect("key");
        let type_body = &src[type_start..type_end];
        let blind = type_body.find("!req.blind").expect("req.blind");
        let guard = type_body
            .find("refuse_unless_focused_editable")
            .expect("edit-leaf");
        let hil = type_body.find("refuse_if_high_il").expect("high_il");
        assert!(
            hil < blind && blind < guard,
            "High-IL then req.blind then edit-leaf:\n{type_body}"
        );
    }

    #[test]
    fn mcp_maps_blind_and_docs_name_the_honesty_lock() {
        let mcp = include_str!("mcp.rs");
        assert!(
            mcp.contains("blind: params.blind"),
            "MCP click/type must map params.blind:\n{mcp}"
        );
        let agents = include_str!("../AGENTS.md");
        let readme = include_str!("../README.md");
        for doc in [agents, readme] {
            assert!(doc.contains("verified: false"), "{doc}");
            assert!(doc.contains("does **not** reach elevated windows"), "{doc}");
            assert!(doc.contains("same-IL UIA-opaque"), "{doc}");
        }
        let contract = include_str!("../docs/AGENT-CONTRACT.md");
        assert!(contract.contains("Blind actuation"), "{contract}");
        assert!(contract.contains("lower-assurance"), "{contract}");
    }

    fn invalid_chr_contract(reason: &str) -> crate::point::PointContract {
        crate::point::PointContract {
            source: crate::point::PointSource::Chr,
            css: Some(crate::point::CssPoint::new(312.5, 208.25)),
            physical: None,
            transform: crate::point::TransformReport::invalid(
                reason,
                Some("f3a9c1d2e4b5a607".into()),
            ),
        }
    }

    #[test]
    fn transform_refusal_verified_false() {
        let env = refuse_transform(
            "s-0129".into(),
            none_target(),
            invalid_chr_contract("no_viewport_anchor"),
            None,
        )
        .unwrap();
        assert!(!env.ok);
        assert!(!env.verified);
        let point = env.point.as_ref().expect("point contract");
        assert_eq!(point.source, crate::point::PointSource::Chr);
        assert!(point.physical.is_none());
        let json = serialize_envelope(&env).unwrap();
        assert!(json.contains("\"verified\":false"), "{json}");
        assert!(json.contains("\"physical\":null"), "{json}");
        assert!(json.contains("\"reason\":\"no_viewport_anchor"), "{json}");
    }

    #[test]
    fn unknown_transform_refuses_before_fence_without_offer() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn click_inner").expect("click_inner");
        let end = src[..src.len()]
            .find("fn dispatch_blind_click")
            .expect("blind");
        let body = &src[start..end];
        let transform = body
            .find("refuse_transform(session_id, info, *contract, identity)")
            .expect("transform refusal");
        let fence = body.find("fence::ensure_installed").expect("fence");
        assert!(
            transform < fence,
            "transform refusal must precede fence::ensure_installed:\n{body}"
        );
        let helper_start = src.find("pub(crate) fn refuse_transform").unwrap();
        let helper_end = src[helper_start..]
            .find("fn unresolved_info")
            .map(|i| helper_start + i)
            .expect("unresolved_info");
        let helper = &src[helper_start..helper_end];
        for forbidden in [
            "foreground::offer",
            "input::left_click",
            "input::move_to",
            "ensure_installed",
        ] {
            assert!(
                !helper.contains(forbidden),
                "refuse_transform must not call {forbidden}:\n{helper}"
            );
        }
    }

    #[test]
    fn stale_viewport_refuses_before_outside_client_and_remember() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn click_inner").expect("click_inner");
        let end = src.find("fn dispatch_blind_click").expect("blind");
        let body = &src[start..end];
        let remap = body.find("final_live_remap").expect("final_live_remap");
        let client = body
            .find("refuse_if_outside_client")
            .expect("outside client");
        let remember = body.find("remember_target").expect("remember");
        assert!(
            remap < client && client < remember,
            "click order must be remap -> outside_client -> remember:\n{body}"
        );
        let helper_start = src.find("fn final_live_remap").unwrap();
        let helper_end = src.find("fn point_for_dispatch").unwrap();
        let helper = &src[helper_start..helper_end];
        assert!(!helper.contains("remember_target"));
        assert!(!helper.contains("challenge::note_actuation"));
        assert!(helper.contains("stale_viewport") || helper.contains("chrome::remap_chr"));
    }

    thread_local! {
        static ACT_REMAP_ANCHOR: std::cell::Cell<Rect> =
            const { std::cell::Cell::new(Rect { x: 100, y: 80, w: 1200, h: 900 }) };
    }

    fn act_remap_page() -> crate::point::PageMetrics {
        crate::point::PageMetrics {
            inner_width: Some(1200.0),
            inner_height: Some(900.0),
            client_width: Some(1200.0),
            client_height: Some(900.0),
            dpr: Some(1.0),
            ..crate::point::PageMetrics::default()
        }
    }

    fn act_remap_map(x: i32, y: i32) -> crate::point::ViewportMap {
        crate::point::build_map(crate::point::MapInputs {
            hwnd: Some(0x1234),
            window_rect: Some(Rect {
                x: -10,
                y: -10,
                w: 4000,
                h: 3000,
            }),
            dpi: Some(96),
            virtual_screen: Some(Rect {
                x: -1920,
                y: -200,
                w: 8000,
                h: 6000,
            }),
            page: act_remap_page(),
            render_widgets: vec![Rect {
                x,
                y,
                w: 1200,
                h: 900,
            }],
            monitors: Vec::new(),
            observe_id: Some("f3a9c1d2e4b5a607".into()),
            page_title: Some("Owner Web Access".into()),
            identity: crate::point::WindowIdentity::Confirmed,
            ..crate::point::MapInputs::default()
        })
    }

    fn act_remap_geometry(page: crate::point::PageMetrics, hwnd: isize) -> crate::point::MapInputs {
        crate::point::MapInputs {
            hwnd: Some(hwnd),
            window_rect: Some(Rect {
                x: -10,
                y: -10,
                w: 4000,
                h: 3000,
            }),
            dpi: Some(96),
            virtual_screen: Some(Rect {
                x: -1920,
                y: -200,
                w: 8000,
                h: 6000,
            }),
            page,
            render_widgets: vec![ACT_REMAP_ANCHOR.with(|c| c.get())],
            uia_document: None,
            monitors: Vec::new(),
            observe_id: Some("f3a9c1d2e4b5a607".into()),
            page_title: Some("Owner Web Access".into()),
            fixture: false,
            identity: crate::point::WindowIdentity::Confirmed,
            fixture_origin: None,
        }
    }

    fn act_remap_identity(_: isize) -> (Option<String>, usize, bool) {
        (Some("Owner Web Access - Google Chrome".into()), 1, true)
    }

    fn edge_page() -> crate::point::PageMetrics {
        crate::point::PageMetrics {
            inner_width: Some(100.0),
            inner_height: Some(100.0),
            client_width: Some(100.0),
            client_height: Some(100.0),
            dpr: Some(1.0),
            ..crate::point::PageMetrics::default()
        }
    }

    fn edge_map_inputs(anchor_x: i32) -> crate::point::MapInputs {
        crate::point::MapInputs {
            hwnd: Some(0x1234),
            window_rect: Some(Rect {
                x: -200,
                y: -50,
                w: 600,
                h: 400,
            }),
            dpi: Some(96),
            virtual_screen: Some(Rect {
                x: -1920,
                y: -200,
                w: 8000,
                h: 6000,
            }),
            page: edge_page(),
            render_widgets: vec![Rect {
                x: anchor_x,
                y: 0,
                w: 100,
                h: 100,
            }],
            uia_document: None,
            monitors: Vec::new(),
            observe_id: Some("f3a9c1d2e4b5a607".into()),
            page_title: Some("Edge".into()),
            fixture: false,
            identity: crate::point::WindowIdentity::Confirmed,
            fixture_origin: None,
        }
    }

    fn edge_remap_geometry(page: crate::point::PageMetrics, _: isize) -> crate::point::MapInputs {
        let mut inputs = edge_map_inputs(-99);
        inputs.page = page;
        inputs
    }

    fn edge_remap_identity(_: isize) -> (Option<String>, usize, bool) {
        (Some("Edge - Google Chrome".into()), 1, true)
    }

    struct FinalRemapSeam;

    impl Drop for FinalRemapSeam {
        fn drop(&mut self) {
            chrome::set_remap_geometry_hook(None);
            chrome::set_remap_identity_hook(None);
            chrome::set_resolve_stub(None);
            crate::foreground::set_foreground_hwnd_hook(None);
            crate::foreground::set_chrome_target_hook(None);
            crate::input::set_send_inputs_hook(None);
            HOVER_DWELL_HOOK.with(|c| *c.borrow_mut() = None);
            let _ = crate::foreground::take_offer_calls();
        }
    }

    fn chr_hit(anchor_x: i32, anchor_y: i32) -> (chrome::ChromeHit, crate::point::CssRect) {
        let map = act_remap_map(anchor_x, anchor_y);
        let css = crate::point::CssRect {
            left: 50.0,
            top: 40.0,
            width: 100.0,
            height: 20.0,
        };
        let contract = contract_for(&map, css.center());
        let rect = map_rect(&contract, css).expect("chr rect");
        (
            chrome::ChromeHit {
                id: "chr:5".into(),
                name: "Continue".into(),
                role: "button".into(),
                rect,
                css_rect: css,
                css_point: css.center(),
                contract,
                hwnd: Some(0x1234),
                map: Some(map),
                assessment: None,
                identity: None,
            },
            css,
        )
    }

    fn hover_ok() -> Result<(), HandsError> {
        Ok(())
    }

    fn final_remap_fixture() -> (
        ResolvedTarget,
        ActuateTarget,
        crate::point::CssRect,
        Rect,
        crate::point::PointContract,
    ) {
        let old = act_remap_map(100, 80);
        let css = crate::point::CssRect {
            left: 50.0,
            top: 40.0,
            width: 100.0,
            height: 20.0,
        };
        let contract = contract_for(&old, css.center());
        let old_rect = map_rect(&contract, css).expect("old rect");
        assert_eq!(
            contract.physical,
            Some(crate::point::PhysicalPoint { x: 200, y: 130 })
        );
        assert_eq!(old_rect.center(), (200, 130));
        let resolved = ResolvedTarget {
            target: Target::ElementId("chr:5".into()),
            kind: "element",
            id: Some("chr:5".into()),
            x: 200,
            y: 130,
            rect: old_rect,
            hwnd: Some(0x1234),
            name: String::new(),
            role: String::new(),
            css_point: Some(css.center()),
            css_rect: Some(css),
            contract: Some(contract.clone()),
            map: Some(old),
            assessment: None,
            identity: None,
            uia_runtime_id: None,
            uia_expected_fp: None,
        };
        let info = resolved_info("element", Some("chr:5".into()), 200, 130);
        (resolved, info, css, old_rect, contract)
    }

    #[test]
    fn final_remap_after_move_updates_rect_and_point() {
        // Behavioural: call `final_live_remap` itself. A move-only re-map must update the
        // point, the rect and the envelope target; deleting any of those assignments fails here.
        let _seam = FinalRemapSeam;
        ACT_REMAP_ANCHOR.with(|c| {
            c.set(Rect {
                x: 100,
                y: 80,
                w: 1200,
                h: 900,
            })
        });
        chrome::set_remap_geometry_hook(Some(act_remap_geometry));
        chrome::set_remap_identity_hook(Some(act_remap_identity));
        let (mut resolved, mut info, css, old_rect, old_contract) = final_remap_fixture();
        assert_eq!(old_contract.physical.map(|p| (p.x, p.y)), Some((200, 130)));

        ACT_REMAP_ANCHOR.with(|c| {
            c.set(Rect {
                x: 300,
                y: 200,
                w: 1200,
                h: 900,
            })
        });
        let refusal =
            final_live_remap("s-0129", &mut info, &mut resolved).expect("final remap result");
        assert!(
            refusal.is_none(),
            "move-only remap must proceed: {refusal:?}"
        );

        let contract = resolved.contract.as_ref().expect("updated contract");
        assert_eq!(
            contract.physical,
            Some(crate::point::PhysicalPoint { x: 400, y: 250 }),
            "contract.physical must follow the moved window"
        );
        assert_eq!(
            (resolved.x, resolved.y),
            (400, 250),
            "resolved.x/y must follow the moved window"
        );
        assert_eq!(
            resolved.rect,
            map_rect(contract, css).expect("new rect"),
            "resolved.rect must be re-mapped"
        );
        assert_ne!(
            resolved.rect, old_rect,
            "rect must not stay at the old origin"
        );
        assert_eq!(resolved.rect.center(), (400, 250));
        assert_eq!(
            (info.x, info.y),
            (400, 250),
            "envelope target must follow the moved window"
        );
        assert_eq!(
            resolved
                .map
                .as_ref()
                .and_then(|m| m.anchor)
                .map(|r| (r.x, r.y)),
            Some((300, 200))
        );
        assert!(crate::foreground::take_offer_calls().is_empty());
    }

    #[test]
    fn final_remap_stale_viewport_refuses_without_input_or_offer() {
        let _seam = FinalRemapSeam;
        ACT_REMAP_ANCHOR.with(|c| {
            c.set(Rect {
                x: 100,
                y: 80,
                w: 1200,
                h: 900,
            })
        });
        chrome::set_remap_geometry_hook(Some(act_remap_geometry));
        chrome::set_remap_identity_hook(Some(act_remap_identity));
        crate::input::set_send_inputs_hook(Some(panic_sends));
        let (mut resolved, mut info, _, old_rect, _) = final_remap_fixture();
        let _ = crate::foreground::take_offer_calls();
        let seed = distinctive_seed_rect();
        remember_target(seed);

        // Same origin, changed size: the recorded page metrics no longer match.
        ACT_REMAP_ANCHOR.with(|c| {
            c.set(Rect {
                x: 300,
                y: 200,
                w: 1100,
                h: 900,
            })
        });
        let env = final_live_remap("s-0129", &mut info, &mut resolved)
            .expect("final remap result")
            .expect("stale refusal envelope");
        assert!(!env.ok, "{env:?}");
        assert!(!env.verified, "{env:?}");
        assert!(env.fence.is_none(), "{env:?}");
        assert!(
            env.error
                .as_deref()
                .unwrap_or_default()
                .starts_with("stale_viewport"),
            "{env:?}"
        );
        let point = env.point.as_ref().expect("point contract");
        assert_eq!(point.source, crate::point::PointSource::Chr);
        assert!(point.physical.is_none(), "{point:?}");
        assert!(
            point
                .transform
                .reason
                .as_deref()
                .unwrap_or_default()
                .starts_with("stale_viewport"),
            "{point:?}"
        );
        // A refused re-map must not move the target, the envelope or `LAST_TARGET`.
        assert_eq!((resolved.x, resolved.y), (200, 130));
        assert_eq!(resolved.rect, old_rect);
        assert_eq!(last_target(), Some(seed));
        assert!(crate::foreground::take_offer_calls().is_empty());
    }

    #[test]
    fn final_remap_missing_contract_point_refuses_fail_closed() {
        let _seam = FinalRemapSeam;
        chrome::set_remap_geometry_hook(Some(act_remap_geometry));
        chrome::set_remap_identity_hook(Some(act_remap_identity));
        crate::input::set_send_inputs_hook(Some(panic_sends));

        let assert_refusal = |label: &str, env: Option<ActuateEnvelope>, reason: &str| {
            let env = env.unwrap_or_else(|| panic!("{label}: must refuse, not fail open"));
            assert!(!env.ok, "{label}: {env:?}");
            assert!(!env.verified, "{label}: {env:?}");
            assert!(env.fence.is_none(), "{label}: {env:?}");
            let point = env.point.as_ref().expect("point contract");
            assert!(point.physical.is_none(), "{label}: {point:?}");
            assert!(
                point
                    .transform
                    .reason
                    .as_deref()
                    .unwrap_or_default()
                    .starts_with(reason),
                "{label}: {point:?}"
            );
        };

        // Missing map.
        let (mut resolved, mut info, _, _, _) = final_remap_fixture();
        resolved.map = None;
        let env = final_live_remap("s-0129", &mut info, &mut resolved).expect("remap result");
        assert_refusal("missing map", env, "stale_viewport");

        // Missing CSS point.
        let (mut resolved, mut info, _, _, _) = final_remap_fixture();
        resolved.css_point = None;
        let env = final_live_remap("s-0129", &mut info, &mut resolved).expect("remap result");
        assert_refusal("missing css point", env, "stale_viewport");

        // Missing contract fails closed rather than returning Ok(None).
        let (mut resolved, mut info, _, _, _) = final_remap_fixture();
        resolved.contract = None;
        let env = final_live_remap("s-0129", &mut info, &mut resolved).expect("remap result");
        assert_refusal("missing contract", env, "window_mismatch");

        // Missing HWND still uses the existing named refusal.
        let (mut resolved, mut info, _, _, _) = final_remap_fixture();
        resolved.hwnd = None;
        let env = final_live_remap("s-0129", &mut info, &mut resolved).expect("remap result");
        assert_refusal("missing hwnd", env, "window_mismatch");
        assert!(crate::foreground::take_offer_calls().is_empty());
    }

    fn fg_notepad() -> Option<isize> {
        Some(0x9999)
    }

    fn only_target_chrome(hwnd: Option<isize>) -> bool {
        hwnd == Some(0x1234)
    }

    #[test]
    fn final_remap_non_chrome_foreground_refuses_window_mismatch() {
        let _seam = FinalRemapSeam;
        ACT_REMAP_ANCHOR.with(|c| {
            c.set(Rect {
                x: 300,
                y: 200,
                w: 1200,
                h: 900,
            })
        });
        chrome::set_remap_geometry_hook(Some(act_remap_geometry));
        // No identity hook: exercise the production foreground comparison.
        crate::foreground::set_foreground_hwnd_hook(Some(fg_notepad));
        crate::foreground::set_chrome_target_hook(Some(only_target_chrome));
        crate::input::set_send_inputs_hook(Some(panic_sends));
        let _ = crate::foreground::take_offer_calls();
        let (mut resolved, mut info, _, old_rect, _) = final_remap_fixture();

        let env = final_live_remap("s-0129", &mut info, &mut resolved)
            .expect("remap result")
            .expect("window_mismatch refusal");
        assert!(!env.ok, "{env:?}");
        assert!(!env.verified, "{env:?}");
        assert!(
            env.error
                .as_deref()
                .unwrap_or_default()
                .starts_with("window_mismatch"),
            "{env:?}"
        );
        let point = env.point.as_ref().expect("point contract");
        assert!(point.physical.is_none(), "{point:?}");
        assert!(
            point
                .transform
                .reason
                .as_deref()
                .unwrap_or_default()
                .starts_with("window_mismatch"),
            "{point:?}"
        );
        // A refused re-map must not move the target or dispatch.
        assert_eq!((resolved.x, resolved.y), (200, 130));
        assert_eq!(resolved.rect, old_rect);
        assert!(crate::foreground::take_offer_calls().is_empty());
    }

    #[test]
    fn chr_click_dispatches_the_final_remap_point_and_rect() {
        let _g = elevation_lock();
        let _seam = FinalRemapSeam;
        ACT_REMAP_ANCHOR.with(|c| {
            c.set(Rect {
                x: 300,
                y: 200,
                w: 1200,
                h: 900,
            })
        });
        chrome::set_remap_geometry_hook(Some(act_remap_geometry));
        chrome::set_remap_identity_hook(Some(act_remap_identity));
        let (hit, css) = chr_hit(100, 80);
        chrome::set_resolve_stub(Some(hit));
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| false));
        crate::foreground::set_foreground_hwnd_hook(Some(fg_10));
        crate::foreground::set_client_rect_hook(Some(hook_client_virtual));
        crate::input::set_send_inputs_hook(Some(noop_sends));
        crate::capture::set_capture_roi_hook(Some(fake_capture));
        lease::reset_for_test();
        crate::cooldown::reset_for_test();
        let _ = crate::foreground::take_offer_calls();

        let env = click(ActuateRequest {
            session_id: Some("s-0129-chr-remap".into()),
            element_id: Some("chr:5".into()),
            ..ActuateRequest::default()
        })
        .expect("click");
        assert!(env.ok, "{env:?}");
        let expected = (400, 250);
        assert_eq!((env.target.x, env.target.y), expected, "{env:?}");
        let contract = env.point.as_ref().expect("point contract");
        assert_eq!(
            contract.physical,
            Some(crate::point::PhysicalPoint {
                x: expected.0,
                y: expected.1
            })
        );
        assert_eq!(
            last_target(),
            Some(map_rect(contract, css).expect("remapped rect"))
        );
        let offers = crate::foreground::take_offer_calls();
        assert!(
            offers
                .iter()
                .any(|(hwnd, point)| *hwnd == Some(0x1234) && *point == expected),
            "offer must use the re-mapped point: {offers:?}"
        );
        lease::reset_for_test();
    }

    #[test]
    fn require_interactable_true_dispatches_and_marks() {
        let _g = elevation_lock();
        let _seam = FinalRemapSeam;
        ACT_REMAP_ANCHOR.with(|c| {
            c.set(Rect {
                x: 300,
                y: 200,
                w: 1200,
                h: 900,
            })
        });
        chrome::set_remap_geometry_hook(Some(act_remap_geometry));
        chrome::set_remap_identity_hook(Some(act_remap_identity));
        let (mut hit, css) = chr_hit(100, 80);
        let mut assessment: crate::hittest::Assessment = serde_json::from_str(include_str!(
            "../tests/fixtures/hittest/replies/unobstructed.json"
        ))
        .expect("assessment");
        assessment.selected_point = hit.contract.css.expect("css");
        assessment.target_rect_css = css;
        hit.assessment = Some(assessment);
        chrome::set_resolve_stub(Some(hit));
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| false));
        crate::foreground::set_foreground_hwnd_hook(Some(fg_10));
        crate::foreground::set_client_rect_hook(Some(hook_client_virtual));
        crate::input::set_send_inputs_hook(Some(noop_sends));
        crate::capture::set_capture_roi_hook(Some(fake_capture));
        lease::reset_for_test();
        crate::cooldown::reset_for_test();
        let _ = crate::foreground::take_offer_calls();

        let env = click(ActuateRequest {
            session_id: Some("s-0128-true".into()),
            element_id: Some("chr:5".into()),
            require_interactable: true,
            ..ActuateRequest::default()
        })
        .expect("click");
        chrome::set_resolve_stub(None);
        assert!(env.ok, "{env:?}");
        assert_eq!(env.interactable_checked, Some(true), "{env:?}");
        assert!(env.assessment.is_some(), "{env:?}");
        let offers = crate::foreground::take_offer_calls();
        assert!(
            offers.iter().any(|(hwnd, _)| *hwnd == Some(0x1234)),
            "unobstructed opted-in click must dispatch: {offers:?}"
        );
        lease::reset_for_test();
    }

    #[test]
    fn chr_click_stale_remap_refuses_before_offer_and_input() {
        let _g = elevation_lock();
        let _seam = FinalRemapSeam;
        ACT_REMAP_ANCHOR.with(|c| {
            c.set(Rect {
                x: 300,
                y: 200,
                w: 1100,
                h: 900,
            })
        });
        chrome::set_remap_geometry_hook(Some(act_remap_geometry));
        chrome::set_remap_identity_hook(Some(act_remap_identity));
        let (hit, _) = chr_hit(100, 80);
        chrome::set_resolve_stub(Some(hit));
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| false));
        crate::foreground::set_foreground_hwnd_hook(Some(fg_10));
        crate::foreground::set_client_rect_hook(Some(hook_client_virtual));
        crate::input::set_send_inputs_hook(Some(panic_sends));
        lease::reset_for_test();
        crate::cooldown::reset_for_test();
        let seed = distinctive_seed_rect();
        remember_target(seed);
        let _ = crate::foreground::take_offer_calls();

        let env = click(ActuateRequest {
            session_id: Some("s-0129-chr-stale".into()),
            element_id: Some("chr:5".into()),
            ..ActuateRequest::default()
        })
        .expect("click");
        assert!(!env.ok, "{env:?}");
        assert!(!env.verified, "{env:?}");
        assert!(
            env.error
                .as_deref()
                .unwrap_or_default()
                .starts_with("stale_viewport"),
            "{env:?}"
        );
        let point = env.point.as_ref().expect("point contract");
        assert!(point.physical.is_none(), "{point:?}");
        assert_eq!((env.target.x, env.target.y), (200, 130), "{env:?}");
        assert_eq!(last_target(), Some(seed));
        assert!(crate::foreground::take_offer_calls().is_empty());
        lease::reset_for_test();
    }

    #[test]
    fn chr_hover_dispatches_the_final_remap_point() {
        let _g = elevation_lock();
        let _seam = FinalRemapSeam;
        ACT_REMAP_ANCHOR.with(|c| {
            c.set(Rect {
                x: 300,
                y: 200,
                w: 1200,
                h: 900,
            })
        });
        chrome::set_remap_geometry_hook(Some(act_remap_geometry));
        chrome::set_remap_identity_hook(Some(act_remap_identity));
        let (hit, _) = chr_hit(100, 80);
        chrome::set_resolve_stub(Some(hit));
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| false));
        crate::foreground::set_foreground_hwnd_hook(Some(fg_10));
        crate::foreground::set_client_rect_hook(Some(hook_client_virtual));
        crate::input::set_send_inputs_hook(Some(noop_sends));
        HOVER_DWELL_HOOK.with(|c| *c.borrow_mut() = Some(hover_ok));
        lease::reset_for_test();
        crate::cooldown::reset_for_test();
        let _ = crate::foreground::take_offer_calls();

        let env = hover(ActuateRequest {
            session_id: Some("s-0129-chr-hover".into()),
            element_id: Some("chr:5".into()),
            ..ActuateRequest::default()
        })
        .expect("hover");
        assert!(env.ok, "{env:?}");
        assert_eq!((env.target.x, env.target.y), (400, 250), "{env:?}");
        let offers = crate::foreground::take_offer_calls();
        assert!(
            offers
                .iter()
                .any(|(hwnd, point)| *hwnd == Some(0x1234) && *point == (400, 250)),
            "{offers:?}"
        );
        lease::reset_for_test();
    }

    #[test]
    fn final_remap_point_left_viewport_serializes_invalid_contract() {
        // A 1 px move (-100 -> -99) across the half-open physical viewport edge: the CSS point
        // 99.5 rounds to -1 inside the old viewport but +1 outside the new one. The refusal must
        // clear `valid`, not emit `valid:true` with `physical:null`.
        let _seam = FinalRemapSeam;
        chrome::set_remap_geometry_hook(Some(edge_remap_geometry));
        chrome::set_remap_identity_hook(Some(edge_remap_identity));
        crate::input::set_send_inputs_hook(Some(panic_sends));
        let old_map = crate::point::build_map(edge_map_inputs(-100));
        assert!(old_map.report.valid, "{:?}", old_map.report.reason);
        let css = crate::point::CssPoint::new(99.5, 50.0);
        let old_contract = contract_for(&old_map, css);
        let old_point = old_contract.physical.expect("old point inside edge");
        let css_rect = crate::point::CssRect {
            left: 99.0,
            top: 49.0,
            width: 1.0,
            height: 2.0,
        };
        let old_rect = map_rect(&old_contract, css_rect).expect("old rect");
        let mut resolved = ResolvedTarget {
            target: Target::ElementId("chr:5".into()),
            kind: "element",
            id: Some("chr:5".into()),
            x: old_point.x,
            y: old_point.y,
            rect: old_rect,
            hwnd: Some(0x1234),
            name: String::new(),
            role: String::new(),
            css_point: Some(css),
            css_rect: Some(css_rect),
            contract: Some(old_contract),
            map: Some(old_map),
            assessment: None,
            identity: None,
            uia_runtime_id: None,
            uia_expected_fp: None,
        };
        let mut info = resolved_info("element", Some("chr:5".into()), old_point.x, old_point.y);
        let _ = crate::foreground::take_offer_calls();

        let env = final_live_remap("s-0129", &mut info, &mut resolved)
            .expect("remap result")
            .expect("edge refusal");
        assert!(!env.ok, "{env:?}");
        assert!(!env.verified, "{env:?}");
        assert!(
            env.error
                .as_deref()
                .unwrap_or_default()
                .starts_with("stale_viewport"),
            "{env:?}"
        );
        let point = env.point.as_ref().expect("point contract");
        assert!(!point.transform.valid, "{point:?}");
        assert!(point.physical.is_none(), "{point:?}");
        assert!(
            point
                .transform
                .reason
                .as_deref()
                .unwrap_or_default()
                .starts_with("stale_viewport"),
            "{point:?}"
        );
        assert_eq!((resolved.x, resolved.y), (old_point.x, old_point.y));
        assert_eq!(resolved.rect, old_rect);
        let json = serialize_envelope(&env).expect("json");
        assert!(json.contains("\"valid\":false"), "{json}");
        assert!(json.contains("\"physical\":null"), "{json}");
        assert!(json.contains("\"verified\":false"), "{json}");
        assert!(crate::foreground::take_offer_calls().is_empty());
    }

    #[test]
    fn final_remap_updates_envelope_target() {
        let src = include_str!("actuate.rs");
        let helper = &src
            [src.find("fn final_live_remap").unwrap()..src.find("fn point_for_dispatch").unwrap()];
        assert!(helper.contains("info.x = p.x"));
        assert!(helper.contains("info.y = p.y"));
        let click =
            &src[src.find("fn click_inner").unwrap()..src.find("fn dispatch_blind_click").unwrap()];
        assert!(click.contains("final_live_remap(&session_id, &mut info, resolved_ref)"));
        let hover =
            &src[src.find("fn hover_inner").unwrap()..src.find("pub fn type_text").unwrap()];
        assert!(hover.contains("final_live_remap(&session_id, &mut info, resolved_ref)"));
    }

    #[test]
    fn hover_transform_guard_before_remember_and_offer() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn hover_inner").expect("hover_inner");
        let end = src.find("pub fn type_text").expect("type_text");
        let body = &src[start..end];
        let transform = body.find("refuse_transform").expect("refuse_transform");
        let remap = body.find("final_live_remap").expect("final_live_remap");
        let client = body
            .find("refuse_if_outside_client")
            .expect("outside_client");
        let remember = body.find("remember_target").expect("remember");
        let offer = body.find("foreground::offer").expect("offer");
        assert!(
            transform < remap && remap < client && client < remember && remember < offer,
            "hover order: transform -> remap -> outside_client -> remember -> offer:\n{body}"
        );
        assert!(!body.contains("note_actuation"));
    }

    #[test]
    fn scroll_target_refuses_invalid_transform_without_move() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn scroll_inner").expect("scroll_inner");
        let end = src.find("pub fn wait_settle").expect("wait_settle");
        let body = &src[start..end];
        assert!(body.contains("hover_inner"));
        let hover =
            &src[src.find("fn hover_inner").unwrap()..src.find("pub fn type_text").unwrap()];
        let transform = hover.find("refuse_transform").unwrap();
        let move_to = hover.find("input::move_to").unwrap();
        assert!(transform < move_to);
        assert!(body.contains("point = env.point"));
        assert!(body.contains("env.point = point"));
    }

    #[test]
    fn scroll_dispatch_failure_retains_point() {
        // F2: a targeted scroll's scroll_wheel failure must keep the same point contract.
        let contract = crate::point::PointContract {
            source: PointSource::Chr,
            css: Some(crate::point::CssPoint::new(10.0, 20.0)),
            physical: Some(crate::point::PhysicalPoint { x: 110, y: 120 }),
            transform: crate::point::TransformReport {
                valid: true,
                method: TransformMethod::RenderWidgetHost,
                confidence: TransformConfidence::High,
                scale: Some(1.0),
                dpr: Some(1.0),
                dpi: Some(96),
                page_zoom: Some(1.0),
                page_viewport: Some(Rect {
                    x: 100,
                    y: 100,
                    w: 500,
                    h: 400,
                }),
                anchor_basis: Some(crate::point::CssBasis::Inner),
                layout_basis: Some(crate::point::CssBasis::Inner),
                reason: None,
                observe_id: Some("f3a9c1d2e4b5a607".into()),
            },
        };
        let target = none_target();
        let mut success = base(
            "s-scroll".into(),
            target.clone(),
            true,
            false,
            false,
            false,
            false,
            None,
        )
        .unwrap();
        success.point = Some(contract.clone());
        let failure = fail_with_point_unverified(
            "s-scroll".into(),
            target,
            HandsError::Input("scroll blocked".into()),
            false,
            false,
            false,
            Some(contract),
        )
        .unwrap();
        assert!(!failure.ok);
        assert!(!failure.verified, "post-dispatch failure is unverified");
        assert_eq!(failure.point, success.point);
        let success_point = success.point.expect("success point");
        let failure_point = failure.point.as_ref().expect("failure point");
        assert_eq!(failure_point.source, success_point.source);
        assert_eq!(failure_point.css, success_point.css);
        assert_eq!(failure_point.physical, success_point.physical);
        assert_eq!(failure_point.transform, success_point.transform);
        let json = serialize_envelope(&failure).unwrap();
        assert!(json.contains("\"point\""), "{json}");
        assert!(
            json.contains("\"physical\":{\"x\":110,\"y\":120}"),
            "{json}"
        );
        assert!(
            json.contains("\"observe_id\":\"f3a9c1d2e4b5a607\""),
            "{json}"
        );

        // The scroll error branch uses the point-retaining failure helper.
        let src = include_str!("actuate.rs");
        let start = src.find("fn scroll_inner").expect("scroll_inner");
        let end = src.find("pub fn wait_settle").expect("wait_settle");
        let body = &src[start..end];
        assert!(
            body.contains(
                "fail_with_point_unverified(session_id, info, err, foregrounded, false, false, point)"
            ),
            "scroll_wheel failure must retain the point and be unverified:\n{body}"
        );
    }

    #[test]
    fn post_dispatch_error_sites_use_unverified_helper() {
        let src = include_str!("actuate.rs");
        let click_start = src.find("fn click_inner").expect("click_inner");
        let click_end = src.find("fn dispatch_blind_click").expect("blind");
        let click = &src[click_start..click_end];
        let first_click = click.find("input::left_click").expect("click left_click");
        let after_click = &click[first_click..];
        assert!(
            after_click.contains("fail_with_point_unverified("),
            "post-dispatch click failures must use the unverified helper:\n{after_click}"
        );
        assert!(
            !after_click.contains("fail_with_point("),
            "post-dispatch click failure still uses the verified helper:\n{after_click}"
        );
        assert!(
            click.contains("env.verified = false;"),
            "click freeze branch must clear verified"
        );

        let blind_start = src.find("fn dispatch_blind_click").expect("blind");
        let blind_end = src.find("pub fn hover(").expect("hover");
        let blind = &src[blind_start..blind_end];
        let blind_click = blind.find("input::left_click").expect("blind left_click");
        assert!(
            blind[blind_click..].contains("fail_with_point_unverified("),
            "post-dispatch blind click failure must be unverified"
        );
        assert!(
            !blind[blind_click..].contains("fail_with_point("),
            "post-dispatch blind click failure still uses the verified helper"
        );
        assert!(blind.contains("env.verified = false;"));

        let hover_start = src.find("fn hover_inner").expect("hover_inner");
        let hover_end = src.find("pub fn type_text").expect("type_text");
        let hover = &src[hover_start..hover_end];
        let hover_dwell = hover.find("hover_dwell()").expect("hover_dwell");
        assert!(
            hover[hover_dwell..].contains("fail_with_point_unverified("),
            "post-dispatch hover failure must be unverified"
        );
        assert!(
            !hover[hover_dwell..].contains("fail_with_point("),
            "post-dispatch hover failure still uses the verified helper"
        );

        let scroll_start = src.find("fn scroll_inner").expect("scroll_inner");
        let scroll_end = src.find("pub fn wait_settle").expect("wait_settle");
        let scroll = &src[scroll_start..scroll_end];
        let wheel = scroll.find("input::scroll_wheel").expect("scroll_wheel");
        assert!(
            scroll[wheel..].contains("fail_with_point_unverified("),
            "scroll dispatch failure must be unverified"
        );
        assert!(
            !scroll[wheel..].contains("fail_with_point("),
            "scroll dispatch failure still uses the verified helper"
        );
    }

    #[test]
    fn no_send_input_on_transform_unknown_source_lock() {
        let src = include_str!("actuate.rs");
        let helper = &src[src.find("pub(crate) fn refuse_transform").unwrap()
            ..src.find("fn unresolved_info").unwrap()];
        assert!(!helper.contains("SendInput"));
        assert!(!helper.contains("left_click"));
        assert!(!helper.contains("move_to"));
        assert!(!helper.contains("offer"));
    }

    #[test]
    fn chr_high_il_probe_uses_resolved_hwnd() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn click_inner").expect("click_inner");
        let end = src.find("fn dispatch_blind_click").expect("blind");
        let body = &src[start..end];
        let high = body.find("refuse_if_high_il").expect("high_il");
        let resolve = body.find("resolve_req").expect("resolve");
        assert!(resolve < high);
        assert!(body.contains("guard_hwnd"));
        assert!(body.contains("resolved.as_ref().and_then(|r| r.hwnd)"));
    }

    fn production(src: &str) -> &str {
        src.split_once("mod tests {")
            .map(|(prod, _)| prod)
            .unwrap_or(src)
    }

    fn compact(src: &str) -> String {
        src.chars().filter(|c| !c.is_whitespace()).collect()
    }

    /// Whitespace-normalized legacy subtraction detector: does any `outer_*` token have a
    /// `-` and its `inner_*` counterpart within a short operand window?
    fn legacy_subtraction(src: &str) -> bool {
        let text = compact(src);
        for (a, b) in [
            ("outer_height", "inner_height"),
            ("outer_width", "inner_width"),
        ] {
            for (pos, _) in text.match_indices(a) {
                let rest = &text[pos + a.len()..];
                if let Some(rel) = rest.find(b)
                    && rel <= 64
                    && rest[..rel].contains('-')
                {
                    return true;
                }
            }
        }
        false
    }

    fn function_spans(src: &str) -> Vec<(&str, &str)> {
        let positions: Vec<usize> = src.match_indices("fn ").map(|(i, _)| i).collect();
        let mut out = Vec::new();
        for (n, &start) in positions.iter().enumerate() {
            let name_start = start + 3;
            let name_end = src[name_start..]
                .find('(')
                .map(|i| name_start + i)
                .unwrap_or(name_start);
            let name = src[name_start..name_end].trim();
            let end = positions.get(n + 1).copied().unwrap_or(src.len());
            out.push((name, &src[start..end]));
        }
        out
    }

    fn span_of<'a>(spans: &'a [(&str, &str)], name: &str) -> Option<&'a str> {
        spans
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, body)| *body)
    }

    fn inject_after_open_brace(src: &str, fname: &str, code: &str) -> String {
        let needle = format!("fn {fname}(");
        let start = src.find(&needle).expect("function to mutate");
        let brace = src[start..]
            .find('{')
            .map(|i| start + i)
            .expect("function body");
        let mut out = String::with_capacity(src.len() + code.len());
        out.push_str(&src[..=brace]);
        out.push_str(code);
        out.push_str(&src[brace + 1..]);
        out
    }

    /// Pure source-lock predicate. Returns human-readable violations (empty = clean).
    fn transform_lock_violations(chrome: &str, target: &str) -> Vec<String> {
        let mut out = Vec::new();
        let prod = production(chrome);
        let norm = prod.split_whitespace().collect::<Vec<_>>().join(" ");
        let spans = function_spans(&norm);

        let legacy_fns: Vec<&str> = spans
            .iter()
            .filter(|(_, body)| legacy_subtraction(body))
            .map(|(name, _)| *name)
            .collect();
        if legacy_fns != vec!["fixture_listing_origin"] {
            out.push(format!(
                "only fixture_listing_origin may contain outer-inner subtraction: {legacy_fns:?}"
            ));
        }

        let calls: Vec<usize> = norm
            .match_indices("fixture_listing_origin(")
            .map(|(i, _)| i)
            .collect();
        if calls.len() != 2 {
            out.push(format!(
                "fixture_listing_origin must have exactly one call site: {calls:?}"
            ));
        } else if let (Some(def_start), Some(def_end), Some(fixture), Some(fixture_start)) = (
            norm.find("fn fixture_listing_origin("),
            norm.find("fn fixture_listing_origin(")
                .and_then(|s| norm[s..].find("fn make_observe_id(").map(|i| s + i)),
            span_of(&spans, "fixture_map"),
            norm.find("fn fixture_map("),
        ) {
            let call = *calls.iter().find(|i| **i > def_end).unwrap_or(&def_start);
            if !(call > fixture_start && call < fixture_start + fixture.len()) {
                out.push("fixture_listing_origin must be called only from fixture_map".into());
            }
            if !fixture.contains("fixture: true") {
                out.push("fixture_map must set fixture: true".into());
            }
            if !fixture.contains("fixture_origin: fixture_listing_origin(") {
                out.push(
                    "fixture_map must use fixture_listing_origin for its listing origin".into(),
                );
            }
        } else {
            out.push("fixture_listing_origin / fixture_map structure is missing".into());
        }

        // Global rule: `outer_width` / `outer_height` may appear only in the ChromeMetrics
        // definition/serde or inside the single fixture_listing_origin function span. This is
        // stronger than a proximity matcher: aliasing a local and subtracting it later, reversed
        // subtraction, or an unnamed helper all fail.
        let cm_start = chrome.find("pub struct ChromeMetrics {");
        let cm_end = cm_start.and_then(|s| chrome[s..].find("\n}").map(|i| s + i + 2));
        let fixture_start = chrome.find("fn fixture_listing_origin(");
        let fixture_end = fixture_start.and_then(|s| chrome[s..].find("\n}").map(|i| s + i + 2));
        let (Some(cm_start), Some(cm_end), Some(fixture_start), Some(fixture_end)) =
            (cm_start, cm_end, fixture_start, fixture_end)
        else {
            out.push("ChromeMetrics/fixture_listing_origin allowed spans are missing".into());
            return out;
        };
        for ident in ["outer_width", "outer_height"] {
            for (pos, _) in chrome.match_indices(ident) {
                let in_metrics = pos >= cm_start && pos < cm_end;
                let in_fixture = pos >= fixture_start && pos < fixture_end;
                if !in_metrics && !in_fixture {
                    out.push(format!(
                        "{ident} appears outside ChromeMetrics/fixture_listing_origin at byte {pos}"
                    ));
                }
            }
        }

        // Keep explicit proof that the single fixture helper still owns the legacy subtraction.
        if !legacy_subtraction(&chrome[fixture_start..fixture_end]) {
            out.push("fixture_listing_origin no longer contains the legacy subtraction".into());
        }

        match span_of(&spans, "hit_from_raw") {
            Some(hit) if hit.contains("contract_for(") => {}
            Some(_) => {
                out.push("hit_from_raw must build the live contract via contract_for".into())
            }
            None => {}
        }

        let target_norm = production(target)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        match span_of(&function_spans(&target_norm), "finish_chr") {
            Some(finish) => {
                let finish = compact(finish);
                if !finish.contains("contract_for(") {
                    out.push("finish_chr must call contract_for".into());
                }
                if !finish.contains("map_point(&contract,css_rect.center())") {
                    out.push("finish_chr must map the contract point".into());
                }
                if !finish.contains("map_rect(&contract,css_rect)") {
                    out.push("finish_chr must map the contract rect".into());
                }
                if finish.contains("found.rect.center()") {
                    out.push("finish_chr must not use a raw rect centre".into());
                }
            }
            None => out.push("missing target::finish_chr".into()),
        }
        out
    }

    #[test]
    fn click_hover_scroll_paths_have_no_outer_inner_transform() {
        // Production code outside the test module must have no legacy subtraction at all.
        for src in [
            production(include_str!("actuate.rs")),
            production(include_str!("target.rs")),
            production(include_str!("point.rs")),
        ] {
            assert!(
                !legacy_subtraction(src),
                "legacy outer-inner arithmetic leaked outside chrome's fixture helper"
            );
        }

        // Positive control: the current tree must satisfy every structural lock.
        let violations =
            transform_lock_violations(include_str!("chrome.rs"), include_str!("target.rs"));
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn transform_lock_rejects_alias_and_whitespace_mutations() {
        let chrome = include_str!("chrome.rs");
        let target = include_str!("target.rs");
        assert!(transform_lock_violations(chrome, target).is_empty());

        // Negative 1: alias the outer metric into a local inside a live function, then subtract.
        let alias = inject_after_open_brace(
            chrome,
            "live_map_inputs",
            " let oh = metrics.outer_height; let _offset = oh - metrics.inner_height;",
        );
        let violations = transform_lock_violations(&alias, target);
        assert!(
            violations
                .iter()
                .any(|v| v.contains("outer_height") && v.contains("outside")),
            "alias mutation must be rejected: {violations:?}"
        );

        // Negative 2: whitespace/formatting variant of the legacy subtraction in a live function.
        let weird = inject_after_open_brace(
            chrome,
            "map_from_raw",
            " let _ = metrics.outer_width\n\t\t-\n metrics.inner_width;",
        );
        let violations = transform_lock_violations(&weird, target);
        assert!(
            violations
                .iter()
                .any(|v| v.contains("outer_width") && v.contains("outside")),
            "whitespace mutation must be rejected: {violations:?}"
        );

        // Negative 3: split alias in an unnamed helper, even outside the named live spans.
        let hidden = inject_after_open_brace(
            chrome,
            "fixture_map",
            " fn hidden_helper(metrics: &ChromeMetrics) -> f64 { let oh = metrics.outer_height; let ih = metrics.inner_height; oh - ih }",
        );
        let violations = transform_lock_violations(&hidden, target);
        assert!(
            violations
                .iter()
                .any(|v| v.contains("outer_height") && v.contains("outside")),
            "unnamed-helper alias must be rejected: {violations:?}"
        );

        assert!(legacy_subtraction(
            "metrics.outer_height\n\t-\nmetrics.inner_height"
        ));
        assert!(legacy_subtraction(
            "metrics . outer_width  -  metrics . inner_width"
        ));
    }

    #[test]
    fn no_cdp_or_synthetic_dom_input() {
        for src in [
            include_str!("actuate.rs"),
            include_str!("target.rs"),
            include_str!("point.rs"),
            include_str!("observe.rs"),
        ] {
            assert!(!src.contains(&["--remote", "debugging", "port"].join("-")));
            assert!(!src.contains(&["--enable", "automation"].join("-")));
            assert!(!src.contains(&["Play", "wright"].join("")));
            assert!(!src.contains(&["element", ".click("].join("")));
            assert!(!src.contains(&["dispatch", "Event("].join("")));
        }
        let ext = include_str!("../extension/content.js");
        assert!(!ext.contains(&["element", ".click("].join("")));
        assert!(!ext.contains(&["dispatch", "Event("].join("")));
        assert!(!ext.contains(&["all_frames", ": true"].join("")));
    }

    #[test]
    fn click_inner_guard_order_transform_interactable_fence_outside() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn click_inner").expect("click_inner");
        let end = src.find("fn dispatch_blind_click").expect("blind");
        let body = &src[start..end];
        let transform = body
            .find("refuse_transform(session_id, info, *contract, identity)")
            .expect("transform refusal");
        let guard = body
            .find("guard_require_interactable")
            .expect("interactable guard");
        let fence = body.find("fence::ensure_installed").expect("fence");
        let gate = body.find("fence::gate_click").expect("gate_click");
        let remap = body.find("final_live_remap").expect("final remap");
        let client = body
            .find("refuse_if_outside_client")
            .expect("outside client");
        let offer = body.find("foreground::offer").expect("offer");
        assert!(
            transform < guard
                && guard < fence
                && fence < gate
                && gate < remap
                && remap < client
                && client < offer,
            "locked order transform < interactable < fence < remap < outside_client < offer:\n{body}"
        );
        assert!(
            body.find("try_snapshot").is_none()
                && body.find("try_resolve").is_none()
                && body.find("note_last_url").is_none(),
            "0108 lock: click_inner must not add a snapshot/resolve/URL call"
        );
    }

    #[test]
    fn require_interactable_blocked_refuses_before_fence_offer_and_input() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn click_inner").expect("click_inner");
        let end = src.find("fn dispatch_blind_click").expect("blind");
        let body = &src[start..end];
        let guard = body
            .find("guard_require_interactable")
            .expect("interactable guard");
        let offer = body.find("foreground::offer").expect("offer");
        let move_to = body.find("input::move_to").expect("move_to");
        let click_input = body.find("input::left_click").expect("left_click");
        assert!(
            guard < offer && guard < move_to && guard < click_input,
            "the refusal must be pre-dispatch:\n{body}"
        );
        // The guard itself never dispatches.
        let hittest = include_str!("hittest.rs");
        let guard_start = hittest
            .find("pub fn guard_require_interactable")
            .expect("guard fn");
        let guard_end = hittest[guard_start..]
            .find("pub fn refusal")
            .map(|i| guard_start + i)
            .expect("refusal fn");
        let guard_body = &hittest[guard_start..guard_end];
        for forbidden in ["foreground::offer", "input::move_to", "input::left_click"] {
            assert!(
                !guard_body.contains(forbidden),
                "guard must not call {forbidden}:\n{guard_body}"
            );
        }
    }

    #[test]
    fn require_interactable_blind_refuses() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn click_inner").expect("click_inner");
        let end = src.find("fn dispatch_blind_click").expect("blind");
        let body = &src[start..end];
        let blind = body
            .find("req.require_interactable && req.blind")
            .expect("blind flag refusal");
        let resolve = body.find("resolve_req").expect("resolve_req");
        assert!(
            blind < resolve,
            "blind + require_interactable must refuse before any DOM read:\n{body}"
        );
        assert!(body.contains("interactable_unavailable"), "{body}");
    }

    #[test]
    fn hover_require_interactable_refuses() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn hover_inner").expect("hover_inner");
        let end = src.find("pub fn type_text").expect("type_text");
        let body = &src[start..end];
        let guard = body
            .find("req.require_interactable")
            .expect("hover flag guard");
        let resolve = body.find("resolve_req").expect("resolve_req");
        let move_to = body.find("input::move_to").expect("move_to");
        assert!(
            guard < resolve && guard < move_to,
            "hover must refuse the flag before resolve/move:\n{body}"
        );
        assert!(body.contains("interactable_unavailable"), "{body}");
    }

    #[test]
    fn require_interactable_true_guard_allows() {
        // Positive control at the guard seam: true is not a refusal. Dispatch wiring is the
        // existing click path; this test pins that the guard returns None for a true verdict.
        let report = crate::point::TransformReport {
            valid: true,
            method: crate::point::TransformMethod::RenderWidgetHost,
            confidence: crate::point::TransformConfidence::High,
            scale: Some(1.0),
            dpr: Some(1.0),
            dpi: Some(96),
            page_zoom: Some(1.0),
            page_viewport: Some(Rect {
                x: 0,
                y: 0,
                w: 1200,
                h: 900,
            }),
            anchor_basis: Some(crate::point::CssBasis::Client),
            layout_basis: Some(crate::point::CssBasis::Client),
            reason: None,
            observe_id: Some("f3a9c1d2e4b5a607".into()),
        };
        let css = crate::point::CssPoint::new(10.0, 20.0);
        let contract = crate::point::PointContract {
            source: PointSource::Chr,
            css: Some(css),
            physical: crate::point::map_point(
                &crate::point::PointContract {
                    source: PointSource::Chr,
                    css: Some(css),
                    physical: None,
                    transform: report.clone(),
                },
                css,
            )
            .ok(),
            transform: report,
        };
        let mut assessment: crate::hittest::Assessment = serde_json::from_str(include_str!(
            "../tests/fixtures/hittest/replies/unobstructed.json"
        ))
        .expect("assessment");
        assessment.selected_point = css;
        assessment.target_rect_css = crate::point::CssRect {
            left: 0.0,
            top: 0.0,
            width: 100.0,
            height: 40.0,
        };
        let resolved = crate::target::ResolvedTarget {
            target: Target::ElementId("chr:0".into()),
            kind: "element",
            id: Some("chr:0".into()),
            x: 10,
            y: 20,
            rect: Rect {
                x: 0,
                y: 0,
                w: 100,
                h: 40,
            },
            hwnd: Some(0x1234),
            name: String::new(),
            role: String::new(),
            css_point: Some(css),
            css_rect: Some(crate::point::CssRect {
                left: 0.0,
                top: 0.0,
                width: 100.0,
                height: 40.0,
            }),
            contract: Some(contract),
            map: None,
            assessment: Some(assessment),
            identity: None,
            uia_runtime_id: None,
            uia_expected_fp: None,
        };
        let info = ActuateTarget {
            kind: "element".into(),
            id: Some("chr:0".into()),
            x: 10,
            y: 20,
        };
        assert!(
            crate::hittest::guard_require_interactable("s-test", &info, &resolved)
                .expect("guard")
                .is_none()
        );
    }

    #[test]
    fn no_process_global_offset_cache() {
        // The map is rebuilt per observe/resolve/guard; no calibrated-once constant.
        let src = include_str!("actuate.rs");
        assert!(src.contains("final_live_remap"));
        assert!(!src.contains(&["OFFSET", "_CACHE"].join("")));
        assert!(!src.contains(&["static ", "OFFSET"].join("")));
    }

    #[test]
    fn identity_refusal_sends_nothing_and_names_state() {
        let r = SnapshotRef::Chrome {
            tab: 12,
            frame: 0,
            doc: "9f3a".into(),
            snap: "a1b2c3d4".into(),
            idx: 7,
        };
        let report = IdentityReport::chrome_failure(&r, IdentityState::Stale, "stale_element");
        let env = refuse_identity(
            "s-identity".into(),
            ActuateTarget {
                kind: "element".into(),
                id: Some(r.to_string()),
                x: 5,
                y: 6,
            },
            report,
        )
        .expect("refusal");
        assert!(!env.ok);
        assert_eq!(env.error.as_deref(), Some("stale_element"));
        assert!(env.point.is_none());
        assert!(env.identity.is_some());
        assert_eq!((env.target.x, env.target.y), (0, 0));
        let json = serde_json::to_string(&env).unwrap();
        assert!(!json.contains("\"point\""));
        assert!(!json.contains("\"verified\""));
        assert!(json.contains("\"binding\":\"scoped\""));
        assert!(json.contains("\"state\":\"stale\""));
    }

    #[test]
    fn stale_identity_refusal_does_not_arm_cooldown() {
        let _g = crate::cooldown::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::cooldown::reset_for_test();
        let r = SnapshotRef::Chrome {
            tab: 12,
            frame: 0,
            doc: "9f3a".into(),
            snap: "a1b2c3d4".into(),
            idx: 7,
        };
        for _ in 0..3 {
            let mut env = refuse_identity(
                "s-stale-cooldown".into(),
                none_target(),
                IdentityReport::chrome_failure(&r, IdentityState::Stale, "stale_identity"),
            )
            .unwrap();
            settle_cooldown("click", &mut env);
        }
        assert_eq!(crate::cooldown::snapshot("s-stale-cooldown").attempt, 0);
    }

    #[test]
    fn binding_precondition_refusal_does_not_arm_cooldown() {
        let _g = crate::cooldown::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::cooldown::reset_for_test();
        let r = SnapshotRef::Chrome {
            tab: 12,
            frame: 0,
            doc: "9f3a".into(),
            snap: "a1b2c3d4".into(),
            idx: 7,
        };
        for _ in 0..3 {
            let mut env = refuse_identity(
                "s-binding-cooldown".into(),
                none_target(),
                IdentityReport::chrome_failure(&r, IdentityState::Unknown, "tab_not_active"),
            )
            .unwrap();
            settle_cooldown("click", &mut env);
        }
        assert_eq!(crate::cooldown::snapshot("s-binding-cooldown").attempt, 0);
    }

    #[test]
    fn unknown_identity_refusal_arms_cooldown_once() {
        let _g = crate::cooldown::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::cooldown::reset_for_test();
        let r = SnapshotRef::Chrome {
            tab: 12,
            frame: 0,
            doc: "9f3a".into(),
            snap: "a1b2c3d4".into(),
            idx: 7,
        };
        let mut env = refuse_identity(
            "s-unknown-cooldown".into(),
            none_target(),
            IdentityReport::chrome_failure(&r, IdentityState::Unknown, "host_down"),
        )
        .unwrap();
        settle_cooldown("click", &mut env);
        assert_eq!(crate::cooldown::snapshot("s-unknown-cooldown").attempt, 1);
    }

    #[test]
    fn exact_identity_transform_refusal_arms_cooldown() {
        let _g = crate::cooldown::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::cooldown::reset_for_test();
        let r = SnapshotRef::Chrome {
            tab: 12,
            frame: 0,
            doc: "9f3a".into(),
            snap: "a1b2c3d4".into(),
            idx: 7,
        };
        let mut env = refuse_transform(
            "s-exact-transform".into(),
            none_target(),
            invalid_chr_contract("no_viewport_anchor"),
            Some(IdentityReport::exact_chrome(&r)),
        )
        .unwrap();
        assert_eq!(
            env.identity.as_ref().map(|i| i.state),
            Some(IdentityState::Exact)
        );
        settle_cooldown("click", &mut env);
        assert_eq!(crate::cooldown::snapshot("s-exact-transform").attempt, 1);
    }

    #[test]
    fn transform_refusal_identity_is_exact_but_error_names_reason() {
        let r = SnapshotRef::Chrome {
            tab: 12,
            frame: 0,
            doc: "9f3a".into(),
            snap: "a1b2c3d4".into(),
            idx: 7,
        };
        let env = refuse_transform(
            "s-transform-identity".into(),
            none_target(),
            invalid_chr_contract("no_viewport_anchor"),
            Some(IdentityReport::exact_chrome(&r)),
        )
        .unwrap();
        assert!(!env.ok);
        assert!(!env.verified);
        assert_eq!(env.error.as_deref(), Some("no_viewport_anchor"));
        assert_eq!(
            env.identity.as_ref().map(|i| i.state),
            Some(IdentityState::Exact)
        );
        assert_eq!(
            env.identity.as_ref().and_then(|i| i.reason.as_deref()),
            Some("transform_invalid")
        );
        let hint = env
            .identity
            .as_ref()
            .and_then(|i| i.hint.as_deref())
            .unwrap_or_default();
        assert!(hint.contains("re-observe"), "{hint}");
        assert!(!hint.contains("identity is exact"), "{hint}");
        let point = env.point.as_ref().expect("point contract");
        assert!(!point.transform.valid);
    }

    #[test]
    fn click_stale_scoped_ref_refuses_after_yield_and_before_offer() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn click_inner(").expect("click_inner");
        let end = src
            .find("fn dispatch_blind_click(")
            .expect("dispatch_blind_click");
        let body = &src[start..end];
        let pos = |needle: &str| body.find(needle).unwrap_or_else(|| panic!("{needle}"));
        let yield_pos = pos("refuse_if_yielded");
        let blocked_pos = pos("refuse_if_blocked");
        let high_il_pos = pos("refuse_if_high_il");
        let identity_pos = pos("refuse_identity");
        let transform_pos = pos("refuse_transform");
        let gate_pos = pos("fence::gate_click");
        let offer_pos = pos("foreground::offer");
        assert!(yield_pos < blocked_pos);
        assert!(blocked_pos < high_il_pos);
        assert!(high_il_pos < identity_pos);
        assert!(identity_pos < transform_pos);
        assert!(identity_pos < gate_pos);
        assert!(identity_pos < offer_pos);
    }

    #[test]
    fn identity_outcome_returns_before_expect() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn click_inner(").expect("click_inner");
        let end = src
            .find("fn dispatch_blind_click(")
            .expect("dispatch_blind_click");
        let body = &src[start..end];
        let identity = body.find("refuse_identity").expect("identity refusal");
        let expect = body
            .find("expect(\"resolved outcome\")")
            .expect("expect site");
        assert!(identity < expect);
    }

    #[test]
    fn final_binding_probe_refuses_changed_tab() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn final_live_remap(").expect("final_live_remap");
        let end = src
            .find("fn point_for_dispatch(")
            .expect("point_for_dispatch");
        let body = &src[start..end];
        let remap = body.find("chrome::remap_chr").expect("remap");
        let probe = body.find("scoped_binding_refusal").expect("binding probe");
        assert!(remap < probe);
        let helper_start = src
            .find("fn scoped_binding_refusal(")
            .expect("scoped_binding_refusal");
        let helper_end = src.find("pub fn click(").expect("click");
        let helper = &src[helper_start..helper_end];
        assert!(helper.contains("tab_not_active"));
        assert!(helper.contains("window_not_focused"));
    }

    #[test]
    fn frame_binding_refusal_checks_doc_and_skips_top() {
        let r = SnapshotRef::Chrome {
            tab: 12,
            frame: 3,
            doc: "c0a6".into(),
            snap: "a1b2c3d4".into(),
            idx: 0,
        };
        let target = Target::Scoped(r.clone());
        let info = ActuateTarget {
            kind: "element".into(),
            id: Some(r.to_string()),
            x: 10,
            y: 20,
        };
        chrome::set_frame_binding_stub(Some(Ok(chrome::FrameBindingEvidence {
            frame: 3,
            doc: "ffff".into(),
            snap: None,
            chain_present: true,
        })));
        let env = frame_binding_refusal("s-frame", &info, &target)
            .expect("probe")
            .expect("refusal");
        assert!(!env.ok);
        assert_eq!(
            env.identity.as_ref().and_then(|i| i.reason.as_deref()),
            Some("frame_document_changed")
        );

        chrome::set_frame_binding_stub(Some(Ok(chrome::FrameBindingEvidence {
            frame: 3,
            doc: "c0a6".into(),
            snap: Some("a1b2c3d4".into()),
            chain_present: true,
        })));
        assert!(
            frame_binding_refusal("s-frame", &info, &target)
                .expect("probe")
                .is_none()
        );

        let top = Target::Scoped(SnapshotRef::Chrome {
            tab: 12,
            frame: 0,
            doc: "c0a6".into(),
            snap: "a1b2c3d4".into(),
            idx: 0,
        });
        chrome::set_frame_binding_stub(Some(Err(IdentityReport::chrome_failure(
            &r,
            IdentityState::Unknown,
            "frame_geometry_unavailable",
        ))));
        assert!(
            frame_binding_refusal("s-frame", &info, &top)
                .expect("top probe")
                .is_none()
        );
        chrome::set_frame_binding_stub(None);
    }

    #[test]
    fn click_checks_frame_binding_before_move() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn click_inner(").expect("click_inner");
        let end = src.find("fn dispatch_blind_click(").expect("blind");
        let body = &src[start..end];
        let frame = body
            .find("frame_binding_refusal")
            .expect("frame binding probe");
        let move_to = body.find("input::move_to").expect("move_to");
        assert!(frame < move_to, "frame_binding must precede move_to");
    }

    #[test]
    fn click_reprobes_before_left_click() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn click_inner(").expect("click_inner");
        let end = src
            .find("fn dispatch_blind_click(")
            .expect("dispatch_blind_click");
        let body = &src[start..end];
        let capture = body.find("capture_roi").expect("capture_roi");
        let probe = body
            .find("scoped_binding_refusal(&session_id, &info, &resolved.target)")
            .expect("pre-click binding probe");
        let click = body.find("input::left_click").expect("left_click");
        assert!(capture < probe);
        assert!(probe < click);
        assert!(body.contains("Narrow R7"));
    }

    #[test]
    fn hover_reprobes_before_move_and_dwell() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn hover_inner(").expect("hover_inner");
        let end = src.find("pub fn type_text(").expect("type_text");
        let body = &src[start..end];
        let offer = body.find("foreground::offer").expect("offer");
        let probe = body
            .find("scoped_binding_refusal(&session_id, &info, &resolved.target)")
            .expect("hover binding probe");
        let move_to = body.find("input::move_to").expect("move_to");
        let dwell = body.find("hover_dwell").expect("hover_dwell");
        assert!(offer < probe);
        assert!(probe < move_to);
        assert!(move_to < dwell);
    }

    #[test]
    fn scroll_reprobes_before_scroll_wheel() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn scroll_inner(").expect("scroll_inner");
        let end = src.find("pub fn wait_settle(").expect("wait_settle");
        let body = &src[start..end];
        let probe = body
            .find("scoped_binding_refusal(&session_id, &info, &target)")
            .expect("scroll binding probe");
        let wheel = body.find("input::scroll_wheel").expect("scroll_wheel");
        assert!(probe < wheel);
    }

    #[test]
    fn click_retry_reprobes_before_second_click() {
        let src = include_str!("actuate.rs");
        let start = src.find("fn click_inner(").expect("click_inner");
        let end = src
            .find("fn dispatch_blind_click(")
            .expect("dispatch_blind_click");
        let body = &src[start..end];
        let retry = body.find("should_retry_click").expect("retry branch");
        let tail = &body[retry..];
        let probe = tail
            .find("scoped_binding_refusal(&session_id, &info, &resolved.target)")
            .expect("retry binding probe");
        let click = tail.find("input::left_click").expect("retry click");
        assert!(probe < click);
    }

    #[test]
    fn scoped_binding_refusal_refuses_a_flip_to_background() {
        let r = SnapshotRef::Chrome {
            tab: 12,
            frame: 0,
            doc: "9f3a".into(),
            snap: "a1b2c3d4".into(),
            idx: 7,
        };
        let target = Target::Scoped(r.clone());
        let info = ActuateTarget {
            kind: "element".into(),
            id: Some(r.to_string()),
            x: 10,
            y: 20,
        };
        chrome::set_binding_sequence(vec![
            Ok(chrome::BindingEvidence {
                tab: 12,
                window: Some(45),
                active: true,
                focused: true,
            }),
            Ok(chrome::BindingEvidence {
                tab: 12,
                window: Some(45),
                active: false,
                focused: true,
            }),
        ]);
        assert!(
            scoped_binding_refusal("s-flip", &info, &target)
                .expect("first probe")
                .is_none()
        );
        let refusal = scoped_binding_refusal("s-flip", &info, &target)
            .expect("second probe")
            .expect("refusal");
        assert!(!refusal.ok);
        assert_eq!(refusal.error.as_deref(), Some("tab_not_active"));
        assert!(refusal.point.is_none());
        let identity = refusal.identity.expect("identity");
        assert_eq!(identity.state, IdentityState::Unknown);
        assert_eq!(identity.reason.as_deref(), Some("tab_not_active"));
    }

    fn scoped_valid_hit() -> (SnapshotRef, chrome::ChromeHit) {
        let map = act_remap_map(100, 80);
        let css = CssRect {
            left: 50.0,
            top: 40.0,
            width: 100.0,
            height: 20.0,
        };
        let contract = crate::point::contract_for(&map, css.center());
        let rect = crate::point::map_rect(&contract, css).expect("scoped rect");
        let r = SnapshotRef::Chrome {
            tab: 12,
            frame: 0,
            doc: "9f3a".into(),
            snap: "a1b2c3d4".into(),
            idx: 7,
        };
        let hit = chrome::ChromeHit {
            id: r.to_string(),
            name: "Continue".into(),
            role: "button".into(),
            rect,
            css_rect: css,
            css_point: css.center(),
            contract,
            hwnd: Some(0x1234),
            map: Some(map),
            assessment: None,
            identity: Some(IdentityReport::exact_chrome(&r)),
        };
        (r, hit)
    }

    #[test]
    fn scoped_fixture_valid_transform_refuses_at_binding_and_sends_nothing() {
        let (r, hit) = scoped_valid_hit();
        chrome::set_scoped_resolve_stub(Some(Ok(hit)));
        chrome::set_binding_stub(Some(Err(IdentityReport::chrome_failure(
            &r,
            IdentityState::Unknown,
            "fixture_snapshot",
        ))));
        ACT_REMAP_ANCHOR.with(|c| {
            c.set(Rect {
                x: 100,
                y: 80,
                w: 1200,
                h: 900,
            })
        });
        chrome::set_remap_geometry_hook(Some(act_remap_geometry));
        chrome::set_remap_identity_hook(Some(act_remap_identity));
        crate::elevation::set_high_il_hook(Some(|_| false));
        input::set_send_inputs_hook(Some(count_identity_sends));
        SEND_COUNT.with(|c| c.set(0));
        let env = hover(ActuateRequest {
            session_id: Some("s-fixture-binding".into()),
            element_id: Some(r.to_string()),
            ..ActuateRequest::default()
        })
        .expect("hover refusal");
        chrome::set_scoped_resolve_stub(None);
        chrome::set_binding_stub(None);
        chrome::set_remap_geometry_hook(None);
        chrome::set_remap_identity_hook(None);
        crate::elevation::set_high_il_hook(None);
        input::set_send_inputs_hook(None);
        assert!(!env.ok);
        assert_eq!(sent_count(), 0);
        assert_eq!(
            env.identity.as_ref().and_then(|i| i.reason.as_deref()),
            Some("fixture_snapshot")
        );
    }

    #[test]
    fn hover_and_scroll_stale_scoped_ref_refuse() {
        let r = SnapshotRef::Chrome {
            tab: 12,
            frame: 0,
            doc: "9f3a".into(),
            snap: "a1b2c3d4".into(),
            idx: 7,
        };
        let report = IdentityReport::chrome_failure(&r, IdentityState::Stale, "stale_element");
        chrome::set_scoped_resolve_stub(Some(Err(report)));
        input::set_send_inputs_hook(Some(count_identity_sends));
        SEND_COUNT.with(|c| c.set(0));
        let hover_env = hover(ActuateRequest {
            session_id: Some("s-hover-stale".into()),
            element_id: Some(r.to_string()),
            ..ActuateRequest::default()
        })
        .expect("hover refusal");
        assert!(!hover_env.ok);
        assert!(hover_env.point.is_none());
        assert_eq!(hover_env.error.as_deref(), Some("stale_element"));
        assert_eq!(
            hover_env.identity.as_ref().map(|i| i.state),
            Some(IdentityState::Stale)
        );

        let scroll_env = scroll(ActuateRequest {
            session_id: Some("s-scroll-stale".into()),
            element_id: Some(r.to_string()),
            dy: Some(1),
            ..ActuateRequest::default()
        })
        .expect("scroll refusal");
        chrome::set_scoped_resolve_stub(None);
        input::set_send_inputs_hook(None);
        assert_eq!(sent_count(), 0, "stale hover/scroll must not send input");
        assert!(!scroll_env.ok);
        assert_eq!(scroll_env.error.as_deref(), Some("stale_element"));
        assert_eq!(
            scroll_env.identity.as_ref().map(|i| i.state),
            Some(IdentityState::Stale)
        );
    }

    #[test]
    fn post_dispatch_binding_refusal_keeps_point_and_unverified() {
        let map = act_remap_map(0, 0);
        let css = CssPoint::new(60.0, 60.0);
        let contract = crate::point::contract_for(&map, css);
        let r = SnapshotRef::Chrome {
            tab: 12,
            frame: 0,
            doc: "9f3a".into(),
            snap: "a1b2c3d4".into(),
            idx: 7,
        };
        let resolved = ResolvedTarget {
            target: Target::Scoped(r.clone()),
            kind: "element",
            id: Some(r.to_string()),
            x: 100,
            y: 100,
            rect: Rect {
                x: 100,
                y: 100,
                w: 100,
                h: 100,
            },
            hwnd: Some(0x1234),
            name: "Sign in".into(),
            role: "Button".into(),
            css_point: Some(css),
            css_rect: Some(CssRect {
                left: 10.0,
                top: 10.0,
                width: 100.0,
                height: 100.0,
            }),
            contract: Some(contract),
            map: Some(map),
            assessment: None,
            identity: Some(IdentityReport::exact_chrome(&r)),
            uia_runtime_id: None,
            uia_expected_fp: None,
        };
        let report = IdentityReport::chrome_failure(&r, IdentityState::Unknown, "tab_not_active");
        let env = post_dispatch_binding_refusal(
            "s-retry".into(),
            none_target(),
            &resolved,
            true,
            true,
            report,
        )
        .expect("post-dispatch refusal");
        assert!(!env.ok);
        assert!(!env.verified);
        assert!(env.post_dispatch);
        assert!(env.point.is_some());
        assert_eq!(env.error.as_deref(), Some("binding_changed_after_dispatch"));
        assert_eq!(
            env.identity.as_ref().and_then(|i| i.reason.as_deref()),
            Some("binding_changed_after_dispatch")
        );
        let hint = env
            .identity
            .as_ref()
            .and_then(|i| i.hint.as_deref())
            .unwrap_or_default();
        assert!(hint.contains("already"), "{hint}");
        assert!(!hint.to_ascii_lowercase().contains("retry"), "{hint}");
        assert!(hint.contains("re-observe"), "{hint}");
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"post_dispatch\":true"));
        assert!(json.contains("\"verified\":false"));

        let pre = refuse_identity(
            "s-pre".into(),
            none_target(),
            IdentityReport::chrome_failure(&r, IdentityState::Unknown, "tab_not_active"),
        )
        .expect("pre-dispatch refusal");
        assert!(!pre.post_dispatch);
        assert_eq!(
            pre.identity.as_ref().and_then(|i| i.reason.as_deref()),
            Some("tab_not_active")
        );
        assert!(
            pre.identity
                .as_ref()
                .and_then(|i| i.hint.as_deref())
                .unwrap_or_default()
                .contains("activate"),
            "pre-dispatch hint remains the activate-and-retry guidance"
        );

        let _g = crate::cooldown::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::cooldown::reset_for_test();
        let mut post = env;
        crate::actuate::settle_cooldown("click", &mut post);
        assert_eq!(
            crate::cooldown::snapshot("s-retry").attempt,
            1,
            "post-dispatch binding change counts toward loop protection"
        );
    }

    #[test]
    fn legacy_envelope_labels_binding_legacy() {
        let mut env = base(
            "s-legacy".into(),
            none_target(),
            false,
            false,
            false,
            false,
            false,
            None,
        )
        .unwrap();
        env.identity = Some(IdentityReport::legacy());
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"binding\":\"legacy\""));
        assert!(!json.contains("\"state\":\"stale\""));
    }

    #[test]
    fn click_exact_scoped_ref_keeps_point_contract() {
        let map = act_remap_map(0, 0);
        let contract = crate::point::contract_for(&map, CssPoint::new(60.0, 60.0));
        let r = SnapshotRef::Chrome {
            tab: 12,
            frame: 0,
            doc: "9f3a".into(),
            snap: "a1b2c3d4".into(),
            idx: 7,
        };
        let resolved = ResolvedTarget {
            target: Target::Scoped(r.clone()),
            kind: "element",
            id: Some(r.to_string()),
            x: 100,
            y: 100,
            rect: Rect {
                x: 100,
                y: 100,
                w: 100,
                h: 100,
            },
            hwnd: Some(0x1234),
            name: "Sign in".into(),
            role: "Button".into(),
            css_point: Some(CssPoint::new(60.0, 60.0)),
            css_rect: Some(CssRect {
                left: 10.0,
                top: 10.0,
                width: 100.0,
                height: 100.0,
            }),
            contract: Some(contract),
            map: Some(map),
            assessment: None,
            identity: Some(IdentityReport::exact_chrome(&r)),
            uia_runtime_id: None,
            uia_expected_fp: None,
        };
        let point = point_for_dispatch(&resolved).expect("point contract");
        assert!(point.transform.valid);
        assert_eq!(resolved.identity.unwrap().state, IdentityState::Exact);
    }
}
