//! High-level click / hover / type / key / scroll / wait_settle / stop.

use std::sync::Mutex;

use serde::Serialize;

use crate::bezier::Rng;
use crate::challenge::{self, ChallengeInfo, YIELD_ERROR};
use crate::error::HandsError;
use crate::fence::{self, FenceInfo};
use crate::foreground;
use crate::input;
use crate::lease;
use crate::logs::{self, LogFence, LogTarget};
use crate::observe::ENVELOPE_MAX_BYTES;
use crate::session::resolve_session_id_from_os;
use crate::settle;
use crate::space::{Rect, Space, ensure_dpi, virtual_screen};
use crate::target::Target;

#[derive(Debug, Clone, Serialize)]
pub struct ActuateTarget {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Serialize)]
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
}

static LAST_TARGET: Mutex<Option<Rect>> = Mutex::new(None);

fn remember_target(rect: Rect) {
    if let Ok(mut slot) = LAST_TARGET.lock() {
        *slot = Some(rect);
    }
}

/// Process-local last click/hover rect. Standalone `wait_settle` does not read this
/// (bare default is the foreground window). Kept so `remember_target` stays the writer
/// (hover yield leftover: remember still happens before `refuse_if_yielded`).
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
    })
}

fn refuse_if_yielded(
    session_id: &str,
    target: ActuateTarget,
) -> Result<Option<ActuateEnvelope>, HandsError> {
    if challenge::yielded() {
        Ok(Some(refuse_yield(session_id.to_string(), target)?))
    } else {
        Ok(None)
    }
}

fn refuse_fence(
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
    let frozen = matches!(err, HandsError::Lease(_));
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

fn session(req: &ActuateRequest) -> Result<String, HandsError> {
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

fn after_actuate(
    tool: &str,
    result: Result<ActuateEnvelope, HandsError>,
    type_len: Option<usize>,
    key: Option<&str>,
) -> Result<ActuateEnvelope, HandsError> {
    logs::ensure_installed();
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
        );
    }
    result
}

fn resolve_req(
    req: &ActuateRequest,
    space: Space,
) -> Result<crate::target::ResolvedTarget, HandsError> {
    let target = Target::parse(req.element_id.as_deref(), req.grid.as_deref(), req.x, req.y)?;
    target.resolve(space)
}

pub fn click(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    after_actuate("click", click_inner(req), None, None)
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
    let resolved = match resolve_req(&req, space) {
        Ok(r) => r,
        Err(err) => return fail(session_id, none_target(), err, false, false, false),
    };
    let info = resolved_info(resolved.kind, resolved.id.clone(), resolved.x, resolved.y);
    if let Some(env) = refuse_if_yielded(&session_id, info.clone())? {
        return Ok(env);
    }
    fence::ensure_installed();
    match fence::gate_click(&session_id, &resolved) {
        Ok(None) => {}
        Ok(Some(info_fence)) => return refuse_fence(session_id, info, info_fence),
        Err(err) => return fail(session_id, info, err, false, false, false),
    }
    challenge::note_actuation_if_proceeding(false);
    remember_target(resolved.rect);
    let mut rng = Rng::from_time();
    let roi = settle::default_roi(space, Some(resolved.rect), (resolved.x, resolved.y));
    let snapshot = match crate::capture::capture_roi(space, roi) {
        Ok(f) => f,
        Err(err) => return fail(session_id, info, err, false, false, false),
    };
    let foregrounded = foreground::offer(resolved.hwnd, (resolved.x, resolved.y));
    if let Err(err) = input::move_to(space, resolved.x, resolved.y, &mut rng) {
        return fail(session_id, info, err, foregrounded, false, false);
    }
    if let Err(err) = input::left_click(&mut rng) {
        return fail(session_id, info, err, foregrounded, false, false);
    }
    let (settled, after) = match settle::wait_settle(space, roi) {
        Ok(v) => v,
        Err(err) => return fail(session_id, info, err, foregrounded, false, false),
    };
    let same = snapshot.width == after.width
        && snapshot.height == after.height
        && settle::changed_ratio(&snapshot.pixels, &after.pixels) < settle::RATIO_LIMIT;
    if same && lease::poll().is_ok() {
        if let Err(err) = input::left_click(&mut rng) {
            return fail(session_id, info, err, foregrounded, true, settled);
        }
        let (settled2, _) = match settle::wait_settle(space, roi) {
            Ok(v) => v,
            Err(err) => return fail(session_id, info, err, foregrounded, true, settled),
        };
        return base(
            session_id,
            info,
            true,
            false,
            true,
            settled2,
            foregrounded,
            None,
        );
    }
    if lease::is_frozen() {
        return base(
            session_id,
            info,
            false,
            true,
            false,
            settled,
            foregrounded,
            Some("desk lease frozen (physical input or Pause/Break)".into()),
        );
    }
    base(
        session_id,
        info,
        true,
        false,
        false,
        settled,
        foregrounded,
        None,
    )
}

