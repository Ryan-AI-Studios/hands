//! Actuation targets: `uia:…`, `chr:<u32>`, `g:{col}:{row}`, or physical `{x,y}`.

use crate::chrome;
use crate::error::HandsError;
use crate::identity::{IdentityReport, IdentityState, SnapshotRef};
use crate::point::{
    CssPoint, CssRect, PointContract, PointSource, ViewportMap, contract_for, explicit_contract,
    map_point, map_rect,
};
use crate::space::{Rect, Space};
use crate::uia::{self, ResolvedElement};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    ElementId(String),
    Scoped(SnapshotRef),
    Grid { col: i32, row: i32 },
    Pixel { x: i32, y: i32 },
}

#[derive(Debug, Clone)]
pub struct ResolvedTarget {
    pub target: Target,
    pub kind: &'static str,
    pub id: Option<String>,
    pub x: i32,
    pub y: i32,
    pub rect: Rect,
    pub hwnd: Option<isize>,
    pub name: String,
    pub role: String,
    pub css_point: Option<CssPoint>,
    pub css_rect: Option<CssRect>,
    pub contract: Option<PointContract>,
    pub(crate) map: Option<ViewportMap>,
    /// Optional DOM assessment carried from the same resolve reply.
    pub assessment: Option<crate::hittest::Assessment>,
    /// Identity report for scoped/legacy element targets. Grid/pixel omit it.
    pub identity: Option<IdentityReport>,
    /// RuntimeId kept from a scoped UIA resolve for the semantic worker dispatch.
    pub(crate) uia_runtime_id: Option<Vec<i32>>,
    /// Own-role/name fingerprint expected by a scoped UIA ref.
    pub(crate) uia_expected_fp: Option<u32>,
}

/// Resolve outcome. A transform-invalid `chr:` is an explicit unresolved refusal carrying
/// its contract, never a fabricated zero-area `ResolvedTarget`.
#[derive(Debug, Clone)]
pub enum TargetOutcome {
    Resolved(Box<ResolvedTarget>),
    Unresolved {
        target: Target,
        contract: Box<PointContract>,
        hwnd: Option<isize>,
        assessment: Option<Box<crate::hittest::Assessment>>,
        identity: Option<IdentityReport>,
    },
    /// Identity could not be proven (stale/ambiguous/unknown). Nothing was sent.
    Identity {
        target: Target,
        report: IdentityReport,
    },
}

impl TargetOutcome {
    pub fn contract(&self) -> Option<&PointContract> {
        match self {
            Self::Resolved(r) => r.contract.as_ref(),
            Self::Unresolved { contract, .. } => Some(contract.as_ref()),
            Self::Identity { .. } => None,
        }
    }

    pub fn hwnd(&self) -> Option<isize> {
        match self {
            Self::Resolved(r) => r.hwnd,
            Self::Unresolved { hwnd, .. } => *hwnd,
            Self::Identity { .. } => None,
        }
    }

    pub fn identity(&self) -> Option<&IdentityReport> {
        match self {
            Self::Resolved(r) => r.identity.as_ref(),
            Self::Unresolved { identity, .. } => identity.as_ref(),
            Self::Identity { report, .. } => Some(report),
        }
    }
}

impl Target {
    /// Exactly one of `element_id` / `grid` / (`x` and `y`).
    pub fn parse(
        element_id: Option<&str>,
        grid: Option<&str>,
        x: Option<i32>,
        y: Option<i32>,
    ) -> Result<Self, HandsError> {
        if let Some(raw) = element_id
            && raw != raw.trim()
            && raw.trim_start().starts_with("sref")
        {
            return Err(HandsError::Target(
                "sref1: malformed_ref: whitespace is not canonical".into(),
            ));
        }
        let el = nonempty(element_id);
        let g = nonempty(grid);
        let xy = match (x, y) {
            (Some(px), Some(py)) => Some((px, py)),
            (None, None) => None,
            _ => {
                return Err(HandsError::Target(
                    "pixel target requires both --x and --y".into(),
                ));
            }
        };
        match (el, g, xy) {
            (Some(id), None, None) => parse_element_id(id),
            (None, Some(cell), None) => {
                let (col, row) = Space::parse_cell_id(cell)?;
                Ok(Self::Grid { col, row })
            }
            (None, None, Some((px, py))) => Ok(Self::Pixel { x: px, y: py }),
            (None, None, None) => Err(HandsError::Target(
                "exactly one of --element-id, --grid, or --x/--y is required".into(),
            )),
            _ => Err(HandsError::Target(
                "exactly one of --element-id, --grid, or --x/--y is required".into(),
            )),
        }
    }

    /// Compatibility wrapper for tests: foreground Chrome HWND, unresolved -> error.
    pub fn resolve(&self, space: Space) -> Result<ResolvedTarget, HandsError> {
        match self.resolve_with(space, crate::foreground::foreground_hwnd())? {
            TargetOutcome::Resolved(resolved) => Ok(*resolved),
            TargetOutcome::Unresolved { contract, .. } => Err(HandsError::Target(
                contract
                    .transform
                    .reason
                    .unwrap_or_else(|| "transform is not valid".into()),
            )),
            TargetOutcome::Identity { report, .. } => Err(HandsError::Target(
                report
                    .reason
                    .clone()
                    .unwrap_or_else(|| report.state_token().to_string()),
            )),
        }
    }

