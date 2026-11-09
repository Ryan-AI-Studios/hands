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
pub fn envelope_for_outcome(outcome: TargetOutcome, detail: &str, samples: u32) -> HittestEnvelope {
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
                None => Assessment::unknown(
                    resolved.css_point.unwrap_or(CssPoint::new(0.0, 0.0)),
                    resolved.css_rect.unwrap_or(CssRect {
                        left: 0.0,
                        top: 0.0,
                        width: 0.0,
                        height: 0.0,
                    }),
                    "assessment_missing",
                ),
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
            let _ = samples;
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
                    "interactable_unavailable: hittest element mode supports chr: only".into(),
                ));
            }
            run_element(&target, samples)?
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
    Ok(envelope_for_outcome(
        outcome,
        chrome::current_pipe_detail(),
        samples,
    ))
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
        Target::ElementId(_) => {
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
    let mut envelope = point_after_snapshot(snapshot, physical, samples)?;
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
    let reply = match chrome::assess_at(css, samples) {
        Ok(reply) => reply,
        Err(_) => return Ok(point_unknown("assessment_timeout")),
    };
    if let Some(before) = identity.as_ref()
        && !identity_matches(before, &reply.identity)
    {
        let reason = if before.url != reply.identity.url {
            "tab_changed"
        } else {
            "stale_page"
        };
        let mut envelope = HittestEnvelope {
            session_id: String::new(),
            ok: true,
            point: Some(contract),
            target_rect: None,
            detail: Some("default".into()),
            assessment: None,
            at_point: Some(unknown_at_point(reason)),
            error: None,
        };
        envelope.at_point.as_mut().unwrap().elapsed_ms = None;
        return Ok(envelope);
    }
    if let Some(echo) = reply.css
        && !points_match(css, echo)
    {
        return Ok(HittestEnvelope {
            session_id: String::new(),
            ok: true,
            point: Some(contract),
            target_rect: None,
            detail: Some("default".into()),
            assessment: None,
            at_point: Some(unknown_at_point("point_mismatch")),
            error: None,
        });
    }
    let mut at_point = reply.at_point;
    bound_at_point(&mut at_point);
    Ok(HittestEnvelope {
        session_id: String::new(),
        ok: true,
        point: Some(contract),
        target_rect: None,
        detail: Some("default".into()),
        assessment: None,
        at_point: Some(at_point),
        error: None,
    })
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
            .is_some_and(|id| id.starts_with("chr:"));
    if !is_chr {
        return refusal(session_id, info, None, None, None, "unavailable").map(Some);
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
        "unavailable" => "interactable_unavailable: click on chr: only".into(),
        "mismatch" => "assessment_point_mismatch: selected_point != point.css".into(),
        other => format!("interactable_{other}: {reason}"),
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
    })
}

