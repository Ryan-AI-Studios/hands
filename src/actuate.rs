//! High-level click / hover / type / key / scroll / wait_settle / stop.

use std::sync::Mutex;

use serde::Serialize;

use crate::bezier::Rng;
use crate::error::HandsError;
use crate::foreground;
use crate::input;
use crate::lease;
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

fn session(req: &ActuateRequest) -> String {
    resolve_session_id_from_os(req.session_id.as_deref())
}

fn resolve_req(
    req: &ActuateRequest,
    space: Space,
) -> Result<crate::target::ResolvedTarget, HandsError> {
    let target = Target::parse(req.element_id.as_deref(), req.grid.as_deref(), req.x, req.y)?;
    target.resolve(space)
}

pub fn click(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    let session_id = session(&req);
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
    let session_id = session(&req);
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
    let mut rng = Rng::from_time();
    let foregrounded = foreground::offer(resolved.hwnd, (resolved.x, resolved.y));
    if let Err(err) = input::move_to(space, resolved.x, resolved.y, &mut rng) {
        return fail(session_id, info, err, foregrounded, false, false);
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
    if let Err(err) = lease::poll() {
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
    let session_id = session(&req);
    let info = none_target();
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
    if let Err(err) = ensure_dpi() {
        return fail(session_id, info, err, false, false, false);
    }
    match input::type_text(text) {
        Ok(_) => base(session_id, info, true, false, false, false, false, None),
        Err(err) => fail(session_id, info, err, false, false, false),
    }
}

pub fn key(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    let session_id = session(&req);
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
    if let Err(err) = ensure_dpi() {
        return fail(session_id, info, err, false, false, false);
    }
    match input::named_key(name) {
        Ok(()) => base(session_id, info, true, false, false, false, false, None),
        Err(err) => fail(session_id, info, err, false, false, false),
    }
}

pub fn scroll(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    let session_id = session(&req);
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
    if has_target {
        match hover(ActuateRequest {
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
    let session_id = session(&req);
    let space = match ensure_dpi().and_then(|_| virtual_screen()) {
        Ok(s) => s,
        Err(err) => return fail(session_id, none_target(), err, false, false, false),
    };
    let roi = match explicit_roi(&req) {
        Ok(Some(r)) => r,
        Ok(None) => {
            let cursor = input::cursor_pos().unwrap_or((0, 0));
            settle::default_roi(space, last_target(), cursor)
        }
        Err(err) => return fail(session_id, none_target(), err, false, false, false),
    };
    let info = ActuateTarget {
        kind: "roi".into(),
        id: None,
        x: roi.x,
        y: roi.y,
    };
    match settle::wait_settle(space, roi) {
        Ok((settled, _)) => base(session_id, info, true, false, false, settled, false, None),
        Err(err) => fail(session_id, info, err, false, false, false),
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

/// CLI `stop` with no live MCP lease is a documented no-op.
pub fn stop(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    let session_id = session(&req);
    lease::freeze_now();
    let frozen = lease::is_frozen();
    base(
        session_id,
        none_target(),
        true,
        frozen,
        false,
        false,
        false,
        if frozen {
            None
        } else {
            Some("no live MCP lease (CLI stop is a no-op after the process exits)".into())
        },
    )
}

/// CLI `stop` without installing hooks — documented no-op.
pub fn stop_cli_noop(req: ActuateRequest) -> Result<ActuateEnvelope, HandsError> {
    let session_id = session(&req);
    base(
        session_id,
        none_target(),
        true,
        false,
        false,
        false,
        false,
        Some("no live MCP lease; Pause/Break still works during a CLI input command".into()),
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
        };
        let err = finalize_envelope(env).expect_err("must not emit oversize");
        assert!(err.to_string().contains("16384"), "{err}");
    }
}