    /// Resolve with an explicit Chrome walk/foreground HWND. `None` refuses `chr:` with
    /// `no_hwnd`; it never falls back to a guessed window.
    pub fn resolve_with(
        &self,
        space: Space,
        hwnd: Option<isize>,
    ) -> Result<TargetOutcome, HandsError> {
        self.resolve_with_opts(space, hwnd, false)
    }

    /// Shared `chr:` resolve used by `click` and `hittest`. `assess:true` asks the extension for
    /// the DOM assessment in the same resolve reply (no second call, 0108 lock intact).
    pub fn resolve_with_opts(
        &self,
        space: Space,
        hwnd: Option<isize>,
        assess: bool,
    ) -> Result<TargetOutcome, HandsError> {
        self.resolve_with_opts_samples(space, hwnd, assess, None)
    }

    pub fn resolve_with_opts_samples(
        &self,
        space: Space,
        hwnd: Option<isize>,
        assess: bool,
        samples: Option<u32>,
    ) -> Result<TargetOutcome, HandsError> {
        match self {
            Self::ElementId(id) => {
                if id.starts_with("chr:") {
                    let mut hit = chrome::try_resolve_assessed(id, hwnd, assess, samples)?;
                    if hit.contract.transform.valid {
                        if finish_chr(id, &mut hit, space)?.is_some() {
                            return Ok(TargetOutcome::Unresolved {
                                target: self.clone(),
                                contract: Box::new(hit.contract),
                                hwnd: hit.hwnd,
                                assessment: hit.assessment.map(Box::new),
                                identity: hit.identity,
                            });
                        }
                        return Ok(TargetOutcome::Resolved(Box::new(hit.into_resolved())));
                    }
                    return Ok(TargetOutcome::Unresolved {
                        target: self.clone(),
                        contract: Box::new(hit.contract),
                        hwnd: hit.hwnd,
                        assessment: hit.assessment.map(Box::new),
                        identity: hit.identity,
                    });
                }
                let runtime_id = parse_runtime_id(id)?;
                let found = uia::resolve_runtime_id(&runtime_id)?;
                finish_element(id, found, space, Some(IdentityReport::legacy()), None, None)
                    .map(|r| TargetOutcome::Resolved(Box::new(r)))
            }
            Self::Scoped(r) => match r {
                SnapshotRef::Chrome { frame, .. } => {
                    if *frame != 0 {
                        return Ok(TargetOutcome::Identity {
                            target: self.clone(),
                            report: IdentityReport::chrome_failure(
                                r,
                                IdentityState::Unknown,
                                "frame_unsupported",
                            ),
                        });
                    }
                    match chrome::try_resolve_ref(r, hwnd, assess, samples) {
                        Ok(mut hit) => {
                            if hit.contract.transform.valid {
                                let hit_id = hit.id.clone();
                                if finish_chr(&hit_id, &mut hit, space)?.is_some() {
                                    return Ok(TargetOutcome::Unresolved {
                                        target: self.clone(),
                                        contract: Box::new(hit.contract),
                                        hwnd: hit.hwnd,
                                        assessment: hit.assessment.map(Box::new),
                                        identity: hit.identity,
                                    });
                                }
                                return Ok(TargetOutcome::Resolved(Box::new(hit.into_resolved())));
                            }
                            Ok(TargetOutcome::Unresolved {
                                target: self.clone(),
                                contract: Box::new(hit.contract),
                                hwnd: hit.hwnd,
                                assessment: hit.assessment.map(Box::new),
                                identity: hit.identity,
                            })
                        }
                        Err(report) => Ok(TargetOutcome::Identity {
                            target: self.clone(),
                            report,
                        }),
                    }
                }
                SnapshotRef::Uia {
                    hwnd: walk_hwnd,
                    fp,
                    ridh,
                } => {
                    let deadline = std::time::Duration::from_millis(500);
                    match uia::resolve_scoped(*walk_hwnd as isize, ridh, fp, deadline) {
                        Ok(found) => {
                            let expected = found.own_fingerprint();
                            let runtime_id = found.runtime_id.clone();
                            finish_element(
                                &r.to_string(),
                                found,
                                space,
                                Some(IdentityReport::exact_uia(r)),
                                Some(runtime_id),
                                Some(expected),
                            )
                            .map(|resolved| TargetOutcome::Resolved(Box::new(resolved)))
                        }
                        Err(failure) => Ok(TargetOutcome::Identity {
                            target: self.clone(),
                            report: IdentityReport::uia_failure(r, failure.state, failure.reason),
                        }),
                    }
                }
            },
            Self::Grid { col, row } => {
                let rect = space.cell_rect(*col, *row);
                if rect.area() == 0 || !space.contains(rect) {
                    return Err(HandsError::Target(format!(
                        "grid cell g:{col}:{row} is zero-area or outside the virtual screen"
                    )));
                }
                let (x, y) = rect.center();
                if !space.contains_point(x, y) {
                    return Err(HandsError::Target(format!(
                        "grid cell g:{col}:{row} center is outside the virtual screen"
                    )));
                }
                Ok(TargetOutcome::Resolved(Box::new(ResolvedTarget {
                    target: self.clone(),
                    kind: "grid",
                    id: Some(format!("g:{col}:{row}")),
                    x,
                    y,
                    rect,
                    hwnd: None,
                    name: String::new(),
                    role: String::new(),
                    css_point: None,
                    css_rect: None,
                    contract: Some(explicit_contract(
                        PointSource::Grid,
                        crate::point::PhysicalPoint { x, y },
                    )),
                    map: None,
                    assessment: None,
                    identity: None,
                    uia_runtime_id: None,
                    uia_expected_fp: None,
                })))
            }
            Self::Pixel { x, y } => {
                if !space.contains_point(*x, *y) {
                    return Err(HandsError::Target(format!(
                        "pixel ({x},{y}) is outside the virtual screen"
                    )));
                }
                let rect = Rect {
                    x: *x,
                    y: *y,
                    w: 1,
                    h: 1,
                };
                if rect.area() == 0 {
                    return Err(HandsError::Target("pixel target has zero area".into()));
                }
                Ok(TargetOutcome::Resolved(Box::new(ResolvedTarget {
                    target: self.clone(),
                    kind: "pixel",
                    id: None,
                    x: *x,
                    y: *y,
                    rect,
                    hwnd: None,
                    name: String::new(),
                    role: String::new(),
                    css_point: None,
                    css_rect: None,
                    contract: Some(explicit_contract(
                        PointSource::Pixel,
                        crate::point::PhysicalPoint { x: *x, y: *y },
                    )),
                    map: None,
                    assessment: None,
                    identity: None,
                    uia_runtime_id: None,
                    uia_expected_fp: None,
                })))
            }
        }
    }
}