pub fn hover(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    after_actuate("hover", hover_inner(req), None, None)
}

fn hover_inner(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    let session_id = match session(&req) {
        Ok(id) => id,
        Err(err) => return fail(raw_session(&req), none_target(), err, false, false, false),
    };
    let space = match ensure_dpi().and_then(|_| virtual_screen()) {
        Ok(s) => s,
        Err(err) => return fail(session_id, none_target(), err, false, false, false),
    };
    let resolved = match resolve_req(&req, space) {
        Ok(r) => r,
        Err(err) => return fail(session_id, none_target(), err, false, false, false),
    };
    remember_target(resolved.rect);
    let info = resolved_info(resolved.kind, resolved.id.clone(), resolved.x, resolved.y);
    if let Some(env) = refuse_if_yielded(&session_id, info.clone())? {
        return Ok(env);
    }
    let mut rng = Rng::from_time();
    let foregrounded = foreground::offer(resolved.hwnd, (resolved.x, resolved.y));
    if let Err(err) = input::move_to(space, resolved.x, resolved.y, &mut rng) {
        return fail(session_id, info, err, foregrounded, false, false);
    }
    if let Err(err) = hover_dwell() {
        return fail(session_id, info, err, foregrounded, false, false);
    }
    base(
        session_id,
        info,
        true,
        false,
        false,
        false,
        foregrounded,
        None,
    )
}

pub fn type_text(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    let type_len = req.text.as_ref().map(|t| t.chars().count());
    after_actuate("type", type_text_inner(req), type_len, None)
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
    if let Err(err) = ensure_dpi() {
        return fail(session_id, info, err, false, false, false);
    }
    challenge::note_actuation();
    match input::type_text(text) {
        Ok(_) => base(session_id, info, true, false, false, false, false, None),
        Err(err) => fail(session_id, info, err, false, false, false),
    }
}

pub fn key(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    let name = req.name.clone();
    after_actuate("key", key_inner(req), None, name.as_deref())
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
    after_actuate("scroll", scroll_inner(req), None, None)
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
    let mut foregrounded = false;
    let mut info = none_target();
    if let Some(env) = refuse_if_yielded(&session_id, info.clone())? {
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
            }
            Err(err) => return fail(session_id, info, err, false, false, false),
        }
    }
    challenge::note_actuation();
    match input::scroll_wheel(dy, req.dx) {
        Ok(()) => base(
            session_id,
            info,
            true,
            false,
            false,
            false,
            foregrounded,
            None,
        ),
        Err(err) => fail(session_id, info, err, foregrounded, false, false),
    }
}

pub fn wait_settle(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    after_actuate("wait_settle", wait_settle_inner(req), None, None)
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

fn hover_dwell() -> Result<(), HandsError> {
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
    after_actuate("stop", stop_inner(req), None, None)
}

fn stop_inner(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    stop_shared(req)
}

/// CLI `stop` without installing hooks — posts the same desk-wide request.
pub fn stop_cli_noop(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    after_actuate("stop", stop_cli_noop_inner(req), None, None)
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
    let frozen = lease::is_frozen();
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
        };
        let json = serialize_envelope(&env).expect("json");
        assert!(json.contains("\"roi\""), "{json}");
        assert!(json.contains("\"w\":800"), "{json}");
        assert!(json.contains("\"h\":600"), "{json}");
        assert!(json.contains("\"x\":10"), "{json}");
        assert!(json.contains("\"y\":20"), "{json}");
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
        };
        let json = serialize_envelope(&env).expect("json");
        assert!(!json.contains("\"roi\""), "{json}");
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
}