fn blocked_error(reason: &str, assessment: Option<&Assessment>) -> String {
    let mut out = format!("interactable_blocked: {reason}");
    if let Some(blocker) = assessment.and_then(|a| a.blocked_by.first())
        && let Some(tag) = blocker.tag.as_deref()
    {
        out.push_str(&format!("; blocked_by={tag}"));
        if let Some(role) = blocker.role.as_deref() {
            out.push_str(&format!("[role={role}]"));
        }
    }
    out
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
        let clipped = a.clipped_by.expect("clipped_by");
        assert_eq!(clipped.tag.as_deref(), Some("section"));
        assert!(a.visible_fraction.unwrap_or(1.0) < a.onscreen_fraction.unwrap_or(1.0));
    }

    #[test]
    fn transformed_reply_passes() {
        assert_eq!(
            assessment("transformed-ancestor.json").interactable,
            Some(true)
        );
    }

    #[test]
    fn open_shadow_overlay_blocks() {
        let a = assessment("open-shadow-overlay.json");
        assert_eq!(a.interactable, Some(false));
        assert_eq!(a.blocked_by[0].tag.as_deref(), Some("div"));
    }

    #[test]
    fn closed_shadow_element_false() {
        let a = assessment("closed-shadow.json");
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
    }

    #[test]
    fn inert_reply() {
        let a = assessment("aria-disabled-inert.json");
        assert_eq!(a.interactable, Some(true));
        assert!(a.notes.iter().any(|n| n == "inert"));
    }

    #[test]
    fn small_target_confidence() {
        let a = assessment("small-target.json");
        assert_eq!(a.interactable, Some(true));
        assert!(matches!(
            a.confidence,
            AssessmentConfidence::Medium | AssessmentConfidence::Low
        ));
    }

    #[test]
    fn point_mode_topmost_element_and_shadow_path() {
        let a = at_point("point-topmost.json");
        assert_eq!(a.interactable, Some(true));
        assert_eq!(a.element.unwrap().tag.as_deref(), Some("button"));
        assert_eq!(a.shadow_path, vec!["app-shell"]);
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
        assert_eq!(contract.physical, Some(map_point(&contract, css).unwrap()));
        let a = Assessment {
            interactable: Some(true),
            selected_point: css,
            target_rect_css: CssRect {
                left: 100.0,
                top: 50.0,
                width: 200.0,
                height: 40.0,
            },
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
            elapsed_ms: Some(1.7),
        };
        validate_assessment(&contract, &a).expect("same point");
    }

    #[test]
    fn assessor_same_point() {
        assessor_same_point_from_payload_rect();
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
            },
            "default",
            25,
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
        let contract = PointContract {
            source: PointSource::Chr,
            css: Some(CssPoint::new(10.0, 20.0)),
            physical: None,
            transform: TransformReport::invalid(
                "point_outside_viewport: CSS point is outside the layout viewport",
                None,
            ),
        };
        let envelope = envelope_for_outcome(
            TargetOutcome::Unresolved {
                target: Target::ElementId("chr:0".into()),
                contract: Box::new(contract.clone()),
                hwnd: None,
                assessment: None,
            },
            "default",
            25,
        );
        let a = envelope.assessment.expect("assessment");
        assert_eq!(a.interactable, None);
        let reason = a.reason.unwrap_or_default();
        assert!(reason.starts_with("invalid_transform:"));
        assert!(reason.contains("point_outside_viewport"));
        let refusal = crate::actuate::refuse_transform(
            "s-test".into(),
            ActuateTarget {
                kind: "element".into(),
                id: Some("chr:0".into()),
                x: 0,
                y: 0,
            },
            contract,
        )
        .expect("refusal");
        assert!(
            refusal
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("point_outside_viewport")
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
        };
        let envelope =
            envelope_for_outcome(TargetOutcome::Resolved(Box::new(resolved)), "default", 25);
        assert_eq!(envelope.target_rect, Some(expected));
        assert_eq!(envelope.detail.as_deref(), Some("default"));
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
        let a = Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 100,
        };
        let b = Rect {
            x: 1,
            y: 0,
            w: 100,
            h: 100,
        };
        assert!(!window_moved(Some(a), Some(a)));
        assert!(window_moved(Some(a), Some(b)));
        assert!(window_moved(Some(a), None));
    }

    #[test]
    fn point_mode_not_foreground_is_unknown() {
        assert_eq!(
            point_unknown("not_foreground")
                .at_point
                .unwrap()
                .reason
                .as_deref(),
            Some("not_foreground")
        );
        assert_eq!(
            point_unknown("not_chrome")
                .at_point
                .unwrap()
                .reason
                .as_deref(),
            Some("not_chrome")
        );
    }

    #[test]
    fn point_mode_does_not_mutate_walk_detail() {
        // The fixture snapshot path never stores; the live path uses the non-storing inner.
        let g = chrome::EnvGuard::lock();
        g.set_snapshot(Some(&chrome::EnvGuard::fixture_path()));
        chrome::set_last_pipe_detail_for_test(Detail::Dom);
        let _ = chrome::try_snapshot_identity(Detail::Default, None);
        assert_eq!(chrome::current_pipe_detail(), "dom");
        g.set_snapshot(None);
        let src = include_str!("chrome.rs");
        let start = src.find("pub fn try_snapshot_identity").expect("identity");
        let end = src[start..]
            .find("fn pipe_resolve_raw")
            .map(|i| start + i)
            .expect("pipe_resolve_raw");
        let body = &src[start..end];
        assert!(
            body.contains("pipe_snapshot_inner(detail, hwnd, false)"),
            "{body}"
        );
        assert!(!body.contains("store_last_pipe_detail"), "{body}");
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
        assert!(
            error.starts_with("interactable_blocked: blocked"),
            "{error}"
        );
        assert!(error.contains("blocked_by="), "{error}");
        assert!(env.point.is_some());
        assert!(env.assessment.is_some());
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
            Some("interactable_unavailable: click on chr: only")
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
        assert_eq!(scripts[0]["all_frames"], serde_json::json!(false));
        assert_eq!(scripts[0]["world"], serde_json::json!("ISOLATED"));
        assert_eq!(manifest["manifest_version"], serde_json::json!(3));
        for missing in ["scripting", "tabs", "debugger", "webRequest"] {
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

    fn test_map(
        report: Option<TransformReport>,
        identity: Option<chrome::PageIdentity>,
    ) -> chrome::ChromeMap {
        chrome::ChromeMap {
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
        }
    }
}
