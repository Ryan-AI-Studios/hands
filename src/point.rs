//! Shared CSS -> physical point contract (0129, consumed by 0128).
//!
//! Coordinate spaces:
//! - **CSS viewport**: `getBoundingClientRect` / `elementFromPoint` space. The origin is the
//!   layout viewport's top-left. `innerWidth/Height` include rendered scrollbars;
//!   `documentElement.clientWidth/Height` exclude them.
//! - **Physical virtual screen**: PMv2 raw device pixels, union of `EnumDisplayMonitors`
//!   `rcMonitor`. May be negative.
//!
//! The map is validated against an OS-observed page viewport anchor (the render-widget child
//! HWND, or the UIA `Document` bounding rect) at resolve/observe time. The scale is the page
//! `devicePixelRatio`; the anchor proves the origin and the page size. When no anchor matches,
//! the contract is `valid:false`, `physical:null`, and a named reason is recorded. It never
//! serializes a guessed point.
//!
//! Ingest uses [`map_point_unbounded`] / [`map_rect_unbounded`]: a pure linear transform with
//! no validity gate and no viewport bound, so off-screen elements/cards survive for the
//! existing `in_viewport` / `retain_hittable_centers` filters. Only the click/hover/scroll
//! path applies [`map_point`] / [`map_rect`].

use serde::{Deserialize, Serialize};

use crate::space::Rect;

/// CSS pixel point relative to the page viewport.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CssPoint {
    pub x: f64,
    pub y: f64,
}

impl CssPoint {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// CSS pixel rectangle relative to the page viewport.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CssRect {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

impl CssRect {
    pub fn center(&self) -> CssPoint {
        CssPoint {
            x: self.left + self.width / 2.0,
            y: self.top + self.height / 2.0,
        }
    }

    pub fn is_positive(&self) -> bool {
        self.width > 0.0 && self.height > 0.0
    }
}

/// Physical virtual-screen pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalPoint {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointSource {
    Chr,
    Uia,
    Grid,
    Pixel,
    Blind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransformMethod {
    RenderWidgetHost,
    UiaDocument,
    Fixture,
    Explicit,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransformConfidence {
    High,
    Medium,
    None,
}

/// Basis that validated the *anchor size* and supplies the scale denominator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CssBasis {
    /// `innerWidth` / `innerHeight` (rendered scrollbars included).
    Inner,
    /// `documentElement.clientWidth` / `clientHeight` (scrollbars excluded).
    Client,
}

/// Serialized/public subset of a [`ViewportMap`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransformReport {
    pub valid: bool,
    pub method: TransformMethod,
    pub confidence: TransformConfidence,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dpr: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dpi: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_zoom: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_viewport: Option<Rect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor_basis: Option<CssBasis>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout_basis: Option<CssBasis>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observe_id: Option<String>,
}

impl TransformReport {
    pub fn invalid(reason: impl Into<String>, observe_id: Option<String>) -> Self {
        Self {
            valid: false,
            method: TransformMethod::None,
            confidence: TransformConfidence::None,
            scale: None,
            dpr: None,
            dpi: None,
            page_zoom: None,
            page_viewport: None,
            anchor_basis: None,
            layout_basis: None,
            reason: Some(bounded_reason(reason.into())),
            observe_id,
        }
    }
}

/// The one shared point contract. 0128 embeds this and never computes its own transform.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PointContract {
    pub source: PointSource,
    pub css: Option<CssPoint>,
    pub physical: Option<PhysicalPoint>,
    #[serde(flatten)]
    pub transform: TransformReport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapError {
    Invalid(String),
    NotCss,
    OutOfViewport,
}

impl std::fmt::Display for MapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "invalid transform: {reason}"),
            Self::NotCss => write!(f, "point is not a CSS-mapped target"),
            Self::OutOfViewport => write!(f, "CSS point is outside the layout viewport"),
        }
    }
}

impl std::error::Error for MapError {}

/// Page metrics carried by the host reply (all values in CSS pixels).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PageMetrics {
    pub inner_width: Option<f64>,
    pub inner_height: Option<f64>,
    pub client_width: Option<f64>,
    pub client_height: Option<f64>,
    pub dpr: Option<f64>,
    pub visual_scale: Option<f64>,
    pub visual_offset_left: Option<f64>,
    pub visual_offset_top: Option<f64>,
}

/// One display monitor with its effective DPI (read-only geometry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorDpi {
    pub rect: Rect,
    pub dpi: u32,
}

/// Window/tab identity outcome for `chr:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowIdentity {
    /// Caption ends with the reported page title.
    Confirmed,
    /// No title claim and exactly one visible daily-Chrome window.
    SingleWindow,
    /// No title claim and two or more visible daily-Chrome windows.
    Ambiguous,
    /// Not daily Chrome, a Chrome-owned popup has focus, or the title disagrees.
    #[default]
    Mismatch,
}

/// Internal map builder input. The map functions themselves are pure; all Win32 reads happen
/// in `chrome`/`foreground`/`uia`/`space` before this struct is built.
#[derive(Debug, Clone, Default)]
pub struct MapInputs {
    pub hwnd: Option<isize>,
    pub window_rect: Option<Rect>,
    pub dpi: Option<u32>,
    pub virtual_screen: Option<Rect>,
    pub page: PageMetrics,
    pub render_widgets: Vec<Rect>,
    pub uia_document: Option<Rect>,
    pub monitors: Vec<MonitorDpi>,
    pub observe_id: Option<String>,
    /// Page title carried by this snapshot/resolve reply. Used by remap identity checks; never
    /// the OS window caption (which includes the browser suffix).
    pub page_title: Option<String>,
    pub fixture: bool,
    pub identity: WindowIdentity,
    /// Fixture/diagnostic listing origin (physical float), legacy `outer-inner` only.
    pub fixture_origin: Option<(f64, f64)>,
}