impl chrome::ChromeHit {
    fn into_resolved(self) -> ResolvedTarget {
        let target = if self.id.starts_with("sref1:") {
            SnapshotRef::parse(&self.id)
                .map(Target::Scoped)
                .unwrap_or_else(|_| Target::ElementId(self.id.clone()))
        } else {
            Target::ElementId(self.id.clone())
        };
        ResolvedTarget {
            target,
            kind: "element",
            id: Some(self.id),
            x: self.rect.center().0,
            y: self.rect.center().1,
            rect: self.rect,
            hwnd: self.hwnd,
            name: self.name,
            role: self.role,
            css_point: Some(self.css_point),
            css_rect: Some(self.css_rect),
            contract: Some(self.contract),
            map: self.map,
            assessment: self.assessment,
            identity: self.identity,
            uia_runtime_id: None,
            uia_expected_fp: None,
        }
    }
}

/// Finish a valid `chr:` hit. Returns `Some(reason)` when the CSS point or rect is outside
/// the layout viewport (the hit's contract is rewritten to the named refusal).
fn finish_chr(
    id: &str,
    hit: &mut chrome::ChromeHit,
    space: Space,
) -> Result<Option<String>, HandsError> {
    let css_rect = hit.css_rect;
    if !css_rect.is_positive() {
        hit.contract = invalid_from(&hit.contract, "zero_area: element has a zero-area CSS rect");
        return Ok(Some("zero_area: element has a zero-area CSS rect".into()));
    }
    let contract = contract_for(
        hit.map
            .as_ref()
            .ok_or_else(|| HandsError::Target("chr: contract has no map".into()))?,
        css_rect.center(),
    );
    let p = match map_point(&contract, css_rect.center()) {
        Ok(p) => p,
        Err(err) => {
            hit.contract = invalid_from(&contract, &format!("point_outside_viewport: {err}"));
            return Ok(Some(format!("point_outside_viewport: {err}")));
        }
    };
    let rect = match map_rect(&contract, css_rect) {
        Ok(rect) => rect,
        Err(err) => {
            hit.contract = invalid_from(&contract, &format!("point_outside_viewport: {err}"));
            return Ok(Some(format!("point_outside_viewport: {err}")));
        }
    };
    if !space.contains_point(p.x, p.y) {
        return Err(HandsError::Target(format!(
            "element {id} center is outside the virtual screen"
        )));
    }
    hit.contract = contract;
    hit.css_point = css_rect.center();
    hit.rect = rect;
    Ok(None)
}

fn invalid_from(contract: &PointContract, reason: &str) -> PointContract {
    let mut out = contract.clone();
    out.physical = None;
    out.transform.valid = false;
    out.transform.method = crate::point::TransformMethod::None;
    out.transform.confidence = crate::point::TransformConfidence::None;
    out.transform.reason = Some(reason.to_string());
    out
}

fn finish_element(
    id: &str,
    found: ResolvedElement,
    space: Space,
    identity: Option<IdentityReport>,
    uia_runtime_id: Option<Vec<i32>>,
    uia_expected_fp: Option<u32>,
) -> Result<ResolvedTarget, HandsError> {
    if found.rect.area() == 0 {
        return Err(HandsError::Target(format!(
            "element {id} has zero-area bounding rect"
        )));
    }
    let (x, y) = found.rect.center();
    if !space.contains_point(x, y) {
        return Err(HandsError::Target(format!(
            "element {id} center is outside the virtual screen"
        )));
    }
    let target = if id.starts_with("sref1:") {
        SnapshotRef::parse(id)
            .map(Target::Scoped)
            .unwrap_or_else(|_| Target::ElementId(id.to_string()))
    } else {
        Target::ElementId(id.to_string())
    };
    Ok(ResolvedTarget {
        target,
        kind: "element",
        id: Some(id.to_string()),
        x,
        y,
        rect: found.rect,
        hwnd: found.hwnd,
        name: found.name,
        role: found.role,
        css_point: None,
        css_rect: None,
        contract: Some(explicit_contract(
            PointSource::Uia,
            crate::point::PhysicalPoint { x, y },
        )),
        map: None,
        assessment: None,
        identity,
        uia_runtime_id,
        uia_expected_fp,
    })
}

