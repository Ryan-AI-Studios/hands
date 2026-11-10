//! Frame-link validation and composition (0131).
//!
//! A frame link is the child frame's **content box** in the parent's CSS viewport, plus the
//! parent's clipping rectangles and the per-level transform/visibility/hit verdict. Composition
//! translates a frame-local CSS point/rect through every link up to frame 0. It never adds or
//! subtracts scroll: `getBoundingClientRect` is already layout-viewport relative in each document.
//!
//! Composition happens once, at the consumer named by the track plan. This module is pure.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::point::{CssPoint, CssRect};

/// Hard cap from spec §3.5.8. Depth 6 (five links plus the root) is refused.
pub const MAX_FRAME_DEPTH: usize = 5;

/// Per-process frame feature switch. `HANDS_FRAMES=0` sends no `frames` field and refuses
/// nonzero refs with `frame_unsupported`. It is not a server-wide/persisted setting; the true
/// rollback is the manifest revert plus an extension reload.
pub fn enabled() -> bool {
    match std::env::var("HANDS_FRAMES") {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => true,
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq)]
pub struct ClipRect {
    #[serde(default)]
    pub left: f64,
    #[serde(default)]
    pub top: f64,
    #[serde(default)]
    pub width: f64,
    #[serde(default)]
    pub height: f64,
}

impl ClipRect {
    fn finite(&self) -> bool {
        self.left.is_finite()
            && self.top.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
            && self.width >= 0.0
            && self.height >= 0.0
    }

    fn to_css(self) -> CssRect {
        CssRect {
            left: self.left,
            top: self.top,
            width: self.width,
            height: self.height,
        }
    }
}

/// One child frame's link in its parent's CSS viewport. `parent` is the parent Chrome frame id.
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
pub struct FrameLink {
    #[serde(default)]
    pub parent: Option<u32>,
    #[serde(default)]
    pub content_left: f64,
    #[serde(default)]
    pub content_top: f64,
    #[serde(default)]
    pub content_width: f64,
    #[serde(default)]
    pub content_height: f64,
    #[serde(default)]
    pub clip: Vec<ClipRect>,
    #[serde(default)]
    pub transform_ok: bool,
    #[serde(default)]
    pub visible: bool,
    #[serde(default)]
    pub hit_ok: bool,
    #[serde(default)]
    pub onscreen: bool,
    #[serde(default)]
    pub viewport_w: f64,
    #[serde(default)]
    pub viewport_h: f64,
    #[serde(default)]
    pub child_viewport_w: Option<f64>,
    #[serde(default)]
    pub child_viewport_h: Option<f64>,
    #[serde(default)]
    pub slot: Option<u32>,
}

impl FrameLink {
    fn finite(&self) -> bool {
        self.content_left.is_finite()
            && self.content_top.is_finite()
            && self.content_width.is_finite()
            && self.content_height.is_finite()
            && self.viewport_w.is_finite()
            && self.viewport_h.is_finite()
            && self.clip.iter().all(ClipRect::finite)
    }

    /// Link validity before composition. Refusals are closed tokens (spec §3.5, §3.6.5).
    pub fn validate(&self) -> Result<(), FrameError> {
        let Some(_parent) = self.parent else {
            return Err(FrameError::MissingParent);
        };
        if !self.finite() {
            return Err(FrameError::Geometry);
        }
        if self.content_width < 1.0 || self.content_height < 1.0 {
            return Err(FrameError::Geometry);
        }
        if self.viewport_w < 1.0 || self.viewport_h < 1.0 {
            return Err(FrameError::Geometry);
        }
        if !self.transform_ok {
            return Err(FrameError::Transform);
        }
        if !self.visible || !self.onscreen {
            return Err(FrameError::NotVisible);
        }
        if !self.hit_ok {
            return Err(FrameError::Occluded);
        }
        Ok(())
    }
}

/// Raw frame row as emitted in a snapshot/resolve reply. `url` is internal only (vendor
/// classification); it is never serialized in the observe envelope.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct RawFrameLink {
    #[serde(default)]
    pub frame: Option<u32>,
    #[serde(default)]
    pub parent: Option<u32>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub snap: Option<String>,
    #[serde(default)]
    pub retained: Option<usize>,
    /// Browser-set frame URL for internal `match_host` classification. Never emitted.
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub link: Option<FrameLink>,
}

impl RawFrameLink {
    pub fn is_ok(&self) -> bool {
        self.status.as_deref() == Some("ok")
    }