/// Internal map: the serializable [`TransformReport`] plus the fields needed for per-use
/// revalidation (the recorded page metrics and the matched anchor).
#[derive(Debug, Clone)]
pub struct ViewportMap {
    pub hwnd: Option<isize>,
    /// Physical float origin of the page viewport (anchor top-left).
    pub origin: Option<(f64, f64)>,
    pub scale: f64,
    pub page: PageMetrics,
    /// Recorded page title from the snapshot/resolve reply (not the OS caption).
    pub page_title: Option<String>,
    pub anchor: Option<Rect>,
    pub anchor_basis: Option<CssBasis>,
    pub layout_basis: Option<CssBasis>,
    pub report: TransformReport,
}

impl Default for ViewportMap {
    fn default() -> Self {
        Self {
            hwnd: None,
            origin: None,
            scale: 1.0,
            page: PageMetrics::default(),
            page_title: None,
            anchor: None,
            anchor_basis: None,
            layout_basis: None,
            report: TransformReport::invalid("no_viewport_anchor", None),
        }
    }
}

enum AnchorPick {
    Unique(Rect, CssBasis),
    Ambiguous,
    None,
}

/// Number of `inner`/`client` CSS sizes an anchor matches within 0.5 CSS px.
fn anchor_basis(rect: Rect, page: &PageMetrics) -> Option<CssBasis> {
    let dpr = page.dpr?;
    if !dpr.is_finite() || dpr <= 0.0 || rect.w <= 0 || rect.h <= 0 {
        return None;
    }
    let inner = match (page.inner_width, page.inner_height) {
        (Some(w), Some(h)) if w.is_finite() && h.is_finite() => {
            (rect.w as f64 / dpr - w).abs() <= 0.5 && (rect.h as f64 / dpr - h).abs() <= 0.5
        }
        _ => false,
    };
    let client = match (page.client_width, page.client_height) {
        (Some(w), Some(h)) if w.is_finite() && h.is_finite() => {
            (rect.w as f64 / dpr - w).abs() <= 0.5 && (rect.h as f64 / dpr - h).abs() <= 0.5
        }
        _ => false,
    };
    if client {
        Some(CssBasis::Client)
    } else if inner {
        Some(CssBasis::Inner)
    } else {
        None
    }
}

fn pick_anchor(rects: &[Rect], page: &PageMetrics) -> AnchorPick {
    let matches: Vec<(Rect, CssBasis)> = rects
        .iter()
        .filter_map(|r| anchor_basis(*r, page).map(|b| (*r, b)))
        .collect();
    match matches.as_slice() {
        [] => AnchorPick::None,
        [(r, b)] => AnchorPick::Unique(*r, *b),
        _ => AnchorPick::Ambiguous,
    }
}

/// Public-to-crate helper: true when `rect` matches a page basis (used before a UIA fallback).
pub fn anchor_matches(rect: Rect, page: &PageMetrics) -> Option<CssBasis> {
    anchor_basis(rect, page)
}

/// True when any supplied render-widget candidate matches the page metrics.
pub fn any_anchor_matches(rects: &[Rect], page: &PageMetrics) -> bool {
    rects.iter().any(|r| anchor_basis(*r, page).is_some())
}

fn layout_dims(page: &PageMetrics) -> Option<(f64, f64, CssBasis)> {
    if let (Some(w), Some(h)) = (page.client_width, page.client_height)
        && w.is_finite()
        && h.is_finite()
        && w > 0.0
        && h > 0.0
    {
        return Some((w, h, CssBasis::Client));
    }
    if let (Some(w), Some(h)) = (page.inner_width, page.inner_height)
        && w.is_finite()
        && h.is_finite()
        && w > 0.0
        && h > 0.0
    {
        return Some((w, h, CssBasis::Inner));
    }
    None
}

fn invalid_map(inputs: &MapInputs, reason: String, method: TransformMethod) -> ViewportMap {
    let dpr = inputs.page.dpr.filter(|d| d.is_finite() && *d > 0.0);
    let dpi = inputs.dpi.filter(|d| *d > 0);
    let page_zoom = dpr.zip(dpi).map(|(d, dpi)| d / (f64::from(dpi) / 96.0));
    let report = TransformReport {
        valid: false,
        method,
        confidence: TransformConfidence::None,
        scale: dpr,
        dpr,
        dpi,
        page_zoom: page_zoom.filter(|z| z.is_finite()),
        page_viewport: None,
        anchor_basis: None,
        layout_basis: None,
        reason: Some(bounded_reason(reason)),
        observe_id: inputs.observe_id.clone(),
    };
    ViewportMap {
        hwnd: inputs.hwnd,
        origin: None,
        scale: dpr.unwrap_or(1.0),
        page: inputs.page,
        page_title: inputs.page_title.clone(),
        anchor: None,
        anchor_basis: None,
        layout_basis: None,
        report,
    }
}