pub fn parse_element_id(id: &str) -> Result<Target, HandsError> {
    if id.starts_with("sref") {
        return SnapshotRef::parse(id).map(Target::Scoped);
    }
    if let Some(rest) = id.strip_prefix("uia:") {
        if rest.is_empty() {
            return Err(HandsError::Target("element id is a bare uia:".into()));
        }
        let _ = parse_runtime_id(id)?;
        return Ok(Target::ElementId(id.to_string()));
    }
    if id.starts_with("chr:") {
        let n = parse_chr_index(id)?;
        return Ok(Target::ElementId(format!("chr:{n}")));
    }
    if let Some((prefix, _)) = id.split_once(':') {
        return Err(HandsError::Target(format!(
            "unknown element id prefix '{prefix}:'"
        )));
    }
    Err(HandsError::Target(format!(
        "element id must start with uia: or chr: (got '{id}')"
    )))
}

pub fn parse_chr_index(id: &str) -> Result<u32, HandsError> {
    let rest = id.strip_prefix("chr:").ok_or_else(|| {
        HandsError::Target(format!("element id must start with chr: (got '{id}')"))
    })?;
    if rest.is_empty()
        || rest.starts_with('+')
        || rest.starts_with('-')
        || rest.contains('.')
        || rest.as_bytes().iter().any(|b| !b.is_ascii_digit())
        || (rest.len() > 1 && rest.starts_with('0'))
    {
        return Err(HandsError::Target(format!(
            "invalid Chrome element id '{id}' (expected chr:<u32>)"
        )));
    }
    rest.parse::<u32>().map_err(|_| {
        HandsError::Target(format!(
            "invalid Chrome element id '{id}' (expected chr:<u32>)"
        ))
    })
}

