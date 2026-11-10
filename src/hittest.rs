//! DOM hit-test / interactivity assessment (0128).
//!
//! The extension owns the DOM read (`elementFromPoint`, shadow piercing, ancestor-clip advice).
//! Rust owns transform truth: this module only consumes the 0129 [`PointContract`] and maps
//! through [`crate::point::map_point`]. The sole inverse arithmetic is
//! [`css_for_physical`], which round-trips through `map_point` and is never a second calibration.

use serde::{Deserialize, Serialize};

use crate::actuate::{ActuateEnvelope, ActuateTarget};
use crate::chrome;
use crate::error::HandsError;
use crate::extract::Detail;
use crate::foreground;
use crate::lease;
use crate::point::{
    CssPoint, CssRect, PhysicalPoint, PointContract, PointSource, TransformReport, map_point,
    map_rect,
};
use crate::target::{ResolvedTarget, Target, TargetOutcome};

pub const DEFAULT_SAMPLES: u32 = 25;
pub const MIN_SAMPLES: u32 = 5;
pub const MAX_SAMPLES: u32 = 49;
const POINT_TOLERANCE: f64 = 1e-9;
const REASON_CAP: usize = 120;
const TAG_CAP: usize = 16;
const ROLE_CAP: usize = 32;
const NAME_CAP: usize = 80;
const NOTES_CAP: usize = 3;
const BLOCKERS_CAP: usize = 3;
const SHADOW_PATH_CAP: usize = 8;

/// One assessment reply. Advisory diagnostics explain a miss; the centre hit is authoritative.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assessment {
    /// Some(true)=target/composed descendant at the selected point; Some(false)=something else;
    /// None=unknown (never a guess).
    pub interactable: Option<bool>,
    pub selected_point: CssPoint,
    pub target_rect_css: CssRect,
    #[serde(default)]
    pub visible_fraction: Option<f64>,
    #[serde(default)]
    pub onscreen_fraction: Option<f64>,
    #[serde(default)]
    pub samples: u32,
    #[serde(default)]
    pub samples_hit: u32,
    #[serde(default)]
    pub blocked_by: Vec<PointBlocker>,
    #[serde(default)]
    pub clipped_by: Option<PointBlocker>,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub reason: Option<String>,
    pub confidence: AssessmentConfidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observe_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<f64>,
}

