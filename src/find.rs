//! Scoped snapshot search and drill-down (0125).
//!
//! `find` is **read-only**: it never dispatches input, takes no lease, arms no cooldown, writes no
//! JSONL row and never calls an inner model. List mode searches an already-bound scope (a Chrome
//! document snapshot, one frame's snapshot, a bounded UIA window/subtree walk). Exact mode
//! (`--ref`) reuses the 0123 resolve, the opt-in 0128 assessment and read-only capability
//! evidence. `ok:true` means the scope was read; it is never proof that a target is live or that
//! an action would succeed.

use serde::{Deserialize, Serialize};

use crate::error::HandsError;
use crate::hittest::Assessment;
use crate::identity::{IdentityReport, SnapshotRef};
use crate::point::CssRect;
use crate::space::Rect;

pub const FIND_DEFAULT_LIMIT: usize = 20;
pub const FIND_MAX_LIMIT: usize = 100;
pub const FIND_TEXT_MAX_CHARS: usize = 64;
pub const FIND_NAME_CAP: usize = 80;
pub const FIND_DEFAULT_UIA_NODES: usize = 1500;
pub const FIND_MAX_UIA_NODES: usize = 4000;
pub const FIND_DEFAULT_DEPTH: u32 = 3;
pub const FIND_MAX_DEPTH: u32 = 8;
/// Chrome JS scan budget; must leave headroom inside the 400 ms client pipe deadline.
pub const FIND_DEFAULT_BUDGET_MS: u64 = 150;
pub const FIND_MAX_BUDGET_MS: u64 = 300;
pub const FIND_UIA_DEFAULT_DEADLINE_MS: u64 = 800;
pub const FIND_UIA_MAX_DEADLINE_MS: u64 = 2000;
/// Declared floor for a byte-shrunk list page. Not a user-facing limit.
pub const FIND_MIN_RETURNED_MATCHES: usize = 12;

const NOTES_CAP: usize = 3;