/// Build a map from already-measured geometry and page metrics. Pure.
pub fn build_map(inputs: MapInputs) -> ViewportMap {
    if inputs.fixture {
        let scale = inputs
            .page
            .dpr
            .filter(|d| d.is_finite() && *d > 0.0)
            .unwrap_or(1.0);
        let dpi = inputs.dpi.filter(|d| *d > 0);
        let page_zoom = dpi.map(|dpi| scale / (f64::from(dpi) / 96.0));
        let report = TransformReport {
            valid: false,
            method: TransformMethod::Fixture,
            confidence: TransformConfidence::None,
            scale: Some(scale),
            dpr: inputs.page.dpr,
            dpi,
            page_zoom: page_zoom.filter(|z| z.is_finite()),
            page_viewport: None,
            anchor_basis: None,
            layout_basis: None,
            reason: Some("fixture_snapshot".into()),
            observe_id: inputs.observe_id.clone(),
        };
        return ViewportMap {
            hwnd: inputs.hwnd,
            origin: inputs.fixture_origin,
            scale,
            page: inputs.page,
            page_title: inputs.page_title.clone(),
            anchor: None,
            anchor_basis: None,
            layout_basis: None,
            report,
        };
    }

    if inputs.identity == WindowIdentity::Mismatch {
        return invalid_map(
            &inputs,
            "window_mismatch: activate the intended Chrome window first".into(),
            TransformMethod::None,
        );
    }
    if inputs.identity == WindowIdentity::Ambiguous {
        return invalid_map(
            &inputs,
            "window_identity_ambiguous: two or more visible Chrome windows and no title claim"
                .into(),
            TransformMethod::None,
        );
    }

    let Some(dpr) = inputs.page.dpr.filter(|d| d.is_finite() && *d > 0.0) else {
        return invalid_map(
            &inputs,
            "bad_dpr: devicePixelRatio is not finite or <= 0".into(),
            TransformMethod::None,
        );
    };
    let Some((layout_w, layout_h, layout_basis)) = layout_dims(&inputs.page) else {
        return invalid_map(
            &inputs,
            "no_page_metrics: host reply lacks innerWidth/innerHeight".into(),
            TransformMethod::None,
        );
    };
    let Some(dpi) = inputs.dpi.filter(|d| *d > 0) else {
        return invalid_map(
            &inputs,
            "bad_dpr: GetDpiForWindow returned no effective DPI".into(),
            TransformMethod::None,
        );
    };
    let page_zoom = dpr / (f64::from(dpi) / 96.0);
    if !page_zoom.is_finite() || !(0.25..=5.0).contains(&page_zoom) {
        return invalid_map(
            &inputs,
            format!("zoom_out_of_range: page_zoom {page_zoom:.3} outside [0.25, 5.0]"),
            TransformMethod::None,
        );
    }
    if let Some(scale) = inputs.page.visual_scale
        && ((scale - 1.0).abs() > 0.001
            || inputs
                .page
                .visual_offset_left
                .is_some_and(|v| v.abs() > 1.0)
            || inputs.page.visual_offset_top.is_some_and(|v| v.abs() > 1.0))
    {
        return invalid_map(
            &inputs,
            "visual_viewport_mismatch: pinch zoom, keyboard shift or device emulation".into(),
            TransformMethod::None,
        );
    }

    let (anchor, method, anchor_basis) = match pick_anchor(&inputs.render_widgets, &inputs.page) {
        AnchorPick::Unique(rect, basis) => (rect, TransformMethod::RenderWidgetHost, basis),
        AnchorPick::Ambiguous => {
            return invalid_map(
                &inputs,
                "anchor_ambiguous: more than one render-widget child matches the page size".into(),
                TransformMethod::None,
            );
        }
        AnchorPick::None => match inputs
            .uia_document
            .and_then(|r| anchor_basis(r, &inputs.page).map(|b| (r, b)))
        {
            Some((rect, basis)) => (rect, TransformMethod::UiaDocument, basis),
            None => {
                return invalid_map(
                    &inputs,
                    "no_viewport_anchor: no matching render-widget child or UIA Document rect"
                        .into(),
                    TransformMethod::None,
                );
            }
        },
    };

    let anchor_css_w = anchor.w as f64 / dpr;
    let anchor_css_h = anchor.h as f64 / dpr;
    let ratio_w = anchor_css_w
        / match anchor_basis {
            CssBasis::Inner => inputs.page.inner_width.unwrap_or(anchor_css_w),
            CssBasis::Client => inputs.page.client_width.unwrap_or(anchor_css_w),
        };
    let ratio_h = anchor_css_h
        / match anchor_basis {
            CssBasis::Inner => inputs.page.inner_height.unwrap_or(anchor_css_h),
            CssBasis::Client => inputs.page.client_height.unwrap_or(anchor_css_h),
        };
    if ((ratio_w - ratio_h).abs() * anchor_css_w) > 0.5 {
        return invalid_map(
            &inputs,
            format!("anchor_size_mismatch: width ratio {ratio_w:.5} vs height ratio {ratio_h:.5}"),
            TransformMethod::None,
        );
    }

    if let Some(window) = inputs.window_rect {
        let last_x = anchor.x.saturating_add(anchor.w).saturating_sub(1);
        let last_y = anchor.y.saturating_add(anchor.h).saturating_sub(1);
        if !window.contains_point(anchor.x, anchor.y) || !window.contains_point(last_x, last_y) {
            return invalid_map(
                &inputs,
                "anchor_outside_window: page viewport is not inside the Chrome window rect".into(),
                TransformMethod::None,
            );
        }
    }
    if let Some(screen) = inputs.virtual_screen {
        let last_x = anchor.x.saturating_add(anchor.w).saturating_sub(1);
        let last_y = anchor.y.saturating_add(anchor.h).saturating_sub(1);
        if !screen.contains_point(anchor.x, anchor.y) || !screen.contains_point(last_x, last_y) {
            return invalid_map(
                &inputs,
                "anchor_outside_virtual_screen: page viewport is outside the virtual screen".into(),
                TransformMethod::None,
            );
        }
    }

    let mut straddle_dpis: Vec<u32> = inputs
        .monitors
        .iter()
        .filter(|m| m.rect.intersects(anchor))
        .map(|m| m.dpi)
        .collect();
    straddle_dpis.sort_unstable();
    straddle_dpis.dedup();
    if straddle_dpis.len() > 1 {
        return invalid_map(
            &inputs,
            format!(
                "mixed_dpi_straddle: viewport intersects dpi {}",
                straddle_dpis
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(" and ")
            ),
            TransformMethod::None,
        );
    }

    let page_viewport = {
        let w = round_i32(layout_w * dpr);
        let h = round_i32(layout_h * dpr);
        match (w, h) {
            (Some(w), Some(h)) if w > 0 && h > 0 => Rect {
                x: round_i32(anchor.x as f64).unwrap_or(anchor.x),
                y: round_i32(anchor.y as f64).unwrap_or(anchor.y),
                w,
                h,
            },
            _ => {
                return invalid_map(
                    &inputs,
                    "anchor_size_mismatch: layout viewport size overflowed i32".into(),
                    TransformMethod::None,
                );
            }
        }
    };

    let confidence = match (method, inputs.identity) {
        (TransformMethod::UiaDocument, _) => TransformConfidence::Medium,
        (_, WindowIdentity::Confirmed) => TransformConfidence::High,
        (_, _) => TransformConfidence::Medium,
    };

    ViewportMap {
        hwnd: inputs.hwnd,
        origin: Some((anchor.x as f64, anchor.y as f64)),
        scale: dpr,
        page: inputs.page,
        page_title: inputs.page_title.clone(),
        anchor: Some(anchor),
        anchor_basis: Some(anchor_basis),
        layout_basis: Some(layout_basis),
        report: TransformReport {
            valid: true,
            method,
            confidence,
            scale: Some(dpr),
            dpr: Some(dpr),
            dpi: Some(dpi),
            page_zoom: Some(page_zoom),
            page_viewport: Some(page_viewport),
            anchor_basis: Some(anchor_basis),
            layout_basis: Some(layout_basis),
            reason: None,
            observe_id: inputs.observe_id,
        },
    }
}