pub fn parse_runtime_id(id: &str) -> Result<Vec<i32>, HandsError> {
    let rest = id.strip_prefix("uia:").ok_or_else(|| {
        HandsError::Target(format!("element id must start with uia: (got '{id}')"))
    })?;
    if rest.is_empty() {
        return Err(HandsError::Target("element id is a bare uia:".into()));
    }
    let mut out = Vec::new();
    for part in rest.split('.') {
        if part.is_empty() {
            return Err(HandsError::Target(format!(
                "element id has an empty RuntimeId token (got '{id}')"
            )));
        }
        let n = part.parse::<i32>().map_err(|_| {
            HandsError::Target(format!("element id RuntimeId is not integers (got '{id}')"))
        })?;
        out.push(n);
    }
    if out.is_empty() {
        return Err(HandsError::Target("element id has empty RuntimeId".into()));
    }
    Ok(out)
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_each_kind() {
        assert!(matches!(
            Target::parse(Some("uia:42.1"), None, None, None).unwrap(),
            Target::ElementId(id) if id == "uia:42.1"
        ));
        assert_eq!(
            Target::parse(None, Some("g:-1:2"), None, None).unwrap(),
            Target::Grid { col: -1, row: 2 }
        );
        assert_eq!(
            Target::parse(None, None, Some(10), Some(20)).unwrap(),
            Target::Pixel { x: 10, y: 20 }
        );
    }

    #[test]
    fn chr_grammar_table() {
        for id in ["chr:0", "chr:1", "chr:42"] {
            assert!(
                matches!(parse_element_id(id).unwrap(), Target::ElementId(s) if s == id),
                "{id}"
            );
        }
        for id in [
            "chr:",
            "chr:abc",
            "chr:007",
            "chr: 1",
            "chr:-1",
            "chr:+1",
            "chr:1.0",
            "chr:4294967296",
        ] {
            let err = parse_element_id(id).unwrap_err();
            let msg = err.to_string();
            assert!(msg.contains("chr:"), "{id}: {msg}");
            assert!(
                !msg.to_ascii_lowercase().contains("unknown prefix"),
                "{id}: {msg}"
            );
        }
        let err = parse_element_id("foo:1").unwrap_err();
        assert!(err.to_string().contains("unknown"), "{err}");
    }

    #[test]
    fn parse_runtime_id_round_trips_0011_address_bar_shape() {
        assert_eq!(
            parse_runtime_id("uia:42.591400.4.0.0.301").unwrap(),
            vec![42, 591400, 4, 0, 0, 301]
        );
    }

    #[test]
    fn parse_rejects_bare_uia_mixed_and_unknown() {
        let err = Target::parse(Some("uia:"), None, None, None).unwrap_err();
        assert!(err.to_string().contains("bare uia:"), "{err}");
        let err = Target::parse(Some("foo:1"), None, None, None).unwrap_err();
        assert!(err.to_string().contains("unknown"), "{err}");
        assert!(Target::parse(Some("uia:42.1"), Some("g:0:0"), None, None).is_err());
        assert!(Target::parse(Some("chr:0"), Some("g:0:0"), None, None).is_err());
        assert!(Target::parse(None, None, Some(1), None).is_err());
        assert!(Target::parse(None, None, None, None).is_err());
        assert!(Target::parse(Some("42.1"), None, None, None).is_err());
    }

    #[test]
    fn resolve_chr0_from_fixture_center() {
        let g = crate::chrome::EnvGuard::lock();
        g.set_snapshot(Some(&crate::chrome::EnvGuard::fixture_path()));
        let space = Space::new(0, 0, 1920, 1080).unwrap();
        let outcome = Target::ElementId("chr:0".into())
            .resolve_with(space, None)
            .unwrap();
        match outcome {
            TargetOutcome::Unresolved { contract, hwnd, .. } => {
                assert!(hwnd.is_none());
                assert_eq!(
                    contract.transform.reason.as_deref(),
                    Some("fixture_snapshot")
                );
                assert!(contract.physical.is_none());
                assert_eq!(contract.css, Some(CssPoint::new(110.0, 36.0)));
            }
            TargetOutcome::Resolved(_) => panic!("fixture must never be a click contract"),
            TargetOutcome::Identity { .. } => panic!("chr: must not be an identity failure"),
        }
    }

    #[test]
    fn resolve_chr_errors_missing_malformed_and_no_pipe() {
        let space = Space::new(0, 0, 1920, 1080).unwrap();
        let g = crate::chrome::EnvGuard::lock();

        g.set_snapshot(Some(std::path::Path::new(
            r"C:\dev\Helping-Hands\hands\tests\fixtures\missing-chrome.json",
        )));
        let err = Target::ElementId("chr:0".into())
            .resolve(space)
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("chr:") || msg.contains("host") || msg.contains("fixture"),
            "{msg}"
        );
        assert!(
            !msg.to_ascii_lowercase().contains("unknown prefix"),
            "{msg}"
        );

        let bad = std::env::temp_dir().join("hands-target-malformed.json");
        std::fs::write(&bad, "not-json").unwrap();
        g.set_snapshot(Some(&bad));
        let err = Target::ElementId("chr:0".into())
            .resolve(space)
            .unwrap_err();
        let msg = err.to_string();
        let _ = std::fs::remove_file(&bad);
        assert!(
            msg.contains("chr:") || msg.contains("host") || msg.contains("fixture"),
            "{msg}"
        );
        assert!(
            !msg.to_ascii_lowercase().contains("unknown prefix"),
            "{msg}"
        );

        g.set_snapshot(None);
        g.set_pipe(Some(r"\\.\pipe\hands-chrome-absent-target-0005"));
        let err = Target::ElementId("chr:0".into())
            .resolve(space)
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("chr:") || msg.contains("host"), "{msg}");
        assert!(
            !msg.to_ascii_lowercase().contains("unknown prefix"),
            "{msg}"
        );
    }

    #[test]
    fn resolve_grid_and_pixel_use_area_and_contains() {
        let space = Space::new(0, 0, 250, 180).unwrap();
        let grid = Target::Grid { col: 2, row: 1 };
        let hit = grid.resolve(space).unwrap();
        assert_eq!(hit.kind, "grid");
        assert_eq!((hit.x, hit.y), space.cell_rect(2, 1).center());
        assert!(space.contains(hit.rect));
        assert!(hit.rect.area() > 0);

        let pixel = Target::Pixel { x: 249, y: 179 };
        let hit = pixel.resolve(space).unwrap();
        assert_eq!((hit.x, hit.y), (249, 179));

        assert!(Target::Pixel { x: 250, y: 0 }.resolve(space).is_err());
        assert!(Target::Grid { col: 40, row: 0 }.resolve(space).is_err());
        assert!(Target::Grid { col: -1, row: 0 }.resolve(space).is_err());
        assert!(
            Target::Grid {
                col: i32::MAX,
                row: 0
            }
            .resolve(space)
            .is_err()
        );
    }

    #[test]
    fn stamped_search_box_cell_is_hittable_neighbor_is_not() {
        let space = Space::new(0, 0, 1920, 1080).unwrap();
        let rect = Rect {
            x: 46,
            y: 620,
            w: 903,
            h: 56,
        };
        assert_eq!(rect.center(), (497, 648));
        assert_eq!(space.cell_id_of_center(rect), "g:49:64");

        let hit = Target::Grid { col: 49, row: 64 }.resolve(space).unwrap();
        assert_eq!((hit.x, hit.y), (495, 645));
        assert!(hit.x >= rect.x && hit.x < rect.x + rect.w);
        assert!(hit.y >= rect.y && hit.y < rect.y + rect.h);
        assert_eq!((hit.x, hit.y), space.cell_rect(49, 64).center());

        let miss = Target::Grid { col: 49, row: 61 }.resolve(space).unwrap();
        assert_eq!((miss.x, miss.y), (495, 615));
        let inside = miss.x >= rect.x
            && miss.x < rect.x + rect.w
            && miss.y >= rect.y
            && miss.y < rect.y + rect.h;
        assert!(!inside);
    }

    fn valid_map() -> crate::point::ViewportMap {
        crate::point::build_map(crate::point::MapInputs {
            hwnd: Some(0x1234),
            window_rect: Some(Rect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            }),
            dpi: Some(96),
            virtual_screen: Some(Rect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            }),
            page: crate::point::PageMetrics {
                inner_width: Some(500.0),
                inner_height: Some(400.0),
                client_width: Some(500.0),
                client_height: Some(400.0),
                dpr: Some(1.0),
                ..crate::point::PageMetrics::default()
            },
            render_widgets: vec![Rect {
                x: 100,
                y: 100,
                w: 500,
                h: 400,
            }],
            uia_document: None,
            monitors: Vec::new(),
            observe_id: Some("f3a9c1d2e4b5a607".into()),
            page_title: None,
            fixture: false,
            identity: crate::point::WindowIdentity::Confirmed,
            fixture_origin: None,
        })
    }

    fn stub_chr(id: &str, contract: PointContract, hwnd: Option<isize>) -> chrome::ChromeHit {
        chrome::ChromeHit {
            id: id.into(),
            name: String::new(),
            role: "Button".into(),
            rect: Rect {
                x: 100,
                y: 100,
                w: 100,
                h: 100,
            },
            css_rect: CssRect {
                left: 10.0,
                top: 10.0,
                width: 100.0,
                height: 100.0,
            },
            css_point: CssPoint::new(60.0, 60.0),
            contract,
            hwnd,
            map: None,
            assessment: None,
            identity: None,
        }
    }

    #[test]
    fn chr_point_is_contract_point_not_rect_center() {
        let map = valid_map();
        let contract = crate::point::contract_for(&map, CssPoint::new(60.0, 60.0));
        let expected = crate::point::map_point(&contract, CssPoint::new(60.0, 60.0)).unwrap();
        let hit = stub_chr("chr:5", contract, Some(0x1234));
        let mut hit = chrome::ChromeHit {
            map: Some(map),
            ..hit
        };
        let space = Space::new(0, 0, 1920, 1080).unwrap();
        assert!(finish_chr("chr:5", &mut hit, space).unwrap().is_none());
        let resolved = hit.into_resolved();
        assert_eq!((resolved.x, resolved.y), (expected.x, expected.y));
        assert_eq!(resolved.rect.center(), (expected.x, expected.y));
        assert_eq!(resolved.hwnd, Some(0x1234));
        assert_eq!(resolved.contract.unwrap().physical, Some(expected));
    }

    #[test]
    fn chr_invalid_contract_is_unresolved_with_reason() {
        let contract = PointContract {
            source: PointSource::Chr,
            css: Some(CssPoint::new(10.0, 20.0)),
            physical: None,
            transform: crate::point::TransformReport::invalid("fixture_snapshot", None),
        };
        chrome::set_resolve_stub(Some(stub_chr("chr:5", contract, None)));
        let space = Space::new(0, 0, 1920, 1080).unwrap();
        let outcome = Target::ElementId("chr:5".into())
            .resolve_with(space, None)
            .unwrap();
        chrome::set_resolve_stub(None);
        match outcome {
            TargetOutcome::Unresolved { contract, .. } => {
                assert_eq!(
                    contract.transform.reason.as_deref(),
                    Some("fixture_snapshot")
                );
                assert!(contract.physical.is_none());
            }
            TargetOutcome::Resolved(_) => panic!("invalid chr: must not fabricate geometry"),
            TargetOutcome::Identity { .. } => panic!("chr: must not be an identity failure"),
        }
    }

    #[test]
    fn chr_without_hwnd_refuses() {
        let contract = PointContract {
            source: PointSource::Chr,
            css: Some(CssPoint::new(1.0, 1.0)),
            physical: None,
            transform: crate::point::TransformReport::invalid("no_hwnd", None),
        };
        chrome::set_resolve_stub(Some(stub_chr("chr:0", contract, None)));
        let space = Space::new(0, 0, 1920, 1080).unwrap();
        let outcome = Target::ElementId("chr:0".into())
            .resolve_with(space, None)
            .unwrap();
        chrome::set_resolve_stub(None);
        assert!(matches!(
            outcome,
            TargetOutcome::Unresolved { hwnd: None, .. }
        ));
    }

    #[test]
    fn chr_zero_area_contract_is_invalid() {
        let map = valid_map();
        let contract = crate::point::contract_for(&map, CssPoint::new(60.0, 60.0));
        let mut hit = stub_chr("chr:5", contract, Some(0x1234));
        hit.map = Some(map);
        hit.css_rect = CssRect {
            left: 10.0,
            top: 10.0,
            width: 0.0,
            height: 0.0,
        };
        let space = Space::new(0, 0, 1920, 1080).unwrap();
        let reason = finish_chr("chr:5", &mut hit, space)
            .unwrap()
            .expect("zero-area refusal");
        assert!(reason.starts_with("zero_area"), "{reason}");
        assert!(!hit.contract.transform.valid);
        assert!(hit.contract.physical.is_none());
        assert!(
            hit.contract
                .transform
                .reason
                .as_deref()
                .unwrap_or_default()
                .starts_with("zero_area")
        );
    }

    #[test]
    fn grid_and_pixel_are_explicit() {
        let space = Space::new(0, 0, 1920, 1080).unwrap();
        for (target, source) in [
            (Target::Grid { col: 2, row: 1 }, PointSource::Grid),
            (Target::Pixel { x: 10, y: 20 }, PointSource::Pixel),
        ] {
            let resolved = target.resolve(space).unwrap();
            let contract = resolved.contract.expect("explicit contract");
            assert_eq!(contract.source, source);
            assert_eq!(
                contract.transform.method,
                crate::point::TransformMethod::Explicit
            );
            assert!(contract.transform.valid);
            assert!(contract.css.is_none());
            assert_eq!(
                contract.physical.map(|p| (p.x, p.y)),
                Some((resolved.x, resolved.y))
            );
        }
    }

    // ------------------------------------------------------- scoped identity

    fn scoped_chrome_hit(r: &SnapshotRef, contract: PointContract) -> chrome::ChromeHit {
        let map = valid_map();
        chrome::ChromeHit {
            id: r.to_string(),
            name: "Sign in".into(),
            role: "Button".into(),
            rect: Rect {
                x: 100,
                y: 100,
                w: 100,
                h: 100,
            },
            css_rect: CssRect {
                left: 10.0,
                top: 10.0,
                width: 100.0,
                height: 100.0,
            },
            css_point: CssPoint::new(60.0, 60.0),
            contract,
            hwnd: Some(0x1234),
            map: Some(map),
            assessment: None,
            identity: Some(crate::identity::IdentityReport::exact_chrome(r)),
        }
    }

    fn chrome_ref(idx: u32) -> SnapshotRef {
        SnapshotRef::Chrome {
            tab: 12,
            frame: 0,
            doc: "9f3a".into(),
            snap: "a1b2c3d4".into(),
            idx,
        }
    }

    #[test]
    fn scoped_chrome_ref_exact() {
        let r = chrome_ref(7);
        let map = valid_map();
        let contract = crate::point::contract_for(&map, CssPoint::new(60.0, 60.0));
        chrome::set_scoped_resolve_stub(Some(Ok(scoped_chrome_hit(&r, contract))));
        let space = Space::new(0, 0, 1920, 1080).unwrap();
        let outcome = Target::Scoped(r.clone())
            .resolve_with(space, Some(0x1234))
            .unwrap();
        chrome::set_scoped_resolve_stub(None);
        match outcome {
            TargetOutcome::Resolved(resolved) => {
                assert_eq!(resolved.id.as_deref(), Some(r.to_string().as_str()));
                let identity = resolved.identity.expect("identity");
                assert_eq!(identity.binding, crate::identity::Binding::Scoped);
                assert_eq!(identity.state, IdentityState::Exact);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn scoped_chrome_fixture_ref_resolves_identity_and_refuses_transform() {
        let g = chrome::EnvGuard::lock();
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/chrome-scoped.json");
        g.set_snapshot(Some(&path));
        let r = SnapshotRef::Chrome {
            tab: 12,
            frame: 0,
            doc: "9f3a".into(),
            snap: "a1b2c3d4".into(),
            idx: 0,
        };
        let space = Space::new(0, 0, 1920, 1080).unwrap();
        let outcome = Target::Scoped(r.clone()).resolve_with(space, None).unwrap();
        g.set_snapshot(None);
        match outcome {
            TargetOutcome::Unresolved {
                contract,
                identity: Some(identity),
                ..
            } => {
                assert_eq!(
                    contract.transform.reason.as_deref(),
                    Some("fixture_snapshot")
                );
                assert_eq!(identity.binding, crate::identity::Binding::Scoped);
                assert_eq!(identity.state, IdentityState::Exact);
                assert_eq!(identity.r#ref.as_deref(), Some(r.to_string().as_str()));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn scoped_chrome_ref_stale_is_identity_failure() {
        let r = chrome_ref(7);
        chrome::set_scoped_resolve_stub(Some(Err(
            crate::identity::IdentityReport::chrome_failure(
                &r,
                IdentityState::Stale,
                "stale_element",
            ),
        )));
        let space = Space::new(0, 0, 1920, 1080).unwrap();
        let outcome = Target::Scoped(r).resolve_with(space, Some(0x1234)).unwrap();
        chrome::set_scoped_resolve_stub(None);
        match outcome {
            TargetOutcome::Identity { report, .. } => {
                assert_eq!(report.state, IdentityState::Stale);
                assert_eq!(report.reason.as_deref(), Some("stale_element"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn scoped_chrome_ref_unknown_is_identity_failure() {
        let r = chrome_ref(7);
        chrome::set_scoped_resolve_stub(Some(Err(
            crate::identity::IdentityReport::chrome_failure(
                &r,
                IdentityState::Unknown,
                "resolve_timeout",
            ),
        )));
        let space = Space::new(0, 0, 1920, 1080).unwrap();
        let outcome = Target::Scoped(r).resolve_with(space, Some(0x1234)).unwrap();
        chrome::set_scoped_resolve_stub(None);
        match outcome {
            TargetOutcome::Identity { report, .. } => {
                assert_eq!(report.state, IdentityState::Unknown);
                assert!(report.retryable);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn malformed_sref_is_hard_error_not_legacy() {
        for bad in [
            "sref1:c.0.0.0000.00000000.0",
            "sref1:c.12.0.9F3A.a1b2c3d4.7",
            "sref2:c.12.0.9f3a.a1b2c3d4.7",
            "sref1:c.12.0.9f3a.a1b2c3d4",
            "sref1:u.0.7d0e1f22.42a1b2c3",
        ] {
            let err = Target::parse(Some(bad), None, None, None).unwrap_err();
            let msg = err.to_string();
            assert!(msg.contains("sref1:"), "{bad}: {msg}");
            assert!(!msg.contains("chr:"), "{bad}: {msg}");
        }
    }

    #[test]
    fn legacy_chr_and_uia_still_resolve() {
        assert!(matches!(
            Target::parse(Some("chr:0"), None, None, None).unwrap(),
            Target::ElementId(id) if id == "chr:0"
        ));
        assert!(matches!(
            Target::parse(Some("uia:42.1"), None, None, None).unwrap(),
            Target::ElementId(id) if id == "uia:42.1"
        ));
        assert_eq!(
            crate::identity::parse_legacy("chr:0"),
            Some(crate::identity::LegacyKind::Chr(0))
        );
    }

    fn uia_ref_with_fp(fp: &str) -> SnapshotRef {
        SnapshotRef::Uia {
            hwnd: 0x1234,
            fp: fp.into(),
            ridh: "deadbeef".into(),
        }
    }

    fn scoped_uia_found() -> uia::ResolvedElement {
        uia::ResolvedElement {
            rect: Rect {
                x: 10,
                y: 20,
                w: 60,
                h: 20,
            },
            name: "Send".into(),
            role: "Button".into(),
            hwnd: Some(0x1234),
            runtime_id: vec![1, 2],
            own_name: "Send".into(),
            own_role: "Button".into(),
            is_password: false,
        }
    }

    fn uia_fp(found: &uia::ResolvedElement) -> String {
        format!("{:08x}", found.own_fingerprint())
    }

    #[test]
    fn scoped_uia_ref_window_mismatch_is_stale() {
        uia::set_scoped_uia_hook(Some(|_, _, _| {
            Err(uia::ScopedFailure {
                state: IdentityState::Stale,
                reason: "window_mismatch",
            })
        }));
        let r = uia_ref_with_fp("00000000");
        let space = Space::new(0, 0, 1920, 1080).unwrap();
        let outcome = Target::Scoped(r).resolve_with(space, None).unwrap();
        uia::set_scoped_uia_hook(None);
        match outcome {
            TargetOutcome::Identity { report, .. } => {
                assert_eq!(report.state, IdentityState::Stale);
                assert_eq!(report.reason.as_deref(), Some("window_mismatch"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn scoped_uia_ref_identity_mismatch_is_stale() {
        uia::set_scoped_uia_hook(Some(|_, _, _| {
            Err(uia::ScopedFailure {
                state: IdentityState::Stale,
                reason: "identity_mismatch",
            })
        }));
        let r = uia_ref_with_fp("00000000");
        let space = Space::new(0, 0, 1920, 1080).unwrap();
        let outcome = Target::Scoped(r).resolve_with(space, None).unwrap();
        uia::set_scoped_uia_hook(None);
        assert!(
            matches!(outcome, TargetOutcome::Identity { report, .. } if report.reason.as_deref() == Some("identity_mismatch"))
        );
    }

    #[test]
    fn scoped_uia_ref_duplicate_is_ambiguous() {
        uia::set_scoped_uia_hook(Some(|_, _, _| {
            Err(uia::ScopedFailure {
                state: IdentityState::Ambiguous,
                reason: "duplicate_runtime_id",
            })
        }));
        let r = uia_ref_with_fp("00000000");
        let space = Space::new(0, 0, 1920, 1080).unwrap();
        let outcome = Target::Scoped(r).resolve_with(space, None).unwrap();
        uia::set_scoped_uia_hook(None);
        assert!(
            matches!(outcome, TargetOutcome::Identity { report, .. } if report.state == IdentityState::Ambiguous)
        );
    }

    #[test]
    fn scoped_uia_ref_timeout_is_unknown() {
        uia::set_scoped_uia_hook(Some(|_, _, _| {
            Err(uia::ScopedFailure {
                state: IdentityState::Unknown,
                reason: "provider_timeout",
            })
        }));
        let r = uia_ref_with_fp("00000000");
        let space = Space::new(0, 0, 1920, 1080).unwrap();
        let outcome = Target::Scoped(r).resolve_with(space, None).unwrap();
        uia::set_scoped_uia_hook(None);
        assert!(
            matches!(outcome, TargetOutcome::Identity { report, .. } if report.state == IdentityState::Unknown && !report.retryable)
        );
    }

    #[test]
    fn scoped_uia_ref_exact_keeps_runtime_id_for_dispatch() {
        let found = scoped_uia_found();
        let fp = uia_fp(&found);
        uia::set_scoped_uia_hook(Some(|_, _, _| Ok(scoped_uia_found())));
        let r = uia_ref_with_fp(&fp);
        let space = Space::new(0, 0, 1920, 1080).unwrap();
        let outcome = Target::Scoped(r.clone()).resolve_with(space, None).unwrap();
        uia::set_scoped_uia_hook(None);
        match outcome {
            TargetOutcome::Resolved(resolved) => {
                assert_eq!(resolved.id.as_deref(), Some(r.to_string().as_str()));
                assert_eq!(resolved.uia_runtime_id, Some(vec![1, 2]));
                assert!(resolved.uia_expected_fp.is_some());
                assert_eq!(
                    resolved.identity.as_ref().map(|i| i.state),
                    Some(IdentityState::Exact)
                );
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