// ---------------------------------------------------------------- vocabulary

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    ChromeSnapshot,
    ChromeSubtree,
    UiaWindow,
    UiaSubtree,
    Element,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeSource {
    StoredSnapshot,
    LiveWalk,
    Fixture,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Paging {
    Snapshot,
    BestEffort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionVerb {
    Invoke,
    Toggle,
    Expand,
    Collapse,
    Select,
    SetValue,
}

impl ActionVerb {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Invoke => "invoke",
            Self::Toggle => "toggle",
            Self::Expand => "expand",
            Self::Collapse => "collapse",
            Self::Select => "select",
            Self::SetValue => "set_value",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    Available,
    Unavailable,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapSource {
    UiaPattern,
    Dom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpandedState {
    Expanded,
    Collapsed,
    PartiallyExpanded,
    Unavailable,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckedState {
    Checked,
    Unchecked,
    Mixed,
    Unavailable,
    Unknown,
}

// ---------------------------------------------------------------- types

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FindFilters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect_css: Option<CssRect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect: Option<Rect>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub subtree: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FindScope {
    pub kind: ScopeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snap: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hwnd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nodes_visited: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_cap: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth_cap: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FindCoverage {
    pub scope_source: ScopeSource,
    pub examined: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_total: Option<usize>,
    pub scope_complete: bool,
    pub page_complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FindMatch {
    #[serde(rename = "ref")]
    pub r#ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idx: Option<u32>,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect_css: Option<CssRect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect: Option<Rect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_ok: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FindState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offscreen: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<CheckedState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expanded: Option<ExpandedState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FindAction {
    pub action: ActionVerb,
    pub support: Support,
    pub evidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FindCapabilities {
    pub source: CapSource,
    pub actions: Vec<FindAction>,
    pub state: FindState,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FindElement {
    #[serde(rename = "ref")]
    pub r#ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect_css: Option<CssRect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect: Option<Rect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub own_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FindEnvelope {
    pub session_id: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<FindScope>,
    pub coverage: FindCoverage,
    pub filters: FindFilters,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matches_total: Option<usize>,
    pub matches_returned: usize,
    pub matches_truncated: bool,
    pub offset: usize,
    pub paging: Paging,
    pub matches: Vec<FindMatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element: Option<FindElement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<IdentityReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<FindCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assessment: Option<Assessment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct FindRequest {
    pub session_id: Option<String>,
    pub within: Option<String>,
    pub window: Option<String>,
    pub r#ref: Option<String>,
    pub role: Option<String>,
    pub text: Option<String>,
    pub rect: Option<String>,
    pub rect_css: Option<String>,
    pub subtree: bool,
    pub depth: Option<u32>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub no_caps: bool,
    pub assess: bool,
    pub budget_ms: Option<u64>,
    pub max_nodes: Option<usize>,
    pub timeout_ms: Option<u64>,
}

impl FindRequest {
    pub fn limit(&self) -> usize {
        self.limit
            .unwrap_or(FIND_DEFAULT_LIMIT)
            .clamp(1, FIND_MAX_LIMIT)
    }

    pub fn offset(&self) -> usize {
        self.offset.unwrap_or(0)
    }

    pub fn budget_ms(&self) -> u64 {
        self.budget_ms
            .unwrap_or(FIND_DEFAULT_BUDGET_MS)
            .clamp(1, FIND_MAX_BUDGET_MS)
    }

    pub fn max_nodes(&self) -> usize {
        self.max_nodes
            .unwrap_or(FIND_DEFAULT_UIA_NODES)
            .clamp(1, FIND_MAX_UIA_NODES)
    }

    /// Chrome scans the stored record (retained ≤ 4000); its default cap is the record cap.
    pub fn chrome_max_nodes(&self) -> usize {
        self.max_nodes
            .unwrap_or(FIND_MAX_UIA_NODES)
            .clamp(1, FIND_MAX_UIA_NODES)
    }

    pub fn depth(&self) -> u32 {
        self.depth
            .unwrap_or(FIND_DEFAULT_DEPTH)
            .clamp(1, FIND_MAX_DEPTH)
    }

    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms
            .unwrap_or(FIND_UIA_DEFAULT_DEADLINE_MS)
            .clamp(1, FIND_UIA_MAX_DEADLINE_MS)
    }
}

// ---------------------------------------------------------------- validation

fn invalid_scope(detail: &str) -> HandsError {
    HandsError::Target(format!("find: invalid_scope: {detail}"))
}

fn invalid_filter(detail: &str) -> HandsError {
    HandsError::Target(format!("find: invalid_filter: {detail}"))
}

fn parse_rect(kind: &str, raw: &str) -> Result<Rect, HandsError> {
    let parts: Vec<&str> = raw.split(',').map(str::trim).collect();
    if parts.len() != 4 {
        return Err(invalid_filter(&format!(
            "{kind} expects four comma-separated numbers"
        )));
    }
    let nums = parts
        .iter()
        .map(|p| p.parse::<f32>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| invalid_filter(&format!("{kind} expects four numbers")))?;
    let to_i32 = |v: f32| -> Result<i32, HandsError> {
        if !v.is_finite() || v < i32::MIN as f32 || v > i32::MAX as f32 {
            return Err(invalid_filter(&format!("{kind} value is out of range")));
        }
        Ok(v.round() as i32)
    };
    Ok(Rect {
        x: to_i32(nums[0])?,
        y: to_i32(nums[1])?,
        w: to_i32(nums[2])?,
        h: to_i32(nums[3])?,
    })
}

fn parse_rect_css(raw: &str) -> Result<CssRect, HandsError> {
    let parts: Vec<&str> = raw.split(',').map(str::trim).collect();
    if parts.len() != 4 {
        return Err(invalid_filter("--rect-css expects left,top,width,height"));
    }
    let nums = parts
        .iter()
        .map(|p| p.parse::<f64>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| invalid_filter("--rect-css expects four numbers"))?;
    if nums.iter().any(|v| !v.is_finite()) {
        return Err(invalid_filter("--rect-css expects finite numbers"));
    }
    Ok(CssRect {
        left: nums[0],
        top: nums[1],
        width: nums[2],
        height: nums[3],
    })
}

#[derive(Debug, Clone)]
enum ScopeInput {
    Chrome(SnapshotRef),
    Uia(SnapshotRef),
    Window(String),
    Element(SnapshotRef),
}

impl ScopeInput {
    fn is_chrome_list(&self) -> bool {
        matches!(self, Self::Chrome(_))
    }

    fn is_uia_list(&self) -> bool {
        matches!(self, Self::Uia(_) | Self::Window(_))
    }
}

fn has_list_filter(req: &FindRequest) -> bool {
    req.role.is_some()
        || req.text.is_some()
        || req.rect.is_some()
        || req.rect_css.is_some()
        || req.subtree
        || req.depth.is_some()
}

/// Exactly one scope form; filters are typed per scope. Hard errors carry `invalid_filter` /
/// `invalid_scope`; a ref parse error keeps the 0123 `malformed_ref` / `unsupported_ref_version`.
fn validate(req: &FindRequest) -> Result<(ScopeInput, FindFilters), HandsError> {
    let forms = usize::from(req.within.is_some())
        + usize::from(req.window.is_some())
        + usize::from(req.r#ref.is_some());
    if forms != 1 {
        return Err(invalid_scope(
            "supply exactly one of --within, --window or --ref",
        ));
    }
    let scope = if let Some(raw) = &req.within {
        let parsed = SnapshotRef::parse(raw)?;
        match parsed {
            SnapshotRef::Chrome { .. } => ScopeInput::Chrome(parsed),
            SnapshotRef::Uia { .. } => ScopeInput::Uia(parsed),
        }
    } else if let Some(raw) = &req.window {
        ScopeInput::Window(raw.clone())
    } else if let Some(raw) = &req.r#ref {
        ScopeInput::Element(SnapshotRef::parse(raw)?)
    } else {
        return Err(invalid_scope("no scope"));
    };

    if req.subtree && !matches!(&scope, ScopeInput::Chrome(_) | ScopeInput::Uia(_)) {
        return Err(invalid_scope(
            "--subtree requires --within (the ref's own element is the root)",
        ));
    }
    if req.depth.is_some() && !scope.is_uia_list() {
        return Err(invalid_filter("--depth is UIA-only"));
    }
    if req.rect.is_some() && !scope.is_uia_list() {
        return Err(invalid_filter(
            "--rect is UIA-only; use --rect-css for Chrome",
        ));
    }
    if req.rect_css.is_some() && !scope.is_chrome_list() {
        return Err(invalid_filter(
            "--rect-css is Chrome-only; use --rect for UIA",
        ));
    }
    if matches!(scope, ScopeInput::Element(_)) && has_list_filter(req) {
        return Err(invalid_filter(
            "filters apply to list mode only (--within/--window)",
        ));
    }
    if let Some(text) = &req.text
        && text.chars().count() > FIND_TEXT_MAX_CHARS
    {
        return Err(invalid_filter(&format!(
            "--text is at most {FIND_TEXT_MAX_CHARS} characters"
        )));
    }

    let filters = FindFilters {
        role: req.role.clone(),
        text: req.text.clone(),
        rect_css: req.rect_css.as_deref().map(parse_rect_css).transpose()?,
        rect: req
            .rect
            .as_deref()
            .map(|r| parse_rect("--rect", r))
            .transpose()?,
        subtree: req.subtree,
        depth: req.depth.map(|_| req.depth()),
    };
    Ok((scope, filters))
}

// ---------------------------------------------------------------- pure predicates

fn is_false(value: &bool) -> bool {
    !*value
}

fn cap_chars(text: &str, cap: usize) -> String {
    text.chars().take(cap).collect()
}

fn css_rect_intersect(a: CssRect, b: CssRect) -> bool {
    let left = a.left.max(b.left);
    let top = a.top.max(b.top);
    let right = (a.left + a.width).min(b.left + b.width);
    let bottom = (a.top + a.height).min(b.top + b.height);
    right > left && bottom > top
}

/// The one Chrome stored-node predicate, shared by the fixture path and pinned against the JS
/// path by `tests/fixtures/find-cases.json`.
pub(crate) fn chrome_row_matches(
    role: &str,
    text: Option<&str>,
    rect_css: Option<CssRect>,
    want_role: Option<&str>,
    want_text: Option<&str>,
    want_rect: Option<CssRect>,
) -> bool {
    if let Some(want) = want_role
        && !role.eq_ignore_ascii_case(want)
    {
        return false;
    }
    if let Some(want) = want_text {
        let haystack = text.unwrap_or("");
        if !cap_chars(haystack, FIND_NAME_CAP)
            .to_ascii_lowercase()
            .contains(&want.to_ascii_lowercase())
        {
            return false;
        }
    }
    if let Some(want) = want_rect {
        let Some(rect) = rect_css else {
            return false;
        };
        if !css_rect_intersect(rect, want) {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------- capability mapping

fn support_from_flag(flag: Option<bool>) -> Support {
    match flag {
        Some(true) => Support::Available,
        Some(false) => Support::Unavailable,
        None => Support::Unknown,
    }
}

fn probe_evidence(available: Option<bool>, token: &str) -> String {
    match available {
        Some(true) => token.to_string(),
        Some(false) => "pattern_null".to_string(),
        None => "probe_timeout".to_string(),
    }
}

fn checked_from_toggle(state: Option<&str>) -> Option<CheckedState> {
    match state {
        Some("on") => Some(CheckedState::Checked),
        Some("off") => Some(CheckedState::Unchecked),
        Some("indeterminate") => Some(CheckedState::Mixed),
        Some(_) => Some(CheckedState::Unknown),
        None => None,
    }
}

fn expanded_from_state(state: Option<&str>) -> Option<ExpandedState> {
    match state {
        Some("expanded") => Some(ExpandedState::Expanded),
        Some("collapsed") => Some(ExpandedState::Collapsed),
        Some("partially_expanded") => Some(ExpandedState::PartiallyExpanded),
        Some("leaf") => Some(ExpandedState::Unavailable),
        Some(_) => Some(ExpandedState::Unknown),
        None => None,
    }
}

fn action(verb: ActionVerb, support: Support, evidence: &str) -> FindAction {
    FindAction {
        action: verb,
        support,
        evidence: evidence.to_string(),
        reason: if support == Support::Unknown {
            Some(evidence.to_string())
        } else {
            None
        },
    }
}

/// Map a read-only UIA probe to the six-verb capability block. `expand`/`collapse` share one
/// pattern query; `unknown` is never `unavailable`.
fn capabilities_from_probe(probe: &crate::uia_actions::CapabilityProbe) -> FindCapabilities {
    let evidence = |flag: Option<bool>, token: &str| {
        if flag.is_none() && probe.denied {
            "probe_denied".to_string()
        } else {
            probe_evidence(flag, token)
        }
    };
    let mut actions = vec![
        action(
            ActionVerb::Invoke,
            support_from_flag(probe.invoke),
            &evidence(probe.invoke, "invoke_pattern"),
        ),
        action(
            ActionVerb::Toggle,
            support_from_flag(probe.toggle),
            &evidence(probe.toggle, "toggle_pattern"),
        ),
        action(
            ActionVerb::Expand,
            support_from_flag(probe.expand_collapse),
            &evidence(probe.expand_collapse, "expand_collapse_pattern"),
        ),
        action(
            ActionVerb::Collapse,
            support_from_flag(probe.expand_collapse),
            &evidence(probe.expand_collapse, "expand_collapse_pattern"),
        ),
        action(
            ActionVerb::Select,
            support_from_flag(probe.selection_item),
            &evidence(probe.selection_item, "selection_item_pattern"),
        ),
        action(
            ActionVerb::SetValue,
            support_from_flag(probe.value),
            &evidence(probe.value, "value_pattern"),
        ),
    ];
    // `evidence` already produced reason tokens; unknown actions carry the probe reason.
    for a in actions.iter_mut() {
        if a.support == Support::Unknown && a.reason.is_none() {
            a.reason = Some(a.evidence.clone());
        }
    }
    let mut notes = vec!["probe_read_only".to_string()];
    if probe.value == Some(true) {
        notes.push("set_value_role_gate_en_only".to_string());
    }
    if probe.offscreen == Some(true) {
        notes.push("offscreen".to_string());
    } else if probe.enabled == Some(false) {
        notes.push("disabled".to_string());
    }
    notes.truncate(NOTES_CAP);
    FindCapabilities {
        source: CapSource::UiaPattern,
        actions,
        state: FindState {
            enabled: probe.enabled,
            offscreen: probe.offscreen,
            read_only: probe.read_only,
            checked: checked_from_toggle(probe.toggle_state.as_deref()),
            expanded: expanded_from_state(probe.expand_state.as_deref()),
            selected: probe.selected,
        },
        notes,
    }
}

/// All six verbs unknown with a named timeout. Used when the merged resolve+probe worker times
/// out (the identity report is also `unknown`).
fn capabilities_for_probe_timeout() -> FindCapabilities {
    let verbs = [
        ActionVerb::Invoke,
        ActionVerb::Toggle,
        ActionVerb::Expand,
        ActionVerb::Collapse,
        ActionVerb::Select,
        ActionVerb::SetValue,
    ];
    FindCapabilities {
        source: CapSource::UiaPattern,
        actions: verbs
            .into_iter()
            .map(|verb| action(verb, Support::Unknown, "probe_timeout"))
            .collect(),
        state: FindState::default(),
        notes: vec!["probe_read_only".to_string()],
    }
}

/// DOM capability mapping. Chrome never reports `unavailable`: markup cannot prove a control is
/// not wired to a delegated handler, so an `unavailable` token from the wire degrades to
/// `unknown` (and stays visible as such).
fn capabilities_from_dom(value: &serde_json::Value) -> Option<FindCapabilities> {
    let obj = value.as_object()?;
    let raw_actions = obj.get("actions")?.as_array()?;
    let mut actions = Vec::new();
    for entry in raw_actions {
        let Some(name) = entry.get("action").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(verb) = parse_action_verb(name) else {
            continue;
        };
        let support = match entry.get("support").and_then(|v| v.as_str()) {
            Some("available") => Support::Available,
            // Deliberate: DOM can never prove absence of a delegated handler.
            _ => Support::Unknown,
        };
        let evidence = entry
            .get("evidence")
            .and_then(|v| v.as_str())
            .unwrap_or("no_markup_evidence")
            .to_string();
        actions.push(action(verb, support, &evidence));
    }
    if actions.is_empty() {
        return None;
    }
    let state: FindState = obj
        .get("state")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .ok()
        .flatten()
        .unwrap_or_default();
    let mut notes = obj
        .get("notes")
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    notes.truncate(NOTES_CAP);
    Some(FindCapabilities {
        source: CapSource::Dom,
        actions,
        state,
        notes,
    })
}

fn parse_action_verb(name: &str) -> Option<ActionVerb> {
    match name {
        "invoke" => Some(ActionVerb::Invoke),
        "toggle" => Some(ActionVerb::Toggle),
        "expand" => Some(ActionVerb::Expand),
        "collapse" => Some(ActionVerb::Collapse),
        "select" => Some(ActionVerb::Select),
        "set_value" | "set-value" => Some(ActionVerb::SetValue),
        _ => None,
    }
}

// ---------------------------------------------------------------- envelope helpers

fn hint_for_reason(reason: &str) -> String {
    match reason {
        "no_snapshot" => "run observe first; use a ref from that envelope".to_string(),
        "stale_document" | "stale_snapshot" | "stale_element" | "stale_identity" => {
            "re-observe and use the new ref".to_string()
        }
        "unsupported_op" => {
            "reload the Helping Hands extension (0.4.0 or newer), then retry".to_string()
        }
        "frame_unsupported" => {
            "frame scope is disabled in this process (HANDS_FRAMES=0); run without it".to_string()
        }
        "tab_not_active" | "window_not_focused" | "tab_hidden" => {
            "activate the target tab in the focused Chrome window, then retry".to_string()
        }
        "not_chrome" => "activate the target Chrome window, then retry".to_string(),
        "window_unreadable" => "pass a live --window hwnd:/title or a UIA ref".to_string(),
        "provider_timeout" | "resolve_timeout" | "host_down" => {
            "the provider did not answer in time; re-observe and retry".to_string()
        }
        "fixture_unavailable" => {
            "fixture mode cannot answer containment; query the live snapshot".to_string()
        }
        _ => "re-observe; the scope could not be read".to_string(),
    }
}

fn scope_from_ref(r: &SnapshotRef, subtree: bool) -> FindScope {
    match r {
        SnapshotRef::Chrome {
            tab,
            frame,
            doc,
            snap,
            ..
        } => FindScope {
            kind: if subtree {
                ScopeKind::ChromeSubtree
            } else {
                ScopeKind::ChromeSnapshot
            },
            tab: Some(*tab),
            window: None,
            frame: Some(*frame),
            doc: Some(doc.clone()),
            snap: Some(snap.clone()),
            hwnd: None,
            retained: None,
            nodes_visited: None,
            node_cap: None,
            depth_cap: None,
        },
        SnapshotRef::Uia { hwnd, .. } => FindScope {
            kind: if subtree {
                ScopeKind::UiaSubtree
            } else {
                ScopeKind::UiaWindow
            },
            tab: None,
            window: None,
            frame: None,
            doc: None,
            snap: None,
            hwnd: Some(format!("{hwnd:x}")),
            retained: None,
            nodes_visited: None,
            node_cap: None,
            depth_cap: None,
        },
    }
}

fn scope_failure(
    session_id: &str,
    filters: FindFilters,
    scope: Option<FindScope>,
    reason: &str,
) -> FindEnvelope {
    FindEnvelope {
        session_id: session_id.to_string(),
        ok: false,
        scope,
        coverage: FindCoverage {
            scope_source: ScopeSource::None,
            examined: 0,
            scope_total: None,
            scope_complete: false,
            page_complete: false,
            reason: Some(reason.to_string()),
        },
        filters,
        matches_total: None,
        matches_returned: 0,
        matches_truncated: false,
        offset: 0,
        paging: Paging::Snapshot,
        matches: Vec::new(),
        element: None,
        identity: None,
        capabilities: None,
        assessment: None,
        error: Some(format!("find: {reason}")),
        hint: Some(hint_for_reason(reason)),
    }
}

// ---------------------------------------------------------------- run

/// Run a `find` request. Configuration errors are hard (`invalid_scope` / `invalid_filter`);
/// a scope that cannot be read is `ok:false` with a named reason; an identity refusal in exact
/// mode is `ok:true` with `element:null`.
pub fn run(req: FindRequest) -> Result<FindEnvelope, HandsError> {
    let session_id = crate::session::resolve_session_id_from_os(req.session_id.as_deref());
    let (scope, filters) = validate(&req)?;
    let envelope = match &scope {
        ScopeInput::Chrome(r) => run_chrome_list(&session_id, r, &filters, &req),
        ScopeInput::Uia(r) => run_uia_list(&session_id, r, &filters, &req),
        ScopeInput::Window(raw) => run_uia_window(&session_id, raw, &filters, &req),
        ScopeInput::Element(r) => run_exact(&session_id, r, &filters, &req),
    };
    finalize(envelope)
}

fn frame_allowed(r: &SnapshotRef) -> bool {
    r.frame() == 0 || crate::frames::enabled()
}

fn run_chrome_list(
    session_id: &str,
    r: &SnapshotRef,
    filters: &FindFilters,
    req: &FindRequest,
) -> FindEnvelope {
    if !frame_allowed(r) {
        return scope_failure(
            session_id,
            filters.clone(),
            Some(scope_from_ref(r, filters.subtree)),
            "frame_unsupported",
        );
    }
    let reply = crate::chrome::query_scope(
        r,
        filters.role.as_deref(),
        filters.text.as_deref(),
        filters.rect_css,
        filters.subtree,
        req.limit(),
        req.offset(),
        req.budget_ms(),
        req.chrome_max_nodes(),
    );
    let reply = match reply {
        Ok(reply) => reply,
        Err(report) => {
            let reason = report
                .reason
                .clone()
                .unwrap_or_else(|| "protocol_error".into());
            let mut envelope = scope_failure(
                session_id,
                filters.clone(),
                Some(scope_from_ref(r, filters.subtree)),
                &reason,
            );
            envelope.identity = Some(report);
            return envelope;
        }
    };
    let frame = reply.frame.unwrap_or_else(|| r.frame());
    let matches = reply
        .matches
        .iter()
        .map(|row| {
            let idx = row.idx;
            let r#ref = idx
                .map(|idx| {
                    SnapshotRef::Chrome {
                        tab: reply.tab.unwrap_or(0),
                        frame,
                        doc: reply.doc.clone(),
                        snap: reply.snap.clone(),
                        idx,
                    }
                    .to_string()
                })
                .unwrap_or_else(|| row.id.clone());
            FindMatch {
                r#ref,
                idx,
                role: cap_chars(&row.role, FIND_NAME_CAP),
                text: row.text.as_deref().map(|t| cap_chars(t, FIND_NAME_CAP)),
                rect_css: row.rect_css,
                rect: None,
                frame: if frame > 0 { Some(frame) } else { None },
                depth: row.depth,
                key_ok: Some(row.key_ok.unwrap_or(false)),
                reason: if !row.connected {
                    Some("stale_element".to_string())
                } else if row.key_ok == Some(false) {
                    Some("stale_identity".to_string())
                } else {
                    None
                },
            }
        })
        .collect::<Vec<_>>();
    let matches_returned = matches.len();
    let matched_total = reply.matched_total;
    let scope_total = Some(reply.retained);
    let scope_complete = !reply.budget_hit && reply.examined >= reply.retained;
    let reason = if reply.budget_hit {
        Some("budget_truncated".to_string())
    } else if reply.examined < reply.retained {
        Some("node_truncated".to_string())
    } else {
        None
    };
    FindEnvelope {
        session_id: session_id.to_string(),
        ok: true,
        scope: Some(FindScope {
            kind: if filters.subtree {
                ScopeKind::ChromeSubtree
            } else {
                ScopeKind::ChromeSnapshot
            },
            tab: reply.tab,
            window: reply.window,
            frame: Some(frame),
            doc: Some(reply.doc.clone()),
            snap: Some(reply.snap.clone()),
            hwnd: None,
            retained: Some(reply.retained),
            nodes_visited: None,
            node_cap: None,
            depth_cap: None,
        }),
        coverage: FindCoverage {
            scope_source: if reply.fixture {
                ScopeSource::Fixture
            } else {
                ScopeSource::StoredSnapshot
            },
            examined: reply.examined,
            scope_total,
            scope_complete,
            page_complete: false,
            reason,
        },
        filters: filters.clone(),
        matches_total: Some(matched_total),
        matches_returned,
        matches_truncated: matched_total > req.offset() + matches_returned,
        offset: req.offset(),
        paging: Paging::Snapshot,
        matches,
        element: None,
        identity: None,
        capabilities: None,
        assessment: None,
        error: None,
        hint: None,
    }
}

fn uia_hwnd_from_ref(r: &SnapshotRef) -> Option<isize> {
    match r {
        SnapshotRef::Uia { hwnd, .. } => isize::try_from(*hwnd).ok().filter(|h| *h > 0),
        SnapshotRef::Chrome { .. } => None,
    }
}

fn run_uia_list(
    session_id: &str,
    r: &SnapshotRef,
    filters: &FindFilters,
    req: &FindRequest,
) -> FindEnvelope {
    let SnapshotRef::Uia { fp, ridh, .. } = r else {
        return scope_failure(
            session_id,
            filters.clone(),
            Some(scope_from_ref(r, filters.subtree)),
            "invalid_scope",
        );
    };
    let Some(hwnd) = uia_hwnd_from_ref(r) else {
        return scope_failure(
            session_id,
            filters.clone(),
            Some(scope_from_ref(r, filters.subtree)),
            "window_unreadable",
        );
    };
    let root = filters.subtree.then_some((ridh.as_str(), fp.as_str()));
    uia_list_common(
        session_id,
        hwnd,
        root,
        filters,
        req,
        scope_from_ref(r, filters.subtree),
    )
}

fn run_uia_window(
    session_id: &str,
    raw: &str,
    filters: &FindFilters,
    req: &FindRequest,
) -> FindEnvelope {
    let windows = crate::foreground::titled_windows();
    let resolved = crate::observe::resolve_window(raw, &windows);
    let window = match resolved {
        Ok(window) => window,
        Err(_) => {
            return scope_failure(session_id, filters.clone(), None, "window_unreadable");
        }
    };
    let scope = FindScope {
        kind: ScopeKind::UiaWindow,
        tab: None,
        window: None,
        frame: None,
        doc: None,
        snap: None,
        hwnd: Some(format!("{:x}", window.hwnd)),
        retained: None,
        nodes_visited: None,
        node_cap: None,
        depth_cap: Some(req.depth()),
    };
    uia_list_common(session_id, window.hwnd, None, filters, req, scope)
}

fn uia_list_common(
    session_id: &str,
    hwnd: isize,
    root: Option<(&str, &str)>,
    filters: &FindFilters,
    req: &FindRequest,
    mut scope: FindScope,
) -> FindEnvelope {
    let max_nodes = req.max_nodes();
    let depth = req.depth();
    scope.node_cap = Some(max_nodes);
    scope.depth_cap = Some(depth);
    let outcome = crate::uia::query_scope(
        hwnd,
        root,
        crate::uia::QueryFilters {
            role: filters.role.clone(),
            text: filters.text.clone(),
            rect: filters.rect,
        },
        max_nodes,
        depth,
        std::time::Duration::from_millis(req.timeout_ms()),
    );
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(failure) => {
            let mut envelope =
                scope_failure(session_id, filters.clone(), Some(scope), failure.reason);
            envelope.coverage.reason = Some(failure.reason.to_string());
            return envelope;
        }
    };
    let hwnd_u64 = u64::try_from(hwnd).ok();
    let mut matches = Vec::new();
    for row in &outcome.rows {
        let Some(hwnd) = hwnd_u64 else {
            break;
        };
        let (text, unnamed) = crate::extract::name_fields(&row.own_name, row.is_password);
        let Some(r#ref) = crate::identity::uia_ref_from_parts(
            hwnd,
            &row.own_role,
            text.as_deref(),
            unnamed,
            &row.runtime_id,
        ) else {
            continue;
        };
        matches.push(FindMatch {
            r#ref,
            idx: None,
            role: cap_chars(&row.role, FIND_NAME_CAP),
            text: if row.is_password || row.own_name.is_empty() {
                None
            } else {
                Some(cap_chars(&row.own_name, FIND_NAME_CAP))
            },
            rect_css: None,
            rect: Some(row.rect),
            frame: None,
            depth: Some(row.depth),
            key_ok: None,
            reason: None,
        });
    }
    let matched_total = matches.len();
    let offset = req.offset();
    let limit = req.limit();
    let returned = matches
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    let matches_returned = returned.len();
    scope.nodes_visited = Some(outcome.examined);
    FindEnvelope {
        session_id: session_id.to_string(),
        ok: true,
        scope: Some(scope),
        coverage: FindCoverage {
            scope_source: ScopeSource::LiveWalk,
            examined: outcome.examined,
            scope_total: None,
            scope_complete: outcome.complete,
            page_complete: false,
            reason: outcome.reason.map(str::to_string),
        },
        filters: filters.clone(),
        matches_total: Some(matched_total),
        matches_returned,
        matches_truncated: matched_total > offset + matches_returned,
        offset,
        paging: Paging::BestEffort,
        matches: returned,
        element: None,
        identity: None,
        capabilities: None,
        assessment: None,
        error: None,
        hint: None,
    }
}

fn exact_scope(r: &SnapshotRef) -> FindScope {
    let mut scope = scope_from_ref(r, false);
    scope.kind = ScopeKind::Element;
    scope
}

fn run_exact(
    session_id: &str,
    r: &SnapshotRef,
    filters: &FindFilters,
    req: &FindRequest,
) -> FindEnvelope {
    match r {
        SnapshotRef::Chrome { .. } => run_exact_chrome(session_id, r, filters, req),
        SnapshotRef::Uia { hwnd, fp, ridh } => {
            let deadline = std::time::Duration::from_millis(req.timeout_ms());
            let hwnd_num = *hwnd;
            if req.no_caps {
                match crate::uia::resolve_scoped(
                    isize::try_from(*hwnd).unwrap_or(0),
                    ridh,
                    fp,
                    deadline,
                ) {
                    Ok(resolved) => {
                        exact_uia_envelope(session_id, r, filters, resolved, None, hwnd_num)
                    }
                    Err(failure) => exact_identity_refusal(
                        session_id,
                        r,
                        filters,
                        IdentityReport::uia_failure(r, failure.state, failure.reason),
                    ),
                }
            } else {
                match crate::uia_actions::probe_scoped_target(
                    isize::try_from(*hwnd).unwrap_or(0),
                    ridh,
                    fp,
                    deadline,
                ) {
                    Ok((resolved, probe)) => {
                        let caps = capabilities_from_probe(&probe);
                        exact_uia_envelope(session_id, r, filters, resolved, Some(caps), hwnd_num)
                    }
                    Err(failure) => {
                        // One worker, one deadline. A provider timeout leaves the identity
                        // unknown and every capability unknown; a resolve refusal leaves
                        // nothing to describe (capabilities stay null).
                        let mut envelope = exact_identity_refusal(
                            session_id,
                            r,
                            filters,
                            IdentityReport::uia_failure(r, failure.state, failure.reason),
                        );
                        if failure.reason == "provider_timeout" {
                            envelope.capabilities = Some(capabilities_for_probe_timeout());
                        }
                        envelope
                    }
                }
            }
        }
    }
}

fn exact_uia_envelope(
    session_id: &str,
    r: &SnapshotRef,
    filters: &FindFilters,
    resolved: crate::uia::ResolvedElement,
    capabilities: Option<FindCapabilities>,
    hwnd: u64,
) -> FindEnvelope {
    let element = FindElement {
        r#ref: r.to_string(),
        id: None,
        role: cap_chars(&resolved.role, FIND_NAME_CAP),
        text: if resolved.is_password || resolved.own_name.is_empty() {
            None
        } else {
            Some(cap_chars(&resolved.own_name, FIND_NAME_CAP))
        },
        rect_css: None,
        rect: Some(resolved.rect),
        frame: None,
        href: None,
        own_name: if resolved.own_name.is_empty() {
            None
        } else {
            Some(cap_chars(&resolved.own_name, FIND_NAME_CAP))
        },
    };
    FindEnvelope {
        session_id: session_id.to_string(),
        ok: true,
        scope: Some(FindScope {
            kind: ScopeKind::Element,
            tab: None,
            window: Some(hwnd as u32),
            frame: None,
            doc: None,
            snap: None,
            hwnd: Some(format!("{hwnd:x}")),
            retained: None,
            nodes_visited: None,
            node_cap: None,
            depth_cap: None,
        }),
        coverage: FindCoverage {
            scope_source: ScopeSource::LiveWalk,
            examined: 1,
            scope_total: Some(1),
            scope_complete: true,
            page_complete: false,
            reason: None,
        },
        filters: filters.clone(),
        matches_total: None,
        matches_returned: 0,
        matches_truncated: false,
        offset: 0,
        paging: Paging::BestEffort,
        matches: Vec::new(),
        element: Some(element),
        identity: Some(IdentityReport::exact_uia(r)),
        capabilities,
        assessment: None,
        error: None,
        hint: None,
    }
}

fn exact_identity_refusal(
    session_id: &str,
    r: &SnapshotRef,
    filters: &FindFilters,
    report: IdentityReport,
) -> FindEnvelope {
    FindEnvelope {
        session_id: session_id.to_string(),
        ok: true,
        scope: Some(exact_scope(r)),
        coverage: FindCoverage {
            scope_source: ScopeSource::None,
            examined: 0,
            scope_total: Some(1),
            scope_complete: false,
            page_complete: false,
            reason: None,
        },
        filters: filters.clone(),
        matches_total: None,
        matches_returned: 0,
        matches_truncated: false,
        offset: 0,
        paging: Paging::BestEffort,
        matches: Vec::new(),
        element: None,
        identity: Some(report),
        capabilities: None,
        assessment: None,
        error: None,
        hint: None,
    }
}

fn run_exact_chrome(
    session_id: &str,
    r: &SnapshotRef,
    filters: &FindFilters,
    req: &FindRequest,
) -> FindEnvelope {
    if !frame_allowed(r) {
        return scope_failure(
            session_id,
            filters.clone(),
            Some(exact_scope(r)),
            "frame_unsupported",
        );
    }
    let hwnd = crate::foreground::foreground_hwnd();
    let result = crate::chrome::try_resolve_ref_with_caps(
        r,
        hwnd,
        req.assess,
        Some(crate::hittest::DEFAULT_SAMPLES),
    );
    let hit = match result {
        Ok(hit) => hit,
        Err(report) => return exact_identity_refusal(session_id, r, filters, report),
    };
    let (tab, frame, doc, snap, idx) = match r {
        SnapshotRef::Chrome {
            tab,
            frame,
            doc,
            snap,
            idx,
        } => (*tab, *frame, doc.clone(), snap.clone(), *idx),
        SnapshotRef::Uia { .. } => {
            return exact_identity_refusal(
                session_id,
                r,
                filters,
                IdentityReport::unknown(Some(r), "protocol_error"),
            );
        }
    };
    let window = hit.identity.as_ref().and_then(|i| i.window);
    let rect = if hit.contract.transform.valid && hit.rect.area() > 0 {
        Some(hit.rect)
    } else {
        None
    };
    let element = FindElement {
        r#ref: r.to_string(),
        // `chr:` is a top-frame-only legacy handle; a frame element keeps only the ref.
        id: (frame == 0).then(|| format!("chr:{idx}")),
        role: cap_chars(&hit.role, FIND_NAME_CAP),
        text: if hit.name.is_empty() {
            None
        } else {
            Some(cap_chars(&hit.name, FIND_NAME_CAP))
        },
        rect_css: Some(hit.css_rect),
        rect,
        frame: (frame > 0).then_some(frame),
        href: hit.href.clone(),
        own_name: None,
    };
    let capabilities = if req.no_caps {
        None
    } else {
        hit.capability.as_ref().and_then(capabilities_from_dom)
    };
    FindEnvelope {
        session_id: session_id.to_string(),
        ok: true,
        scope: Some(FindScope {
            kind: ScopeKind::Element,
            tab: Some(tab),
            window,
            frame: Some(frame),
            doc: Some(doc),
            snap: Some(snap),
            hwnd: None,
            retained: None,
            nodes_visited: None,
            node_cap: None,
            depth_cap: None,
        }),
        coverage: FindCoverage {
            scope_source: ScopeSource::StoredSnapshot,
            examined: 1,
            scope_total: Some(1),
            scope_complete: true,
            page_complete: false,
            reason: None,
        },
        filters: filters.clone(),
        matches_total: None,
        matches_returned: 0,
        matches_truncated: false,
        offset: 0,
        paging: Paging::Snapshot,
        matches: Vec::new(),
        element: Some(element),
        identity: hit.identity.clone(),
        capabilities,
        assessment: hit.assessment,
        error: None,
        hint: None,
    }
}

// ---------------------------------------------------------------- budget / finalize

fn serialized_len(envelope: &FindEnvelope) -> Result<usize, HandsError> {
    serde_json::to_string(envelope)
        .map(|json| json.len())
        .map_err(|err| HandsError::Chrome(format!("find envelope serialize: {err}")))
}

/// Shrink order (closed): omit absent optionals (serde) → capability notes → hint →
/// assessment diagnostics → declared tail truncation. Never renames `ref`, never drops
/// `scope`/`coverage`/`filters`/counters, never returns an empty page while matches remain.
pub fn finalize(mut envelope: FindEnvelope) -> Result<FindEnvelope, HandsError> {
    let target = crate::observe::DEFAULT_ENVELOPE_MAX_BYTES;
    if serialized_len(&envelope)? <= target {
        return Ok(envelope);
    }
    if let Some(caps) = envelope.capabilities.as_mut() {
        caps.notes.clear();
    }
    if serialized_len(&envelope)? <= target {
        return Ok(envelope);
    }
    envelope.hint = None;
    if serialized_len(&envelope)? <= target {
        return Ok(envelope);
    }
    if let Some(assessment) = envelope.assessment.as_mut() {
        assessment.blocked_by.clear();
        assessment.clipped_by = None;
        assessment.notes.clear();
    }
    if serialized_len(&envelope)? <= target {
        return Ok(envelope);
    }
    let floor = envelope.matches.len().min(FIND_MIN_RETURNED_MATCHES);
    while envelope.matches.len() > floor && serialized_len(&envelope)? > target {
        envelope.matches.pop();
    }
    while envelope.matches.len() > 1 && serialized_len(&envelope)? > target {
        envelope.matches.pop();
    }
    envelope.matches_returned = envelope.matches.len();
    envelope.matches_truncated = envelope
        .matches_total
        .is_some_and(|total| total > envelope.offset + envelope.matches_returned)
        || envelope.matches.len() < floor;
    if serialized_len(&envelope)? > target {
        // The page is already minimal; a pathological fixed head is not shrinkable.
    }
    let hard = crate::observe::ENVELOPE_MAX_BYTES;
    let len = serialized_len(&envelope)?;
    if len > hard {
        return Err(HandsError::Chrome(format!(
            "find envelope is {len} bytes (hard max {hard})"
        )));
    }
    Ok(envelope)
}

pub fn serialize(envelope: &FindEnvelope) -> Result<String, HandsError> {
    serde_json::to_string(envelope)
        .map_err(|err| HandsError::Chrome(format!("find envelope serialize: {err}")))
}

// ---------------------------------------------------------------- token hygiene

/// Every reason/evidence/note token the envelope can emit, for the lowercase snake_case test.
pub const CLOSED_TOKENS: &[&str] = &[
    "stale_document",
    "stale_snapshot",
    "stale_element",
    "stale_identity",
    "not_visible",
    "tab_unavailable",
    "no_content",
    "no_tab",
    "not_chrome",
    "host_down",
    "resolve_timeout",
    "protocol_error",
    "window_unreadable",
    "unbound_extension",
    "window_mismatch",
    "duplicate_runtime_id",
    "duplicate_identity",
    "frame_unsupported",
    "frame_link_missing",
    "frame_link_ambiguous",
    "frame_geometry_unavailable",
    "frame_probe_timeout",
    "frame_budget_exceeded",
    "tab_not_active",
    "window_not_focused",
    "tab_hidden",
    "no_snapshot",
    "unsupported_op",
    "fixture_unavailable",
    "invalid_filter",
    "invalid_scope",
    "malformed_ref",
    "unsupported_ref_version",
    "budget_truncated",
    "node_truncated",
    "depth_truncated",
    "provider_timeout",
    "pattern_null",
    "probe_timeout",
    "probe_denied",
    "no_markup_evidence",
    "dom_advertised_not_proven",
    "set_value_role_gate_en_only",
    "probe_read_only",
    "offscreen",
    "disabled",
    "invoke_pattern",
    "toggle_pattern",
    "expand_collapse_pattern",
    "selection_item_pattern",
    "value_pattern",
    "role_button",
    "role_markup",
    "role_checkbox",
    "role_expand",
    "role_selection",
    "aria_checked",
    "aria_expanded",
    "aria_selected",
    "editable_role",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome_ref(tab: u32, frame: u32, doc: &str, snap: &str, idx: u32) -> SnapshotRef {
        SnapshotRef::Chrome {
            tab,
            frame,
            doc: doc.to_string(),
            snap: snap.to_string(),
            idx,
        }
    }

    fn request(within: &str) -> FindRequest {
        FindRequest {
            within: Some(within.to_string()),
            ..FindRequest::default()
        }
    }

    fn dense_match(index: usize) -> FindMatch {
        FindMatch {
            r#ref: format!("sref1:c.c.0.9f3a.a1b2c3d4.{}", base36ish(index as u32 + 1)),
            idx: Some(index as u32),
            role: "Button".into(),
            text: Some(
                "A very long dense control label that fills the sixty four char cap 0123".into(),
            ),
            rect_css: Some(CssRect {
                left: 100.0,
                top: 50.0,
                width: 200.0,
                height: 40.0,
            }),
            rect: None,
            frame: None,
            depth: None,
            key_ok: Some(true),
            reason: None,
        }
    }

    fn base36ish(value: u32) -> String {
        let mut v = value;
        if v == 0 {
            return "0".into();
        }
        let mut out = Vec::new();
        while v > 0 {
            let d = (v % 36) as u8;
            out.push(if d < 10 {
                (b'0' + d) as char
            } else {
                (b'a' + d - 10) as char
            });
            v /= 36;
        }
        out.iter().rev().collect()
    }

    fn dense_envelope(count: usize) -> FindEnvelope {
        FindEnvelope {
            session_id: "s-0125".into(),
            ok: true,
            scope: Some(FindScope {
                kind: ScopeKind::ChromeSnapshot,
                tab: Some(12),
                window: Some(45),
                frame: Some(0),
                doc: Some("9f3a".into()),
                snap: Some("a1b2c3d4".into()),
                hwnd: None,
                retained: Some(312),
                nodes_visited: None,
                node_cap: None,
                depth_cap: None,
            }),
            coverage: FindCoverage {
                scope_source: ScopeSource::Fixture,
                examined: 312,
                scope_total: Some(312),
                scope_complete: true,
                page_complete: false,
                reason: None,
            },
            filters: FindFilters::default(),
            matches_total: Some(count),
            matches_returned: count,
            matches_truncated: false,
            offset: 0,
            paging: Paging::Snapshot,
            matches: (0..count).map(dense_match).collect(),
            element: None,
            identity: None,
            capabilities: None,
            assessment: None,
            error: None,
            hint: None,
        }
    }

    #[test]
    fn subtree_is_containment_not_index_range() {
        // The JS predicate uses DOM containment on the stored node and skips every node that is
        // not a descendant; it never slices the index range after the root.
        let js = include_str!("../extension/content.js");
        let start = js.find("function queryScoped(").expect("queryScoped");
        let end = js[start..]
            .find("function nodeContains(")
            .map(|i| start + i)
            .unwrap_or(js.len());
        let body = &js[start..end];
        assert!(body.contains("nodeContains(root, node)"));
        assert!(body.contains("node === root"));
        assert!(!body.contains("i > rootIdx"));
        // The wire carries the resolved integer, never a second ref.
        let chrome = include_str!("chrome.rs");
        let qstart = chrome.find("pub fn query_scope(").expect("query_scope");
        let qend = chrome[qstart..]
            .find("fn query_reply_from_json(")
            .map(|i| qstart + i)
            .unwrap_or(chrome.len());
        let qbody = &chrome[qstart..qend];
        assert!(qbody.contains("\"subtreeRoot\""));
        assert!(!qbody.contains("\"op\": \"snapshot\""));
        assert!(!qbody.contains("\"path\""));
    }

    fn set_env(key: &str, value: Option<&str>) {
        unsafe {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }

    #[test]
    fn frame_zero_output_unchanged() {
        let _guard = crate::chrome::EnvGuard::lock();
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/find-cases.json");
        _guard.set_snapshot(Some(&path));
        let envelope = run(FindRequest {
            within: Some("sref1:c.c.0.7a1b.feedface.0".into()),
            session_id: Some("s-frame0".into()),
            ..FindRequest::default()
        })
        .expect("frame-0 run");
        assert!(envelope.ok);
        assert_eq!(envelope.scope.as_ref().and_then(|s| s.frame), Some(0));
        let json: serde_json::Value =
            serde_json::from_str(&serialize(&envelope).expect("json")).expect("value");
        for row in json["matches"].as_array().expect("matches") {
            assert!(
                row.get("frame").is_none(),
                "frame 0 omits the frame key: {row}"
            );
        }

        // The kill switch is per-process and refuses only a nonzero frame.
        set_env("HANDS_FRAMES", Some("0"));
        assert!(frame_allowed(&chrome_ref(12, 0, "9f3a", "a1b2c3d4", 1)));
        let r = chrome_ref(12, 3, "9f3a", "a1b2c3d4", 1);
        let refused = run(FindRequest {
            r#ref: Some(r.to_string()),
            session_id: Some("s-frame0".into()),
            ..FindRequest::default()
        })
        .expect("frame-3 run");
        set_env("HANDS_FRAMES", None);
        assert!(!refused.ok);
        assert_eq!(
            refused.coverage.reason.as_deref(),
            Some("frame_unsupported")
        );
    }

    #[test]
    fn uia_walk_reports_bounds_and_never_absence() {
        crate::uia::set_uia_query_hook(Some(|_hwnd, _root, _nodes, _depth| {
            Ok(crate::uia::QueryScopeOutcome {
                rows: Vec::new(),
                examined: 1500,
                complete: false,
                reason: Some("node_truncated"),
            })
        }));
        let req = request("sref1:u.1a2b3c.7d0e1f22.42a1b2c3");
        let env = run(req).expect("run");
        crate::uia::set_uia_query_hook(None);
        assert!(env.ok, "a capped walk is still a read");
        assert!(!env.coverage.scope_complete);
        assert_eq!(env.coverage.reason.as_deref(), Some("node_truncated"));
        assert!(!env.coverage.page_complete, "never absence");
        assert_eq!(env.matches_total, Some(0));
        assert_eq!(env.paging, Paging::BestEffort);
        assert_eq!(env.scope.as_ref().and_then(|s| s.nodes_visited), Some(1500));
    }

    #[test]
    fn request_defaults_and_limits() {
        let req = FindRequest::default();
        assert_eq!(req.limit(), FIND_DEFAULT_LIMIT);
        assert_eq!(req.offset(), 0);
        assert_eq!(req.budget_ms(), FIND_DEFAULT_BUDGET_MS);
        assert_eq!(req.max_nodes(), FIND_DEFAULT_UIA_NODES);
        assert_eq!(req.depth(), FIND_DEFAULT_DEPTH);
        assert_eq!(req.timeout_ms(), FIND_UIA_DEFAULT_DEADLINE_MS);
        let clamped = FindRequest {
            limit: Some(0),
            budget_ms: Some(99_999),
            max_nodes: Some(99_999),
            depth: Some(0),
            timeout_ms: Some(99_999),
            ..FindRequest::default()
        };
        assert_eq!(clamped.limit(), 1);
        assert_eq!(clamped.budget_ms(), FIND_MAX_BUDGET_MS);
        assert_eq!(clamped.max_nodes(), FIND_MAX_UIA_NODES);
        assert_eq!(clamped.depth(), 1);
        assert_eq!(clamped.timeout_ms(), FIND_UIA_MAX_DEADLINE_MS);
        let wide = FindRequest {
            limit: Some(99_999),
            ..FindRequest::default()
        };
        assert_eq!(wide.limit(), FIND_MAX_LIMIT);
    }

    #[test]
    fn closed_tokens_are_lowercase_snake_case() {
        assert!(!CLOSED_TOKENS.is_empty());
        for token in CLOSED_TOKENS {
            assert!(
                token
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "{token}"
            );
            assert!(!token.starts_with('_') && !token.ends_with('_'), "{token}");
        }
        assert_eq!(ActionVerb::SetValue.as_str(), "set_value");
    }

    fn slice_between<'a>(src: &'a str, start: &str, end: &str) -> &'a str {
        let from = src.find(start).unwrap_or_else(|| panic!("missing {start}"));
        let to = src[from..]
            .find(end)
            .map(|i| from + i)
            .unwrap_or_else(|| panic!("missing {end} after {start}"));
        &src[from..to]
    }

    /// Every double-quoted snake_case literal in a capability emitter body. Action verbs are the
    /// only non-token entries allowed; anything else must be in `CLOSED_TOKENS`.
    fn emitter_tokens(body: &str) -> Vec<String> {
        let bytes = body.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] != b'"' {
                i += 1;
                continue;
            }
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end] != b'"' {
                end += 1;
            }
            if end >= bytes.len() {
                break;
            }
            let token = &body[start..end];
            if token.contains('_')
                && token
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            {
                out.push(token.to_string());
            }
            i = end + 1;
        }
        out.sort();
        out.dedup();
        out
    }

    fn js_role_table(js: &str, name: &str) -> Vec<String> {
        let start_marker = format!("const {name} = [");
        let block = slice_between(js, &start_marker, "];");
        let mut out = Vec::new();
        for piece in block[start_marker.len()..].split(',') {
            let piece = piece.trim();
            if let Some(value) = piece.strip_prefix('"').and_then(|p| p.strip_suffix('"')) {
                out.push(value.to_string());
            }
        }
        out
    }

    #[test]
    fn capability_evidence_tokens_are_closed_and_unified() {
        let js = include_str!("../extension/content.js");
        let rust = include_str!("chrome.rs");
        let js_body = slice_between(
            js,
            "function domCapability(",
            "function isPostcheckRendered(",
        );
        let rust_body = slice_between(
            rust,
            "fn fixture_capability(",
            "/// Frame pre-dispatch probe evidence",
        );

        let allowed_non_tokens = ["set_value"];
        for (label, body) in [("content.js", js_body), ("chrome.rs", rust_body)] {
            for token in emitter_tokens(body) {
                if allowed_non_tokens.contains(&token.as_str()) {
                    continue;
                }
                assert!(
                    CLOSED_TOKENS.contains(&token.as_str()),
                    "{label} emits capability token '{token}' that is absent from CLOSED_TOKENS"
                );
            }
        }

        // Fixture evidence is a role-only subset of live evidence (live adds aria-* evidence).
        let live = emitter_tokens(js_body);
        for token in emitter_tokens(rust_body) {
            assert!(
                live.contains(&token),
                "fixture emits '{token}' but the live DOM predicate never does"
            );
        }
        assert!(!live.contains(&"editable_surface".to_string()));
        assert!(live.contains(&"editable_role".to_string()));

        // One role→verb table shared by live and fixture.
        for (name, want) in [
            ("CAP_INVOKE_ROLES", crate::chrome::CAP_INVOKE_ROLES),
            ("CAP_TOGGLE_ROLES", crate::chrome::CAP_TOGGLE_ROLES),
            ("CAP_EXPAND_ROLES", crate::chrome::CAP_EXPAND_ROLES),
            ("CAP_SELECT_ROLES", crate::chrome::CAP_SELECT_ROLES),
            ("CAP_VALUE_ROLES", crate::chrome::CAP_VALUE_ROLES),
        ] {
            let got = js_role_table(js, name);
            let want = want.iter().map(|s| s.to_string()).collect::<Vec<_>>();
            assert_eq!(got, want, "{name} drift between content.js and chrome.rs");
        }
    }

    #[test]
    fn invalid_filter_is_a_hard_error() {
        let bad_text = FindRequest {
            text: Some("x".repeat(FIND_TEXT_MAX_CHARS + 1)),
            ..request("sref1:c.c.0.9f3a.a1b2c3d4.1")
        };
        let err = validate(&bad_text).expect_err("long text");
        assert!(err.to_string().contains("invalid_filter"), "{err}");

        let depth_on_chrome = FindRequest {
            depth: Some(2),
            ..request("sref1:c.c.0.9f3a.a1b2c3d4.1")
        };
        assert!(
            validate(&depth_on_chrome)
                .expect_err("chrome depth")
                .to_string()
                .contains("invalid_filter")
        );

        let rect_css_on_uia = FindRequest {
            rect_css: Some("0,0,10,10".into()),
            ..request("sref1:u.1a2b3c.7d0e1f22.42a1b2c3")
        };
        assert!(
            validate(&rect_css_on_uia)
                .expect_err("uia rect css")
                .to_string()
                .contains("invalid_filter")
        );

        let bad_rect = FindRequest {
            rect: Some("1,2,3".into()),
            ..request("sref1:u.1a2b3c.7d0e1f22.42a1b2c3")
        };
        assert!(
            validate(&bad_rect)
                .expect_err("bad rect")
                .to_string()
                .contains("invalid_filter")
        );
    }

    #[test]
    fn subtree_with_window_or_ref_is_invalid_scope() {
        let with_ref = FindRequest {
            within: None,
            r#ref: Some("sref1:c.c.0.9f3a.a1b2c3d4.1".into()),
            subtree: true,
            ..FindRequest::default()
        };
        assert!(
            validate(&with_ref)
                .expect_err("ref subtree")
                .to_string()
                .contains("invalid_scope")
        );
        let with_window = FindRequest {
            window: Some("Notepad".into()),
            subtree: true,
            ..FindRequest::default()
        };
        assert!(
            validate(&with_window)
                .expect_err("window subtree")
                .to_string()
                .contains("invalid_scope")
        );
        // Both forms at once is also ambiguous.
        let both = FindRequest {
            within: Some("sref1:c.c.0.9f3a.a1b2c3d4.1".into()),
            r#ref: Some("sref1:c.c.0.9f3a.a1b2c3d4.1".into()),
            ..FindRequest::default()
        };
        assert!(
            validate(&both)
                .expect_err("two scopes")
                .to_string()
                .contains("invalid_scope")
        );
    }

    #[test]
    fn cli_and_mcp_share_one_runner() {
        let main = include_str!("main.rs");
        let mcp = include_str!("mcp.rs");
        assert!(main.contains("hands::find::FindRequest"));
        assert!(main.contains("hands::find::run(req)"));
        assert!(mcp.contains("crate::find::run(req)"));
    }

    #[test]
    fn snapshot_scope_is_read_only() {
        let js = include_str!("../extension/content.js");
        let start = js.find("function queryScoped(").expect("queryScoped");
        let end = js[start..]
            .find("\nfunction ")
            .map(|i| start + i)
            .unwrap_or(js.len());
        let body = &js[start..end];
        for banned in [
            "walk(",
            "buildResolve(",
            "buildSnapshot(",
            "nextSnap(",
            "lastSnapshot =",
            "op: \"snapshot\"",
            "op:\"snapshot\"",
        ] {
            assert!(!body.contains(banned), "query op must not contain {banned}");
        }
        let sw = include_str!("../extension/sw.js");
        assert!(
            sw.contains("op !== \"query\""),
            "the SW must allowlist the query op"
        );
        let find_src = include_str!("find.rs");
        let start = find_src.find("pub fn run(").expect("run");
        let end = find_src[start..]
            .find("fn frame_allowed(")
            .map(|i| start + i)
            .unwrap_or(find_src.len());
        assert!(!find_src[start..end].contains("op: \"snapshot\""));
    }

    #[test]
    fn stale_snapshot_never_queries_the_newer_snapshot() {
        let _guard = crate::chrome::EnvGuard::lock();
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/chrome-scoped.json");
        _guard.set_snapshot(Some(&path));
        let r = chrome_ref(12, 0, "9f3a", "ffffffff", 0);
        let err = crate::chrome::query_scope(&r, None, None, None, false, 20, 0, 150, 4000)
            .expect_err("stale snapshot");
        assert_eq!(err.reason.as_deref(), Some("stale_snapshot"));
    }

    #[test]
    fn no_snapshot_names_observe() {
        let envelope = scope_failure("s-1", FindFilters::default(), None, "no_snapshot");
        assert!(!envelope.ok);
        assert_eq!(envelope.error.as_deref(), Some("find: no_snapshot"));
        let hint = envelope.hint.unwrap_or_default();
        assert!(hint.contains("observe"), "{hint}");
    }

    #[test]
    fn filters_are_anded_and_ordered() {
        assert!(chrome_row_matches(
            "Button",
            Some("Sign in"),
            Some(CssRect {
                left: 0.0,
                top: 0.0,
                width: 100.0,
                height: 40.0
            }),
            Some("button"),
            Some("sign"),
            None
        ));
        assert!(!chrome_row_matches(
            "Button",
            Some("Sign in"),
            None,
            Some("button"),
            Some("out"),
            None
        ));
        assert!(!chrome_row_matches(
            "Hyperlink",
            Some("Sign in"),
            None,
            Some("button"),
            None,
            None
        ));
        assert!(!chrome_row_matches(
            "Button",
            Some("Sign in"),
            Some(CssRect {
                left: 500.0,
                top: 500.0,
                width: 10.0,
                height: 10.0
            }),
            None,
            None,
            Some(CssRect {
                left: 0.0,
                top: 0.0,
                width: 100.0,
                height: 100.0
            })
        ));
    }

    #[test]
    fn offset_and_limit_keep_total_exact() {
        let env = dense_envelope(60);
        assert_eq!(env.matches_total, Some(60));
        let mut paged = env.clone();
        paged.offset = 10;
        paged.matches = paged.matches.into_iter().skip(10).take(20).collect();
        paged.matches_returned = paged.matches.len();
        paged.matches_truncated = true;
        let json = serde_json::to_value(&paged).expect("json");
        assert_eq!(json["matches_total"], 60);
        assert_eq!(json["offset"], 10);
        assert_eq!(json["matches_returned"], 20);
        assert_eq!(json["matches_truncated"], true);
    }

    fn find_cases() -> serde_json::Value {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/find-cases.json");
        serde_json::from_str(&std::fs::read_to_string(path).expect("find-cases")).expect("json")
    }

    #[test]
    fn shared_filter_cases() {
        let doc = find_cases();
        let elements = doc["elements"].as_array().expect("elements");
        let cases = doc["cases"].as_array().expect("cases");
        for case in cases {
            let want_role = case["role"].as_str();
            let want_text = case["text"].as_str();
            let expected = case["expected"]
                .as_array()
                .expect("expected")
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>();
            let mut got = Vec::new();
            for el in elements {
                let rect: CssRect = serde_json::from_value(el["rectCss"].clone()).expect("rect");
                let id = el["id"].as_str().expect("id");
                if chrome_row_matches(
                    el["role"].as_str().unwrap_or(""),
                    el["text"].as_str(),
                    Some(rect),
                    want_role,
                    want_text,
                    None,
                ) {
                    got.push(id);
                }
            }
            assert_eq!(got, expected, "case {}", case["name"]);
        }
    }

    #[test]
    fn dense_fixture_bytes_stay_pinned() {
        // 36-char session id, as used for the recorded DoD-7 numbers.
        const SESSION: &str = "s-0125-dense-bytes-pinned-0123456789";
        let _guard = crate::chrome::EnvGuard::lock();
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/find-dense.json");
        _guard.set_snapshot(Some(&path));
        for (limit, want_bytes, want_returned) in [
            (1usize, 645usize, 1usize),
            (12, 3114, 12),
            (13, 3340, 13),
            (20, 4018, 16),
        ] {
            let envelope = run(FindRequest {
                within: Some("sref1:c.c.0.7a1b.feedface.0".into()),
                role: Some("button".into()),
                limit: Some(limit),
                session_id: Some(SESSION.into()),
                ..FindRequest::default()
            })
            .expect("dense fixture run");
            assert!(envelope.ok);
            assert_eq!(
                envelope.coverage.examined, 60,
                "the stored walk is 60 nodes"
            );
            assert_eq!(envelope.coverage.scope_total, Some(60));
            assert!(envelope.coverage.scope_complete);
            assert_eq!(envelope.matches_total, Some(20));
            assert_eq!(
                envelope.matches_returned, want_returned,
                "limit {limit} returned"
            );
            let len = serialize(&envelope).expect("json").len();
            assert_eq!(len, want_bytes, "limit {limit} byte envelope");
            assert!(len <= crate::observe::DEFAULT_ENVELOPE_MAX_BYTES);
            assert!(envelope.matches_truncated);
        }
    }

    #[test]
    fn dense_match_floor_fits_4kib() {
        let env = finalize(dense_envelope(60)).expect("finalize");
        let json = serialize(&env).expect("json");
        assert!(
            json.len() <= crate::observe::DEFAULT_ENVELOPE_MAX_BYTES,
            "{} bytes",
            json.len()
        );
        assert_eq!(env.matches_total, Some(60), "total is never dropped");
        assert!(
            env.matches_returned >= FIND_MIN_RETURNED_MATCHES,
            "returned {}",
            env.matches_returned
        );
        assert!(env.matches_truncated);
        let dropped = 60 - env.offset - env.matches_returned;
        assert!(dropped > 0, "dropped count is recoverable");
    }

    #[test]
    fn hard_cap_is_16kib() {
        assert_eq!(crate::observe::ENVELOPE_MAX_BYTES, 16_384);
        let mut env = dense_envelope(1);
        env.scope = Some(FindScope {
            kind: ScopeKind::ChromeSnapshot,
            tab: Some(1),
            window: None,
            frame: Some(0),
            doc: Some("a".repeat(20_000)),
            snap: Some("b".repeat(8)),
            hwnd: None,
            retained: Some(1),
            nodes_visited: None,
            node_cap: None,
            depth_cap: None,
        });
        let err = finalize(env).expect_err("hard cap");
        assert!(err.to_string().contains("hard max"), "{err}");
    }

    #[test]
    fn truncation_is_declared_and_recoverable() {
        let env = finalize(dense_envelope(40)).expect("finalize");
        let json: serde_json::Value =
            serde_json::from_str(&serialize(&env).expect("json")).expect("value");
        assert_eq!(json["matches_truncated"], true);
        assert_eq!(json["matches_total"], 40);
        assert_eq!(
            json["matches_returned"].as_u64().unwrap() as usize,
            env.matches_returned
        );
        assert_eq!(
            40 - json["offset"].as_u64().unwrap() as usize
                - json["matches_returned"].as_u64().unwrap() as usize,
            40 - env.matches_returned
        );
        assert!(!env.matches.is_empty());
    }

    #[test]
    fn zero_matches_never_claims_page_absence() {
        let mut env = dense_envelope(0);
        env.matches.clear();
        env.matches_truncated = false;
        let json = serialize(&finalize(env).expect("finalize")).expect("json");
        assert!(json.contains("\"page_complete\":false"), "{json}");
        assert!(!json.contains("matches_truncated\":true"), "{json}");
        assert!(!json.contains("absent"), "{json}");
        let mcp = include_str!("mcp.rs");
        assert!(mcp.contains("no match in this snapshot"));
    }

    #[test]
    fn wire_shape_has_no_compact_alias() {
        let src = include_str!("find.rs");
        assert!(!src.contains("rename = \"r\""));
        let start = src.find("pub struct FindMatch {").expect("FindMatch");
        let end = src[start..]
            .find("}\n")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(body.contains("#[serde(rename = \"ref\")]"));
        assert!(!body.contains("rename = \"r\""));
    }

    #[test]
    fn chrome_never_reports_unavailable() {
        let wire = serde_json::json!({
            "source": "dom",
            "actions": [
                {"action": "invoke", "support": "available", "evidence": "role_button"},
                {"action": "toggle", "support": "unavailable", "evidence": "no_markup_evidence"},
            ],
            "state": {"enabled": true},
            "notes": ["dom_advertised_not_proven"],
        });
        let caps = capabilities_from_dom(&wire).expect("caps");
        assert_eq!(caps.source, CapSource::Dom);
        assert_eq!(caps.actions[0].support, Support::Available);
        assert_eq!(
            caps.actions[1].support,
            Support::Unknown,
            "DOM can never prove a verb is unwired"
        );
        assert!(
            caps.actions
                .iter()
                .all(|a| a.support != Support::Unavailable)
        );
    }

    #[test]
    fn uia_pattern_null_is_unavailable_not_unknown() {
        assert_eq!(support_from_flag(Some(true)), Support::Available);
        assert_eq!(support_from_flag(Some(false)), Support::Unavailable);
        assert_eq!(support_from_flag(None), Support::Unknown);
        let probe = crate::uia_actions::CapabilityProbe {
            invoke: Some(true),
            toggle: Some(false),
            expand_collapse: None,
            selection_item: Some(false),
            value: None,
            enabled: Some(true),
            offscreen: Some(false),
            ..Default::default()
        };
        let caps = capabilities_from_probe(&probe);
        assert_eq!(caps.actions.len(), 6);
        assert_eq!(caps.actions[0].support, Support::Available);
        assert_eq!(caps.actions[1].support, Support::Unavailable);
        assert_eq!(caps.actions[2].support, Support::Unknown);
        assert_eq!(caps.actions[2].evidence, "probe_timeout");
    }

    #[test]
    fn capability_timeout_is_unknown() {
        let caps = capabilities_for_probe_timeout();
        assert_eq!(caps.actions.len(), 6);
        for action in &caps.actions {
            assert_eq!(action.support, Support::Unknown);
            assert_eq!(action.evidence, "probe_timeout");
        }
        assert!(caps.state.enabled.is_none());
    }

    #[test]
    fn find_never_dispatches_or_focuses() {
        let src = include_str!("find.rs");
        let prod = &src[..src.find("#[cfg(test)]").unwrap_or(src.len())];
        for banned in [
            "finish_chr(",
            "finish_element(",
            "resolve_with_opts_samples(",
            "SendInput",
            "input::",
            "fence::",
            "lease::",
            "cooldown::",
            "logs::record",
            "SetFocus(",
            "ElementFromPoint(",
            ".click(",
            ".focus(",
            "point::map_point",
            "point::map_rect",
        ] {
            assert!(
                !prod.contains(banned),
                "find.rs must not reference {banned}"
            );
        }
    }

    #[test]
    fn uia_query_never_reads_current_value() {
        let src = include_str!("uia.rs");
        let start = src.find("fn sta_query_scope(").expect("sta_query_scope");
        let end = src[start..]
            .find("fn sta_resolve_scoped(")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(!body.contains(".CurrentValue("));
        assert!(!body.contains(".value_pattern("));
        let row_start = src.find("fn query_row(").expect("query_row");
        let row_end = src[row_start..]
            .find("fn query_row_matches(")
            .map(|i| row_start + i)
            .unwrap_or(src.len());
        assert!(!src[row_start..row_end].contains(".CurrentValue("));
    }

    #[test]
    fn uia_scope_requires_window_root_not_desktop_root() {
        let src = include_str!("uia.rs");
        let start = src.find("fn sta_query_scope(").expect("sta_query_scope");
        let end = src[start..]
            .find("fn has_first_child(")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(body.contains("ElementFromHandle"));
        assert!(!body.contains("GetRootElement"));
    }

    #[test]
    fn uia_subtree_root_resolves_by_ridh_and_fp() {
        let src = include_str!("uia.rs");
        let start = src
            .find("fn resolve_scoped_com(")
            .expect("resolve_scoped_com");
        let end = src[start..]
            .find("fn sta_resolve_scoped(")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(body.contains("runtime_id_hash"));
        assert!(
            body.contains("scoped_pair_index"),
            "the subtree resolver must reuse the 0123 arbitration by pair index"
        );
        assert!(!body.contains("duplicate_runtime_id"));
        assert!(!body.contains("window_mismatch"));
        assert!(
            !body.contains("candidate.runtime_id =="),
            "the element must join by pair index, not runtime_id alone"
        );
        assert!(!body.contains("resolve_on("));
    }

    #[test]
    fn find_uia_refs_match_observe_refs() {
        let hwnd = 0x1a2b3c_u64;
        let runtime = [42, 1];
        let ref_from_find =
            crate::identity::uia_ref_from_parts(hwnd, "Button", Some("Send"), None, &runtime)
                .expect("find ref");
        // The observe formula: fingerprint over own role/text/unnamed + runtime hash.
        let fp = crate::identity::fingerprint_hex("Button", Some("Send"), None);
        let observe_ref = format!(
            "sref1:u.{hwnd:x}.{fp}.{}",
            crate::identity::runtime_id_hash(&runtime)
        );
        assert_eq!(ref_from_find, observe_ref);
        assert!(crate::identity::uia_ref_from_parts(hwnd, "Button", None, None, &[]).is_none());
    }

    #[test]
    fn uia_role_filter_accepts_canonical_control_type() {
        let row = crate::uia::QueryRow {
            role: "Schaltfläche".into(),
            name: "Senden".into(),
            own_role: "Schaltfläche".into(),
            own_name: "Senden".into(),
            kind: crate::extract::ControlKind::Button,
            is_password: false,
            rect: Rect {
                x: 0,
                y: 0,
                w: 10,
                h: 10,
            },
            is_offscreen: false,
            is_enabled: Some(true),
            runtime_id: vec![1, 2],
            depth: 1,
        };
        let button = crate::uia::QueryFilters {
            role: Some("button".to_string()),
            ..Default::default()
        };
        assert!(
            crate::uia::query_row_matches(&row, &button),
            "canonical English must match a localized emitted role"
        );
        let edit = crate::uia::QueryFilters {
            role: Some("edit".to_string()),
            ..Default::default()
        };
        assert!(!crate::uia::query_row_matches(&row, &edit));
        let text = crate::uia::QueryFilters {
            text: Some("send".to_string()),
            ..Default::default()
        };
        assert!(crate::uia::query_row_matches(&row, &text));
        let rect = crate::uia::QueryFilters {
            rect: Some(Rect {
                x: 5,
                y: 5,
                w: 10,
                h: 10,
            }),
            ..Default::default()
        };
        assert!(crate::uia::query_row_matches(&row, &rect));
    }

    #[test]
    fn frame_scope_source_lock_and_kill_switch() {
        // Source lock only: the `{frameId}` routing half is behavioural in the Node harness
        // (`sw_forwards_query_by_tab_and_frame_with_evidence`) and the kill switch is behavioural
        // in `frame_zero_output_unchanged`.
        let src = include_str!("find.rs");
        assert!(src.contains("crate::frames::enabled()"));
        assert!(src.contains("frame_unsupported"));
        let chrome = include_str!("chrome.rs");
        assert!(chrome.contains("\"frame\": frame"));
    }

    #[test]
    fn old_extension_kebab_and_empty_map_to_unsupported_op() {
        assert_eq!(
            crate::chrome::query_error_token("unsupported-op").1,
            "unsupported_op"
        );
        assert_eq!(
            crate::chrome::query_error_token("empty").1,
            "unsupported_op"
        );
    }

    #[test]
    fn manifest_permissions_unchanged() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../extension/manifest.json")).expect("manifest");
        assert_eq!(manifest["version"], "0.4.1");
        assert_eq!(
            manifest["permissions"],
            serde_json::json!(["nativeMessaging"])
        );
        assert_eq!(
            manifest["host_permissions"],
            serde_json::json!(["<all_urls>"])
        );
        assert_eq!(manifest["content_scripts"][0]["all_frames"], true);
        assert_eq!(manifest["content_scripts"][0]["match_about_blank"], true);
    }

    #[test]
    fn docs_name_the_limits() {
        let contract = include_str!("../docs/AGENT-CONTRACT.md");
        let readme = include_str!("../README.md");
        let mcp = include_str!("mcp.rs");
        for needle in ["capability", "page_complete", "disabled", "HANDS_FRAMES=0"] {
            assert!(contract.contains(needle), "AGENT-CONTRACT missing {needle}");
        }
        assert!(readme.contains("find"));
        for needle in [
            "read-only",
            "no match in this snapshot",
            "capability",
            "HANDS_FRAMES=0",
            "rect_css",
        ] {
            assert!(mcp.contains(needle), "mcp description missing {needle}");
        }
    }

    #[test]
    fn detached_node_is_never_key_ok_true() {
        // The live extension marks a detached node `connected:false`; Rust refuses key_ok.
        let src = include_str!("chrome.rs");
        assert!(src.contains("key_ok: if connected {"));
        assert!(src.contains("Some(false)"));
    }

    #[test]
    fn exact_uia_uses_one_worker() {
        let src = include_str!("uia_actions.rs");
        let start = src
            .find("pub(crate) fn probe_scoped_target(")
            .expect("probe");
        let end = src[start..]
            .find("fn sta_probe_scoped_target(")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert_eq!(body.matches("WorkerSlot::acquire").count(), 1);
        assert!(!body.contains("resolve_on("));
        let probe_start = src.find("fn sta_probe_scoped_target(").expect("sta_probe");
        let probe_end = src[probe_start..]
            .find("fn pattern_available")
            .map(|i| probe_start + i)
            .unwrap_or(src.len());
        let probe_body = &src[probe_start..probe_end];
        assert!(probe_body.contains("resolve_scoped_com"));
        assert!(!probe_body.contains("CurrentValue"));
    }

    #[test]
    fn geometry_unavailable_frame_is_still_queryable() {
        let r = chrome_ref(12, 3, "9f3a", "a1b2c3d4", 0);
        let reply = serde_json::json!({
            "tab": 12,
            "window": 45,
            "frame": 3,
            "doc": "9f3a",
            "snap": "a1b2c3d4",
            "retained": 4,
            "examined": 4,
            "budgetHit": false,
            "matchedTotal": 1,
            "matches": [{"id": "chr:1", "idx": 1, "role": "Button", "text": "Go",
                         "rectCss": {"left": 1.0, "top": 2.0, "width": 3.0, "height": 4.0},
                         "keyOk": true, "connected": true}],
            "active": true,
            "focused": true,
            "fallback": false,
            "visibilityState": "visible",
        });
        let parsed = crate::chrome::query_reply_for_test(&reply, &r).expect("queryable");
        assert_eq!(parsed.frame, Some(3));
        assert_eq!(parsed.matched_total, 1);
        assert!(!reply.to_string().contains("frame_geometry_unavailable"));
    }

    #[test]
    fn exact_mode_reuses_identity_and_assessment() {
        let r = chrome_ref(12, 0, "9f3a", "a1b2c3d4", 0);
        let mut hit = stub_chrome_hit();
        hit.identity = Some(IdentityReport::exact_chrome(&r).with_window(Some(45)));
        hit.assessment = Some(crate::hittest::Assessment::unknown(
            hit.css_rect.center(),
            hit.css_rect,
            "advisory",
        ));
        hit.capability = Some(serde_json::json!({
            "source": "dom",
            "actions": [{"action": "invoke", "support": "available", "evidence": "role_button"}],
            "state": {"enabled": true},
            "notes": ["dom_advertised_not_proven"],
        }));
        crate::chrome::set_scoped_resolve_stub(Some(Ok(hit)));
        let req = FindRequest {
            within: None,
            r#ref: Some(r.to_string()),
            assess: true,
            ..FindRequest::default()
        };
        let env = run(req).expect("run");
        crate::chrome::set_scoped_resolve_stub(None);
        assert!(env.ok);
        let identity = env.identity.expect("identity");
        assert_eq!(identity.state, crate::identity::IdentityState::Exact);
        assert!(env.assessment.is_some());
        assert!(env.element.is_some());
        let caps = env.capabilities.expect("caps");
        assert_eq!(caps.source, CapSource::Dom);
        assert_eq!(caps.actions[0].support, Support::Available);
    }

    #[test]
    fn exact_mode_identity_refusal_is_ok_true() {
        let r = chrome_ref(12, 0, "9f3a", "a1b2c3d4", 9);
        crate::chrome::set_scoped_resolve_stub(Some(Err(IdentityReport::chrome_failure(
            &r,
            crate::identity::IdentityState::Stale,
            "stale_element",
        ))));
        let req = FindRequest {
            r#ref: Some(r.to_string()),
            ..FindRequest::default()
        };
        let env = run(req).expect("run");
        crate::chrome::set_scoped_resolve_stub(None);
        assert!(env.ok, "the query ran");
        assert!(env.element.is_none());
        assert!(env.capabilities.is_none());
        let identity = env.identity.expect("identity");
        assert_eq!(identity.state, crate::identity::IdentityState::Stale);
    }

    #[test]
    fn exact_mode_off_screen_and_zero_area_are_answers_not_errors() {
        let r = chrome_ref(12, 0, "9f3a", "a1b2c3d4", 0);
        let mut hit = stub_chrome_hit();
        hit.rect = Rect {
            x: -500,
            y: -500,
            w: 0,
            h: 0,
        };
        hit.css_rect = CssRect {
            left: -600.0,
            top: -600.0,
            width: 1.0,
            height: 1.0,
        };
        hit.contract.transform.valid = true;
        crate::chrome::set_scoped_resolve_stub(Some(Ok(hit)));
        let req = FindRequest {
            r#ref: Some(r.to_string()),
            ..FindRequest::default()
        };
        let env = run(req).expect("run");
        crate::chrome::set_scoped_resolve_stub(None);
        assert!(env.ok);
        assert!(env.element.is_some());
        // Zero-area physical rect is omitted, never a hard Target error.
        assert!(env.element.as_ref().and_then(|e| e.rect).is_none());
    }

    #[test]
    fn frame_exact_mode_geometry_refusal_is_honest() {
        let r = chrome_ref(12, 3, "9f3a", "a1b2c3d4", 1);
        crate::chrome::set_scoped_resolve_stub(Some(Err(IdentityReport::chrome_failure(
            &r,
            crate::identity::IdentityState::Unknown,
            "frame_geometry_unavailable",
        ))));
        let req = FindRequest {
            r#ref: Some(r.to_string()),
            ..FindRequest::default()
        };
        let env = run(req).expect("run");
        crate::chrome::set_scoped_resolve_stub(None);
        assert!(env.ok);
        assert!(env.element.is_none());
        let identity = env.identity.expect("identity");
        assert_eq!(
            identity.reason.as_deref(),
            Some("frame_geometry_unavailable")
        );
    }

    fn stub_chrome_hit() -> crate::chrome::ChromeHit {
        let rect = CssRect {
            left: 100.0,
            top: 50.0,
            width: 200.0,
            height: 40.0,
        };
        crate::chrome::ChromeHit {
            id: "sref1:c.c.0.9f3a.a1b2c3d4.0".into(),
            name: "Sign in".into(),
            role: "Button".into(),
            rect: Rect {
                x: 100,
                y: 50,
                w: 200,
                h: 40,
            },
            css_rect: rect,
            css_point: rect.center(),
            contract: crate::point::PointContract {
                source: crate::point::PointSource::Chr,
                css: Some(rect.center()),
                physical: Some(crate::point::PhysicalPoint { x: 200, y: 70 }),
                transform: crate::point::TransformReport {
                    valid: true,
                    method: crate::point::TransformMethod::RenderWidgetHost,
                    confidence: crate::point::TransformConfidence::High,
                    scale: Some(1.0),
                    dpr: Some(1.0),
                    dpi: Some(96),
                    page_zoom: Some(1.0),
                    page_viewport: None,
                    anchor_basis: None,
                    layout_basis: None,
                    reason: None,
                    observe_id: None,
                },
            },
            hwnd: Some(0x1234),
            href: None,
            map: None,
            assessment: None,
            capability: None,
            identity: None,
        }
    }

    #[test]
    fn docs_lists_the_find_surface() {
        let main = include_str!("main.rs");
        for flag in [
            "within",
            "window",
            "\"ref\"",
            "role",
            "text",
            "rect",
            "rect-css",
            "subtree",
            "depth",
            "limit",
            "offset",
            "no-caps",
            "assess",
            "budget-ms",
            "max-nodes",
            "timeout-ms",
            "session_id",
        ] {
            assert!(main.contains(flag), "CLI help missing {flag}");
        }
    }
}