/// Rebuild a map at the final guard from live geometry plus the *recorded* page metrics.
/// Move-only returns a new map; any size/basis/DPI/monitor change is `stale_viewport`.
/// Page zoom between resolve and dispatch is not observable here (no second metrics read).
pub fn remap(old: &ViewportMap, live: MapInputs) -> Result<ViewportMap, String> {
    if live.page != old.page {
        return Err("stale_viewport: page metrics changed".into());
    }
    let new = build_map(live);
    if !new.report.valid {
        let reason = new
            .report
            .reason
            .clone()
            .unwrap_or_else(|| "unknown".into());
        if reason.starts_with("window_") {
            return Err(reason);
        }
        return Err(format!("stale_viewport: {reason}"));
    }
    if new.report.dpi != old.report.dpi {
        return Err(format!(
            "stale_viewport: window DPI changed {:?} -> {:?}",
            old.report.dpi, new.report.dpi
        ));
    }
    if new.anchor_basis != old.anchor_basis || new.layout_basis != old.layout_basis {
        return Err("stale_viewport: viewport basis changed".into());
    }
    let (Some(old_anchor), Some(new_anchor)) = (old.anchor, new.anchor) else {
        return Err("stale_viewport: anchor disappeared".into());
    };
    if old_anchor.w != new_anchor.w || old_anchor.h != new_anchor.h {
        return Err(format!(
            "stale_viewport: anchor size changed {}x{} -> {}x{}",
            old_anchor.w, old_anchor.h, new_anchor.w, new_anchor.h
        ));
    }
    Ok(new)
}

/// Click-time map: validity + layout-viewport bound. Defined for `source == Chr` only.
pub fn map_point(contract: &PointContract, css: CssPoint) -> Result<PhysicalPoint, MapError> {
    if contract.source != PointSource::Chr {
        return Err(MapError::NotCss);
    }
    if !contract.transform.valid {
        return Err(MapError::Invalid(
            contract
                .transform
                .reason
                .clone()
                .unwrap_or_else(|| "invalid".into()),
        ));
    }
    let scale = contract
        .transform
        .scale
        .filter(|s| s.is_finite() && *s > 0.0)
        .ok_or_else(|| MapError::Invalid("bad_scale".into()))?;
    let viewport = contract
        .transform
        .page_viewport
        .ok_or_else(|| MapError::Invalid("no_viewport_anchor".into()))?;
    if !css.x.is_finite() || !css.y.is_finite() {
        return Err(MapError::Invalid("bad_point".into()));
    }
    let layout_w = viewport.w as f64 / scale;
    let layout_h = viewport.h as f64 / scale;
    if css.x < 0.0 || css.y < 0.0 || css.x >= layout_w || css.y >= layout_h {
        return Err(MapError::OutOfViewport);
    }
    let x = round_i32(viewport.x as f64 + css.x * scale)
        .ok_or_else(|| MapError::Invalid("point_overflow".into()))?;
    let y = round_i32(viewport.y as f64 + css.y * scale)
        .ok_or_else(|| MapError::Invalid("point_overflow".into()))?;
    if !viewport.contains_point(x, y) {
        return Err(MapError::OutOfViewport);
    }
    Ok(PhysicalPoint { x, y })
}