    pub fn is_transient(&self) -> bool {
        matches!(
            self.reason.as_deref(),
            Some(
                "frame_late"
                    | "frame_budget_exceeded"
                    | "frame_cap_exceeded"
                    | "frame_link_missing"
                    | "frame_geometry_unavailable"
            )
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    MissingParent,
    MissingLink,
    Cycle,
    Depth,
    Geometry,
    Transform,
    NotVisible,
    Occluded,
}

impl FrameError {
    pub fn token(self) -> &'static str {
        match self {
            Self::MissingParent | Self::MissingLink => "frame_geometry_unavailable",
            Self::Cycle => "frame_geometry_unavailable",
            Self::Depth => "frame_depth_exceeded",
            Self::Geometry => "frame_geometry_unavailable",
            Self::Transform => "frame_geometry_unavailable",
            Self::NotVisible => "frame_not_visible",
            Self::Occluded => "frame_occluded",
        }
    }
}

fn lookup(rows: &[RawFrameLink], frame: u32) -> Option<&FrameLink> {
    rows.iter()
        .find(|row| row.frame == Some(frame) && row.is_ok())
        .and_then(|row| row.link.as_ref())
}

fn intersect(a: CssRect, b: CssRect) -> Option<CssRect> {
    let left = a.left.max(b.left);
    let top = a.top.max(b.top);
    let right = (a.left + a.width).min(b.left + b.width);
    let bottom = (a.top + a.height).min(b.top + b.height);
    let width = right - left;
    let height = bottom - top;
    if width <= 0.0 || height <= 0.0 {
        None
    } else {
        Some(CssRect {
            left,
            top,
            width,
            height,
        })
    }
}

fn clip_rect(rect: CssRect, link: &FrameLink) -> Result<CssRect, FrameError> {
    let mut out = rect;
    for clip in &link.clip {
        let Some(next) = intersect(out, clip.to_css()) else {
            return Err(FrameError::NotVisible);
        };
        out = next;
    }
    Ok(out)
}

fn point_in_clip(point: CssPoint, link: &FrameLink) -> bool {
    link.clip.iter().all(|clip| {
        point.x >= clip.left
            && point.y >= clip.top
            && point.x < clip.left + clip.width
            && point.y < clip.top + clip.height
    })
}

fn walk_links(
    rows: &[RawFrameLink],
    frame: u32,
    mut apply: impl FnMut(&FrameLink, CssRect) -> Result<CssRect, FrameError>,
    start: CssRect,
) -> Result<CssRect, FrameError> {
    if frame == 0 {
        return Ok(start);
    }
    let mut rect = start;
    let mut current = frame;
    let mut seen = HashSet::new();
    for depth in 0..=MAX_FRAME_DEPTH {
        if current == 0 {
            return Ok(rect);
        }
        if depth == MAX_FRAME_DEPTH {
            return Err(FrameError::Depth);
        }
        if !seen.insert(current) {
            return Err(FrameError::Cycle);
        }
        let link = lookup(rows, current).ok_or(FrameError::MissingLink)?;
        link.validate()?;
        rect = apply(link, rect)?;
        current = link.parent.ok_or(FrameError::MissingParent)?;
    }
    Err(FrameError::Depth)
}

/// Translate a frame-local CSS rect into top-frame CSS viewport coordinates **without**
/// clipping. Used alongside `compose_rect` to detect a clipped target (spec §3.5.11).
pub fn compose_rect_unclipped(
    rows: &[RawFrameLink],
    frame: u32,
    local: CssRect,
) -> Result<CssRect, FrameError> {
    walk_links(
        rows,
        frame,
        |link, rect| {
            Ok(CssRect {
                left: link.content_left + rect.left,
                top: link.content_top + rect.top,
                width: rect.width,
                height: rect.height,
            })
        },
        local,
    )
}

/// Translate a frame-local CSS rect into top-frame CSS viewport coordinates.
pub fn compose_rect(
    rows: &[RawFrameLink],
    frame: u32,
    local: CssRect,
) -> Result<CssRect, FrameError> {
    if !local.left.is_finite()
        || !local.top.is_finite()
        || !local.width.is_finite()
        || !local.height.is_finite()
        || local.width <= 0.0
        || local.height <= 0.0
    {
        return Err(FrameError::Geometry);
    }
    walk_links(
        rows,
        frame,
        |link, rect| {
            let translated = CssRect {
                left: link.content_left + rect.left,
                top: link.content_top + rect.top,
                width: rect.width,
                height: rect.height,
            };
            clip_rect(translated, link)
        },
        local,
    )
}

/// Translate a frame-local CSS point into top-frame CSS viewport coordinates.
pub fn compose_point(
    rows: &[RawFrameLink],
    frame: u32,
    local: CssPoint,
) -> Result<CssPoint, FrameError> {
    if !local.x.is_finite() || !local.y.is_finite() {
        return Err(FrameError::Geometry);
    }
    walk_links(
        rows,
        frame,
        |link, rect| {
            let point = CssPoint {
                x: link.content_left + rect.left,
                y: link.content_top + rect.top,
            };
            if !point_in_clip(point, link) {
                return Err(FrameError::NotVisible);
            }
            Ok(CssRect {
                left: point.x,
                top: point.y,
                width: rect.width,
                height: rect.height,
            })
        },
        CssRect {
            left: local.x,
            top: local.y,
            width: 1.0,
            height: 1.0,
        },
    )
    .map(|rect| CssPoint {
        x: rect.left,
        y: rect.top,
    })
}

/// Walk `frame` up to 0 and return links deepest-first. Used by fixture resolves and by the
/// extension reply parser; `None` means a missing/cyclic/over-depth chain.
pub fn chain_for(rows: &[RawFrameLink], frame: u32) -> Option<Vec<FrameLink>> {
    let mut chain = Vec::new();
    let mut current = frame;
    let mut seen = HashSet::new();
    for _ in 0..=MAX_FRAME_DEPTH {
        if current == 0 {
            return Some(chain);
        }
        if !seen.insert(current) {
            return None;
        }
        let link = lookup(rows, current)?.clone();
        current = link.parent?;
        chain.push(link);
    }
    None
}

/// Build a frame-id -> link map for callers that need per-level inspection.
pub fn links_by_frame(rows: &[RawFrameLink]) -> HashMap<u32, FrameLink> {
    rows.iter()
        .filter_map(|row| match (row.frame, row.link.as_ref()) {
            (Some(frame), Some(link)) if row.is_ok() => Some((frame, link.clone())),
            _ => None,
        })
        .collect()
}

/// True when any frame row is a transient non-`ok` status (solver yield hold).
pub fn frames_transient(rows: &[RawFrameLink]) -> bool {
    rows.iter().any(|row| !row.is_ok() && row.is_transient())
}

/// Validate an emitted origin: scheme + host + optional port, no userinfo/path/query.
pub fn valid_origin(origin: &str) -> Option<String> {
    let origin = origin.trim().to_ascii_lowercase();
    if origin.is_empty() || origin == "null" || origin.len() > 80 {
        return None;
    }
    let scheme_end = origin.find("://")?;
    if scheme_end == 0 {
        return None;
    }
    let scheme = &origin[..scheme_end];
    if !scheme
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
    {
        return None;
    }
    let rest = &origin[scheme_end + 3..];
    if rest.is_empty()
        || rest.contains('/')
        || rest.contains('?')
        || rest.contains('#')
        || rest.contains('@')
        || rest.contains(' ')
    {
        return None;
    }
    Some(origin)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(parent: u32, left: f64, top: f64, w: f64, h: f64) -> FrameLink {
        FrameLink {
            parent: Some(parent),
            content_left: left,
            content_top: top,
            content_width: w,
            content_height: h,
            clip: vec![ClipRect {
                left,
                top,
                width: w,
                height: h,
            }],
            transform_ok: true,
            visible: true,
            hit_ok: true,
            onscreen: true,
            viewport_w: 1920.0,
            viewport_h: 1080.0,
            ..FrameLink::default()
        }
    }

    fn row(frame: u32, link: FrameLink) -> RawFrameLink {
        RawFrameLink {
            frame: Some(frame),
            status: Some("ok".into()),
            link: Some(link),
            ..RawFrameLink::default()
        }
    }

    fn rect(l: f64, t: f64, w: f64, h: f64) -> CssRect {
        CssRect {
            left: l,
            top: t,
            width: w,
            height: h,
        }
    }

    #[test]
    fn frame_offset_composes_once() {
        let rows = vec![row(3, link(0, 40.0, 60.0, 300.0, 250.0))];
        let out = compose_rect(&rows, 3, rect(10.0, 20.0, 100.0, 40.0)).expect("compose");
        assert_eq!(out.left, 50.0);
        assert_eq!(out.top, 80.0);
        assert_eq!(out.width, 100.0);
    }

    #[test]
    fn scrolled_child_does_not_double_scroll() {
        // The child reports a scroll-relative local rect. No scroll term is added: the
        // content-box origin is applied exactly once.
        let rows = vec![row(3, link(0, 10.0, 20.0, 300.0, 250.0))];
        let out = compose_rect(&rows, 3, rect(5.0, 10.0, 50.0, 20.0)).expect("compose");
        assert_eq!(out.left, 15.0);
        assert_eq!(out.top, 30.0);
        // A scroll-relative rect above the child viewport is clipped by the content box,
        // never shifted by a scroll term.
        let clipped = compose_rect(&rows, 3, rect(5.0, -15.0, 50.0, 20.0)).expect("compose");
        assert_eq!(clipped.top, 20.0);
    }

    #[test]
    fn nested_links_compose_in_order() {
        let rows = vec![
            row(5, link(3, 5.0, 7.0, 200.0, 100.0)),
            row(3, link(0, 40.0, 60.0, 300.0, 250.0)),
        ];
        let out = compose_rect(&rows, 5, rect(1.0, 2.0, 10.0, 10.0)).expect("compose");
        assert_eq!(out.left, 46.0);
        assert_eq!(out.top, 69.0);
    }

    #[test]
    fn negative_parent_origin_is_allowed() {
        let rows = vec![row(3, link(0, -100.0, 20.0, 300.0, 250.0))];
        let out = compose_point(&rows, 3, CssPoint::new(10.0, 10.0)).expect("compose");
        assert_eq!(out.x, -90.0);
    }

    #[test]
    fn clip_at_parent_refuses() {
        let mut l = link(0, 0.0, 0.0, 300.0, 250.0);
        l.clip = vec![ClipRect {
            left: 0.0,
            top: 0.0,
            width: 100.0,
            height: 100.0,
        }];
        let rows = vec![row(3, l)];
        assert_eq!(
            compose_rect(&rows, 3, rect(200.0, 200.0, 50.0, 50.0)),
            Err(FrameError::NotVisible)
        );
    }

    #[test]
    fn transform_visibility_and_occlusion_are_named() {
        let mut l = link(0, 0.0, 0.0, 300.0, 250.0);
        l.transform_ok = false;
        let rows = vec![row(3, l)];
        assert_eq!(
            compose_rect(&rows, 3, rect(1.0, 1.0, 10.0, 10.0)),
            Err(FrameError::Transform)
        );

        let mut l = link(0, 0.0, 0.0, 300.0, 250.0);
        l.visible = false;
        let rows = vec![row(3, l)];
        assert_eq!(
            compose_rect(&rows, 3, rect(1.0, 1.0, 10.0, 10.0)),
            Err(FrameError::NotVisible)
        );

        let mut l = link(0, 0.0, 0.0, 300.0, 250.0);
        l.hit_ok = false;
        let rows = vec![row(3, l)];
        assert_eq!(
            compose_rect(&rows, 3, rect(1.0, 1.0, 10.0, 10.0)),
            Err(FrameError::Occluded)
        );
    }

    #[test]
    fn depth_cycle_missing_and_nonfinite_refuse() {
        let rows = vec![
            row(6, link(5, 0.0, 0.0, 10.0, 10.0)),
            row(5, link(4, 0.0, 0.0, 10.0, 10.0)),
            row(4, link(3, 0.0, 0.0, 10.0, 10.0)),
            row(3, link(2, 0.0, 0.0, 10.0, 10.0)),
            row(2, link(1, 0.0, 0.0, 10.0, 10.0)),
            row(1, link(0, 0.0, 0.0, 10.0, 10.0)),
        ];
        assert_eq!(
            compose_rect(&rows, 6, rect(1.0, 1.0, 5.0, 5.0)),
            Err(FrameError::Depth)
        );

        let rows = vec![
            row(3, link(2, 0.0, 0.0, 10.0, 10.0)),
            row(2, link(3, 0.0, 0.0, 10.0, 10.0)),
        ];
        assert_eq!(
            compose_rect(&rows, 3, rect(1.0, 1.0, 5.0, 5.0)),
            Err(FrameError::Cycle)
        );

        assert_eq!(
            compose_rect(&[], 3, rect(1.0, 1.0, 5.0, 5.0)),
            Err(FrameError::MissingLink)
        );

        let mut l = link(0, 0.0, 0.0, 300.0, 250.0);
        l.content_left = f64::NAN;
        let rows = vec![row(3, l)];
        assert_eq!(
            compose_rect(&rows, 3, rect(1.0, 1.0, 5.0, 5.0)),
            Err(FrameError::Geometry)
        );
    }

    #[test]
    fn frame_zero_is_identity() {
        assert_eq!(
            compose_rect(&[], 0, rect(1.0, 2.0, 3.0, 4.0)).expect("top"),
            rect(1.0, 2.0, 3.0, 4.0)
        );
    }

    #[test]
    fn origin_is_clamped() {
        assert_eq!(
            valid_origin("HTTP://Widget.Example:443"),
            Some("http://widget.example:443".into())
        );
        assert_eq!(valid_origin("null"), None);
        assert_eq!(valid_origin("https://x.test/path"), None);
        assert_eq!(valid_origin("https://user@x.test"), None);
    }

    #[test]
    fn transient_reasons_are_closed() {
        let row = RawFrameLink {
            status: Some("unavailable".into()),
            reason: Some("frame_late".into()),
            ..RawFrameLink::default()
        };
        assert!(row.is_transient());
        let row = RawFrameLink {
            status: Some("unavailable".into()),
            reason: Some("restricted_scheme".into()),
            ..RawFrameLink::default()
        };
        assert!(!row.is_transient());
    }
}