impl Assessment {
    /// Honest unknown: null verdict, named reason, `confidence:"none"`.
    pub fn unknown(selected_point: CssPoint, target_rect_css: CssRect, reason: &str) -> Self {
        Self {
            interactable: None,
            selected_point,
            target_rect_css,
            visible_fraction: None,
            onscreen_fraction: None,
            samples: 0,
            samples_hit: 0,
            blocked_by: Vec::new(),
            clipped_by: None,
            notes: Vec::new(),
            reason: Some(bounded_reason(reason)),
            confidence: AssessmentConfidence::None,
            observe_id: None,
            elapsed_ms: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssessmentConfidence {
    High,
    Medium,
    Low,
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PointBlocker {
    pub kind: BlockerKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect_css: Option<CssRect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockerKind {
    Element,
    Viewport,
    Iframe,
    Shadow,
    Background,
}

/// Point-mode answer: what is under the physical point right now?
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtPoint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element: Option<PointBlocker>,
    #[serde(default)]
    pub shadow_path: Vec<String>,
    pub interactable: Option<bool>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub notes: Vec<String>,
    pub confidence: AssessmentConfidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<f64>,
    /// Chrome frame id when the point was composed through a linked iframe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HittestEnvelope {
    pub session_id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub point: Option<PointContract>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_rect: Option<crate::space::Rect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assessment: Option<Assessment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at_point: Option<AtPoint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct HittestRequest {
    pub session_id: Option<String>,
    pub element_id: Option<String>,
    pub grid: Option<String>,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub samples: Option<u32>,
}

impl HittestRequest {
    pub fn samples(&self) -> u32 {
        self.samples
            .unwrap_or(DEFAULT_SAMPLES)
            .clamp(MIN_SAMPLES, MAX_SAMPLES)
    }
}

/// One point equality: element mode must match `contract.css`; point mode must echo the request.
pub fn validate_assessment(
    contract: &PointContract,
    assessment: &Assessment,
) -> Result<(), &'static str> {
    let Some(css) = contract.css else {
        return Err("point_mismatch");
    };
    if !points_match(css, assessment.selected_point) {
        return Err("point_mismatch");
    }
    Ok(())
}

pub fn points_match(a: CssPoint, b: CssPoint) -> bool {
    (a.x - b.x).abs() <= POINT_TOLERANCE && (a.y - b.y).abs() <= POINT_TOLERANCE
}

/// Bound page-controlled text before it can reach stdout/MCP. Names are never logged.
pub fn bound_assessment(assessment: &mut Assessment) {
    assessment.reason = assessment.reason.as_deref().map(bounded_reason);
    assessment.blocked_by.truncate(BLOCKERS_CAP);
    for blocker in &mut assessment.blocked_by {
        bound_blocker(blocker);
    }
    if let Some(blocker) = assessment.clipped_by.as_mut() {
        bound_blocker(blocker);
    }
    assessment.notes.truncate(NOTES_CAP);
    for note in &mut assessment.notes {
        *note = bounded(note, ROLE_CAP);
    }
    // A sampled grid can exceed the exact area ratio at a viewport edge. The invariant is
    // `visible_fraction <= onscreen_fraction`; clamp rather than emit an impossible estimate.
    if let (Some(visible), Some(onscreen)) =
        (assessment.visible_fraction, assessment.onscreen_fraction)
    {
        let visible = if visible.is_finite() {
            visible.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let onscreen = if onscreen.is_finite() {
            onscreen.clamp(0.0, 1.0)
        } else {
            0.0
        };
        assessment.onscreen_fraction = Some(onscreen);
        assessment.visible_fraction = Some(visible.min(onscreen));
    }
}

pub fn bound_at_point(at_point: &mut AtPoint) {
    if let Some(element) = at_point.element.as_mut() {
        bound_blocker(element);
    }
    at_point.shadow_path.truncate(SHADOW_PATH_CAP);
    for tag in &mut at_point.shadow_path {
        *tag = bounded(tag, TAG_CAP);
    }
    at_point.reason = at_point.reason.as_deref().map(bounded_reason);
    at_point.notes.truncate(NOTES_CAP);
    for note in &mut at_point.notes {
        *note = bounded(note, ROLE_CAP);
    }
}

fn bound_blocker(blocker: &mut PointBlocker) {
    blocker.tag = blocker.tag.as_deref().map(|s| bounded(s, TAG_CAP));
    blocker.role = blocker.role.as_deref().map(|s| bounded(s, ROLE_CAP));
    blocker.name = blocker.name.as_deref().map(|s| bounded(s, NAME_CAP));
    blocker.reason = blocker.reason.as_deref().map(bounded_reason);
}

fn bounded(s: &str, cap: usize) -> String {
    s.chars().take(cap).collect()
}

fn bounded_reason(s: &str) -> String {
    bounded(s, REASON_CAP)
}

/// Build a valid-or-refusal assessment from an element-mode outcome.
pub fn envelope_for_outcome(outcome: TargetOutcome, detail: &str) -> HittestEnvelope {
    match outcome {
        TargetOutcome::Resolved(resolved) => {
            let contract = resolved.contract.clone().unwrap_or_else(|| PointContract {
                source: PointSource::Chr,
                css: resolved.css_point,
                physical: None,
                transform: TransformReport::invalid("no_transform", None),
            });
            let mut assessment = resolved.assessment.clone();
            let target_rect = resolved
                .css_rect
                .and_then(|css_rect| map_rect(&contract, css_rect).ok());
            assessment = Some(match assessment {
                None => {
                    let mut assessment = Assessment::unknown(
                        resolved.css_point.unwrap_or(CssPoint::new(0.0, 0.0)),
                        resolved.css_rect.unwrap_or(CssRect {
                            left: 0.0,
                            top: 0.0,
                            width: 0.0,
                            height: 0.0,
                        }),
                        "assessment_missing",
                    );
                    assessment.observe_id = contract.transform.observe_id.clone();
                    assessment
                }
                Some(mut assessment) => {
                    if let Err(reason) = validate_assessment(&contract, &assessment) {
                        let css = resolved.css_point.unwrap_or(assessment.selected_point);
                        let rect = resolved.css_rect.unwrap_or(assessment.target_rect_css);
                        assessment = Assessment::unknown(css, rect, reason);
                    }
                    assessment.observe_id = contract.transform.observe_id.clone();
                    bound_assessment(&mut assessment);
                    assessment
                }
            });
            HittestEnvelope {
                session_id: String::new(),
                ok: true,
                point: Some(contract),
                target_rect,
                detail: Some(detail.to_string()),
                assessment,
                at_point: None,
                error: None,
            }
        }
        TargetOutcome::Unresolved {
            contract,
            assessment,
            ..
        } => {
            let css = contract.css.unwrap_or(CssPoint::new(0.0, 0.0));
            let reason = format!(
                "invalid_transform: {}",
                contract
                    .transform
                    .reason
                    .clone()
                    .unwrap_or_else(|| "no_transform".into())
            );
            let rect = assessment
                .as_ref()
                .map(|a| a.target_rect_css)
                .unwrap_or(CssRect {
                    left: 0.0,
                    top: 0.0,
                    width: 0.0,
                    height: 0.0,
                });
            let mut assessment = unknown_from(assessment.map(|b| *b), css, rect, &reason);
            assessment.observe_id = contract.transform.observe_id.clone();
            HittestEnvelope {
                session_id: String::new(),
                ok: true,
                point: Some(*contract),
                target_rect: None,
                detail: Some(detail.to_string()),
                assessment: Some(assessment),
                at_point: None,
                error: None,
            }
        }
        TargetOutcome::Identity { report, .. } => HittestEnvelope {
            session_id: String::new(),
            ok: false,
            point: None,
            target_rect: None,
            detail: Some(detail.to_string()),
            assessment: None,
            at_point: None,
            error: Some(
                report
                    .reason
                    .clone()
                    .unwrap_or_else(|| report.state_token().to_string()),
            ),
        },
    }
}

fn unknown_from(
    previous: Option<Assessment>,
    css: CssPoint,
    rect: CssRect,
    reason: &str,
) -> Assessment {
    let mut assessment = previous.unwrap_or_else(|| Assessment::unknown(css, rect, reason));
    assessment.interactable = None;
    assessment.reason = Some(bounded_reason(reason));
    assessment.confidence = AssessmentConfidence::None;
    bound_assessment(&mut assessment);
    assessment
}

/// Run a `hittest` request. `ok:true` means the query ran; `interactable:null` is an answer.
pub fn run(req: HittestRequest) -> Result<HittestEnvelope, HandsError> {
    let session_id = crate::session::resolve_session_id_from_os(req.session_id.as_deref());
    let samples = req.samples();
    let target = Target::parse(req.element_id.as_deref(), req.grid.as_deref(), req.x, req.y)?;
    let mut envelope = match &target {
        Target::ElementId(id) => {
            if !id.starts_with("chr:") {
                return Err(HandsError::Target(
                    "interactable_unavailable: hittest element mode supports chr: or sref1:c only"
                        .into(),
                ));
            }
            run_element(&target, samples)?
        }
        Target::Scoped(crate::identity::SnapshotRef::Chrome { .. }) => {
            run_element(&target, samples)?
        }
        Target::Scoped(crate::identity::SnapshotRef::Uia { .. }) => {
            return Err(HandsError::Target(
                "interactable_unavailable: DOM hit-test is Chrome-only (sref1:u is UIA)".into(),
            ));
        }
        Target::Grid { .. } | Target::Pixel { .. } => run_point(&target, samples)?,
    };
    envelope.session_id = session_id;
    finalize(envelope)
}

fn run_element(target: &Target, samples: u32) -> Result<HittestEnvelope, HandsError> {
    let space = crate::space::ensure_dpi().and_then(|_| crate::space::virtual_screen())?;
    let hwnd = foreground::foreground_hwnd();
    let outcome = target.resolve_with_opts_samples(space, hwnd, true, Some(samples))?;
    Ok(envelope_for_outcome(outcome, chrome::current_pipe_detail()))
}

fn run_point(target: &Target, samples: u32) -> Result<HittestEnvelope, HandsError> {
    let space = crate::space::ensure_dpi().and_then(|_| crate::space::virtual_screen())?;
    let physical = match target {
        Target::Pixel { x, y } => {
            if !space.contains_point(*x, *y) {
                return Ok(point_unknown("outside_virtual_screen"));
            }
            PhysicalPoint { x: *x, y: *y }
        }
        Target::Grid { col, row } => {
            let rect = space.cell_rect(*col, *row);
            if rect.area() == 0 || !space.contains(rect) {
                return Ok(point_unknown("outside_virtual_screen"));
            }
            let (x, y) = rect.center();
            if !space.contains_point(x, y) {
                return Ok(point_unknown("outside_virtual_screen"));
            }
            PhysicalPoint { x, y }
        }
        Target::ElementId(_) | Target::Scoped(_) => {
            return Err(HandsError::Target(
                "interactable_unavailable: point mode takes --x/--y or --grid".into(),
            ));
        }
    };

    let Some(hwnd) = foreground::foreground_hwnd() else {
        return Ok(point_unknown("not_foreground"));
    };
    if !foreground::target_is_chrome(Some(hwnd)) {
        return Ok(point_unknown("not_chrome"));
    }
    if foreground::foreground_hwnd() != Some(hwnd) {
        return Ok(point_unknown("not_foreground"));
    }
    let window_before = foreground::window_rect(hwnd);
    let snapshot = chrome::try_snapshot_identity(Detail::Default, Some(hwnd));
    let mut envelope = match point_after_snapshot(snapshot, physical, samples) {
        Ok(envelope) => envelope,
        Err(err) => {
            return Ok(HittestEnvelope {
                session_id: String::new(),
                ok: false,
                point: None,
                target_rect: None,
                detail: None,
                assessment: None,
                at_point: None,
                error: Some(err.tool_message()),
            });
        }
    };
    if envelope.ok && envelope.at_point.is_some() {
        let window_after = foreground::window_rect(hwnd);
        if window_moved(window_before, window_after) {
            envelope.at_point = Some(unknown_at_point("window_moved"));
        }
    }
    Ok(envelope)
}

/// Point-mode half after the (non-storing) snapshot. Split out for golden/unit tests.
pub fn point_after_snapshot(
    snapshot: chrome::SnapshotOutcome,
    physical: PhysicalPoint,
    samples: u32,
) -> Result<HittestEnvelope, HandsError> {
    let map = match snapshot {
        chrome::SnapshotOutcome::Map(map) => *map,
        chrome::SnapshotOutcome::HostDown => {
            return Err(HandsError::Chrome(
                "Chrome host is not connected; hittest point mode needs a live host".into(),
            ));
        }
        chrome::SnapshotOutcome::Loading => return Ok(point_unknown("page_loading")),
        chrome::SnapshotOutcome::Timeout => return Ok(point_unknown("snapshot_timeout")),
    };
    let Some(report) = map.transform.clone() else {
        return Ok(point_unknown("invalid_transform: no_transform"));
    };
    if !report.valid {
        return Ok(point_unknown(&format!(
            "invalid_transform: {}",
            report.reason.as_deref().unwrap_or("no_transform")
        )));
    }
    let identity = map.identity.clone();
    let assess_v = identity.as_ref().and_then(|i| i.assess_v).unwrap_or(0);
    if assess_v < 1 {
        return Ok(point_unknown("assessment_unavailable"));
    }
    let (css, contract) = match css_for_physical(&report, physical) {
        Ok(v) => v,
        Err(reason) => return Ok(point_unknown(&reason)),
    };
    let tab = map.tab.unwrap_or(0);
    let reply = match chrome::assess_at(css, samples) {
        Ok(reply) => reply,
        Err(chrome::AssessError::HostDown(err)) => {
            return Err(HandsError::Chrome(format!(
                "Chrome host is not connected; hittest point mode needs a live host: {err}"
            )));
        }
        Err(chrome::AssessError::Timeout(_)) => return Ok(point_unknown("assessment_timeout")),
        Err(chrome::AssessError::Unavailable(_)) => {
            return Ok(point_unknown("assessment_unavailable"));
        }
    };
    if reply.at_point.reason.as_deref() == Some("cross_frame") {
        return point_from_frame(contract, css, identity.as_ref(), tab, reply, samples);
    }
    Ok(point_envelope_from_reply(
        contract,
        css,
        identity.as_ref(),
        reply,
    ))
}

fn frame_chain_rows(
    frame: u32,
    chain: &[crate::frames::FrameLink],
) -> Vec<crate::frames::RawFrameLink> {
    let mut rows = Vec::new();
    let mut current = frame;
    for link in chain {
        rows.push(crate::frames::RawFrameLink {
            frame: Some(current),
            status: Some("ok".into()),
            link: Some(link.clone()),
            ..crate::frames::RawFrameLink::default()
        });
        let Some(parent) = link.parent else {
            return Vec::new();
        };
        current = parent;
        if current == 0 {
            break;
        }
    }
    rows
}

/// Enter a linked iframe from a top `cross_frame` point reply: explicit `{tab, frameId}` assess,
/// compose the child's local point/rect through the fresh chain, and require the same one-point
/// equality as 0128. Any missing/ambiguous geometry is unknown, never a fallback.
fn point_from_frame(
    contract: PointContract,
    top_css: CssPoint,
    before: Option<&chrome::PageIdentity>,
    tab: u32,
    top_reply: chrome::AssessReply,
    samples: u32,
) -> Result<HittestEnvelope, HandsError> {
    if let Some(before) = before
        && !identity_matches(before, &top_reply.identity)
    {
        let reason = if before.url != top_reply.identity.url {
            "tab_changed"
        } else {
            "stale_page"
        };
        return Ok(point_reply_envelope(contract, unknown_at_point(reason)));
    }
    let Some(echo) = top_reply.css else {
        return Ok(point_reply_envelope(
            contract,
            unknown_at_point("point_mismatch"),
        ));
    };
    if !points_match(top_css, echo) {
        return Ok(point_reply_envelope(
            contract,
            unknown_at_point("point_mismatch"),
        ));
    }
    let (Some(frame), Some(local)) = (top_reply.iframe_frame, top_reply.local_css) else {
        // Keep the informative top `cross_frame` verdict (with the iframe blocker) when the
        // frame is unlinked; it is still fail-closed, not a guessed point.
        return Ok(point_reply_envelope(contract, top_reply.at_point));
    };
    if tab == 0 {
        return Ok(point_reply_envelope(
            contract,
            unknown_at_point("frame_geometry_unavailable"),
        ));
    }
    let child = match chrome::assess_at_frame(tab, frame, local, samples) {
        Ok(reply) => reply,
        Err(chrome::AssessError::HostDown(err)) => {
            return Err(HandsError::Chrome(format!(
                "Chrome host is not connected; frame point mode needs a live host: {err}"
            )));
        }
        Err(chrome::AssessError::Timeout(_)) => return Ok(point_unknown("assessment_timeout")),
        Err(chrome::AssessError::Unavailable(_)) => {
            return Ok(point_unknown("assessment_unavailable"));
        }
    };
    if child.frame != Some(frame) || child.top_metrics.is_none() {
        return Ok(point_reply_envelope(
            contract,
            unknown_at_point("frame_geometry_unavailable"),
        ));
    }
    let rows = frame_chain_rows(frame, &child.frame_chain);
    let composed = match crate::frames::compose_point(&rows, frame, local) {
        Ok(point) => point,
        Err(err) => {
            return Ok(point_reply_envelope(
                contract,
                unknown_at_point(err.token()),
            ));
        }
    };
    if (composed.x - top_css.x).abs() > 1.0 || (composed.y - top_css.y).abs() > 1.0 {
        return Ok(point_reply_envelope(
            contract,
            unknown_at_point("frame_geometry_unavailable"),
        ));
    }
    let mut at_point = child.at_point;
    bound_at_point(&mut at_point);
    at_point.frame = Some(frame);
    if let Some(element) = at_point.element.as_mut()
        && let Some(rect) = element.rect_css
    {
        element.rect_css = crate::frames::compose_rect(&rows, frame, rect).ok();
    }
    Ok(point_reply_envelope(contract, at_point))
}

/// Validate one point-mode reply. A missing/incorrect CSS echo fails closed; an unchanged
/// page identity is required before any element can be named.
fn point_envelope_from_reply(
    contract: PointContract,
    css: CssPoint,
    identity: Option<&chrome::PageIdentity>,
    reply: chrome::AssessReply,
) -> HittestEnvelope {
    if let Some(before) = identity
        && !identity_matches(before, &reply.identity)
    {
        let reason = if before.url != reply.identity.url {
            "tab_changed"
        } else {
            "stale_page"
        };
        return point_reply_envelope(contract, unknown_at_point(reason));
    }
    let Some(echo) = reply.css else {
        return point_reply_envelope(contract, unknown_at_point("point_mismatch"));
    };
    if !points_match(css, echo) {
        return point_reply_envelope(contract, unknown_at_point("point_mismatch"));
    }
    let mut at_point = reply.at_point;
    bound_at_point(&mut at_point);
    point_reply_envelope(contract, at_point)
}

fn point_reply_envelope(contract: PointContract, at_point: AtPoint) -> HittestEnvelope {
    HittestEnvelope {
        session_id: String::new(),
        ok: true,
        point: Some(contract),
        target_rect: None,
        detail: Some("default".into()),
        assessment: None,
        at_point: Some(at_point),
        error: None,
    }
}

fn point_unknown(reason: &str) -> HittestEnvelope {
    HittestEnvelope {
        session_id: String::new(),
        ok: true,
        point: None,
        target_rect: None,
        detail: None,
        assessment: None,
        at_point: Some(unknown_at_point(reason)),
        error: None,
    }
}

fn unknown_at_point(reason: &str) -> AtPoint {
    AtPoint {
        element: None,
        shadow_path: Vec::new(),
        interactable: None,
        reason: Some(bounded_reason(reason)),
        notes: Vec::new(),
        confidence: AssessmentConfidence::None,
        elapsed_ms: None,
        frame: None,
    }
}

/// Documented inverse of the same 0129 affine map. The only CSS<->physical arithmetic in 0128;
/// it round-trips through `map_point` within 1 physical px.
pub fn css_for_physical(
    report: &TransformReport,
    physical: PhysicalPoint,
) -> Result<(CssPoint, PointContract), String> {
    if !report.valid {
        return Err(format!(
            "invalid_transform: {}",
            report.reason.as_deref().unwrap_or("no_transform")
        ));
    }
    let scale = report
        .scale
        .filter(|s| s.is_finite() && *s > 0.0)
        .ok_or_else(|| "invalid_transform: bad_scale".to_string())?;
    let viewport = report
        .page_viewport
        .ok_or_else(|| "invalid_transform: no_viewport_anchor".to_string())?;
    let layout_w = viewport.w as f64 / scale;
    let layout_h = viewport.h as f64 / scale;
    let css = CssPoint {
        x: (f64::from(physical.x) - f64::from(viewport.x)) / scale,
        y: (f64::from(physical.y) - f64::from(viewport.y)) / scale,
    };
    if !css.x.is_finite() || !css.y.is_finite() {
        return Err("outside_page".into());
    }
    if css.x < 0.0 || css.y < 0.0 || css.x >= layout_w || css.y >= layout_h {
        return Err("outside_page".into());
    }
    let contract = PointContract {
        source: PointSource::Chr,
        css: Some(css),
        physical: Some(physical),
        transform: report.clone(),
    };
    let mapped = map_point(&contract, css).map_err(|err| match err {
        crate::point::MapError::OutOfViewport => "outside_page".to_string(),
        other => format!("invalid_transform: {other}"),
    })?;
    if (i64::from(mapped.x) - i64::from(physical.x)).abs() > 1
        || (i64::from(mapped.y) - i64::from(physical.y)).abs() > 1
    {
        return Err("point_rounding_mismatch".into());
    }
    Ok((css, contract))
}

/// Point-mode page identity: url/title and inner size/DPR must not have changed.
pub fn identity_matches(before: &chrome::PageIdentity, after: &chrome::PageIdentity) -> bool {
    before.url == after.url
        && before.title == after.title
        && dim_matches(before.inner_width, after.inner_width)
        && dim_matches(before.inner_height, after.inner_height)
        && dpr_matches(before.dpr, after.dpr)
}

fn dim_matches(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => (a - b).abs() <= 0.5,
        (None, None) => true,
        _ => false,
    }
}

fn dpr_matches(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => (a - b).abs() <= 1e-6,
        (None, None) => true,
        _ => false,
    }
}

pub fn window_moved(before: Option<crate::space::Rect>, after: Option<crate::space::Rect>) -> bool {
    match (before, after) {
        (Some(a), Some(b)) => a != b,
        (None, None) => false,
        _ => true,
    }
}

/// Opt-in `click --require-interactable` guard. Runs after the transform refusal and before the
/// fence; returns `Some(refusal)` or `None` when the target may proceed.
pub fn guard_require_interactable(
    session_id: &str,
    info: &ActuateTarget,
    resolved: &ResolvedTarget,
) -> Result<Option<ActuateEnvelope>, HandsError> {
    let is_chr = resolved.kind == "element"
        && resolved
            .id
            .as_deref()
            .is_some_and(|id| id.starts_with("chr:") || id.starts_with("sref1:c."));
    if !is_chr {
        return refusal(
            session_id,
            info,
            resolved.contract.clone(),
            resolved.assessment.clone(),
            None,
            "unavailable",
        )
        .map(Some);
    }
    let Some(contract) = resolved.contract.clone() else {
        return refusal(session_id, info, None, None, None, "missing").map(Some);
    };
    let Some(assessment) = resolved.assessment.clone() else {
        return refusal(session_id, info, Some(contract), None, None, "missing").map(Some);
    };
    if let Err(_reason) = validate_assessment(&contract, &assessment) {
        return refusal(
            session_id,
            info,
            Some(contract),
            Some(assessment),
            None,
            "mismatch",
        )
        .map(Some);
    }
    match assessment.interactable {
        Some(true) => Ok(None),
        Some(false) => refusal(
            session_id,
            info,
            Some(contract),
            Some(assessment),
            None,
            "blocked",
        )
        .map(Some),
        None => refusal(
            session_id,
            info,
            Some(contract),
            Some(assessment),
            None,
            "unknown",
        )
        .map(Some),
    }
}

/// Pre-dispatch refusal builder. `error` carries structural tokens only (never page text).
pub fn refusal(
    session_id: &str,
    info: &ActuateTarget,
    point: Option<PointContract>,
    assessment: Option<Assessment>,
    reason_override: Option<String>,
    kind: &str,
) -> Result<ActuateEnvelope, HandsError> {
    let reason = reason_override
        .or_else(|| assessment.as_ref().and_then(|a| a.reason.clone()))
        .unwrap_or_else(|| "blocked".into());
    let error = match kind {
        "blocked" => blocked_error(&reason, assessment.as_ref()),
        "unknown" => format!("interactable_unknown: {reason}"),
        "missing" => "assessment_missing: extension did not return an assessment".into(),
        "unavailable" => "interactable_unavailable: click on chr: or sref1:c only".into(),
        "mismatch" => "assessment_point_mismatch: selected_point != point.css".into(),
        other => format!("interactable_{other}: {reason}"),
    };
    let refusal = match kind {
        "blocked" => crate::outcome::RefusalToken::InteractableBlocked,
        "unknown" => crate::outcome::RefusalToken::InteractableUnknown,
        "unavailable" => crate::outcome::RefusalToken::InteractableUnavailable,
        _ => crate::outcome::RefusalToken::AssessmentUnavailable,
    };
    crate::actuate::finalize_envelope(ActuateEnvelope {
        session_id: session_id.to_string(),
        ok: false,
        frozen: lease::is_frozen(),
        target: info.clone(),
        retried: false,
        settled: false,
        foregrounded: false,
        error: Some(bounded_reason(&error)),
        fence: None,
        challenge: None,
        roi: None,
        miss: None,
        navigated: false,
        attempt: None,
        cooldown_ms: None,
        loop_suspected: false,
        guidance: None,
        point,
        verified: true,
        semantic: None,
        assessment,
        interactable_checked: Some(true),
        identity: None,
        post_dispatch: false,
        delivery: None,
        outcome: None,
        refusal: Some(refusal),
    })
}

fn blocked_error(reason: &str, _assessment: Option<&Assessment>) -> String {
    // Structural tokens only. Tag/role are page-controlled attribute values and are never
    // written to the error/JSONL (they remain capped on `assessment.blocked_by`).
    format!("interactable_blocked: {reason}")
}

pub fn finalize(mut envelope: HittestEnvelope) -> Result<HittestEnvelope, HandsError> {
    let json = serde_json::to_string(&envelope)
        .map_err(|err| HandsError::Chrome(format!("hittest envelope serialize: {err}")))?;
    if json.len() > crate::observe::DEFAULT_ENVELOPE_MAX_BYTES {
        // Shrink advisory blockers before ever failing the query.
        if let Some(assessment) = envelope.assessment.as_mut() {
            assessment.blocked_by.clear();
            assessment.clipped_by = None;
            assessment.notes.clear();
        }
        if let Some(at_point) = envelope.at_point.as_mut() {
            at_point.notes.clear();
            at_point.shadow_path.clear();
        }
    }
    let json = serde_json::to_string(&envelope)
        .map_err(|err| HandsError::Chrome(format!("hittest envelope serialize: {err}")))?;
    if json.len() > crate::observe::ENVELOPE_MAX_BYTES {
        return Err(HandsError::Chrome(format!(
            "hittest envelope is {} bytes (hard max {})",
            json.len(),
            crate::observe::ENVELOPE_MAX_BYTES
        )));
    }
    Ok(envelope)
}

pub fn serialize(envelope: &HittestEnvelope) -> Result<String, HandsError> {
    serde_json::to_string(envelope)
        .map_err(|err| HandsError::Chrome(format!("hittest envelope serialize: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actuate::{ActuateEnvelope, serialize_envelope};
    use crate::extract::ListingMeta;
    use crate::identity::IdentityState;
    use crate::point::{TransformConfidence, TransformMethod};
    use crate::space::Rect;

    fn reply(name: &str) -> String {
        let path = format!(
            "{}/tests/fixtures/hittest/replies/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{path}: {err}"))
    }

    fn assessment(name: &str) -> Assessment {
        let mut a: Assessment = serde_json::from_str(&reply(name)).expect("assessment json");
        bound_assessment(&mut a);
        a
    }

    fn at_point(name: &str) -> AtPoint {
        serde_json::from_str(&reply(name)).expect("at_point json")
    }

    fn valid_report(scale: f64, viewport: Rect) -> TransformReport {
        TransformReport {
            valid: true,
            method: TransformMethod::RenderWidgetHost,
            confidence: TransformConfidence::High,
            scale: Some(scale),
            dpr: Some(scale),
            dpi: Some((96.0 * scale).round() as u32),
            page_zoom: Some(1.0),
            page_viewport: Some(viewport),
            anchor_basis: Some(crate::point::CssBasis::Client),
            layout_basis: Some(crate::point::CssBasis::Client),
            reason: None,
            observe_id: Some("f3a9c1d2e4b5a607".into()),
        }
    }

    fn contract(css: Option<CssPoint>, report: TransformReport) -> PointContract {
        let mut contract = PointContract {
            source: PointSource::Chr,
            css,
            physical: None,
            transform: report,
        };
        if let Some(css) = css {
            contract.physical = map_point(&contract, css).ok();
        }
        contract
    }

    fn zero_rect() -> CssRect {
        CssRect {
            left: 0.0,
            top: 0.0,
            width: 0.0,
            height: 0.0,
        }
    }

    // ---------------------------------------------------------------- reply shapes

    #[test]
    fn golden_geometry_is_consistent() {
        for name in [
            "unobstructed.json",
            "sticky-header.json",
            "modal-backdrop.json",
            "cookie-banner.json",
            "pointer-events-none.json",
            "clipped-container.json",
            "transformed-ancestor.json",
            "open-shadow-overlay.json",
            "closed-shadow.json",
            "closed-shadow-uninspectable.json",
            "partial-offscreen.json",
            "aria-disabled-inert.json",
            "nested-scroll.json",
            "iframe-overlay.json",
            "small-target.json",
            "fixed-in-overflow.json",
            "absolute-above-clipper.json",
            "body-overflow.json",
            "light-dom-custom-element.json",
        ] {
            let a = assessment(name);
            assert!(
                points_match(a.selected_point, a.target_rect_css.center()),
                "{name}: selected_point must be the target rect centre"
            );
            if let (Some(visible), Some(onscreen)) = (a.visible_fraction, a.onscreen_fraction) {
                assert!(
                    visible <= onscreen + 1e-9,
                    "{name}: visible {visible} > onscreen {onscreen}"
                );
            }
            if a.samples_hit < a.samples && a.interactable == Some(false) {
                assert!(
                    !a.blocked_by.is_empty(),
                    "{name}: a blocked verdict with misses must name a blocker"
                );
            }
        }
    }

    #[test]
    fn assessment_parses_blocked_and_unobstructed_replies() {
        let blocked = assessment("sticky-header.json");
        assert_eq!(blocked.interactable, Some(false));
        assert_eq!(blocked.reason.as_deref(), Some("blocked"));
        assert_eq!(blocked.blocked_by[0].kind, BlockerKind::Element);
        assert_eq!(blocked.blocked_by[0].tag.as_deref(), Some("header"));

        let open = assessment("unobstructed.json");
        assert_eq!(open.interactable, Some(true));
        assert!(open.reason.is_none());
        assert!(open.blocked_by.is_empty());
    }

    #[test]
    fn unobstructed_reply_passes() {
        let a = assessment("unobstructed.json");
        assert_eq!(a.interactable, Some(true));
        assert_eq!(a.confidence, AssessmentConfidence::High);
        assert!(a.visible_fraction.unwrap_or(0.0) >= 0.99);
    }

    #[test]
    fn sticky_reply_blocks() {
        let a = assessment("sticky-header.json");
        assert_eq!(a.interactable, Some(false));
        assert_eq!(a.blocked_by[0].tag.as_deref(), Some("header"));
    }

    #[test]
    fn modal_reply_blocks() {
        let a = assessment("modal-backdrop.json");
        assert_eq!(a.interactable, Some(false));
        assert_eq!(a.blocked_by[0].role.as_deref(), Some("dialog"));
    }

    #[test]
    fn cookie_reply_blocks() {
        let a = assessment("cookie-banner.json");
        assert_eq!(a.interactable, Some(false));
        assert_eq!(a.blocked_by[0].role.as_deref(), Some("dialog"));
    }

    #[test]
    fn pointer_events_none_overlay_is_not_a_blocker() {
        let a = assessment("pointer-events-none.json");
        assert_eq!(a.interactable, Some(true));
        assert!(a.blocked_by.is_empty());
        assert!(!a.notes.iter().any(|n| n == "shadow_closed"));
    }

    #[test]
    fn describe_filters_pointer_events_none() {
        let js = include_str!("../extension/content.js");
        assert!(
            js.contains("pointerEvents"),
            "describe must read pointerEvents"
        );
        assert!(
            js.contains("=== \"none\""),
            "describe must drop pointer-events:none candidates"
        );
        let a = assessment("pointer-events-none.json");
        assert!(a.blocked_by.iter().all(|b| b.tag.as_deref() != Some("div")));
    }

    #[test]
    fn clipped_reply_names_ancestor() {
        let a = assessment("clipped-container.json");
        assert_eq!(a.interactable, Some(true));
        assert_eq!(a.selected_point, CssPoint::new(201.0, 61.0));
        assert_eq!(
            a.target_rect_css,
            CssRect {
                left: 101.0,
                top: 41.0,
                width: 200.0,
                height: 40.0,
            }
        );
        let clipped = a.clipped_by.expect("clipped_by");
        assert_eq!(clipped.tag.as_deref(), Some("section"));
        assert_eq!(clipped.name.as_deref(), Some("Sign in"));
        assert_eq!(clipped.role, None);
        assert_eq!(
            clipped.rect_css,
            Some(CssRect {
                left: 100.0,
                top: 40.0,
                width: 122.0,
                height: 62.0,
            })
        );
        assert_eq!(a.blocked_by[0].tag.as_deref(), Some("html"));
        assert!(a.visible_fraction.unwrap_or(1.0) < a.onscreen_fraction.unwrap_or(1.0));
    }

    #[test]
    fn nested_scroll_centre_outside_clip_is_false() {
        let a = assessment("nested-scroll.json");
        assert_eq!(a.interactable, Some(false));
        assert_eq!(a.reason.as_deref(), Some("clipped_out"));
        assert_eq!(a.blocked_by[0].tag.as_deref(), Some("html"));
        let clipped = a.clipped_by.expect("clipped_by");
        assert_eq!(clipped.tag.as_deref(), Some("div"));
        assert_eq!(clipped.name.as_deref(), Some("Sign in"));
        assert_eq!(clipped.role, None);
        assert_eq!(
            clipped.rect_css,
            Some(CssRect {
                left: 80.0,
                top: 40.0,
                width: 240.0,
                height: 120.0,
            })
        );
    }

    #[test]
    fn transformed_reply_passes() {
        let a = assessment("transformed-ancestor.json");
        assert_eq!(a.interactable, Some(true));
        assert_eq!(a.selected_point, CssPoint::new(220.0, 80.0));
        assert_eq!(
            a.target_rect_css,
            CssRect {
                left: 120.0,
                top: 60.0,
                width: 200.0,
                height: 40.0,
            }
        );
    }

    #[test]
    fn open_shadow_overlay_blocks() {
        let a = assessment("open-shadow-overlay.json");
        assert_eq!(a.interactable, Some(false));
        assert_eq!(a.blocked_by[0].tag.as_deref(), Some("div"));
        assert_eq!(a.blocked_by[0].role.as_deref(), Some("dialog"));
        assert_eq!(a.confidence, AssessmentConfidence::High);
    }

    #[test]
    fn closed_shadow_element_false() {
        // Chrome 88+ exposes chrome.dom, so the closed root is pierceable; the inner overlay is
        // named and the `shadow_pierced_closed` note lowers confidence to medium.
        let a = assessment("closed-shadow.json");
        assert_eq!(a.interactable, Some(false));
        assert_eq!(a.blocked_by[0].kind, BlockerKind::Element);
        assert_eq!(a.reason.as_deref(), Some("blocked"));
        assert!(a.notes.iter().any(|n| n == "shadow_pierced_closed"));
        assert_eq!(a.confidence, AssessmentConfidence::Medium);
    }

    #[test]
    fn closed_shadow_uninspectable_is_shadow_closed() {
        // Hypothetical branch for a host whose closed root cannot be inspected (no chrome.dom).
        let a = assessment("closed-shadow-uninspectable.json");
        assert_eq!(a.interactable, Some(false));
        assert_eq!(a.blocked_by[0].kind, BlockerKind::Shadow);
        assert_eq!(a.reason.as_deref(), Some("shadow_closed"));
        assert_eq!(a.confidence, AssessmentConfidence::Low);
    }

    #[test]
    fn closed_shadow_point_unknown() {
        let a = at_point("point-closed-shadow.json");
        assert_eq!(a.interactable, None);
        assert_eq!(a.reason.as_deref(), Some("shadow_closed"));
    }

    #[test]
    fn light_dom_custom_element_is_not_shadow_closed() {
        let a = assessment("light-dom-custom-element.json");
        assert_eq!(a.interactable, Some(true));
        assert!(!a.notes.iter().any(|n| n == "shadow_closed"));
        assert!(a.blocked_by.is_empty());
    }

    #[test]
    fn fixed_in_overflow_is_interactable() {
        assert_eq!(
            assessment("fixed-in-overflow.json").interactable,
            Some(true)
        );
    }

    #[test]
    fn absolute_above_clipper_is_interactable() {
        assert_eq!(
            assessment("absolute-above-clipper.json").interactable,
            Some(true)
        );
    }

    #[test]
    fn body_overflow_positive_control() {
        let a = assessment("body-overflow.json");
        assert_eq!(a.interactable, Some(true));
        assert!(a.notes.iter().any(|n| n == "containing_block_skip"));
    }

    #[test]
    fn centre_hit_overrides_clip_model() {
        let a = assessment("clipped-container.json");
        assert_eq!(a.interactable, Some(true));
        let clipped = a.clipped_by.expect("advisory clip");
        assert_eq!(clipped.tag.as_deref(), Some("section"));
    }

    #[test]
    fn iframe_overlay_is_opaque() {
        let a = assessment("iframe-overlay.json");
        assert_eq!(a.interactable, Some(false));
        assert_eq!(a.reason.as_deref(), Some("cross_frame"));
        assert_eq!(a.blocked_by[0].kind, BlockerKind::Iframe);
        assert_eq!(a.blocked_by[0].name.as_deref(), Some("Opaque frame"));
    }

    #[test]
    fn point_mode_iframe_names_cross_frame() {
        let a = at_point("point-iframe.json");
        assert_eq!(a.interactable, None);
        assert_eq!(a.reason.as_deref(), Some("cross_frame"));
    }

    #[test]
    fn partial_offscreen_fraction() {
        let a = assessment("partial-offscreen.json");
        assert!(a.onscreen_fraction.unwrap_or(1.0) < 1.0);
        assert!(a.visible_fraction.unwrap_or(1.0) <= a.onscreen_fraction.unwrap_or(0.0));
    }

    #[test]
    fn aria_disabled_is_a_note_not_a_verdict() {
        let a = assessment("aria-disabled-inert.json");
        assert_eq!(a.interactable, Some(true));
        assert!(a.notes.iter().any(|n| n == "aria_disabled"));
        // Any advisory note lowers confidence from high to medium.
        assert_eq!(a.confidence, AssessmentConfidence::Medium);
    }

    #[test]
    fn inert_reply() {
        let a = assessment("aria-disabled-inert.json");
        assert_eq!(a.interactable, Some(true));
        assert!(a.notes.iter().any(|n| n == "aria_disabled"));
        // The `inert` div in this fixture is a sibling, not an ancestor; no inert note is
        // invented. An inert ancestor would be excluded from hit testing entirely.
        assert!(!a.notes.iter().any(|n| n == "inert"));
    }

    #[test]
    fn small_target_confidence() {
        let a = assessment("small-target.json");
        assert_eq!(a.interactable, Some(true));
        assert_eq!(a.confidence, AssessmentConfidence::High);
    }

    #[test]
    fn point_mode_topmost_element_and_shadow_path() {
        let a = at_point("point-topmost.json");
        assert_eq!(a.interactable, Some(true));
        let element = a.element.unwrap();
        assert_eq!(element.tag.as_deref(), Some("button"));
        assert_eq!(element.role.as_deref(), Some("button"));
        assert_eq!(a.shadow_path, vec!["app-shell"]);
        assert_eq!(a.confidence, AssessmentConfidence::Medium);
    }

    #[test]
    fn point_mode_body_is_not_interactive() {
        let a = at_point("point-body.json");
        assert_eq!(a.interactable, None);
        assert_eq!(a.reason.as_deref(), Some("not_interactive"));
        assert!(a.element.is_none());
    }

    #[test]
    fn fixture_golden_table_is_documented() {
        let readme = include_str!("../tests/fixtures/hittest/README.md");
        for name in [
            "clipped-container.json",
            "transformed-ancestor.json",
            "aria-disabled-inert.json",
            "iframe-overlay.json",
            "point-topmost.json",
            "open-shadow-pointer-events-none.json",
        ] {
            assert!(readme.contains(name), "README missing {name}");
        }
    }

    #[test]
    fn point_topmost_fixture_declares_role_and_shadow() {
        let html = include_str!("../tests/fixtures/hittest/point-topmost.html");
        assert!(html.contains("attachShadow"));
        assert!(html.contains("role"));
        assert!(html.contains("button"));
        assert!(html.contains("aria-label"));
        let a = at_point("point-topmost.json");
        assert_eq!(a.element.unwrap().role.as_deref(), Some("button"));
        assert_eq!(a.shadow_path, vec!["app-shell"]);
        assert_eq!(a.confidence, AssessmentConfidence::Medium);
    }

    #[test]
    fn point_mode_requires_interactive_hit() {
        let js = include_str!("../extension/content.js");
        assert!(js.contains("function isInteractiveHit"));
        assert!(js.contains("not_interactive"));
        let a = at_point("point-body.json");
        assert_eq!(a.interactable, None);
        assert_eq!(a.reason.as_deref(), Some("not_interactive"));
    }

    #[test]
    fn point_mode_shadow_pointer_events_none_keeps_host_hit() {
        // The host is the target and covers the point; a pointer-events:none shadow inner does
        // not change what receives the press, so the document-level host hit is retained.
        let a = at_point("open-shadow-pointer-events-none.json");
        assert_eq!(a.interactable, Some(true));
        assert_eq!(a.reason, None);
        assert_eq!(a.element.unwrap().tag.as_deref(), Some("div"));
        let js = include_str!("../extension/content.js");
        assert!(
            js.contains("shadowPointerEventsNone"),
            "pierce must detect a pointer-events:none inner hit"
        );
    }

    #[test]
    fn visible_fraction_is_clamped_to_onscreen() {
        let mut a = Assessment {
            interactable: None,
            selected_point: CssPoint::new(200.0, 70.0),
            target_rect_css: zero_rect(),
            visible_fraction: Some(0.9),
            onscreen_fraction: Some(0.5),
            samples: 25,
            samples_hit: 22,
            blocked_by: Vec::new(),
            clipped_by: None,
            notes: Vec::new(),
            reason: Some("assessment_timeout".into()),
            confidence: AssessmentConfidence::None,
            observe_id: None,
            elapsed_ms: None,
        };
        bound_assessment(&mut a);
        assert_eq!(a.visible_fraction, Some(0.5));
        assert_eq!(a.onscreen_fraction, Some(0.5));
        let js = include_str!("../extension/content.js");
        assert!(
            js.contains("Math.min(visibleBase, onscreen)"),
            "JS must clamp the sampled fraction to the area fraction"
        );
    }

    #[test]
    fn visible_fraction_le_onscreen_fraction() {
        for name in [
            "unobstructed.json",
            "sticky-header.json",
            "clipped-container.json",
            "partial-offscreen.json",
        ] {
            let a = assessment(name);
            if let (Some(v), Some(o)) = (a.visible_fraction, a.onscreen_fraction) {
                assert!(v <= o + 1e-9, "{name}: {v} > {o}");
            }
        }
    }

    #[test]
    fn false_is_never_paired_with_confidence_none() {
        for name in [
            "sticky-header.json",
            "modal-backdrop.json",
            "cookie-banner.json",
            "closed-shadow.json",
            "iframe-overlay.json",
        ] {
            let a = assessment(name);
            assert_eq!(a.interactable, Some(false), "{name}");
            assert_ne!(a.confidence, AssessmentConfidence::None, "{name}");
        }
    }

    #[test]
    fn unknown_is_null_not_false() {
        let unknown =
            Assessment::unknown(CssPoint::new(1.0, 2.0), zero_rect(), "assessment_timeout");
        assert_eq!(unknown.interactable, None);
        assert_eq!(unknown.confidence, AssessmentConfidence::None);
        assert_eq!(unknown.reason.as_deref(), Some("assessment_timeout"));
    }

    // ---------------------------------------------------------------- one point / bounds

    #[test]
    fn assessor_same_point_from_payload_rect() {
        // Real resolve reply shape: derive the selected point from the rectCss payload with the
        // JS formula (`left + width/2`, `top + height/2`) and validate the reply against a
        // contract built from that same payload.
        let value: serde_json::Value =
            serde_json::from_str(&reply("resolve-assessed.json")).expect("resolve reply");
        let rect: CssRect = serde_json::from_value(value["rectCss"].clone()).expect("rectCss");
        let mut a: Assessment =
            serde_json::from_value(value["assessment"].clone()).expect("assessment");
        let expected = CssPoint::new(rect.left + rect.width / 2.0, rect.top + rect.height / 2.0);
        assert_eq!(a.selected_point, expected);
        let report = valid_report(
            1.0,
            Rect {
                x: 0,
                y: 0,
                w: 1200,
                h: 900,
            },
        );
        let contract = contract(Some(expected), report);
        assert_eq!(
            contract.physical,
            Some(map_point(&contract, expected).unwrap())
        );
        validate_assessment(&contract, &a).expect("same point");
        // A half-pixel offset is a real mismatch, not a tautology.
        a.selected_point.x += 0.5;
        assert_eq!(validate_assessment(&contract, &a), Err("point_mismatch"));
    }

    #[test]
    fn assessment_point_mismatch_refuses() {
        let report = valid_report(
            1.0,
            Rect {
                x: 100,
                y: 80,
                w: 1200,
                h: 900,
            },
        );
        let css = CssPoint::new(10.0, 20.0);
        let contract = contract(Some(css), report);
        let mut a = Assessment::unknown(css, zero_rect(), "blocked");
        a.selected_point = CssPoint::new(10.001, 20.0);
        assert_eq!(validate_assessment(&contract, &a), Err("point_mismatch"));
        let refused = unknown_from(Some(a), css, zero_rect(), "point_mismatch");
        assert_eq!(refused.interactable, None);
        assert_eq!(refused.reason.as_deref(), Some("point_mismatch"));
    }

    #[test]
    fn contract_css_none_refuses() {
        let report = valid_report(
            1.0,
            Rect {
                x: 0,
                y: 0,
                w: 1200,
                h: 900,
            },
        );
        let contract = contract(None, report);
        let a = Assessment::unknown(CssPoint::new(1.0, 1.0), zero_rect(), "blocked");
        assert_eq!(validate_assessment(&contract, &a), Err("point_mismatch"));
    }

    #[test]
    fn bounds_cap_names_and_blockers() {
        let mut a = Assessment {
            interactable: Some(false),
            selected_point: CssPoint::new(1.0, 1.0),
            target_rect_css: zero_rect(),
            visible_fraction: Some(0.0),
            onscreen_fraction: Some(1.0),
            samples: 25,
            samples_hit: 0,
            blocked_by: (0..4)
                .map(|i| PointBlocker {
                    kind: BlockerKind::Element,
                    tag: Some("x".repeat(40)),
                    role: Some(format!("role{i}{}", "y".repeat(40))),
                    name: Some("n".repeat(200)),
                    rect_css: Some(zero_rect()),
                    reason: Some("r".repeat(200)),
                })
                .collect(),
            clipped_by: None,
            notes: vec!["n".repeat(40); 5],
            reason: Some("r".repeat(400)),
            confidence: AssessmentConfidence::High,
            observe_id: None,
            elapsed_ms: None,
        };
        bound_assessment(&mut a);
        assert_eq!(a.blocked_by.len(), BLOCKERS_CAP);
        assert_eq!(
            a.blocked_by[0].tag.as_deref().unwrap().chars().count(),
            TAG_CAP
        );
        assert_eq!(
            a.blocked_by[0].role.as_deref().unwrap().chars().count(),
            ROLE_CAP
        );
        assert_eq!(
            a.blocked_by[0].name.as_deref().unwrap().chars().count(),
            NAME_CAP
        );
        assert_eq!(a.notes.len(), NOTES_CAP);
        assert_eq!(a.reason.as_deref().unwrap().chars().count(), REASON_CAP);
    }

    #[test]
    fn rust_overrides_js_verdict_on_invalid_transform() {
        let contract = PointContract {
            source: PointSource::Chr,
            css: Some(CssPoint::new(1.0, 2.0)),
            physical: None,
            transform: TransformReport::invalid("no_viewport_anchor", None),
        };
        let mut js = Assessment::unknown(CssPoint::new(1.0, 2.0), zero_rect(), "blocked");
        js.interactable = Some(true);
        let envelope = envelope_for_outcome(
            TargetOutcome::Unresolved {
                target: Target::ElementId("chr:0".into()),
                contract: Box::new(contract),
                hwnd: None,
                assessment: Some(Box::new(js)),
                identity: None,
            },
            "default",
        );
        let a = envelope.assessment.expect("assessment");
        assert_eq!(a.interactable, None);
        assert!(
            a.reason
                .as_deref()
                .unwrap_or_default()
                .starts_with("invalid_transform: no_viewport_anchor")
        );
        assert_eq!(a.confidence, AssessmentConfidence::None);
    }

    #[test]
    fn hittest_and_click_agree_on_off_viewport_centre() {
        let _g = crate::chrome::EnvGuard::lock();
        let contract = PointContract {
            source: PointSource::Chr,
            css: Some(CssPoint::new(10.0, 20.0)),
            physical: None,
            transform: TransformReport::invalid(
                "point_outside_viewport: CSS point is outside the layout viewport",
                None,
            ),
        };
        let hit = chrome::ChromeHit {
            id: "chr:0".into(),
            name: String::new(),
            role: "Button".into(),
            rect: Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
            css_rect: CssRect {
                left: 0.0,
                top: 0.0,
                width: 100.0,
                height: 40.0,
            },
            css_point: CssPoint::new(50.0, 20.0),
            contract,
            hwnd: Some(0x1234),
            href: None,
            map: None,
            assessment: None,
            capability: None,
            identity: None,
        };
        chrome::set_resolve_stub(Some(hit));
        crate::elevation::set_caller_rid_hook(Some(|| 0x2000));
        crate::elevation::set_high_il_hook(Some(|_| false));
        crate::foreground::set_foreground_hwnd_hook(Some(|| Some(0x1234)));
        let hittest_env = run(HittestRequest {
            session_id: Some("s-0128-agree".into()),
            element_id: Some("chr:0".into()),
            ..HittestRequest::default()
        })
        .expect("hittest");
        let click_env = crate::actuate::click(crate::actuate::ActuateRequest {
            session_id: Some("s-0128-agree".into()),
            element_id: Some("chr:0".into()),
            ..crate::actuate::ActuateRequest::default()
        })
        .expect("click");
        chrome::set_resolve_stub(None);
        crate::elevation::set_caller_rid_hook(None);
        crate::elevation::set_high_il_hook(None);
        crate::foreground::set_foreground_hwnd_hook(None);
        let a = hittest_env.assessment.expect("assessment");
        assert_eq!(a.interactable, None);
        let reason = a.reason.unwrap_or_default();
        assert!(reason.starts_with("invalid_transform:"));
        assert!(reason.contains("point_outside_viewport"));
        assert!(!click_env.ok, "{click_env:?}");
        assert!(
            click_env
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("point_outside_viewport"),
            "{click_env:?}"
        );
    }

    #[test]
    fn hittest_element_mode_includes_target_rect() {
        let report = valid_report(
            1.25,
            Rect {
                x: 100,
                y: 80,
                w: 1500,
                h: 1125,
            },
        );
        let css = CssPoint::new(200.0, 70.0);
        let contract = contract(Some(css), report);
        let css_rect = CssRect {
            left: 100.0,
            top: 50.0,
            width: 200.0,
            height: 40.0,
        };
        let expected = map_rect(&contract, css_rect).unwrap();
        let resolved = ResolvedTarget {
            target: Target::ElementId("chr:12".into()),
            kind: "element",
            id: Some("chr:12".into()),
            x: expected.center().0,
            y: expected.center().1,
            rect: expected,
            hwnd: None,
            name: "Sign in".into(),
            role: "Button".into(),
            css_point: Some(css),
            css_rect: Some(css_rect),
            contract: Some(contract),
            map: None,
            assessment: Some(Assessment {
                interactable: Some(true),
                selected_point: css,
                target_rect_css: css_rect,
                visible_fraction: Some(1.0),
                onscreen_fraction: Some(1.0),
                samples: 25,
                samples_hit: 25,
                blocked_by: Vec::new(),
                clipped_by: None,
                notes: Vec::new(),
                reason: None,
                confidence: AssessmentConfidence::High,
                observe_id: None,
                elapsed_ms: None,
            }),
            identity: None,
            uia_runtime_id: None,
            uia_expected_fp: None,
        };
        let envelope = envelope_for_outcome(TargetOutcome::Resolved(Box::new(resolved)), "default");
        assert_eq!(envelope.target_rect, Some(expected));
        assert_eq!(envelope.detail.as_deref(), Some("default"));
    }

    #[test]
    fn assessment_missing_carries_observe_id() {
        let mut resolved = blocked_resolved();
        resolved.assessment = None;
        let expected = resolved
            .contract
            .as_ref()
            .and_then(|c| c.transform.observe_id.clone());
        let envelope = envelope_for_outcome(TargetOutcome::Resolved(Box::new(resolved)), "default");
        let a = envelope.assessment.expect("assessment");
        assert_eq!(a.reason.as_deref(), Some("assessment_missing"));
        assert_eq!(a.observe_id, expected);
    }

    // ---------------------------------------------------------------- inverse / point mode

    #[test]
    fn inverse_round_trip() {
        for (scale, vx, vy) in [
            (1.0, 0, 0),
            (1.25, 100, 80),
            (1.5, -1920, -200),
            (2.0, 40, 60),
        ] {
            let report = valid_report(
                scale,
                Rect {
                    x: vx,
                    y: vy,
                    w: (1200.0 * scale).round() as i32,
                    h: (900.0 * scale).round() as i32,
                },
            );
            for (px, py) in [(vx + 10, vy + 20), (vx + 800, vy + 600)] {
                let physical = PhysicalPoint { x: px, y: py };
                let (css, contract) = css_for_physical(&report, physical).expect("inverse");
                let mapped = map_point(&contract, css).expect("forward");
                assert!((mapped.x - physical.x).abs() <= 1, "{scale}: {mapped:?}");
                assert!((mapped.y - physical.y).abs() <= 1, "{scale}: {mapped:?}");
            }
        }
    }

    #[test]
    fn point_mode_invalid_transform_is_unknown() {
        let envelope = point_after_snapshot(
            chrome::SnapshotOutcome::Map(Box::new(test_map(
                Some(TransformReport::invalid("no_viewport_anchor", None)),
                None,
            ))),
            PhysicalPoint { x: 10, y: 20 },
            25,
        )
        .expect("query");
        let at = envelope.at_point.expect("at_point");
        assert_eq!(at.interactable, None);
        assert!(
            at.reason
                .as_deref()
                .unwrap_or_default()
                .starts_with("invalid_transform: no_viewport_anchor")
        );
        assert!(envelope.point.is_none());
    }

    #[test]
    fn point_mode_outside_page_is_unknown() {
        let report = valid_report(
            1.0,
            Rect {
                x: 0,
                y: 0,
                w: 1200,
                h: 900,
            },
        );
        assert_eq!(
            css_for_physical(&report, PhysicalPoint { x: -1, y: 10 }),
            Err("outside_page".into())
        );
        assert_eq!(
            css_for_physical(&report, PhysicalPoint { x: 1300, y: 10 }),
            Err("outside_page".into())
        );
    }

    #[test]
    fn three_blocker_envelope_fits_4kib() {
        let mut a = Assessment {
            interactable: Some(false),
            selected_point: CssPoint::new(200.0, 70.0),
            target_rect_css: CssRect {
                left: 100.0,
                top: 50.0,
                width: 200.0,
                height: 40.0,
            },
            visible_fraction: Some(0.0),
            onscreen_fraction: Some(1.0),
            samples: 25,
            samples_hit: 0,
            blocked_by: (0..3)
                .map(|i| PointBlocker {
                    kind: BlockerKind::Element,
                    tag: Some(format!("tag{i}")),
                    role: Some("dialog".into()),
                    name: Some("n".repeat(80)),
                    rect_css: Some(CssRect {
                        left: 0.0,
                        top: 0.0,
                        width: 1200.0,
                        height: 800.0,
                    }),
                    reason: Some("overlay".into()),
                })
                .collect(),
            clipped_by: None,
            notes: vec!["aria_disabled".into(), "containing_block_skip".into()],
            reason: Some("blocked".into()),
            confidence: AssessmentConfidence::High,
            observe_id: Some("f3a9c1d2e4b5a607".into()),
            elapsed_ms: Some(2.0),
        };
        bound_assessment(&mut a);
        let envelope = finalize(HittestEnvelope {
            session_id: "s-4k".into(),
            ok: true,
            point: None,
            target_rect: None,
            detail: Some("default".into()),
            assessment: Some(a),
            at_point: None,
            error: None,
        })
        .expect("4k envelope");
        let json = serialize(&envelope).expect("json");
        assert!(json.len() <= 4096, "{} bytes", json.len());
    }

    #[test]
    fn point_mode_missing_css_echo_is_mismatch() {
        let report = valid_report(
            1.0,
            Rect {
                x: 0,
                y: 0,
                w: 1200,
                h: 900,
            },
        );
        let css = CssPoint::new(200.0, 70.0);
        let contract = contract(Some(css), report);
        let reply = chrome::AssessReply {
            at_point: at_point("point-topmost.json"),
            css: None,
            iframe_frame: None,
            local_css: None,
            frame: None,
            top_metrics: None,
            frame_chain: Vec::new(),
            identity: chrome::PageIdentity::default(),
        };
        let envelope = point_envelope_from_reply(contract, css, None, reply);
        let at = envelope.at_point.expect("at_point");
        assert_eq!(at.interactable, None);
        assert_eq!(at.reason.as_deref(), Some("point_mismatch"));
    }

    #[test]
    fn point_mode_old_extension_does_not_send_assess() {
        chrome::reset_assess_calls();
        let envelope = point_after_snapshot(
            chrome::SnapshotOutcome::Map(Box::new(test_map(
                Some(valid_report(
                    1.0,
                    Rect {
                        x: 0,
                        y: 0,
                        w: 1200,
                        h: 900,
                    },
                )),
                Some(chrome::PageIdentity {
                    url: Some("https://example.test/".into()),
                    title: "Example".into(),
                    inner_width: Some(1200.0),
                    inner_height: Some(900.0),
                    dpr: Some(1.0),
                    assess_v: None,
                }),
            ))),
            PhysicalPoint { x: 10, y: 20 },
            25,
        )
        .expect("query");
        assert_eq!(
            envelope.at_point.expect("at_point").reason.as_deref(),
            Some("assessment_unavailable")
        );
        assert_eq!(chrome::assess_calls(), 0, "old extension must not be asked");
    }

    #[test]
    fn point_mode_new_extension_attempts_assess() {
        chrome::reset_assess_calls();
        let _result = point_after_snapshot(
            chrome::SnapshotOutcome::Map(Box::new(test_map(
                Some(valid_report(
                    1.0,
                    Rect {
                        x: 0,
                        y: 0,
                        w: 1200,
                        h: 900,
                    },
                )),
                Some(chrome::PageIdentity {
                    url: Some("https://example.test/".into()),
                    title: "Example".into(),
                    inner_width: Some(1200.0),
                    inner_height: Some(900.0),
                    dpr: Some(1.0),
                    assess_v: Some(1),
                }),
            ))),
            PhysicalPoint { x: 10, y: 20 },
            25,
        );
        assert_eq!(
            chrome::assess_calls(),
            1,
            "assess_v>=1 must reach the assess call"
        );
    }

    #[test]
    fn point_mode_tab_or_metrics_changed_is_unknown() {
        let before = chrome::PageIdentity {
            url: Some("https://example.test/".into()),
            title: "Example".into(),
            inner_width: Some(1200.0),
            inner_height: Some(900.0),
            dpr: Some(1.0),
            assess_v: Some(1),
        };
        let mut after = before.clone();
        assert!(identity_matches(&before, &after));
        after.url = Some("https://example.test/next".into());
        assert!(!identity_matches(&before, &after));
        after.url = before.url.clone();
        after.inner_width = Some(1100.0);
        assert!(!identity_matches(&before, &after));
        after.inner_width = before.inner_width;
        after.dpr = Some(1.25);
        assert!(!identity_matches(&before, &after));
    }

    #[test]
    fn point_mode_window_moved_is_unknown() {
        let space = crate::space::ensure_dpi()
            .and_then(|_| crate::space::virtual_screen())
            .expect("space");
        let x = space.origin_x + space.width / 2;
        let y = space.origin_y + space.height / 2;
        crate::foreground::set_foreground_hwnd_hook(Some(|| Some(0x1234)));
        crate::foreground::set_chrome_target_hook(Some(|_| true));
        WRECT_CALLS.with(|c| c.set(0));
        crate::foreground::set_window_rect_hook(Some(moving_window_rect));
        chrome::set_snapshot_identity_stub(Some(chrome::SnapshotOutcome::Map(Box::new(test_map(
            Some(TransformReport::invalid("no_viewport_anchor", None)),
            None,
        )))));
        let envelope = run_point(&Target::Pixel { x, y }, 25).expect("run_point");
        crate::foreground::set_window_rect_hook(None);
        crate::foreground::set_foreground_hwnd_hook(None);
        crate::foreground::set_chrome_target_hook(None);
        chrome::set_snapshot_identity_stub(None);
        assert_eq!(
            envelope.at_point.expect("at_point").reason.as_deref(),
            Some("window_moved")
        );
    }

    thread_local! {
        static WRECT_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    fn moving_window_rect(_: isize) -> Option<Rect> {
        let n = WRECT_CALLS.with(|c| {
            let n = c.get();
            c.set(n + 1);
            n
        });
        Some(if n == 0 {
            Rect {
                x: 0,
                y: 0,
                w: 100,
                h: 100,
            }
        } else {
            Rect {
                x: 1,
                y: 0,
                w: 100,
                h: 100,
            }
        })
    }

    #[test]
    fn point_mode_not_foreground_is_unknown() {
        let space = crate::space::ensure_dpi()
            .and_then(|_| crate::space::virtual_screen())
            .expect("space");
        let x = space.origin_x + space.width / 2;
        let y = space.origin_y + space.height / 2;
        crate::foreground::set_foreground_hwnd_hook(Some(|| None));
        let envelope = run_point(&Target::Pixel { x, y }, 25).expect("run_point");
        crate::foreground::set_foreground_hwnd_hook(None);
        assert_eq!(
            envelope.at_point.expect("at_point").reason.as_deref(),
            Some("not_foreground")
        );
    }

    #[test]
    fn point_mode_not_chrome_is_unknown() {
        let space = crate::space::ensure_dpi()
            .and_then(|_| crate::space::virtual_screen())
            .expect("space");
        let x = space.origin_x + space.width / 2;
        let y = space.origin_y + space.height / 2;
        crate::foreground::set_foreground_hwnd_hook(Some(|| Some(0x1234)));
        crate::foreground::set_chrome_target_hook(Some(|_| false));
        let envelope = run_point(&Target::Pixel { x, y }, 25).expect("run_point");
        crate::foreground::set_foreground_hwnd_hook(None);
        crate::foreground::set_chrome_target_hook(None);
        assert_eq!(
            envelope.at_point.expect("at_point").reason.as_deref(),
            Some("not_chrome")
        );
    }

    #[test]
    fn point_mode_does_not_mutate_walk_detail() {
        // Drives the real `pipe_snapshot_inner` store flag: point mode uses store=false, the
        // ordinary snapshot path stores. A regression that flips the flag fails this test.
        let g = chrome::EnvGuard::lock();
        g.set_snapshot(None);
        chrome::set_snapshot_identity_stub(None);
        chrome::set_pipe_snapshot_stub(Some(test_map(
            Some(TransformReport::invalid("no_viewport_anchor", None)),
            None,
        )));
        chrome::set_last_pipe_detail_for_test(Detail::Dom);
        let _ = chrome::try_snapshot_identity(Detail::Default, None);
        assert_eq!(chrome::current_pipe_detail(), "dom");
        let _ = chrome::try_snapshot(Detail::Default, None);
        assert_eq!(chrome::current_pipe_detail(), "default");
        chrome::set_pipe_snapshot_stub(None);
    }

    // ---------------------------------------------------------------- refusal / markers

    fn target_info() -> ActuateTarget {
        ActuateTarget {
            kind: "element".into(),
            id: Some("chr:12".into()),
            x: 350,
            y: 167,
        }
    }

    fn blocked_resolved() -> ResolvedTarget {
        let report = valid_report(
            1.25,
            Rect {
                x: 100,
                y: 80,
                w: 1500,
                h: 1125,
            },
        );
        let css = CssPoint::new(200.0, 170.0);
        let contract = contract(Some(css), report);
        ResolvedTarget {
            target: Target::ElementId("chr:12".into()),
            kind: "element",
            id: Some("chr:12".into()),
            x: 350,
            y: 292,
            rect: Rect {
                x: 225,
                y: 268,
                w: 250,
                h: 50,
            },
            hwnd: Some(0x1234),
            name: "Sign in".into(),
            role: "Button".into(),
            css_point: Some(css),
            css_rect: Some(CssRect {
                left: 100.0,
                top: 150.0,
                width: 200.0,
                height: 40.0,
            }),
            contract: Some(contract),
            map: None,
            assessment: Some(assessment("cookie-banner.json")),
            identity: None,
            uia_runtime_id: None,
            uia_expected_fp: None,
        }
    }

    #[test]
    fn require_interactable_blocked_refuses() {
        let env = guard_require_interactable("s-test", &target_info(), &blocked_resolved())
            .expect("guard")
            .expect("refusal");
        assert!(!env.ok);
        assert_eq!(env.interactable_checked, Some(true));
        let error = env.error.as_deref().unwrap_or_default();
        assert_eq!(error, "interactable_blocked: blocked", "{error}");
        assert!(!error.contains("blocked_by="), "{error}");
        assert!(env.point.is_some());
        assert!(env.assessment.is_some());
    }

    fn frame_link(left: f64) -> crate::frames::FrameLink {
        crate::frames::FrameLink {
            parent: Some(0),
            content_left: left,
            content_top: 60.0,
            content_width: 300.0,
            content_height: 250.0,
            clip: vec![crate::frames::ClipRect {
                left,
                top: 60.0,
                width: 300.0,
                height: 250.0,
            }],
            transform_ok: true,
            visible: true,
            hit_ok: true,
            onscreen: true,
            viewport_w: 800.0,
            viewport_h: 600.0,
            ..crate::frames::FrameLink::default()
        }
    }

    fn child_assess(chain: Vec<crate::frames::FrameLink>) -> chrome::AssessReply {
        chrome::AssessReply {
            at_point: AtPoint {
                element: Some(PointBlocker {
                    kind: BlockerKind::Element,
                    tag: Some("button".into()),
                    role: None,
                    name: None,
                    rect_css: Some(CssRect {
                        left: 10.0,
                        top: 10.0,
                        width: 20.0,
                        height: 20.0,
                    }),
                    reason: None,
                }),
                shadow_path: Vec::new(),
                interactable: Some(true),
                reason: None,
                notes: Vec::new(),
                confidence: AssessmentConfidence::High,
                elapsed_ms: None,
                frame: None,
            },
            css: Some(CssPoint::new(10.0, 10.0)),
            identity: chrome::PageIdentity::default(),
            iframe_frame: None,
            local_css: None,
            frame: Some(3),
            top_metrics: Some(chrome::ChromeMetrics {
                inner_width: 800.0,
                inner_height: 600.0,
                ..chrome::ChromeMetrics::default()
            }),
            frame_chain: chain,
        }
    }

    fn top_cross_frame(frame: Option<u32>) -> chrome::AssessReply {
        chrome::AssessReply {
            at_point: AtPoint {
                element: None,
                shadow_path: Vec::new(),
                interactable: None,
                reason: Some("cross_frame".into()),
                notes: Vec::new(),
                confidence: AssessmentConfidence::None,
                elapsed_ms: None,
                frame: None,
            },
            css: Some(CssPoint::new(50.0, 70.0)),
            identity: chrome::PageIdentity::default(),
            iframe_frame: frame,
            local_css: Some(CssPoint::new(10.0, 10.0)),
            frame: None,
            top_metrics: None,
            frame_chain: Vec::new(),
        }
    }

    fn point_map() -> chrome::ChromeMap {
        let mut map = test_map(
            Some(valid_report(
                1.0,
                Rect {
                    x: 0,
                    y: 0,
                    w: 800,
                    h: 600,
                },
            )),
            None,
        );
        map.tab = Some(12);
        map.identity = Some(chrome::PageIdentity {
            assess_v: Some(1),
            ..chrome::PageIdentity::default()
        });
        map
    }

    #[test]
    fn point_mode_enters_a_linked_frame_and_composes() {
        chrome::set_assess_sequence(vec![
            Ok(top_cross_frame(Some(3))),
            Ok(child_assess(vec![frame_link(40.0)])),
        ]);
        let env = point_after_snapshot(
            chrome::SnapshotOutcome::Map(Box::new(point_map())),
            PhysicalPoint { x: 50, y: 70 },
            25,
        )
        .expect("point");
        chrome::set_assess_sequence(Vec::new());
        let at = env.at_point.expect("at_point");
        assert_eq!(at.interactable, Some(true));
        assert_eq!(at.frame, Some(3), "frame provenance is retained");
        assert_eq!(
            at.element.expect("element").rect_css.expect("rect").left,
            50.0
        );
    }

    #[test]
    fn point_mode_refuses_a_frame_point_mismatch() {
        chrome::set_assess_sequence(vec![
            Ok(top_cross_frame(Some(3))),
            Ok(child_assess(vec![frame_link(45.0)])),
        ]);
        let env = point_after_snapshot(
            chrome::SnapshotOutcome::Map(Box::new(point_map())),
            PhysicalPoint { x: 50, y: 70 },
            25,
        )
        .expect("point");
        chrome::set_assess_sequence(Vec::new());
        let at = env.at_point.expect("at_point");
        assert_eq!(at.interactable, None);
        assert_eq!(at.reason.as_deref(), Some("frame_geometry_unavailable"));
    }

    #[test]
    fn point_mode_unlinked_cross_frame_stays_informative() {
        chrome::set_assess_sequence(vec![Ok(top_cross_frame(None))]);
        let env = point_after_snapshot(
            chrome::SnapshotOutcome::Map(Box::new(point_map())),
            PhysicalPoint { x: 50, y: 70 },
            25,
        )
        .expect("point");
        chrome::set_assess_sequence(Vec::new());
        let at = env.at_point.expect("at_point");
        assert_eq!(at.interactable, None);
        assert_eq!(at.reason.as_deref(), Some("cross_frame"));
    }

    #[test]
    fn composed_frame_assessment_passes_require_interactable() {
        let mut resolved = blocked_resolved();
        let contract = resolved.contract.clone().expect("contract");
        let css = contract.css.expect("css");
        let mut assessment = Assessment::unknown(
            css,
            CssRect {
                left: css.x - 10.0,
                top: css.y - 10.0,
                width: 20.0,
                height: 20.0,
            },
            "ok",
        );
        assessment.interactable = Some(true);
        assessment.confidence = AssessmentConfidence::High;
        resolved.assessment = Some(assessment);
        let refusal =
            guard_require_interactable("s-frame", &target_info(), &resolved).expect("guard");
        assert!(
            refusal.is_none(),
            "a composed frame assessment must not falsely refuse"
        );
    }

    #[test]
    fn require_interactable_unknown_refuses() {
        let mut resolved = blocked_resolved();
        let mut a = assessment("cookie-banner.json");
        a.interactable = None;
        a.reason = Some("assessment_timeout".into());
        a.confidence = AssessmentConfidence::None;
        resolved.assessment = Some(a);
        let env = guard_require_interactable("s-test", &target_info(), &resolved)
            .expect("guard")
            .expect("refusal");
        assert!(!env.ok);
        assert_eq!(
            env.error.as_deref(),
            Some("interactable_unknown: assessment_timeout")
        );
    }

    #[test]
    fn require_interactable_missing_assessment_refuses() {
        let mut resolved = blocked_resolved();
        resolved.assessment = None;
        let env = guard_require_interactable("s-test", &target_info(), &resolved)
            .expect("guard")
            .expect("refusal");
        assert_eq!(
            env.error.as_deref(),
            Some("assessment_missing: extension did not return an assessment")
        );
    }

    #[test]
    fn require_interactable_non_chr_refuses() {
        let mut resolved = blocked_resolved();
        resolved.id = Some("uia:1.2".into());
        resolved.kind = "element";
        let env = guard_require_interactable("s-test", &target_info(), &resolved)
            .expect("guard")
            .expect("refusal");
        assert_eq!(
            env.error.as_deref(),
            Some("interactable_unavailable: click on chr: or sref1:c only")
        );
    }

    #[test]
    fn require_interactable_mismatch_refuses() {
        let mut resolved = blocked_resolved();
        let mut a = assessment("cookie-banner.json");
        a.selected_point = CssPoint::new(1.0, 2.0);
        resolved.assessment = Some(a);
        let env = guard_require_interactable("s-test", &target_info(), &resolved)
            .expect("guard")
            .expect("refusal");
        assert_eq!(
            env.error.as_deref(),
            Some("assessment_point_mismatch: selected_point != point.css")
        );
    }

    #[test]
    fn interactable_true_allows() {
        let mut resolved = blocked_resolved();
        let mut a = assessment("unobstructed.json");
        a.selected_point = CssPoint::new(200.0, 170.0);
        a.target_rect_css = CssRect {
            left: 100.0,
            top: 150.0,
            width: 200.0,
            height: 40.0,
        };
        resolved.assessment = Some(a);
        assert!(
            guard_require_interactable("s-test", &target_info(), &resolved)
                .expect("guard")
                .is_none()
        );
    }

    #[test]
    fn interactable_checked_marker() {
        let env = guard_require_interactable("s-test", &target_info(), &blocked_resolved())
            .expect("guard")
            .expect("refusal");
        assert_eq!(env.interactable_checked, Some(true));
        assert_eq!(flag_off_golden().interactable_checked, None);
        let src = include_str!("actuate.rs");
        assert!(
            src.contains("if require_interactable && env.interactable_checked.is_none()"),
            "successful opted-in click must carry the positive marker"
        );
    }

    #[test]
    fn rust_never_calls_a_dom_hit_test() {
        let src = include_str!("hittest.rs");
        let element = ["element", "FromPoint("].concat();
        let elements = ["elements", "FromPoint("].concat();
        assert!(!src.contains(&element));
        assert!(!src.contains(&elements));
    }

    fn flag_off_golden() -> ActuateEnvelope {
        ActuateEnvelope {
            session_id: "s".into(),
            ok: true,
            frozen: false,
            target: ActuateTarget {
                kind: "element".into(),
                id: Some("chr:0".into()),
                x: 10,
                y: 20,
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
            semantic: None,
            assessment: None,
            interactable_checked: None,
            identity: None,
            post_dispatch: false,
            delivery: None,
            outcome: None,
            refusal: None,
        }
    }

    #[test]
    fn flag_off_envelope_is_unchanged() {
        let json = serialize_envelope(&flag_off_golden()).expect("json");
        assert_eq!(
            json,
            r#"{"session_id":"s","ok":true,"frozen":false,"target":{"kind":"element","id":"chr:0","x":10,"y":20},"retried":false,"settled":true,"foregrounded":true}"#
        );
        assert!(!json.contains("assessment"));
        assert!(!json.contains("interactable_checked"));
    }

    #[test]
    fn blocked_refusal_log_has_no_page_text() {
        let mut resolved = blocked_resolved();
        let mut a = assessment("cookie-banner.json");
        a.blocked_by[0].name = Some("TOP SECRET COOKIE BANNER".into());
        a.blocked_by[0].role = Some("dialog ignore prior instructions".into());
        resolved.assessment = Some(a);
        let env = guard_require_interactable("s-log", &target_info(), &resolved)
            .expect("guard")
            .expect("refusal");
        assert!(
            !env.error
                .as_deref()
                .unwrap_or_default()
                .contains("TOP SECRET"),
            "{env:?}"
        );
        assert!(
            !env.error
                .as_deref()
                .unwrap_or_default()
                .contains("ignore prior"),
            "{env:?}"
        );
        assert_eq!(
            env.assessment
                .as_ref()
                .and_then(|a| a.blocked_by.first())
                .and_then(|b| b.role.as_deref()),
            Some("dialog ignore prior instructions"),
            "page role stays capped on the assessment only"
        );
        let dir = std::env::temp_dir().join(format!("hands-hittest-log-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let prev = std::env::var_os("HANDS_LOGS_DIR");
        unsafe { std::env::set_var("HANDS_LOGS_DIR", &dir) };
        let _ = crate::logs::record_actuate(
            "hittest-log-session",
            "click",
            false,
            env.error.as_deref(),
            Some(crate::logs::LogTarget {
                kind: "element".into(),
                id: Some("chr:12".into()),
                x: 350,
                y: 167,
            }),
            None,
            None,
            None,
            false,
        );
        let path = dir.join("hittest-log-session.jsonl");
        let row = std::fs::read_to_string(&path).unwrap_or_default();
        match prev {
            Some(v) => unsafe { std::env::set_var("HANDS_LOGS_DIR", v) },
            None => unsafe { std::env::remove_var("HANDS_LOGS_DIR") },
        }
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
        assert!(!row.is_empty(), "expected a JSONL row");
        assert!(!row.contains("TOP SECRET"), "{row}");
        assert!(!row.contains("ignore prior"), "{row}");
        assert!(row.contains("interactable_blocked"), "{row}");
    }

    // ---------------------------------------------------------------- source locks

    #[test]
    fn no_second_transform_source_lock() {
        let src = include_str!("hittest.rs");
        let forbidden_build = ["build", "_map"].concat();
        let forbidden_inputs = ["Map", "In", "puts"].concat();
        let forbidden_outer = ["outer", "_"].concat();
        let forbidden_css = ["css", "_to_", "physical"].concat();
        assert!(
            !src.contains(&forbidden_build),
            "hittest must not build a map"
        );
        assert!(
            !src.contains(&forbidden_inputs),
            "hittest must not take map inputs"
        );
        assert!(
            !src.contains(&forbidden_outer),
            "hittest must not do outer-* arithmetic"
        );
        assert!(!src.contains(&forbidden_css));
        let start = src.find("pub fn css_for_physical").expect("inverse");
        let end = src[start..]
            .find("pub fn identity_matches")
            .map(|i| start + i)
            .expect("identity_matches");
        let inverse = &src[start..end];
        assert!(inverse.contains("map_point"), "{inverse}");
        assert!(inverse.contains("page_viewport"), "{inverse}");
    }

    #[test]
    fn no_synthetic_input_source_lock() {
        let js = include_str!("../extension/content.js");
        for forbidden in [
            "dispatchEvent",
            ".click(",
            ".focus(",
            "scrollIntoView",
            "MutationObserver",
            "requestAnimationFrame",
        ] {
            assert!(
                !js.contains(forbidden),
                "content.js must not use {forbidden}"
            );
        }
        assert!(js.contains("chrome.dom"));
        assert!(js.contains("openOrClosedShadowRoot"));
    }

    #[test]
    fn manifest_permissions_unchanged() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../extension/manifest.json")).expect("manifest");
        assert_eq!(
            manifest["permissions"],
            serde_json::json!(["nativeMessaging"])
        );
        assert_eq!(
            manifest["host_permissions"],
            serde_json::json!(["<all_urls>"])
        );
        let scripts = manifest["content_scripts"].as_array().expect("scripts");
        assert_eq!(scripts[0]["all_frames"], serde_json::json!(true));
        assert_eq!(scripts[0]["match_about_blank"], serde_json::json!(true));
        assert_eq!(scripts[0]["world"], serde_json::json!("ISOLATED"));
        assert_eq!(manifest["manifest_version"], serde_json::json!(3));
        for missing in [
            "scripting",
            "tabs",
            "debugger",
            "webRequest",
            "webNavigation",
        ] {
            assert!(
                !manifest["permissions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v == missing),
                "manifest must not add {missing}"
            );
        }
    }

    #[test]
    fn no_debug_protocol_source_lock() {
        let remote = ["--remote", "-debugging-port"].concat();
        let runtime_enable = ["Runtime", ".enable"].concat();
        let websocket = ["webSocket", "DebuggerUrl"].concat();
        let target_files = [
            include_str!("hittest.rs"),
            include_str!("../extension/content.js"),
            include_str!("../extension/sw.js"),
        ];
        for src in target_files {
            assert!(!src.contains(&remote));
            assert!(!src.contains(&runtime_enable));
            assert!(!src.contains(&websocket));
        }
    }

    #[test]
    fn viewport_rect_uses_client_basis() {
        let js = include_str!("../extension/content.js");
        let start = js.find("function assessViewport").expect("assessViewport");
        let end = js[start..]
            .find("function intersectRects")
            .map(|i| start + i)
            .expect("intersectRects");
        let body = &js[start..end];
        assert!(body.contains("clientWidth"), "{body}");
        assert!(body.contains("clientHeight"), "{body}");
        assert!(body.contains("innerWidth"), "{body}");
        assert!(body.contains("innerHeight"), "{body}");
    }

    #[test]
    fn element_mode_accepts_scoped_chrome_ref() {
        let src = include_str!("hittest.rs");
        let start = src.find("pub fn run(").expect("run");
        let end = src.find("fn run_element(").expect("run_element");
        let body = &src[start..end];
        assert!(body.contains("Target::Scoped(crate::identity::SnapshotRef::Chrome"));
        assert!(body.contains("run_element(&target, samples)"));
    }

    #[test]
    fn envelope_for_identity_has_no_point_or_assessment() {
        let r = crate::identity::SnapshotRef::Chrome {
            tab: 12,
            frame: 0,
            doc: "9f3a".into(),
            snap: "a1b2c3d4".into(),
            idx: 7,
        };
        let report = crate::identity::IdentityReport::chrome_failure(
            &r,
            IdentityState::Stale,
            "stale_element",
        );
        let envelope = envelope_for_outcome(
            TargetOutcome::Identity {
                target: Target::Scoped(r),
                report,
            },
            "default",
        );
        assert!(!envelope.ok);
        assert!(envelope.point.is_none());
        assert!(envelope.target_rect.is_none());
        assert!(envelope.assessment.is_none());
        assert_eq!(envelope.error.as_deref(), Some("stale_element"));
    }

    #[test]
    fn guard_accepts_scoped_ref_with_assessment() {
        let r = crate::identity::SnapshotRef::Chrome {
            tab: 12,
            frame: 0,
            doc: "9f3a".into(),
            snap: "a1b2c3d4".into(),
            idx: 7,
        };
        let css = CssPoint::new(200.0, 70.0);
        let report = valid_report(
            1.0,
            Rect {
                x: 100,
                y: 50,
                w: 800,
                h: 600,
            },
        );
        let mut resolved = blocked_resolved();
        resolved.target = Target::Scoped(r.clone());
        resolved.id = Some(r.to_string());
        resolved.css_point = Some(css);
        resolved.css_rect = Some(CssRect {
            left: 100.0,
            top: 50.0,
            width: 200.0,
            height: 40.0,
        });
        resolved.contract = Some(contract(Some(css), report));
        resolved.assessment = Some(assessment("unobstructed.json"));
        resolved.identity = Some(crate::identity::IdentityReport::exact_chrome(&r));
        let info = ActuateTarget {
            kind: "element".into(),
            id: Some(r.to_string()),
            x: 350,
            y: 167,
        };
        assert!(
            guard_require_interactable("s-test", &info, &resolved)
                .expect("guard")
                .is_none()
        );
    }

    fn test_map(
        report: Option<TransformReport>,
        identity: Option<chrome::PageIdentity>,
    ) -> chrome::ChromeMap {
        chrome::ChromeMap {
            frame_rows: Vec::new(),
            frames_transient: false,
            tab: None,
            url: Some("https://example.test/".into()),
            title: "Example".into(),
            main_text: String::new(),
            elements: Vec::new(),
            cards: Vec::new(),
            listing: ListingMeta {
                result_count: None,
                local_matches: None,
                empty_state: None,
                zip: None,
                radius: None,
                cards_walked: 0,
            },
            cards_walked: 0,
            transform: report,
            identity,
            snapshot: None,
            rev: None,
        }
    }
}