/// Click-time rect map. `map_rect(..).center() == map_point(..center())` exactly.
pub fn map_rect(contract: &PointContract, css: CssRect) -> Result<Rect, MapError> {
    let scale = contract
        .transform
        .scale
        .filter(|s| s.is_finite() && *s > 0.0)
        .ok_or_else(|| MapError::Invalid("bad_scale".into()))?;
    if !css.is_positive() || !css.left.is_finite() || !css.top.is_finite() {
        return Err(MapError::Invalid("bad_rect".into()));
    }
    let p = map_point(contract, css.center())?;
    let w = round_i32(css.width * scale)
        .filter(|w| *w > 0)
        .ok_or_else(|| MapError::Invalid("rect_overflow".into()))?;
    let h = round_i32(css.height * scale)
        .filter(|h| *h > 0)
        .ok_or_else(|| MapError::Invalid("rect_overflow".into()))?;
    Ok(Rect {
        x: p.x - w / 2,
        y: p.y - h / 2,
        w,
        h,
    })
}

/// Ingest-only map: pure linear transform, no validity or viewport bound.
pub(crate) fn map_point_unbounded(map: &ViewportMap, css: CssPoint) -> Option<PhysicalPoint> {
    if !css.x.is_finite() || !css.y.is_finite() {
        return None;
    }
    let (ox, oy) = map.origin?;
    if !map.scale.is_finite() || map.scale <= 0.0 {
        return None;
    }
    Some(PhysicalPoint {
        x: round_i32(ox + css.x * map.scale)?,
        y: round_i32(oy + css.y * map.scale)?,
    })
}

/// Ingest-only rect map: same arithmetic as [`map_rect`], no validity or viewport bound.
pub(crate) fn map_rect_unbounded(map: &ViewportMap, css: CssRect) -> Option<Rect> {
    if !css.is_positive() || !css.left.is_finite() || !css.top.is_finite() {
        return None;
    }
    let p = map_point_unbounded(map, css.center())?;
    let w = round_i32(css.width * map.scale).filter(|w| *w > 0)?;
    let h = round_i32(css.height * map.scale).filter(|h| *h > 0)?;
    Some(Rect {
        x: p.x - w / 2,
        y: p.y - h / 2,
        w,
        h,
    })
}

/// Build the click/hover/scroll contract for a CSS point under a validated map.
pub fn contract_for(map: &ViewportMap, css: CssPoint) -> PointContract {
    let mut contract = PointContract {
        source: PointSource::Chr,
        css: Some(css),
        physical: None,
        transform: map.report.clone(),
    };
    contract.physical = map_point(&contract, css).ok();
    contract
}

/// Explicit (non-CSS) contract for `uia:` / grid / pixel / blind targets.
pub fn explicit_contract(source: PointSource, physical: PhysicalPoint) -> PointContract {
    PointContract {
        source,
        css: None,
        physical: Some(physical),
        transform: TransformReport {
            valid: true,
            method: TransformMethod::Explicit,
            confidence: TransformConfidence::High,
            scale: None,
            dpr: None,
            dpi: None,
            page_zoom: None,
            page_viewport: None,
            anchor_basis: None,
            layout_basis: None,
            reason: None,
            observe_id: None,
        },
    }
}

fn round_i32(v: f64) -> Option<i32> {
    if !v.is_finite() {
        return None;
    }
    let r = v.round();
    if r < f64::from(i32::MIN) || r > f64::from(i32::MAX) {
        return None;
    }
    Some(r as i32)
}

fn bounded_reason(reason: String) -> String {
    const MAX: usize = 120;
    let mut out = String::new();
    for ch in reason.chars() {
        if out.chars().count() >= MAX {
            break;
        }
        out.push(ch);
    }
    out
}

/// Normalize a Chrome caption and compare it to the reported page title.
/// Strip one trailing ` - Google Chrome` / ` - Chromium`; ASCII lowercase only.
pub fn title_matches(page_title: &str, caption: &str) -> bool {
    let page = normalize_ws(page_title).to_ascii_lowercase();
    if page.is_empty() {
        return false;
    }
    let mut cap = normalize_ws(caption).to_ascii_lowercase();
    for suffix in [" - google chrome", " - chromium"] {
        if let Some(stripped) = cap.strip_suffix(suffix) {
            cap = stripped.trim_end().to_string();
            break;
        }
    }
    !cap.is_empty() && cap.ends_with(&page)
}

/// Resolve identity confidence from a page title, caption and visible Chrome-window count.
pub fn identity_from(
    page_title: &str,
    caption: Option<&str>,
    visible_chrome_windows: usize,
) -> WindowIdentity {
    let page = normalize_ws(page_title);
    if !page.is_empty()
        && let Some(caption) = caption.filter(|c| !normalize_ws(c).is_empty())
    {
        return if title_matches(&page, caption) {
            WindowIdentity::Confirmed
        } else {
            WindowIdentity::Mismatch
        };
    }
    if visible_chrome_windows == 1 {
        WindowIdentity::SingleWindow
    } else {
        WindowIdentity::Ambiguous
    }
}

fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------- observe id

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Process-local monotonic generation. Incremented per observe snapshot and per `chr:` resolve.
pub fn next_generation() -> u64 {
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// FNV-1a64 over the pinned identity string, 16 lowercase hex chars.
pub fn observe_id(parts: &str) -> String {
    format!("{:016x}", fnv1a64(parts.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(dpr: f64, inner: (f64, f64), client: Option<(f64, f64)>) -> PageMetrics {
        PageMetrics {
            inner_width: Some(inner.0),
            inner_height: Some(inner.1),
            client_width: client.map(|c| c.0),
            client_height: client.map(|c| c.1),
            dpr: Some(dpr),
            ..PageMetrics::default()
        }
    }

    fn inputs(anchor: Rect, page: PageMetrics) -> MapInputs {
        MapInputs {
            hwnd: Some(0x1234),
            window_rect: Some(Rect {
                x: -10,
                y: -10,
                w: 8000,
                h: 6000,
            }),
            dpi: Some((96.0 * page.dpr.unwrap_or(1.0)).round() as u32),
            virtual_screen: Some(Rect {
                x: -1920,
                y: -200,
                w: 20000,
                h: 10000,
            }),
            page,
            render_widgets: vec![anchor],
            uia_document: None,
            monitors: Vec::new(),
            observe_id: Some("f3a9c1d2e4b5a607".into()),
            page_title: None,
            fixture: false,
            identity: WindowIdentity::Confirmed,
            fixture_origin: None,
        }
    }

    fn valid_contract(page_viewport: Rect, scale: f64) -> PointContract {
        PointContract {
            source: PointSource::Chr,
            css: Some(CssPoint::new(10.0, 20.0)),
            physical: None,
            transform: TransformReport {
                valid: true,
                method: TransformMethod::RenderWidgetHost,
                confidence: TransformConfidence::High,
                scale: Some(scale),
                dpr: Some(scale),
                dpi: Some((96.0 * scale).round() as u32),
                page_zoom: Some(1.0),
                page_viewport: Some(page_viewport),
                anchor_basis: Some(CssBasis::Client),
                layout_basis: Some(CssBasis::Client),
                reason: None,
                observe_id: Some("f3a9c1d2e4b5a607".into()),
            },
        }
    }

    #[test]
    fn scale_table_inner_and_client() {
        // C1: CSS-space tolerance, not a fixed physical pixel bound.
        for dpr in [1.0, 1.25, 1.5, 1.75, 2.0] {
            for (inner, client) in [
                ((1200.0, 900.0), None),
                ((1200.0, 900.0), Some((1185.0, 900.0))),
            ] {
                let p = page(dpr, inner, client);
                let (w, h) = client.unwrap_or(inner);
                let anchor = Rect {
                    x: 100,
                    y: 80,
                    w: (w * dpr).round() as i32,
                    h: (h * dpr).round() as i32,
                };
                let map = build_map(inputs(anchor, p));
                assert!(
                    map.report.valid,
                    "dpr={dpr} inner={inner:?} client={client:?}"
                );
                assert_eq!(map.report.scale, Some(dpr));
                assert_eq!(
                    map.report.anchor_basis,
                    Some(if client.is_some() {
                        CssBasis::Client
                    } else {
                        CssBasis::Inner
                    })
                );
            }
        }
    }

    #[test]
    fn anchor_tolerance_css_space_odd_widths() {
        // C14: odd physical widths at 1.25/1.75/2.0/3.0/4.0 must not spuriously refuse.
        for dpr in [1.25, 1.75, 2.0, 3.0, 4.0] {
            for inner_w in [1537.0, 1539.0] {
                let p = page(dpr, (inner_w, 900.0), None);
                let w = (inner_w * dpr).round() as i32;
                let h = (900.0 * dpr).round() as i32;
                let map = build_map(inputs(Rect { x: 0, y: 0, w, h }, p));
                assert!(
                    map.report.valid,
                    "dpr={dpr} inner_w={inner_w} w={w}: {:?}",
                    map.report.reason
                );
            }
        }
    }

    #[test]
    fn map_point_rounding_and_negative_origin() {
        // C2
        let contract = valid_contract(
            Rect {
                x: -1920,
                y: -200,
                w: 1185,
                h: 900,
            },
            1.0,
        );
        let p = map_point(&contract, CssPoint::new(0.0, 0.0)).unwrap();
        assert_eq!((p.x, p.y), (-1920, -200));
        let p = map_point(&contract, CssPoint::new(100.5, 50.4)).unwrap();
        assert_eq!((p.x, p.y), (-1820, -150));
    }

    #[test]
    fn map_rect_center_equals_map_point() {
        // C7
        let contract = valid_contract(
            Rect {
                x: 100,
                y: 80,
                w: 1500,
                h: 1125,
            },
            1.25,
        );
        let css = CssRect {
            left: 100.0,
            top: 50.0,
            width: 200.0,
            height: 40.0,
        };
        let rect = map_rect(&contract, css).unwrap();
        let p = map_point(&contract, css.center()).unwrap();
        assert_eq!(rect.center(), (p.x, p.y));
        assert_eq!(
            rect,
            Rect {
                x: 225,
                y: 143,
                w: 250,
                h: 50
            }
        );
    }

    #[test]
    fn map_unbounded_keeps_off_screen_rect() {
        // C13 (ingest)
        let page = page(1.0, (1200.0, 900.0), None);
        let map = build_map(inputs(
            Rect {
                x: 100,
                y: 80,
                w: 1200,
                h: 900,
            },
            page,
        ));
        assert!(map.report.valid);
        let off = CssRect {
            left: 10.0,
            top: 11000.0,
            width: 100.0,
            height: 30.0,
        };
        let rect = map_rect_unbounded(&map, off).unwrap();
        assert_eq!(rect.y, 11080);
    }

    #[test]
    fn invalid_bad_dpr() {
        let mut p = page(1.0, (1200.0, 900.0), None);
        p.dpr = Some(f64::NAN);
        let map = build_map(inputs(
            Rect {
                x: 0,
                y: 0,
                w: 1200,
                h: 900,
            },
            p,
        ));
        assert!(!map.report.valid);
        assert!(map.report.reason.unwrap().starts_with("bad_dpr"));
    }

    #[test]
    fn invalid_anchor_size_mismatch() {
        let map = build_map(inputs(
            Rect {
                x: 0,
                y: 0,
                w: 1000,
                h: 900,
            },
            page(1.0, (1200.0, 900.0), None),
        ));
        assert!(!map.report.valid);
        assert!(map.report.reason.unwrap().starts_with("no_viewport_anchor"));
    }

    #[test]
    fn invalid_mixed_dpi_straddle() {
        // C4
        let mut i = inputs(
            Rect {
                x: 0,
                y: 0,
                w: 1200,
                h: 900,
            },
            page(1.0, (1200.0, 900.0), None),
        );
        i.monitors = vec![
            MonitorDpi {
                rect: Rect {
                    x: 0,
                    y: 0,
                    w: 800,
                    h: 1080,
                },
                dpi: 96,
            },
            MonitorDpi {
                rect: Rect {
                    x: 800,
                    y: 0,
                    w: 1120,
                    h: 1080,
                },
                dpi: 192,
            },
        ];
        let map = build_map(i);
        assert!(!map.report.valid);
        assert!(map.report.reason.unwrap().starts_with("mixed_dpi_straddle"));
    }

    #[test]
    fn invalid_visual_viewport_mismatch() {
        let mut i = inputs(
            Rect {
                x: 0,
                y: 0,
                w: 1200,
                h: 900,
            },
            page(1.0, (1200.0, 900.0), None),
        );
        i.page.visual_scale = Some(1.5);
        let map = build_map(i);
        assert!(!map.report.valid);
        assert!(
            map.report
                .reason
                .unwrap()
                .starts_with("visual_viewport_mismatch")
        );
    }

    #[test]
    fn stale_viewport_detects_resize_and_display_change() {
        // C5
        let base_page = page(1.0, (1200.0, 900.0), None);
        let old = build_map(inputs(
            Rect {
                x: 100,
                y: 80,
                w: 1200,
                h: 900,
            },
            base_page,
        ));
        let mut resize = inputs(
            Rect {
                x: 100,
                y: 80,
                w: 1100,
                h: 900,
            },
            base_page,
        );
        resize.page.inner_width = Some(1100.0);
        // build_map validates against the new metrics, so force a recorded-metrics mismatch.
        resize.page = base_page;
        assert!(
            remap(&old, resize)
                .unwrap_err()
                .starts_with("stale_viewport")
        );

        let mut dpi = inputs(
            Rect {
                x: 100,
                y: 80,
                w: 1200,
                h: 900,
            },
            base_page,
        );
        dpi.dpi = Some(120);
        assert!(remap(&old, dpi).unwrap_err().starts_with("stale_viewport"));
        assert!(
            remap(
                &old,
                inputs(
                    Rect {
                        x: 100,
                        y: 80,
                        w: 1200,
                        h: 900,
                    },
                    base_page
                )
            )
            .is_ok()
        );
    }

    #[test]
    fn move_only_remaps_origin() {
        // C6
        let base_page = page(1.0, (1200.0, 900.0), None);
        let old = build_map(inputs(
            Rect {
                x: 100,
                y: 80,
                w: 1200,
                h: 900,
            },
            base_page,
        ));
        let moved = build_map(inputs(
            Rect {
                x: 300,
                y: 200,
                w: 1200,
                h: 900,
            },
            base_page,
        ));
        let new = remap(
            &old,
            inputs(
                Rect {
                    x: 300,
                    y: 200,
                    w: 1200,
                    h: 900,
                },
                base_page,
            ),
        )
        .unwrap();
        assert_eq!(new.origin, moved.origin);
        assert_eq!(new.anchor.map(|r| (r.x, r.y)), Some((300, 200)));
        assert!(new.report.valid);
    }

    #[test]
    fn layout_bound_uses_client_basis() {
        // C1/§6.4.8
        let _p = page(1.0, (1200.0, 900.0), Some((1185.0, 900.0)));
        let mut contract = valid_contract(
            Rect {
                x: 100,
                y: 80,
                w: 1185,
                h: 900,
            },
            1.0,
        );
        contract.transform.layout_basis = Some(CssBasis::Client);
        assert!(map_point(&contract, CssPoint::new(1184.0, 100.0)).is_ok());
        assert_eq!(
            map_point(&contract, CssPoint::new(1185.0, 100.0)),
            Err(MapError::OutOfViewport)
        );
    }

    #[test]
    fn observe_id_changes_per_snapshot_even_unchanged_page() {
        let a = observe_id(&format!("same|page|1|{}", next_generation()));
        let b = observe_id(&format!("same|page|1|{}", next_generation()));
        assert_ne!(a, b);
        assert_eq!(a.len(), 16);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        // A fresh process may reuse an id for the same layout; it is not a content hash.
        assert_eq!(observe_id("same|page|1|7"), observe_id("same|page|1|7"));
    }

    #[test]
    fn fixture_is_never_a_click_contract() {
        // R19/§6.9
        let mut i = inputs(
            Rect {
                x: 0,
                y: 0,
                w: 1200,
                h: 900,
            },
            page(1.0, (1200.0, 900.0), None),
        );
        i.fixture = true;
        i.fixture_origin = Some((10.0, 20.0));
        let map = build_map(i);
        assert!(!map.report.valid);
        assert_eq!(map.report.method, TransformMethod::Fixture);
        assert_eq!(map.report.reason.as_deref(), Some("fixture_snapshot"));
        let contract = contract_for(&map, CssPoint::new(1.0, 1.0));
        assert!(contract.physical.is_none());
        assert_eq!(
            map_point(&contract, CssPoint::new(1.0, 1.0)),
            Err(MapError::Invalid("fixture_snapshot".into()))
        );
    }

    #[test]
    fn contract_fake_assessor_same_point() {
        // C11 / 0128 DoD-3 seam
        let contract = valid_contract(
            Rect {
                x: 100,
                y: 80,
                w: 1500,
                h: 1125,
            },
            1.25,
        );
        let assessed_css = contract.css.unwrap();
        let physical = map_point(&contract, assessed_css).unwrap();
        assert_eq!(physical, PhysicalPoint { x: 113, y: 105 });
        // A fake assessor embeds the contract and maps the same CSS point.
        assert_eq!(contract.css, Some(assessed_css));
    }

    #[test]
    fn title_match_normalizes_chrome_suffix() {
        // C15
        assert!(title_matches(
            "Example page",
            "Example page - Google Chrome"
        ));
        assert!(title_matches(
            "Example page",
            "Other tab - Example  Page - Chromium"
        ));
        assert!(!title_matches(
            "Example page",
            "Example page 2 - Google Chrome"
        ));
        assert!(!title_matches("", "anything - Google Chrome"));
    }

    #[test]
    fn two_chrome_windows_without_title_refuses() {
        // C15
        assert_eq!(identity_from("", None, 1), WindowIdentity::SingleWindow);
        assert_eq!(identity_from("", None, 2), WindowIdentity::Ambiguous);
        assert_eq!(
            identity_from("Page", Some("Page - Google Chrome"), 2),
            WindowIdentity::Confirmed
        );
        assert_eq!(
            identity_from("Page", Some("Other - Google Chrome"), 1),
            WindowIdentity::Mismatch
        );
    }

    #[test]
    fn reason_tokens_are_stable_and_bounded() {
        for token in [
            "bad_dpr",
            "no_page_metrics",
            "no_viewport_anchor",
            "anchor_ambiguous",
            "anchor_size_mismatch",
            "anchor_outside_window",
            "anchor_outside_virtual_screen",
            "mixed_dpi_straddle",
            "stale_viewport",
            "window_mismatch",
            "window_identity_ambiguous",
            "not_chrome",
            "high_il_unreadable",
            "zoom_out_of_range",
            "no_hwnd",
            "point_outside_viewport",
            "visual_viewport_mismatch",
            "fixture_snapshot",
        ] {
            let report = TransformReport::invalid(format!("{token}: detail"), None);
            assert!(report.reason.unwrap().starts_with(token));
        }
        let long = TransformReport::invalid("x".repeat(500), None);
        assert!(long.reason.unwrap().chars().count() <= 120);
        let src = include_str!("point.rs");
        let forbidden_a = ["anchor", "disagreement"].join("_");
        let forbidden_b = ["legacy", "diagnostic", "only"].join("_");
        assert!(!src.contains(&forbidden_a));
        assert!(!src.contains(&forbidden_b));
    }

    #[test]
    fn explicit_sources_are_labeled_not_css() {
        let c = explicit_contract(PointSource::Grid, PhysicalPoint { x: 5, y: 6 });
        assert!(c.transform.valid);
        assert_eq!(c.transform.method, TransformMethod::Explicit);
        assert!(c.css.is_none());
        assert_eq!(
            map_point(&c, CssPoint::new(0.0, 0.0)),
            Err(MapError::NotCss)
        );
    }

    /// Owner/desk read-only probe. Never sends input, never changes display/zoom.
    #[test]
    #[ignore = "owner/desk read-only anchor probe; never in CI"]
    fn live_anchor_probe() {
        let Some(hwnd) = crate::foreground::foreground_hwnd() else {
            eprintln!("live_anchor_probe: no foreground window");
            return;
        };
        eprintln!(
            "hwnd=0x{hwnd:x} class={}",
            crate::foreground::class_name_of(hwnd)
        );
        eprintln!("window={:?}", crate::foreground::window_rect(hwnd));
        eprintln!("dpi={:?}", crate::foreground::dpi_for_window(hwnd));
        eprintln!(
            "virtual_screen={:?}",
            crate::space::virtual_screen().ok().map(|s| s.as_rect())
        );
        eprintln!("monitors={:?}", crate::space::monitor_dpis());
        let widgets = crate::foreground::render_widget_host_rects(hwnd);
        eprintln!("render_widgets={widgets:?}");
        eprintln!(
            "uia_document={:?}",
            crate::uia::document_rect(hwnd, std::time::Duration::from_millis(500))
        );
        let outcome = crate::chrome::try_snapshot(crate::extract::Detail::Default, Some(hwnd));
        eprintln!("snapshot={outcome:?}");
        if let Some(map) = outcome.into_map() {
            eprintln!("transform={:?}", map.transform);
        }
    }
}
