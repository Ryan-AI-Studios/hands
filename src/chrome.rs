//! MCP/CLI client for the Chrome host (fixture or named pipe).

use serde::Deserialize;
use std::fs;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE, OPEN_EXISTING,
};
use windows::Win32::System::Pipes::WaitNamedPipeW;
use windows::core::PCWSTR;

use crate::error::HandsError;
use crate::extract::{
    CARD_DEALER_CAP, CARD_DELIVERY_CAP, CARD_DISTANCE_CAP, CARD_KIND_CAP, CARD_MILES_CAP,
    CARD_OF_CAP, Card, Detail, EMPTY_STATE_CAP, Element, LOCAL_MATCHES_CAP, ListingMeta,
    RADIUS_CAP, RESULT_COUNT_CAP, ZIP_CAP, infer_card_kind, prefer_local_cards, take_chars,
    take_opt_chars,
};
use crate::foreground;
use crate::hittest::{Assessment, AtPoint};
use crate::identity::{Binding, IdentityReport, IdentityState, SnapshotBinding, SnapshotRef};
use crate::native_host::{CLIENT_TIMEOUT_MS, client_timeout, exchange_pipe_deadline, pipe_name};
use crate::point::{
    CssPoint, CssRect, MapInputs, PageMetrics, PointContract, TransformReport, ViewportMap,
    build_map, contract_for, identity_from, map_rect_unbounded, next_generation, observe_id, remap,
};
use crate::space::Rect;

static LAST_PIPE_DETAIL: Mutex<Detail> = Mutex::new(Detail::Default);

pub const SNAPSHOT_ENV: &str = "HANDS_CHROME_SNAPSHOT";
pub const PIPE_ENV: &str = "HANDS_CHROME_PIPE";
pub const CHROME_HINT_HOST_DOWN: &str =
    "Chrome host down — run hands native-host-doctor (MCP: native_host_doctor)";
pub const CHROME_HINT_LOADING: &str = "Page loading — retry observe or wait_settle";
pub const CHROME_HINT_TIMEOUT: &str =
    "Chrome snapshot timed out (400 ms) — retry observe or wait_settle";
pub const CHROME_HINT_NOT_CHROME_TAB: &str = "not a Chrome tab; use uia:";

/// Snapshot hints apply only when this observe walked daily Chrome.
/// Otherwise replace (including HostDown doctor) so agents do not doctor Tauri/Notepad.
pub(crate) fn chrome_hint_for_observe(
    chrome_walk: bool,
    snapshot_hint: Option<String>,
) -> Option<String> {
    if chrome_walk {
        snapshot_hint
    } else {
        Some(CHROME_HINT_NOT_CHROME_TAB.into())
    }
}

#[derive(Debug, Clone)]
pub enum SnapshotOutcome {
    Map(Box<ChromeMap>),
    Loading,
    Timeout,
    HostDown,
}

impl SnapshotOutcome {
    pub fn into_map(self) -> Option<ChromeMap> {
        match self {
            Self::Map(map) => Some(*map),
            _ => None,
        }
    }

    pub fn has_map(&self) -> bool {
        matches!(self, Self::Map(_))
    }

    pub fn host_up(&self) -> bool {
        !matches!(self, Self::HostDown)
    }

    pub fn chrome_hint(&self) -> Option<String> {
        match self {
            Self::Map(_) => None,
            Self::Loading => Some(CHROME_HINT_LOADING.into()),
            Self::Timeout => Some(CHROME_HINT_TIMEOUT.into()),
            Self::HostDown => Some(CHROME_HINT_HOST_DOWN.into()),
        }
    }
}
pub const CARD_TITLE_CAP: usize = 80;
pub const CARD_PRICE_CAP: usize = 24;
pub const CARD_HREF_CAP: usize = 200;
pub const CARD_CAP: usize = 8;
pub const CARD_WALK_CAP: usize = 24;

#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Debug, Clone)]
pub struct ChromeMap {
    /// Frame rows from the same reply, retained even for a non-storing observe so the
    /// challenge/solver path can hold a transient frame miss without minting refs.
    pub(crate) frame_rows: Vec<crate::identity::FrameBinding>,
    pub(crate) frames_transient: bool,
    /// Chrome tab id from the same reply. Kept even for a non-storing snapshot so hittest point
    /// mode can issue an explicit frame assess.
    pub(crate) tab: Option<u32>,
    pub url: Option<String>,
    pub title: String,
    pub main_text: String,
    pub elements: Vec<Element>,
    pub cards: Vec<Card>,
    pub listing: ListingMeta,
    pub cards_walked: usize,
    /// Page-level map/validity. Serialized as the observe envelope's compact `transform`.
    pub transform: Option<TransformReport>,
    /// Snapshot page identity/capability for 0128 point mode. Never serialized on observe.
    pub(crate) identity: Option<PageIdentity>,
    /// Snapshot-bound target identity (tab/document/snapshot), when the extension supplied it.
    pub snapshot: Option<SnapshotBinding>,
}

/// Page identity captured from the same snapshot reply as the transform. Used only by
/// `hittest` point mode; not part of the observe envelope.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PageIdentity {
    pub url: Option<String>,
    pub title: String,
    pub inner_width: Option<f64>,
    pub inner_height: Option<f64>,
    pub dpr: Option<f64>,
    /// `assess_v` capability marker; absent on an old extension.
    pub assess_v: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ChromeHit {
    pub id: String,
    pub name: String,
    pub role: String,
    pub rect: Rect,
    pub css_rect: CssRect,
    pub css_point: CssPoint,
    pub contract: PointContract,
    pub hwnd: Option<isize>,
    /// Page href from the scoped resolve reply when the stored payload carried one.
    pub href: Option<String>,
    pub(crate) map: Option<ViewportMap>,
    /// Optional DOM assessment returned by the same resolve reply (`assess:true`).
    pub assessment: Option<Assessment>,
    /// Optional read-only DOM capability block returned by the same resolve reply (`caps:true`).
    /// Kept as raw JSON so `find` owns the capability vocabulary without a module cycle.
    pub capability: Option<serde_json::Value>,
    /// Scoped/legacy identity report for the resolved element.
    pub identity: Option<IdentityReport>,
}

#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
pub struct ChromeMetrics {
    #[serde(default, rename = "screenX")]
    pub screen_x: f64,
    #[serde(default, rename = "screenY")]
    pub screen_y: f64,
    #[serde(default, rename = "outerWidth")]
    pub outer_width: f64,
    #[serde(default, rename = "outerHeight")]
    pub outer_height: f64,
    #[serde(default, rename = "innerWidth")]
    pub inner_width: f64,
    #[serde(default, rename = "innerHeight")]
    pub inner_height: f64,
    #[serde(default, rename = "devicePixelRatio")]
    pub device_pixel_ratio: f64,
    #[serde(default, rename = "clientWidth")]
    pub client_width: Option<f64>,
    #[serde(default, rename = "clientHeight")]
    pub client_height: Option<f64>,
    #[serde(default, rename = "visualViewportWidth")]
    pub visual_viewport_width: Option<f64>,
    #[serde(default, rename = "visualViewportHeight")]
    pub visual_viewport_height: Option<f64>,
    #[serde(default, rename = "visualViewportScale")]
    pub visual_viewport_scale: Option<f64>,
    #[serde(default, rename = "visualViewportOffsetLeft")]
    pub visual_viewport_offset_left: Option<f64>,
    #[serde(default, rename = "visualViewportOffsetTop")]
    pub visual_viewport_offset_top: Option<f64>,
    #[serde(default, rename = "screenAvailLeft")]
    pub screen_avail_left: Option<f64>,
    #[serde(default, rename = "screenAvailTop")]
    pub screen_avail_top: Option<f64>,
    #[serde(default, rename = "screenWidth")]
    pub screen_width: Option<f64>,
    #[serde(default, rename = "screenHeight")]
    pub screen_height: Option<f64>,
}

impl ChromeMetrics {
    fn page(&self) -> PageMetrics {
        PageMetrics {
            inner_width: Some(self.inner_width),
            inner_height: Some(self.inner_height),
            client_width: self.client_width,
            client_height: self.client_height,
            dpr: Some(self.device_pixel_ratio),
            visual_scale: self.visual_viewport_scale,
            visual_offset_left: self.visual_viewport_offset_left,
            visual_offset_top: self.visual_viewport_offset_top,
        }
    }

    fn identity(&self) -> PageIdentity {
        PageIdentity {
            url: None,
            title: String::new(),
            inner_width: self.inner_width.is_finite().then_some(self.inner_width),
            inner_height: self.inner_height.is_finite().then_some(self.inner_height),
            dpr: self
                .device_pixel_ratio
                .is_finite()
                .then_some(self.device_pixel_ratio),
            assess_v: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct RawNode {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(rename = "rectCss")]
    rect_css: CssRect,
    #[serde(default, rename = "href")]
    _href: Option<String>,
    /// Nonzero Chrome frame id; absent/0 is the top frame.
    #[serde(default)]
    frame: Option<u32>,
    /// Walk index in the owning frame document (used by fixture frame refs).
    #[serde(default)]
    idx: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct RawTop {
    #[serde(default)]
    frame: Option<u32>,
    #[serde(default)]
    metrics: ChromeMetrics,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawCard {
    #[serde(default)]
    title: String,
    #[serde(default)]
    price: String,
    #[serde(default)]
    href: String,
    #[serde(rename = "rectCss")]
    rect_css: CssRect,
    #[serde(default)]
    miles: Option<String>,
    #[serde(default)]
    dealer: Option<String>,
    #[serde(default)]
    distance: Option<String>,
    #[serde(rename = "of", default)]
    listing_of: Option<String>,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    delivery: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct RawSnapshot {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    main_text: Option<String>,
    #[serde(default)]
    elements: Vec<RawNode>,
    #[serde(default)]
    cards: Vec<RawCard>,
    metrics: ChromeMetrics,
    #[serde(default)]
    result_count: Option<String>,
    #[serde(default)]
    local_matches: Option<String>,
    #[serde(default)]
    empty_state: Option<String>,
    #[serde(default)]
    zip: Option<String>,
    #[serde(default)]
    radius: Option<String>,
    #[serde(default)]
    cards_walked: Option<usize>,
    /// Additive per-resolve DOM assessment. Never present on a snapshot.
    #[serde(default)]
    assessment: Option<Assessment>,
    /// Snapshot capability marker (`assess_v: 1`) from the content script.
    #[serde(default)]
    assess_v: Option<u32>,
    /// Named-tab routing/reply evidence (snapshot/resolve/binding).
    #[serde(default)]
    tab: Option<u32>,
    #[serde(default)]
    window: Option<u32>,
    #[serde(default)]
    frame: Option<u32>,
    #[serde(default)]
    doc: Option<String>,
    #[serde(default)]
    snap: Option<String>,
    #[serde(default)]
    retained: Option<usize>,
    #[serde(default)]
    stored: Option<bool>,
    #[serde(default)]
    fallback: Option<bool>,
    /// Frame rows (links and restricted statuses) from the extension's opt-in fan-out.
    #[serde(default)]
    frames: Vec<crate::frames::RawFrameLink>,
    #[serde(default)]
    frames_total: Option<usize>,
    #[serde(default)]
    frames_truncated: Option<bool>,
}

/// Fixture/diagnostic **listing** origin using the legacy `outer-inner` formula.
/// Production-reachable only through `HANDS_CHROME_SNAPSHOT`; never called by a click.
fn fixture_listing_origin(metrics: &ChromeMetrics) -> Option<(f64, f64)> {
    let dpr = metrics.device_pixel_ratio;
    if !dpr.is_finite() || dpr <= 0.0 {
        return None;
    }
    if !metrics.screen_x.is_finite()
        || !metrics.screen_y.is_finite()
        || !metrics.outer_width.is_finite()
        || !metrics.outer_height.is_finite()
        || !metrics.inner_width.is_finite()
        || !metrics.inner_height.is_finite()
    {
        return None;
    }
    let chrome_ui_y = (metrics.outer_height - metrics.inner_height).max(0.0);
    let chrome_ui_x = (metrics.outer_width - metrics.inner_width).max(0.0) / 2.0;
    Some((
        (metrics.screen_x + chrome_ui_x) * dpr,
        (metrics.screen_y + chrome_ui_y) * dpr,
    ))
}

fn make_observe_id(raw: &RawSnapshot, hwnd: Option<isize>, element_count: usize) -> String {
    let m = &raw.metrics;
    let opt = |v: Option<f64>| v.map(|x| x.to_string()).unwrap_or_else(|| "none".into());
    let parts = format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
        raw.url.as_deref().unwrap_or(""),
        raw.title.as_deref().unwrap_or(""),
        m.inner_width,
        m.inner_height,
        m.device_pixel_ratio,
        opt(m.client_width),
        opt(m.client_height),
        hwnd.map(|h| h.to_string()).unwrap_or_default(),
        next_generation(),
        element_count,
    );
    observe_id(&parts)
}

/// A focused Chrome-owned window that is not the resolved Chrome window (for example an owned
/// popup) is a `window_mismatch`; never re-root the click behind it.
fn focused_chrome_popup_owns_foreground(target: isize) -> bool {
    match foreground::foreground_hwnd() {
        Some(fg) if fg != target => foreground::target_is_chrome(Some(fg)),
        _ => false,
    }
}

/// A focused non-Chrome window that is not the resolved Chrome window is also a `window_mismatch`:
/// the click would otherwise land on that foreign window if `offer` cannot raise Chrome.
fn focused_non_chrome_foreground(target: isize) -> bool {
    match foreground::foreground_hwnd() {
        Some(fg) if fg != target => !foreground::target_is_chrome(Some(fg)),
        _ => false,
    }
}

fn observe_identity(title: Option<&str>, hwnd: Option<isize>) -> crate::point::WindowIdentity {
    let Some(hwnd) = hwnd else {
        return crate::point::WindowIdentity::Mismatch;
    };
    if !foreground::target_is_chrome(Some(hwnd)) || focused_chrome_popup_owns_foreground(hwnd) {
        return crate::point::WindowIdentity::Mismatch;
    }
    let caption = foreground::title(Some(hwnd));
    let count = foreground::visible_chrome_window_count();
    identity_from(title.unwrap_or(""), Some(&caption), count)
}

#[cfg(test)]
type GeometryHook = fn(&ChromeMetrics, Option<&str>, isize) -> MapInputs;

#[cfg(test)]
thread_local! {
    static GEOMETRY_HOOK: std::cell::RefCell<Option<GeometryHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_geometry_hook(hook: Option<GeometryHook>) {
    GEOMETRY_HOOK.with(|c| *c.borrow_mut() = hook);
}

#[cfg(test)]
type RemapGeometryHook = fn(PageMetrics, isize) -> MapInputs;

#[cfg(test)]
thread_local! {
    static REMAP_HOOK: std::cell::RefCell<Option<RemapGeometryHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_remap_geometry_hook(hook: Option<RemapGeometryHook>) {
    REMAP_HOOK.with(|c| *c.borrow_mut() = hook);
}

/// Test seam for the live window identity read used by [`remap_chr`] (caption, visible Chrome
/// window count, chrome-ness). Returns `(caption, count, is_chrome)`.
#[cfg(test)]
type RemapIdentityHook = fn(isize) -> (Option<String>, usize, bool);

#[cfg(test)]
thread_local! {
    static REMAP_IDENTITY_HOOK: std::cell::RefCell<Option<RemapIdentityHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_remap_identity_hook(hook: Option<RemapIdentityHook>) {
    REMAP_IDENTITY_HOOK.with(|c| *c.borrow_mut() = hook);
}

#[cfg(test)]
thread_local! {
    static RESOLVE_STUB: std::cell::RefCell<Option<ChromeHit>> =
        const { std::cell::RefCell::new(None) };
    static SNAPSHOT_IDENTITY_STUB: std::cell::RefCell<Option<SnapshotOutcome>> =
        const { std::cell::RefCell::new(None) };
    static PIPE_SNAPSHOT_STUB: std::cell::RefCell<Option<ChromeMap>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_resolve_stub(hit: Option<ChromeHit>) {
    RESOLVE_STUB.with(|c| *c.borrow_mut() = hit);
}

#[cfg(test)]
pub(crate) fn set_snapshot_identity_stub(outcome: Option<SnapshotOutcome>) {
    SNAPSHOT_IDENTITY_STUB.with(|c| *c.borrow_mut() = outcome);
}

#[cfg(test)]
pub(crate) fn set_pipe_snapshot_stub(map: Option<ChromeMap>) {
    PIPE_SNAPSHOT_STUB.with(|c| *c.borrow_mut() = map);
}

fn live_inputs_from_page(
    page: PageMetrics,
    title: Option<&str>,
    hwnd: isize,
    observe_id: Option<String>,
    identity: crate::point::WindowIdentity,
) -> MapInputs {
    let chrome = foreground::is_chrome_hwnd(hwnd);
    let render_widgets = if chrome {
        foreground::render_widget_host_rects(hwnd)
    } else {
        Vec::new()
    };
    let uia_document = if chrome && !crate::point::any_anchor_matches(&render_widgets, &page) {
        crate::uia::document_rect(hwnd, std::time::Duration::from_millis(500))
    } else {
        None
    };
    MapInputs {
        hwnd: Some(hwnd),
        window_rect: foreground::window_rect(hwnd),
        dpi: foreground::dpi_for_window(hwnd),
        virtual_screen: crate::space::virtual_screen().ok().map(|s| s.as_rect()),
        page,
        render_widgets,
        uia_document,
        monitors: crate::space::monitor_dpis(),
        observe_id,
        page_title: title.map(str::to_string),
        fixture: false,
        identity,
        fixture_origin: None,
    }
}

fn live_map_inputs(
    metrics: &ChromeMetrics,
    title: Option<&str>,
    hwnd: isize,
    observe_id: Option<String>,
) -> MapInputs {
    let identity = observe_identity(title, Some(hwnd));
    live_inputs_from_page(metrics.page(), title, hwnd, observe_id, identity)
}

fn build_live_map(
    metrics: &ChromeMetrics,
    title: Option<&str>,
    hwnd: isize,
    observe_id: Option<String>,
) -> ViewportMap {
    #[cfg(test)]
    {
        if let Some(hook) = GEOMETRY_HOOK.with(|c| *c.borrow()) {
            return build_map(hook(metrics, title, hwnd));
        }
    }
    build_map(live_map_inputs(metrics, title, hwnd, observe_id))
}

fn fixture_map(raw: &RawSnapshot, hwnd: Option<isize>, obs: String) -> ViewportMap {
    build_map(MapInputs {
        hwnd,
        page: raw.metrics.page(),
        page_title: raw.title.clone(),
        fixture: true,
        fixture_origin: fixture_listing_origin(&raw.metrics),
        observe_id: Some(obs),
        ..MapInputs::default()
    })
}

fn no_hwnd_map(raw: &RawSnapshot, obs: String) -> ViewportMap {
    let dpr = raw
        .metrics
        .device_pixel_ratio
        .is_finite()
        .then_some(raw.metrics.device_pixel_ratio)
        .filter(|d| *d > 0.0);
    ViewportMap {
        hwnd: None,
        origin: None,
        scale: dpr.unwrap_or(1.0),
        page: raw.metrics.page(),
        page_title: raw.title.clone(),
        anchor: None,
        anchor_basis: None,
        layout_basis: None,
        report: TransformReport {
            reason: Some("no_hwnd: chr: resolve has no Chrome HWND to anchor".into()),
            observe_id: Some(obs),
            dpr,
            ..TransformReport::invalid("no_hwnd", None)
        },
    }
}

fn map_for_raw(raw: &RawSnapshot, hwnd: Option<isize>, fixture: bool, obs: String) -> ViewportMap {
    if fixture {
        return fixture_map(raw, hwnd, obs);
    }
    match hwnd {
        Some(h) => build_live_map(&raw.metrics, raw.title.as_deref(), h, Some(obs)),
        None => no_hwnd_map(raw, obs),
    }
}

fn remap_inputs(
    old: &ViewportMap,
    hwnd: isize,
    identity: crate::point::WindowIdentity,
) -> MapInputs {
    #[cfg(test)]
    {
        if let Some(hook) = REMAP_HOOK.with(|c| *c.borrow()) {
            let mut inputs = hook(old.page, hwnd);
            inputs.page = old.page;
            inputs.page_title = old.page_title.clone();
            inputs.identity = identity;
            return inputs;
        }
    }
    let mut inputs = live_inputs_from_page(
        old.page,
        old.page_title.as_deref(),
        hwnd,
        old.report.observe_id.clone(),
        identity,
    );
    inputs.page = old.page;
    inputs.page_title = old.page_title.clone();
    inputs
}

/// Testable remap core. `caption` is the OS window caption; `old.page_title` is the recorded
/// page title from the snapshot/resolve reply. Identity is evaluated with the recorded title,
/// never by substituting the caption for the page title.
fn remap_chr_with(
    old: &ViewportMap,
    hwnd: isize,
    caption: Option<&str>,
    chrome_window_count: usize,
    is_chrome: bool,
) -> Result<ViewportMap, String> {
    let identity = if is_chrome {
        identity_from(
            old.page_title.as_deref().unwrap_or(""),
            caption,
            chrome_window_count,
        )
    } else {
        crate::point::WindowIdentity::Mismatch
    };
    let live = remap_inputs(old, hwnd, identity);
    remap(old, live)
}

/// Remap a valid `chr:` contract at the final guard. Live geometry only; the recorded page
/// metrics are re-used (no second page-metrics read). Page zoom between resolve and dispatch
/// is not detectable here.
pub fn remap_chr(old: &ViewportMap, hwnd: isize) -> Result<ViewportMap, String> {
    if old.page.dpr.is_none() {
        return Err("stale_viewport: recorded page metrics have no dpr".into());
    }
    let (caption, count, is_chrome) = remap_identity(hwnd);
    remap_chr_with(old, hwnd, caption.as_deref(), count, is_chrome)
}

fn remap_identity(hwnd: isize) -> (Option<String>, usize, bool) {
    #[cfg(test)]
    {
        if let Some(hook) = REMAP_IDENTITY_HOOK.with(|c| *c.borrow()) {
            return hook(hwnd);
        }
    }
    let is_chrome = foreground::target_is_chrome(Some(hwnd))
        && !focused_chrome_popup_owns_foreground(hwnd)
        && !focused_non_chrome_foreground(hwnd);
    if is_chrome {
        (
            Some(foreground::title(Some(hwnd))),
            foreground::visible_chrome_window_count(),
            true,
        )
    } else {
        (None, 0, false)
    }
}

pub fn try_snapshot(detail: Detail, hwnd: Option<isize>) -> SnapshotOutcome {
    try_snapshot_with_store(detail, hwnd, true)
}

/// `store=false` asks the content script to leave its agent-facing snapshot record alone
/// (internal observe/doctor/hittest reads must not retire a scoped ref).
pub fn try_snapshot_with_store(
    detail: Detail,
    hwnd: Option<isize>,
    store: bool,
) -> SnapshotOutcome {
    match snapshot_env() {
        Some(path) => match load_fixture(&path, hwnd) {
            Ok(map) => SnapshotOutcome::Map(Box::new(map)),
            Err(_) => SnapshotOutcome::HostDown,
        },
        None => match pipe_snapshot(detail, hwnd, store) {
            Ok(map) => SnapshotOutcome::Map(Box::new(map)),
            Err(err) => classify_snapshot_err(&err),
        },
    }
}

pub fn classify_snapshot_err(err: &HandsError) -> SnapshotOutcome {
    let s = err.to_string();
    if s.contains("client timed out after") {
        return SnapshotOutcome::Timeout;
    }
    if let Some(rest) = s.strip_prefix("Chrome host error: ") {
        let token = rest.trim().trim_matches('"');
        if matches!(
            token,
            "no-content" | "no_content" | "no-tab" | "no_tab" | "empty"
        ) {
            return SnapshotOutcome::Loading;
        }
        return SnapshotOutcome::Loading;
    }
    SnapshotOutcome::HostDown
}

pub fn try_resolve(id: &str, hwnd: Option<isize>) -> Result<ChromeHit, HandsError> {
    try_resolve_assessed(id, hwnd, false, None)
}

/// `chr:` resolve with the optional DOM assessment (`assess:true`). The assessment is parsed
/// from the same reply; an old extension simply omits it.
pub fn try_resolve_assessed(
    id: &str,
    hwnd: Option<isize>,
    assess: bool,
    samples: Option<u32>,
) -> Result<ChromeHit, HandsError> {
    #[cfg(test)]
    {
        if let Some(hit) = RESOLVE_STUB.with(|c| c.borrow().clone()) {
            return Ok(hit);
        }
    }
    let fixture = snapshot_env().is_some();
    let raw = match snapshot_env() {
        Some(path) => load_fixture_raw(&path)?,
        None => pipe_resolve_raw(id, assess, samples)?,
    };
    let mut raw = raw;
    if !assess {
        raw.assessment = None;
    }
    hit_from_raw(raw, id, hwnd, fixture)
}

fn hit_from_raw(
    raw: RawSnapshot,
    id: &str,
    hwnd: Option<isize>,
    fixture: bool,
) -> Result<ChromeHit, HandsError> {
    let index = raw
        .elements
        .iter()
        .position(|n| {
            n.id.as_deref()
                .and_then(canonical_chr)
                .is_some_and(|cid| cid == id)
        })
        .ok_or_else(|| {
            HandsError::Chrome(format!(
                "Chrome element {id} not found in host/fixture snapshot"
            ))
        })?;
    let node = raw.elements[index].clone();
    let obs = make_observe_id(&raw, hwnd, raw.elements.len());
    let map = map_for_raw(&raw, hwnd, fixture, obs.clone());
    let contract = contract_for(&map, node.rect_css.center());
    let rect = crate::point::map_rect(&contract, node.rect_css).unwrap_or(Rect {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    });
    let mut assessment = raw.assessment;
    if let Some(a) = assessment.as_mut() {
        a.observe_id = Some(obs);
    }
    Ok(ChromeHit {
        id: node
            .id
            .as_deref()
            .and_then(canonical_chr)
            .unwrap_or_else(|| id.to_string()),
        name: node.text.unwrap_or_default(),
        role: node.role.unwrap_or_else(|| "Other".into()),
        rect,
        css_rect: node.rect_css,
        css_point: node.rect_css.center(),
        contract,
        hwnd,
        href: None,
        map: Some(map),
        assessment,
        capability: None,
        identity: Some(IdentityReport::legacy()),
    })
}

fn snapshot_env() -> Option<String> {
    std::env::var(SNAPSHOT_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn load_fixture(path: &str, hwnd: Option<isize>) -> Result<ChromeMap, HandsError> {
    let bytes = fs::read(path).map_err(|_| {
        HandsError::Chrome(format!(
            "Chrome snapshot fixture is missing or unreadable ({path}); cannot resolve chr: (host/fixture)"
        ))
    })?;
    parse_snapshot_bytes(&bytes, hwnd).map_err(|_| {
        HandsError::Chrome(format!(
            "Chrome snapshot fixture is invalid ({path}); cannot resolve chr: (host/fixture)"
        ))
    })
}

fn load_fixture_raw(path: &str) -> Result<RawSnapshot, HandsError> {
    let bytes = fs::read(path).map_err(|_| {
        HandsError::Chrome(format!(
            "Chrome snapshot fixture is missing or unreadable ({path}); cannot resolve chr: (host/fixture)"
        ))
    })?;
    serde_json::from_slice(&bytes)
        .map_err(|err| HandsError::Chrome(format!("Chrome snapshot fixture JSON: {err}")))
}

fn parse_snapshot_bytes(bytes: &[u8], hwnd: Option<isize>) -> Result<ChromeMap, HandsError> {
    let raw: RawSnapshot = serde_json::from_slice(bytes)
        .map_err(|err| HandsError::Chrome(format!("Chrome snapshot JSON: {err}")))?;
    let obs = make_observe_id(&raw, hwnd, raw.elements.len());
    Ok(map_from_raw(raw, hwnd, true, obs))
}

/// Test-only bridge so `observe` tests can drive the production live-map path from a raw JSON
/// snapshot without exposing `RawSnapshot`.
#[cfg(test)]
pub(crate) fn map_from_raw_json(raw_json: &str, hwnd: Option<isize>, obs: String) -> ChromeMap {
    let raw: RawSnapshot = serde_json::from_str(raw_json).expect("raw snapshot JSON");
    map_from_raw(raw, hwnd, false, obs)
}

fn valid_lower_hex(raw: &str, len: usize) -> bool {
    raw.len() == len
        && raw
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn frame_bindings(rows: &[crate::frames::RawFrameLink]) -> Vec<crate::identity::FrameBinding> {
    let mut frames = Vec::new();
    for row in rows.iter().take(12) {
        let frame = row.frame.filter(|f| *f > 0);
        let ok = row.is_ok()
            && row.doc.as_deref().is_some_and(|d| valid_lower_hex(d, 4))
            && row.snap.as_deref().is_some_and(|s| valid_lower_hex(s, 8));
        if frame.is_none() && row.reason.is_none() {
            continue;
        }
        frames.push(crate::identity::FrameBinding {
            frame,
            parent: row.parent.filter(|p| *p > 0),
            doc: if ok {
                row.doc.clone().unwrap_or_default()
            } else {
                String::new()
            },
            snap: if ok {
                row.snap.clone().unwrap_or_default()
            } else {
                String::new()
            },
            retained: row.retained.unwrap_or(0),
            status: if ok { "ok" } else { "unavailable" }.into(),
            reason: if ok {
                None
            } else {
                Some(
                    row.reason
                        .clone()
                        .unwrap_or_else(|| "frame_geometry_unavailable".into()),
                )
            },
            origin: row.origin.as_deref().and_then(crate::frames::valid_origin),
        });
    }
    frames
}

fn snapshot_binding(
    raw: &RawSnapshot,
    hwnd: Option<isize>,
    fixture: bool,
) -> Option<SnapshotBinding> {
    if raw.stored == Some(false) {
        return None;
    }
    let fallback = raw.fallback.unwrap_or(false);
    let source = if fixture {
        "fixture"
    } else if fallback {
        "fallback"
    } else {
        "live"
    };
    let tab = raw.tab.filter(|t| *t > 0);
    let doc = raw.doc.clone().filter(|d| valid_lower_hex(d, 4));
    let snap = raw.snap.clone().filter(|s| valid_lower_hex(s, 8));
    let retained = raw.retained.unwrap_or(raw.elements.len());
    let frames = frame_bindings(&raw.frames);
    let frames_total = raw
        .frames_total
        .unwrap_or(raw.frames.len())
        .max(frames.len());
    let frames_truncated = raw.frames_truncated.unwrap_or(false) || raw.frames.len() > frames.len();
    if let (Some(tab), Some(doc), Some(snap)) = (tab, doc, snap) {
        return Some(SnapshotBinding {
            tab: Some(tab),
            window: raw.window,
            frame: raw.frame.unwrap_or(0),
            hwnd: None,
            doc,
            snap,
            retained,
            source: source.into(),
            fallback,
            reason: None,
            frames,
            frames_total,
            frames_truncated,
        });
    }
    // An agent-facing Chrome walk with an old/quiet extension still gets an honest block;
    // refs are withheld because `doc`/`snap` are empty.
    hwnd.map(|_| SnapshotBinding {
        tab,
        window: raw.window,
        frame: raw.frame.unwrap_or(0),
        hwnd: None,
        doc: String::new(),
        snap: String::new(),
        retained,
        source: source.into(),
        fallback,
        reason: Some("unbound_extension".into()),
        frames,
        frames_total,
        frames_truncated,
    })
}

fn map_from_raw(raw: RawSnapshot, hwnd: Option<isize>, fixture: bool, obs: String) -> ChromeMap {
    let map = map_for_raw(&raw, hwnd, fixture, obs);
    let mut snapshot = snapshot_binding(&raw, hwnd, fixture);
    let link_rows = raw.frames.clone();
    let frame_rows = frame_bindings(&raw.frames);
    let frames_transient = raw
        .frames
        .iter()
        .any(|row| !row.is_ok() && row.is_transient());
    let mut dropped_frames: Vec<u32> = Vec::new();
    let mut elements = Vec::new();
    for (i, node) in raw.elements.into_iter().enumerate() {
        let frame = node.frame.filter(|f| *f > 0).unwrap_or(0);
        let composed = if frame > 0 {
            // Composition happens here for an aggregated snapshot: the node rect is
            // frame-local; the map is the top-frame viewport.
            match crate::frames::compose_rect(&link_rows, frame, node.rect_css) {
                Ok(rect) => rect,
                Err(_) => {
                    dropped_frames.push(frame);
                    continue;
                }
            }
        } else {
            node.rect_css
        };
        let Some(rect) = map_rect_unbounded(&map, composed) else {
            continue;
        };
        let id = if frame > 0 {
            // `chr:` is top-frame-only. A stored child carries its sref1 token; a non-storing
            // internal read carries a ref-less `frame:N:idx`. Both stay observable so solver
            // detection and target selection can see frame content; only the former mints a ref.
            let fallback = format!("frame:{frame}:{}", node.idx.unwrap_or(i as u32));
            match node.id.as_deref() {
                Some(id) if !id.trim().is_empty() && !id.starts_with("chr:") => id.to_string(),
                _ => fallback,
            }
        } else {
            node.id
                .as_deref()
                .and_then(canonical_chr)
                .unwrap_or_else(|| format!("chr:{i}"))
        };
        let (text, unnamed) =
            crate::extract::name_fields(node.text.as_deref().unwrap_or(""), false);
        elements.push(Element {
            challenge_vendor: frame == 0 || frame_is_vendor(&link_rows, frame),
            frame: (frame > 0).then_some(frame),
            id,
            role: node.role.unwrap_or_else(|| "Other".into()),
            text,
            rect,
            grid: None,
            unnamed,
            sref: None,
        });
    }
    let mut cards = Vec::new();
    for card in raw.cards {
        if cards.len() >= CARD_WALK_CAP {
            break;
        }
        if !href_ok(&card.href) {
            continue;
        }
        let Some(rect) = map_rect_unbounded(&map, card.rect_css) else {
            continue;
        };
        let mut mapped = Card {
            title: take_chars(&card.title, CARD_TITLE_CAP),
            price: take_chars(&card.price, CARD_PRICE_CAP),
            href: take_chars(&card.href, CARD_HREF_CAP),
            rect,
            miles: take_opt_chars(card.miles, CARD_MILES_CAP),
            dealer: take_opt_chars(card.dealer, CARD_DEALER_CAP),
            distance: take_opt_chars(card.distance, CARD_DISTANCE_CAP),
            listing_of: take_opt_chars(card.listing_of, CARD_OF_CAP),
            kind: take_opt_chars(
                if card.kind.is_empty() {
                    None
                } else {
                    Some(card.kind)
                },
                CARD_KIND_CAP,
            ),
            delivery: take_opt_chars(
                if card.delivery.is_empty() {
                    None
                } else {
                    Some(card.delivery)
                },
                CARD_DELIVERY_CAP,
            ),
        };
        if mapped.kind.is_none() {
            mapped.kind = infer_card_kind(&mapped);
        }
        cards.push(mapped);
    }
    let collected_len = cards.len();
    let cards_walked = raw
        .cards_walked
        .filter(|n| *n > collected_len)
        .unwrap_or(collected_len);
    let cards = prefer_local_cards(cards, CARD_CAP);
    let mut identity = raw.metrics.identity();
    identity.url = raw.url.clone().filter(|s| !s.trim().is_empty());
    identity.title = raw.title.clone().unwrap_or_default();
    identity.assess_v = raw.assess_v;
    // A frame whose only elements were clipped must not stay `ok` with no rows: name the
    // frame `frame_not_visible` rather than silently dropping every child element.
    if let Some(binding) = snapshot.as_mut() {
        for row in &mut binding.frames {
            if let Some(frame) = row.frame
                && row.status == "ok"
                && dropped_frames.contains(&frame)
                && !elements.iter().any(|el| el.frame == Some(frame))
            {
                row.status = "unavailable".into();
                row.reason = Some("frame_not_visible".into());
            }
        }
    }
    ChromeMap {
        frame_rows,
        frames_transient,
        tab: raw.tab,
        url: raw.url.filter(|s| !s.trim().is_empty()),
        title: raw.title.unwrap_or_default(),
        main_text: raw.main_text.unwrap_or_default(),
        elements,
        cards,
        listing: ListingMeta {
            result_count: take_opt_chars(raw.result_count, RESULT_COUNT_CAP),
            local_matches: take_opt_chars(raw.local_matches, LOCAL_MATCHES_CAP),
            empty_state: take_opt_chars(raw.empty_state, EMPTY_STATE_CAP),
            zip: take_opt_chars(raw.zip, ZIP_CAP),
            radius: take_opt_chars(raw.radius, RADIUS_CAP),
            cards_walked,
        },
        cards_walked,
        transform: Some(map.report.clone()),
        identity: Some(identity),
        snapshot,
    }
}

fn frame_is_vendor(rows: &[crate::frames::RawFrameLink], frame: u32) -> bool {
    rows.iter()
        .find(|row| row.frame == Some(frame))
        .and_then(|row| row.url.as_deref())
        .is_some_and(|url| crate::challenge::vendor_url(url).is_some())
}

fn canonical_chr(id: &str) -> Option<String> {
    let rest = id.strip_prefix("chr:")?;
    if rest.is_empty()
        || rest.starts_with('+')
        || rest.starts_with('-')
        || (rest.len() > 1 && rest.starts_with('0'))
        || !rest.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    rest.parse::<u32>().ok().map(|n| format!("chr:{n}"))
}

fn href_ok(href: &str) -> bool {
    let t = href.trim();
    if t.is_empty() {
        return false;
    }
    !t.to_ascii_lowercase().starts_with("javascript:")
}

fn store_last_pipe_detail(detail: Detail) {
    *LAST_PIPE_DETAIL.lock().unwrap_or_else(|e| e.into_inner()) = detail;
}

fn last_pipe_detail() -> Detail {
    *LAST_PIPE_DETAIL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Walk-detail space the process last stored (`default` | `dom`). Echoed by `hittest` element
/// mode so a caller can tell which walk space a `chr:` index came from.
pub fn current_pipe_detail() -> &'static str {
    match last_pipe_detail() {
        Detail::Default => "default",
        Detail::Dom => "dom",
    }
}

#[cfg(test)]
pub(crate) fn set_last_pipe_detail_for_test(detail: Detail) {
    store_last_pipe_detail(detail);
}

pub fn resolve_request(
    id: &str,
    detail: Detail,
    assess: bool,
    samples: Option<u32>,
) -> serde_json::Value {
    let mut req = serde_json::json!({"op": "resolve", "id": id});
    if detail == Detail::Dom {
        req["detail"] = serde_json::Value::String("dom".into());
    }
    if assess {
        req["assess"] = serde_json::Value::Bool(true);
        if let Some(samples) = samples {
            req["samples"] = serde_json::Value::from(samples);
        }
    }
    req
}

/// Scoped resolve request. It names the tab/document/snapshot/index and never carries a
/// legacy `id` field, so the extension cannot re-walk the live DOM for it.
pub fn resolve_ref_request(
    r: &SnapshotRef,
    assess: bool,
    samples: Option<u32>,
) -> serde_json::Value {
    resolve_ref_request_caps(r, assess, samples, false)
}

/// Same request with the additive `caps:true` read-only capability flag. Without the flag
/// the reply stays byte-identical to the pre-0125 resolve (test-pinned).
pub fn resolve_ref_request_caps(
    r: &SnapshotRef,
    assess: bool,
    samples: Option<u32>,
    caps: bool,
) -> serde_json::Value {
    match r {
        SnapshotRef::Chrome {
            tab,
            frame,
            doc,
            snap,
            idx,
        } => {
            let mut req = serde_json::json!({
                "op": "resolve",
                "v": 1,
                "tab": tab,
                "frame": frame,
                "doc": doc,
                "snap": snap,
                "idx": idx,
            });
            if assess {
                req["assess"] = serde_json::Value::Bool(true);
                if let Some(samples) = samples {
                    req["samples"] = serde_json::Value::from(samples);
                }
            }
            if caps {
                req["caps"] = serde_json::Value::Bool(true);
            }
            req
        }
        SnapshotRef::Uia { .. } => serde_json::json!({"op": "resolve", "v": 1, "uia": true}),
    }
}

/// Reply evidence for the final pre-dispatch tabs/windows probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BindingEvidence {
    pub tab: u32,
    pub window: Option<u32>,
    pub active: bool,
    pub focused: bool,
}

fn error_token(reply: &serde_json::Value) -> Option<String> {
    reply
        .get("error")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
}

/// Query-path error classification. The SW's kebab `unsupported-op` short-circuit and the
/// secondary `empty` port-close both mean "this extension does not answer `query`"; neither is a
/// malformed reply. `empty` is mapped here only for the query path so the Completed resolve
/// contract keeps `protocol_error` for its own port-close case.
pub(crate) fn query_error_token(token: &str) -> (IdentityState, String) {
    if token == "empty" {
        return (IdentityState::Unknown, "unsupported_op".to_string());
    }
    classify_ref_error(token)
}

#[cfg(test)]
#[allow(clippy::result_large_err)]
pub(crate) fn query_reply_for_test(
    reply: &serde_json::Value,
    r: &SnapshotRef,
) -> Result<QueryReply, IdentityReport> {
    query_reply_from_json(reply, r)
}

fn classify_ref_error(token: &str) -> (IdentityState, String) {
    let token = match token {
        "no-content" => "no_content",
        "no-tab" => "no_tab",
        "unsupported-op" => "unsupported_op",
        other => other,
    };
    let state = match token {
        "stale_document"
        | "stale_snapshot"
        | "stale_element"
        | "stale_identity"
        | "not_visible"
        | "identity_mismatch"
        | "window_mismatch"
        | "frame_unavailable"
        | "frame_document_changed"
        | "frame_not_visible" => IdentityState::Stale,
        "duplicate_runtime_id" | "duplicate_identity" | "frame_link_ambiguous" => {
            IdentityState::Ambiguous
        }
        "tab_unavailable"
        | "no_tab"
        | "no_content"
        | "not_chrome"
        | "host_down"
        | "resolve_timeout"
        | "protocol_error"
        | "frame_unsupported"
        | "window_unreadable"
        | "unbound_extension"
        | "provider_timeout"
        | "tab_not_active"
        | "window_not_focused"
        | "tab_hidden"
        | "frame_geometry_unavailable"
        | "frame_occluded"
        | "frame_probe_timeout"
        | "frame_depth_exceeded"
        | "frame_link_missing"
        | "frame_budget_exceeded"
        | "no_snapshot"
        | "unsupported_op" => IdentityState::Unknown,
        _ => IdentityState::Unknown,
    };
    let reason = match token {
        "no_tab"
        | "no_content"
        | "not_chrome"
        | "host_down"
        | "resolve_timeout"
        | "frame_unsupported"
        | "window_unreadable"
        | "provider_timeout"
        | "tab_not_active"
        | "window_not_focused"
        | "tab_hidden"
        | "tab_unavailable"
        | "no_snapshot"
        | "unsupported_op"
        | "stale_document"
        | "stale_snapshot"
        | "stale_element"
        | "stale_identity"
        | "not_visible"
        | "duplicate_runtime_id"
        | "duplicate_identity"
        | "identity_mismatch"
        | "window_mismatch"
        | "unbound_extension"
        | "frame_unavailable"
        | "frame_document_changed"
        | "frame_geometry_unavailable"
        | "frame_not_visible"
        | "frame_occluded"
        | "frame_probe_timeout"
        | "frame_depth_exceeded"
        | "frame_link_ambiguous"
        | "frame_link_missing"
        | "frame_budget_exceeded" => token,
        _ => "protocol_error",
    };
    (state, reason.to_string())
}

#[allow(clippy::result_large_err)]
fn chrome_ref_parts(r: &SnapshotRef) -> Result<(u32, u32, &str, &str, u32), IdentityReport> {
    match r {
        SnapshotRef::Chrome {
            tab,
            frame,
            doc,
            snap,
            idx,
        } => Ok((*tab, *frame, doc, snap, *idx)),
        SnapshotRef::Uia { .. } => Err(IdentityReport::unknown(Some(r), "protocol_error")),
    }
}

/// Pure scoped-reply parser. Every malformed/echo/binding mismatch is a named report;
/// nothing here guesses a point.
#[allow(clippy::result_large_err)]
pub(crate) fn hit_from_scoped_reply(
    reply: &serde_json::Value,
    r: &SnapshotRef,
    hwnd: Option<isize>,
    fixture: bool,
    assess: bool,
) -> Result<ChromeHit, IdentityReport> {
    let (tab, frame, doc, snap, idx) = chrome_ref_parts(r)?;
    let report_window = reply
        .get("window")
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok());
    if let Some(token) = error_token(reply) {
        let (state, reason) = classify_ref_error(&token);
        return Err(IdentityReport::chrome_failure(r, state, &reason).with_window(report_window));
    }
    let echo_u32 = |key: &str| {
        reply
            .get(key)
            .and_then(|v| v.as_u64())
            .and_then(|n| u32::try_from(n).ok())
    };
    let echo_str = |key: &str| reply.get(key).and_then(|v| v.as_str());
    if echo_u32("tab") != Some(tab)
        || echo_u32("frame") != Some(frame)
        || echo_str("doc") != Some(doc)
        || echo_str("snap") != Some(snap)
        || echo_u32("idx") != Some(idx)
    {
        return Err(IdentityReport::chrome_failure(
            r,
            IdentityState::Unknown,
            "protocol_error",
        ));
    }
    if !fixture {
        match reply.get("active").and_then(|v| v.as_bool()) {
            Some(true) => {}
            Some(false) => {
                return Err(IdentityReport::chrome_failure(
                    r,
                    IdentityState::Unknown,
                    "tab_not_active",
                ));
            }
            None => {
                return Err(IdentityReport::chrome_failure(
                    r,
                    IdentityState::Unknown,
                    "protocol_error",
                ));
            }
        }
        match reply.get("focused").and_then(|v| v.as_bool()) {
            Some(true) => {}
            Some(false) => {
                return Err(IdentityReport::chrome_failure(
                    r,
                    IdentityState::Unknown,
                    "window_not_focused",
                ));
            }
            None => {
                return Err(IdentityReport::chrome_failure(
                    r,
                    IdentityState::Unknown,
                    "protocol_error",
                ));
            }
        }
        match reply.get("visibilityState").and_then(|v| v.as_str()) {
            Some("visible") => {}
            Some(_) => {
                return Err(IdentityReport::chrome_failure(
                    r,
                    IdentityState::Unknown,
                    "tab_hidden",
                ));
            }
            None => {
                return Err(IdentityReport::chrome_failure(
                    r,
                    IdentityState::Unknown,
                    "protocol_error",
                ));
            }
        }
    }
    let mut node: RawNode = serde_json::from_value(reply.clone())
        .map_err(|_| IdentityReport::chrome_failure(r, IdentityState::Unknown, "protocol_error"))?;
    let (metrics, top_url, top_title, frame_rows) = if frame > 0 {
        // The map is always built from the top frame. A child's 300x250 metrics would match
        // no top viewport anchor (`no_viewport_anchor`) and must never build the map.
        let top: RawTop = reply
            .get("top")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| frame_geometry(r))?
            .ok_or_else(|| frame_geometry(r))?;
        if top.frame != Some(0) {
            return Err(frame_geometry(r));
        }
        let chain: Vec<crate::frames::FrameLink> = reply
            .get("chain")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| frame_geometry(r))?
            .ok_or_else(|| frame_geometry(r))?;
        if chain.is_empty() || chain.len() > crate::frames::MAX_FRAME_DEPTH {
            return Err(frame_geometry(r));
        }
        let mut rows = Vec::new();
        let mut current = frame;
        for (i, link) in chain.iter().enumerate() {
            let Some(parent) = link.parent else {
                return Err(frame_geometry(r));
            };
            rows.push(crate::frames::RawFrameLink {
                frame: Some(current),
                status: Some("ok".into()),
                link: Some(link.clone()),
                ..Default::default()
            });
            current = parent;
            if current == 0 {
                if i + 1 != chain.len() {
                    return Err(frame_geometry(r));
                }
                break;
            }
        }
        if current != 0 {
            return Err(frame_geometry(r));
        }
        // Child metrics are geometry evidence only; compare the target content box to the
        // child's own layout viewport (scrollbar-inclusive, as `innerWidth` is).
        let child_metrics: ChromeMetrics = reply
            .get("metrics")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| frame_geometry(r))?
            .ok_or_else(|| frame_geometry(r))?;
        if child_metrics.inner_width.is_finite()
            && child_metrics.inner_width > 0.0
            && (chain[0].content_width - child_metrics.inner_width).abs() > 1.0
        {
            return Err(frame_geometry(r));
        }
        if child_metrics.inner_height.is_finite()
            && child_metrics.inner_height > 0.0
            && (chain[0].content_height - child_metrics.inner_height).abs() > 1.0
        {
            return Err(frame_geometry(r));
        }
        let composed = crate::frames::compose_rect(&rows, frame, node.rect_css)
            .map_err(|err| frame_report(r, err))?;
        let unclipped = crate::frames::compose_rect_unclipped(&rows, frame, node.rect_css)
            .map_err(|err| frame_report(r, err))?;
        if (unclipped.width - composed.width).abs() > 0.01
            || (unclipped.height - composed.height).abs() > 0.01
        {
            // A clipped sliver must be refused, not clicked at the centre of what remains.
            return Err(frame_report(r, crate::frames::FrameError::NotVisible));
        }
        node.rect_css = composed;
        remember_frame_chain(&r.to_string(), &chain);
        (top.metrics, top.url, top.title, rows)
    } else {
        let metrics: ChromeMetrics = reply
            .get("metrics")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| {
                IdentityReport::chrome_failure(r, IdentityState::Unknown, "protocol_error")
            })?
            .ok_or_else(|| {
                IdentityReport::chrome_failure(r, IdentityState::Unknown, "protocol_error")
            })?;
        (
            metrics,
            reply
                .get("url")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            reply
                .get("title")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            Vec::new(),
        )
    };
    let mut assessment = if assess {
        reply
            .get("assessment")
            .cloned()
            .filter(|v| !v.is_null())
            .map(|value| {
                serde_json::from_value(value).unwrap_or_else(|_| {
                    Assessment::unknown(
                        raw_node_rect(reply).center(),
                        raw_node_rect(reply),
                        "assessment_unavailable",
                    )
                })
            })
    } else {
        None
    };
    if frame > 0
        && let Some(assessment) = assessment.as_mut()
    {
        compose_assessment(assessment, &frame_rows, frame).map_err(|err| frame_report(r, err))?;
    }
    let raw = RawSnapshot {
        url: top_url,
        title: top_title,
        main_text: None,
        elements: vec![node],
        cards: Vec::new(),
        metrics,
        result_count: None,
        local_matches: None,
        empty_state: None,
        zip: None,
        radius: None,
        cards_walked: None,
        assessment,
        assess_v: reply
            .get("assess_v")
            .and_then(|v| v.as_u64())
            .and_then(|n| u32::try_from(n).ok()),
        tab: Some(tab),
        window: report_window,
        frame: Some(frame),
        doc: Some(doc.to_string()),
        snap: Some(snap.to_string()),
        retained: Some(idx as usize + 1),
        stored: None,
        fallback: reply.get("fallback").and_then(|v| v.as_bool()),
        frames: frame_rows,
        frames_total: None,
        frames_truncated: None,
    };
    let node = raw.elements[0].clone();
    let obs = make_observe_id(&raw, hwnd, 1);
    let map = map_for_raw(&raw, hwnd, fixture, obs.clone());
    let contract = contract_for(&map, node.rect_css.center());
    let rect = crate::point::map_rect(&contract, node.rect_css).unwrap_or(Rect {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    });
    let mut assessment = raw.assessment;
    if let Some(a) = assessment.as_mut() {
        a.observe_id = Some(obs);
    }
    Ok(ChromeHit {
        id: r.to_string(),
        name: node.text.unwrap_or_default(),
        role: node.role.unwrap_or_else(|| "Other".into()),
        rect,
        css_rect: node.rect_css,
        css_point: node.rect_css.center(),
        contract,
        hwnd,
        href: reply
            .get("href")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        map: Some(map),
        assessment,
        capability: reply.get("capability").cloned().filter(|v| !v.is_null()),
        identity: Some(IdentityReport::exact_chrome(r).with_window(report_window)),
    })
}

/// Compose the child's assessment point/rect (and advisory blocker rects) into top-frame CSS
/// once, through the same validated chain as the element. `validate_assessment` then compares
/// the composed point to the composed contract, not a child-local one.
fn compose_assessment(
    assessment: &mut Assessment,
    rows: &[crate::frames::RawFrameLink],
    frame: u32,
) -> Result<(), crate::frames::FrameError> {
    assessment.selected_point =
        crate::frames::compose_point(rows, frame, assessment.selected_point)?;
    assessment.target_rect_css =
        crate::frames::compose_rect(rows, frame, assessment.target_rect_css)?;
    for blocker in &mut assessment.blocked_by {
        if let Some(rect) = blocker.rect_css {
            blocker.rect_css = crate::frames::compose_rect(rows, frame, rect).ok();
        }
    }
    if let Some(blocker) = assessment.clipped_by.as_mut()
        && let Some(rect) = blocker.rect_css
    {
        blocker.rect_css = crate::frames::compose_rect(rows, frame, rect).ok();
    }
    Ok(())
}

fn frame_geometry(r: &SnapshotRef) -> IdentityReport {
    IdentityReport::chrome_failure(r, IdentityState::Unknown, "frame_geometry_unavailable")
}

fn frame_report(r: &SnapshotRef, err: crate::frames::FrameError) -> IdentityReport {
    let state = match err {
        crate::frames::FrameError::NotVisible => IdentityState::Stale,
        _ => IdentityState::Unknown,
    };
    IdentityReport::chrome_failure(r, state, err.token())
}

fn raw_node_rect(reply: &serde_json::Value) -> CssRect {
    reply
        .get("rectCss")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .ok()
        .flatten()
        .unwrap_or(CssRect {
            left: 0.0,
            top: 0.0,
            width: 0.0,
            height: 0.0,
        })
}

#[cfg(test)]
thread_local! {
    static SCOPED_RESOLVE_STUB: std::cell::RefCell<Option<Result<ChromeHit, IdentityReport>>> =
        const { std::cell::RefCell::new(None) };
    static BINDING_STUB: std::cell::RefCell<Option<Result<BindingEvidence, IdentityReport>>> =
        const { std::cell::RefCell::new(None) };
    static BINDING_SEQUENCE: std::cell::RefCell<Vec<Result<BindingEvidence, IdentityReport>>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static FRAME_BINDING_STUB: std::cell::RefCell<
        Option<Result<FrameBindingEvidence, IdentityReport>>,
    > = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_frame_binding_stub(stub: Option<Result<FrameBindingEvidence, IdentityReport>>) {
    FRAME_BINDING_STUB.with(|c| *c.borrow_mut() = stub);
}

#[cfg(test)]
pub(crate) fn set_scoped_resolve_stub(stub: Option<Result<ChromeHit, IdentityReport>>) {
    SCOPED_RESOLVE_STUB.with(|c| *c.borrow_mut() = stub);
}

#[cfg(test)]
pub(crate) fn set_binding_stub(stub: Option<Result<BindingEvidence, IdentityReport>>) {
    BINDING_STUB.with(|c| *c.borrow_mut() = stub);
}

#[cfg(test)]
pub(crate) fn set_binding_sequence(sequence: Vec<Result<BindingEvidence, IdentityReport>>) {
    BINDING_SEQUENCE.with(|c| *c.borrow_mut() = sequence);
}

/// One stored-node row of a scoped Chrome list query.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QueryRow {
    pub id: String,
    pub idx: Option<u32>,
    pub role: String,
    pub text: Option<String>,
    pub rect_css: Option<CssRect>,
    pub key_ok: Option<bool>,
    pub connected: bool,
    pub depth: Option<u32>,
    pub href: Option<String>,
}

/// Parsed scoped Chrome list reply. Every failure is an [`IdentityReport`]; nothing guesses.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QueryReply {
    pub tab: Option<u32>,
    pub window: Option<u32>,
    pub frame: Option<u32>,
    pub doc: String,
    pub snap: String,
    pub retained: usize,
    pub examined: usize,
    pub budget_hit: bool,
    pub matched_total: usize,
    pub matches: Vec<QueryRow>,
    pub url: Option<String>,
    pub title: Option<String>,
    pub fixture: bool,
}

/// Read-only list query over the **stored** snapshot only. It never sends `op:"snapshot"`, never
/// walks, and never replaces the record. `subtree` sends the scope ref's own resolved integer.
#[allow(clippy::too_many_arguments, clippy::result_large_err)]
pub fn query_scope(
    r: &SnapshotRef,
    role: Option<&str>,
    text: Option<&str>,
    rect_css: Option<CssRect>,
    subtree: bool,
    limit: usize,
    offset: usize,
    budget_ms: u64,
    max_nodes: usize,
) -> Result<QueryReply, IdentityReport> {
    let (tab, frame, doc, snap, idx) = chrome_ref_parts(r)?;
    if let Some(path) = snapshot_env() {
        let raw = load_fixture_raw(&path)
            .map_err(|_| IdentityReport::chrome_failure(r, IdentityState::Unknown, "host_down"))?;
        return fixture_query(
            &raw, r, role, text, rect_css, subtree, limit, offset, max_nodes,
        );
    }
    let mut req = serde_json::json!({
        "op": "query",
        "v": 1,
        "tab": tab,
        "frame": frame,
        "doc": doc,
        "snap": snap,
        "limit": limit,
        "offset": offset,
        "budgetMs": budget_ms,
        "maxNodes": max_nodes,
    });
    if let Some(role) = role {
        req["role"] = serde_json::Value::from(role);
    }
    if let Some(text) = text {
        req["text"] = serde_json::Value::from(text);
    }
    if let Some(rect) = rect_css {
        req["rectCss"] = serde_json::to_value(rect).unwrap_or(serde_json::Value::Null);
    }
    if subtree {
        req["subtreeRoot"] = serde_json::Value::from(idx);
    }
    let reply = pipe_request(&req).map_err(|err| {
        let text = err.to_string();
        let reason = if text.contains("client timed out after") {
            "resolve_timeout"
        } else {
            "host_down"
        };
        IdentityReport::chrome_failure(r, IdentityState::Unknown, reason)
    })?;
    query_reply_from_json(&reply, r)
}

#[allow(clippy::result_large_err)]
fn query_reply_from_json(
    reply: &serde_json::Value,
    r: &SnapshotRef,
) -> Result<QueryReply, IdentityReport> {
    let (tab, frame, doc, snap, _idx) = chrome_ref_parts(r)?;
    let report_window = reply
        .get("window")
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok());
    if let Some(token) = error_token(reply) {
        let (state, reason) = query_error_token(&token);
        return Err(IdentityReport::chrome_failure(r, state, &reason).with_window(report_window));
    }
    let echo_u32 = |key: &str| {
        reply
            .get(key)
            .and_then(|v| v.as_u64())
            .and_then(|n| u32::try_from(n).ok())
    };
    let echo_str = |key: &str| reply.get(key).and_then(|v| v.as_str());
    if echo_str("doc") != Some(doc) {
        return Err(IdentityReport::chrome_failure(
            r,
            IdentityState::Stale,
            "stale_document",
        ));
    }
    if echo_str("snap") != Some(snap) {
        return Err(IdentityReport::chrome_failure(
            r,
            IdentityState::Stale,
            "stale_snapshot",
        ));
    }
    if echo_u32("frame") != Some(frame) {
        return Err(IdentityReport::chrome_failure(
            r,
            IdentityState::Unknown,
            "protocol_error",
        ));
    }
    match reply.get("active").and_then(|v| v.as_bool()) {
        Some(true) => {}
        Some(false) => {
            return Err(IdentityReport::chrome_failure(
                r,
                IdentityState::Unknown,
                "tab_not_active",
            ));
        }
        None => {
            return Err(IdentityReport::chrome_failure(
                r,
                IdentityState::Unknown,
                "protocol_error",
            ));
        }
    }
    match reply.get("focused").and_then(|v| v.as_bool()) {
        Some(true) => {}
        Some(false) => {
            return Err(IdentityReport::chrome_failure(
                r,
                IdentityState::Unknown,
                "window_not_focused",
            ));
        }
        None => {
            return Err(IdentityReport::chrome_failure(
                r,
                IdentityState::Unknown,
                "protocol_error",
            ));
        }
    }
    match reply.get("visibilityState").and_then(|v| v.as_str()) {
        Some("visible") => {}
        Some(_) => {
            return Err(IdentityReport::chrome_failure(
                r,
                IdentityState::Unknown,
                "tab_hidden",
            ));
        }
        None => {
            return Err(IdentityReport::chrome_failure(
                r,
                IdentityState::Unknown,
                "protocol_error",
            ));
        }
    }
    if reply.get("fallback").and_then(|v| v.as_bool()) == Some(true) {
        // A named-tab query is never served by the active-tab fallback.
        return Err(IdentityReport::chrome_failure(
            r,
            IdentityState::Unknown,
            "protocol_error",
        ));
    }
    let retained = reply
        .get("retained")
        .and_then(|v| v.as_u64())
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| {
            IdentityReport::chrome_failure(r, IdentityState::Unknown, "protocol_error")
        })?;
    let examined = reply
        .get("examined")
        .and_then(|v| v.as_u64())
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| {
            IdentityReport::chrome_failure(r, IdentityState::Unknown, "protocol_error")
        })?;
    let matched_total = reply
        .get("matchedTotal")
        .and_then(|v| v.as_u64())
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| {
            IdentityReport::chrome_failure(r, IdentityState::Unknown, "protocol_error")
        })?;
    let budget_hit = reply
        .get("budgetHit")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| {
            IdentityReport::chrome_failure(r, IdentityState::Unknown, "protocol_error")
        })?;
    let rows: Vec<RawQueryRow> = reply
        .get("matches")
        .cloned()
        .filter(|v| !v.is_null())
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| IdentityReport::chrome_failure(r, IdentityState::Unknown, "protocol_error"))?
        .ok_or_else(|| {
            IdentityReport::chrome_failure(r, IdentityState::Unknown, "protocol_error")
        })?;
    Ok(QueryReply {
        tab: Some(tab),
        window: report_window,
        frame: Some(frame),
        doc: doc.to_string(),
        snap: snap.to_string(),
        retained,
        examined,
        budget_hit,
        matched_total,
        matches: rows.into_iter().map(QueryRow::from).collect(),
        url: echo_str("url").map(str::to_string),
        title: echo_str("title").map(str::to_string),
        fixture: false,
    })
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct RawQueryRow {
    #[serde(default)]
    id: String,
    #[serde(default)]
    idx: Option<u32>,
    #[serde(default)]
    role: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default, rename = "rectCss")]
    rect_css: Option<CssRect>,
    #[serde(default, rename = "keyOk")]
    key_ok: Option<bool>,
    #[serde(default)]
    connected: Option<bool>,
    #[serde(default)]
    depth: Option<u32>,
    #[serde(default)]
    href: Option<String>,
}

impl From<RawQueryRow> for QueryRow {
    fn from(raw: RawQueryRow) -> Self {
        let connected = raw.connected.unwrap_or(true);
        Self {
            id: raw.id,
            idx: raw.idx,
            role: raw.role,
            text: raw.text,
            rect_css: raw.rect_css,
            // A detached node is never `key_ok:true`; the ref will refuse at resolve.
            key_ok: if connected { raw.key_ok } else { Some(false) },
            connected,
            depth: raw.depth,
            href: raw.href,
        }
    }
}

/// Fixture-mode query: the fixture snapshot is already the stored walk, so filtering it is the
/// same predicate without a live DOM. Containment is unknowable here and is named, never guessed.
#[allow(clippy::too_many_arguments, clippy::result_large_err)]
fn fixture_query(
    raw: &RawSnapshot,
    r: &SnapshotRef,
    role_want: Option<&str>,
    text_want: Option<&str>,
    rect_css: Option<CssRect>,
    subtree: bool,
    limit: usize,
    offset: usize,
    max_nodes: usize,
) -> Result<QueryReply, IdentityReport> {
    let (tab, frame, doc, snap, _idx) = chrome_ref_parts(r)?;
    if raw.doc.as_deref() != Some(doc) {
        return Err(IdentityReport::chrome_failure(
            r,
            IdentityState::Stale,
            "stale_document",
        ));
    }
    if raw.snap.as_deref() != Some(snap) {
        return Err(IdentityReport::chrome_failure(
            r,
            IdentityState::Stale,
            "stale_snapshot",
        ));
    }
    if subtree {
        return Err(IdentityReport::chrome_failure(
            r,
            IdentityState::Unknown,
            "fixture_unavailable",
        ));
    }
    let mut matched: Vec<QueryRow> = Vec::new();
    let retained = raw.retained.unwrap_or(raw.elements.len());
    let scope_nodes: Vec<&RawNode> = raw
        .elements
        .iter()
        .filter(|el| el.frame.unwrap_or(0) == frame)
        .take(max_nodes)
        .collect();
    for el in &scope_nodes {
        let role = el.role.clone().unwrap_or_default();
        let id = el.id.clone().unwrap_or_default();
        let idx = el
            .idx
            .or_else(|| id.strip_prefix("chr:").and_then(|n| n.parse::<u32>().ok()));
        if !crate::find::chrome_row_matches(
            &role,
            el.text.as_deref(),
            Some(el.rect_css),
            role_want,
            text_want,
            rect_css,
        ) {
            continue;
        }
        matched.push(QueryRow {
            id,
            idx,
            role,
            text: el.text.clone(),
            rect_css: Some(el.rect_css),
            key_ok: Some(true),
            connected: true,
            depth: None,
            href: None,
        });
    }
    let matched_total = matched.len();
    let matches = matched
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    Ok(QueryReply {
        tab: Some(tab),
        window: raw.window,
        frame: Some(frame),
        doc: doc.to_string(),
        snap: snap.to_string(),
        retained,
        examined: scope_nodes.len(),
        budget_hit: false,
        matched_total,
        matches,
        url: raw.url.clone(),
        title: raw.title.clone(),
        fixture: true,
    })
}

/// Scoped Chrome resolve. Live mode requires the current foreground HWND to be the intended
/// Chrome window; a scoped request is never served by whatever tab is in front.
#[allow(clippy::result_large_err)]
pub fn try_resolve_ref(
    r: &SnapshotRef,
    hwnd: Option<isize>,
    assess: bool,
    samples: Option<u32>,
) -> Result<ChromeHit, IdentityReport> {
    resolve_ref_full(r, hwnd, assess, samples, false)
}

/// Read-only `find --ref` resolve that additionally asks for the additive `caps:true` DOM
/// capability block. Never dispatched by an actuation path.
#[allow(clippy::result_large_err)]
pub fn try_resolve_ref_with_caps(
    r: &SnapshotRef,
    hwnd: Option<isize>,
    assess: bool,
    samples: Option<u32>,
) -> Result<ChromeHit, IdentityReport> {
    resolve_ref_full(r, hwnd, assess, samples, true)
}

#[allow(clippy::result_large_err)]
fn resolve_ref_full(
    r: &SnapshotRef,
    hwnd: Option<isize>,
    assess: bool,
    samples: Option<u32>,
    caps: bool,
) -> Result<ChromeHit, IdentityReport> {
    #[cfg(test)]
    {
        if let Some(stub) = SCOPED_RESOLVE_STUB.with(|c| c.borrow().clone()) {
            return stub;
        }
    }
    if let Some(path) = snapshot_env() {
        let raw = load_fixture_raw(&path)
            .map_err(|_| IdentityReport::chrome_failure(r, IdentityState::Unknown, "host_down"))?;
        return scoped_fixture_hit(&raw, r, hwnd, assess, caps);
    }
    let Some(hwnd) = hwnd else {
        return Err(IdentityReport::chrome_failure(
            r,
            IdentityState::Unknown,
            "not_chrome",
        ));
    };
    // Do not short-circuit on a non-Chrome foreground: a background Chrome window behind
    // Notepad must resolve through the extension reply and come back as `window_not_focused`
    // / `tab_not_active`, not `not_chrome`.
    let req = resolve_ref_request_caps(r, assess, samples, caps);
    let reply = pipe_request(&req).map_err(|err| {
        let text = err.to_string();
        let reason = if text.contains("client timed out after") {
            "resolve_timeout"
        } else {
            "host_down"
        };
        IdentityReport::chrome_failure(r, IdentityState::Unknown, reason)
    })?;
    let hit = hit_from_scoped_reply(&reply, r, Some(hwnd), false, assess)?;
    if !foreground::target_is_chrome(Some(hwnd)) {
        return Err(IdentityReport::chrome_failure(
            r,
            IdentityState::Unknown,
            "not_chrome",
        ));
    }
    Ok(hit)
}

#[allow(clippy::result_large_err)]
fn scoped_fixture_hit(
    raw: &RawSnapshot,
    r: &SnapshotRef,
    hwnd: Option<isize>,
    assess: bool,
    caps: bool,
) -> Result<ChromeHit, IdentityReport> {
    let (tab, frame, doc, snap, idx) = chrome_ref_parts(r)?;
    let node = if frame > 0 {
        // Fixture parity: a frame node resolves by its owning frame and walk index (or exact
        // id), never by `canonical_chr` (a frame node's id is an `sref1:` token).
        raw.elements
            .iter()
            .find(|n| {
                n.frame == Some(frame)
                    && (n.idx == Some(idx) || n.id.as_deref() == Some(&r.to_string()))
            })
            .ok_or_else(|| {
                IdentityReport::chrome_failure(r, IdentityState::Stale, "stale_element")
            })?
    } else {
        raw.elements
            .iter()
            .find(|n| n.id.as_deref().and_then(canonical_chr) == Some(format!("chr:{idx}")))
            .ok_or_else(|| {
                IdentityReport::chrome_failure(r, IdentityState::Stale, "stale_element")
            })?
    };
    let mut reply = serde_json::json!({
        "id": node.id,
        "role": node.role,
        "text": node.text,
        "rectCss": node.rect_css,
        "metrics": raw.metrics,
        "url": raw.url,
        "title": raw.title,
        "tab": tab,
        "frame": frame,
        "doc": doc,
        "snap": snap,
        "idx": idx,
        "active": true,
        "focused": true,
        "visibilityState": "visible",
        "assess_v": raw.assess_v,
    });
    if frame > 0 {
        let Some(chain) = crate::frames::chain_for(&raw.frames, frame) else {
            return Err(frame_geometry(r));
        };
        let target = chain[0].clone();
        // The child reply's metrics are evidence only; the synthetic pair matches the measured
        // content box so the size check exercises the same predicate as a live reply.
        reply["metrics"] = serde_json::json!({
            "innerWidth": target.content_width,
            "innerHeight": target.content_height,
            "devicePixelRatio": raw.metrics.device_pixel_ratio,
        });
        reply["top"] = serde_json::json!({
            "frame": 0,
            "metrics": raw.metrics,
            "url": raw.url,
            "title": raw.title,
        });
        reply["chain"] = serde_json::to_value(&chain).unwrap_or(serde_json::Value::Null);
    }
    if assess && let Some(a) = &raw.assessment {
        reply["assessment"] = serde_json::to_value(a).unwrap_or(serde_json::Value::Null);
    }
    if caps {
        let role = node.role.clone().unwrap_or_else(|| "Other".into());
        reply["capability"] = fixture_capability(&role);
    }
    hit_from_scoped_reply(&reply, r, hwnd, true, assess)
}

/// Role→verb tables mirrored by `extension/content.js` (`CAP_*` consts). A parity test keeps the
/// live and fixture vocabularies identical.
pub(crate) const CAP_INVOKE_ROLES: &[&str] = &[
    "button",
    "hyperlink",
    "tabitem",
    "menuitem",
    "checkbox",
    "radiobutton",
    "treeitem",
];
pub(crate) const CAP_TOGGLE_ROLES: &[&str] = &["checkbox", "radiobutton"];
pub(crate) const CAP_EXPAND_ROLES: &[&str] = &["combobox"];
pub(crate) const CAP_SELECT_ROLES: &[&str] = &["tabitem", "menuitem", "treeitem"];
pub(crate) const CAP_VALUE_ROLES: &[&str] = &["edit", "combobox"];

/// Fixture-only DOM capability evidence: role markup, no live attributes. The Node harness and
/// the live extension own the real attribute predicates; this keeps fixture mode honest about
/// what a role alone can say (never `unavailable`) and uses the same role tables/tokens as live.
fn fixture_capability(role: &str) -> serde_json::Value {
    let role = role.to_ascii_lowercase();
    let has = |table: &[&str]| table.contains(&role.as_str());
    let invoke = has(CAP_INVOKE_ROLES);
    let toggle = has(CAP_TOGGLE_ROLES);
    let expand = has(CAP_EXPAND_ROLES);
    let select = has(CAP_SELECT_ROLES);
    let set_value = has(CAP_VALUE_ROLES);
    let action = |verb: &str, available: bool, evidence: &str| {
        serde_json::json!({
            "action": verb,
            "support": if available { "available" } else { "unknown" },
            "evidence": if available { evidence } else { "no_markup_evidence" },
        })
    };
    let invoke_evidence = if role == "button" {
        "role_button"
    } else {
        "role_markup"
    };
    serde_json::json!({
        "source": "dom",
        "actions": [
            action("invoke", invoke, invoke_evidence),
            action("toggle", toggle, "role_checkbox"),
            action("expand", expand, "role_expand"),
            action("collapse", expand, "role_expand"),
            action("select", select, "role_selection"),
            action("set_value", set_value, "editable_role"),
        ],
        "state": {"enabled": true},
        "notes": ["dom_advertised_not_proven"],
    })
}

/// Frame pre-dispatch probe evidence. A missing chain means the frame geometry is unavailable;
/// a different document means the child navigated.
#[derive(Debug, Clone)]
pub struct FrameBindingEvidence {
    pub frame: u32,
    pub doc: String,
    pub snap: Option<String>,
    pub chain_present: bool,
}

// Process-local resolve-time chain, keyed by the exact ref. The pre-dispatch `frame_binding`
// re-read compares against it so a moved/resized iframe is refused before the pointer moves.
thread_local! {
    static FRAME_CHAINS: std::cell::RefCell<
        std::collections::HashMap<String, Vec<crate::frames::FrameLink>>,
    > = std::cell::RefCell::new(std::collections::HashMap::new());
}

fn remember_frame_chain(key: &str, chain: &[crate::frames::FrameLink]) {
    FRAME_CHAINS.with(|cell| {
        let mut map = cell.borrow_mut();
        if map.len() >= 32 {
            map.clear();
        }
        map.insert(key.to_string(), chain.to_vec());
    });
}

fn cached_frame_chain(key: &str) -> Option<Vec<crate::frames::FrameLink>> {
    FRAME_CHAINS.with(|cell| cell.borrow().get(key).cloned())
}

fn clip_matches(a: &[crate::frames::ClipRect], b: &[crate::frames::ClipRect]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).all(|(a, b)| {
        (a.left - b.left).abs() <= 1.0
            && (a.top - b.top).abs() <= 1.0
            && (a.width - b.width).abs() <= 1.0
            && (a.height - b.height).abs() <= 1.0
    })
}

/// 1 CSS px tolerance for a moved/resized/clipped/re-flagged frame chain (spec §3.5.12).
fn frame_chains_match(
    expected: &[crate::frames::FrameLink],
    fresh: &[crate::frames::FrameLink],
) -> bool {
    if expected.len() != fresh.len() {
        return false;
    }
    expected.iter().zip(fresh).all(|(a, b)| {
        a.parent == b.parent
            && (a.content_left - b.content_left).abs() <= 1.0
            && (a.content_top - b.content_top).abs() <= 1.0
            && (a.content_width - b.content_width).abs() <= 1.0
            && (a.content_height - b.content_height).abs() <= 1.0
            && a.transform_ok == b.transform_ok
            && a.visible == b.visible
            && a.hit_ok == b.hit_ok
            && a.onscreen == b.onscreen
            && clip_matches(&a.clip, &b.clip)
    })
}

/// Distinct `frame_binding` re-read immediately before `move_to` (0131 DoD-5). It never uses
/// the observe-time chain and never falls back to the top-frame `binding` op.
#[allow(clippy::result_large_err)]
pub fn verify_frame_binding(
    tab: u32,
    frame: u32,
    doc: &str,
    snap: &str,
    idx: u32,
) -> Result<FrameBindingEvidence, IdentityReport> {
    #[cfg(test)]
    {
        if let Some(stub) = FRAME_BINDING_STUB.with(|c| c.borrow().clone()) {
            return stub;
        }
    }
    let r = SnapshotRef::Chrome {
        tab,
        frame,
        doc: doc.to_string(),
        snap: snap.to_string(),
        idx,
    };
    let reply = pipe_request(&serde_json::json!({
        "op": "frame_binding",
        "tab": tab,
        "frame": frame,
        "doc": doc,
        "snap": snap,
        "idx": idx,
    }))
    .map_err(|err| {
        let text = err.to_string();
        let reason = if text.contains("client timed out after") {
            "frame_probe_timeout"
        } else {
            "frame_unavailable"
        };
        IdentityReport::chrome_failure(&r, IdentityState::Unknown, reason)
    })?;
    if let Some(token) = error_token(&reply) {
        let (state, reason) = classify_ref_error(&token);
        return Err(IdentityReport::chrome_failure(&r, state, &reason));
    }
    let echo_tab = reply
        .get("tab")
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok());
    let echo_frame = reply
        .get("frame")
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok());
    if echo_tab != Some(tab) || echo_frame != Some(frame) {
        return Err(IdentityReport::chrome_failure(
            &r,
            IdentityState::Unknown,
            "protocol_error",
        ));
    }
    if reply.get("connected").and_then(|v| v.as_bool()) != Some(true) {
        return Err(IdentityReport::chrome_failure(
            &r,
            IdentityState::Unknown,
            "frame_unavailable",
        ));
    }
    let echo_idx = reply
        .get("idx")
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok());
    if echo_idx != Some(idx) {
        return Err(IdentityReport::chrome_failure(
            &r,
            IdentityState::Unknown,
            "protocol_error",
        ));
    }
    if !reply.get("rectCss").is_some_and(|v| !v.is_null()) {
        return Err(IdentityReport::chrome_failure(
            &r,
            IdentityState::Stale,
            "stale_element",
        ));
    }
    let echo_doc = reply.get("doc").and_then(|v| v.as_str()).unwrap_or("");
    if echo_doc != doc {
        return Err(IdentityReport::chrome_failure(
            &r,
            IdentityState::Stale,
            "frame_document_changed",
        ));
    }
    // The requested snapshot is part of the binding: a child that re-stored a different
    // snapshot between resolve and move must not be clicked at the old coordinates.
    if reply.get("snap").and_then(|v| v.as_str()) != Some(snap) {
        return Err(IdentityReport::chrome_failure(
            &r,
            IdentityState::Stale,
            "stale_snapshot",
        ));
    }
    let chain_present = reply.get("chain").is_some_and(|v| !v.is_null());
    if !chain_present {
        return Err(IdentityReport::chrome_failure(
            &r,
            IdentityState::Unknown,
            "frame_geometry_unavailable",
        ));
    }
    // Fail closed: a chain that was not cached from the resolve cannot be compared, so the
    // frame geometry is unverifiable (`frame_geometry_unavailable`), never accepted.
    let Some(expected) = cached_frame_chain(&r.to_string()) else {
        return Err(IdentityReport::chrome_failure(
            &r,
            IdentityState::Unknown,
            "frame_geometry_unavailable",
        ));
    };
    let fresh: Vec<crate::frames::FrameLink> = reply
        .get("chain")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| {
            IdentityReport::chrome_failure(&r, IdentityState::Unknown, "frame_geometry_unavailable")
        })?
        .unwrap_or_default();
    if !frame_chains_match(&expected, &fresh) {
        return Err(IdentityReport::chrome_failure(
            &r,
            IdentityState::Unknown,
            "frame_geometry_unavailable",
        ));
    }
    Ok(FrameBindingEvidence {
        frame,
        doc: echo_doc.to_string(),
        snap: reply
            .get("snap")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        chain_present,
    })
}

/// Final pre-dispatch tabs/windows probe. No content-script read.
#[allow(clippy::result_large_err)]
pub fn verify_binding(tab: u32) -> Result<BindingEvidence, IdentityReport> {
    #[cfg(test)]
    {
        let next = BINDING_SEQUENCE.with(|c| {
            let mut sequence = c.borrow_mut();
            if sequence.is_empty() {
                None
            } else {
                Some(sequence.remove(0))
            }
        });
        if let Some(result) = next {
            return result;
        }
        if let Some(stub) = BINDING_STUB.with(|c| c.borrow().clone()) {
            return stub;
        }
    }
    if snapshot_env().is_some() {
        // Fixture mode has no live tab/window evidence; fail closed so a future valid
        // fixture transform can never dispatch through a synthesized binding proof.
        return Err(binding_report(
            tab,
            IdentityState::Unknown,
            "fixture_snapshot",
        ));
    }
    let reply = pipe_request(&serde_json::json!({"op": "binding", "tab": tab})).map_err(|err| {
        let text = err.to_string();
        let reason = if text.contains("client timed out after") {
            "resolve_timeout"
        } else {
            "host_down"
        };
        binding_report(tab, IdentityState::Unknown, reason)
    })?;
    if let Some(token) = error_token(&reply) {
        let (state, reason) = classify_ref_error(&token);
        return Err(binding_report(tab, state, &reason));
    }
    let echoed = reply
        .get("tab")
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok());
    if echoed != Some(tab) {
        return Err(binding_report(
            tab,
            IdentityState::Unknown,
            "protocol_error",
        ));
    }
    let window = reply
        .get("window")
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok());
    Ok(BindingEvidence {
        tab,
        window,
        active: reply
            .get("active")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        focused: reply
            .get("focused")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    })
}

impl IdentityReport {
    fn with_state(mut self, state: IdentityState) -> Self {
        self.state = state;
        self
    }
}

fn binding_report(tab: u32, state: IdentityState, reason: &str) -> IdentityReport {
    let mut out = IdentityReport::unknown(None, reason).with_state(state);
    out.binding = Binding::Scoped;
    out.tab = Some(tab);
    out
}

/// Parts of the clicked scoped ref echoed into a `postcheck` request. `doc`/`snap` prove the
/// `url_changed`/`title_changed` baseline; `idx` addresses the stored node for
/// `element_present`/`element_absent`.
#[derive(Debug, Clone, Copy)]
pub struct PostcheckRef<'a> {
    pub doc: &'a str,
    pub snap: &'a str,
    pub idx: u32,
}

/// Closed postcheck transport failure. Never carries page text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostcheckError {
    Timeout(String),
    Unavailable(String),
    HostDown(String),
}

/// Boolean-only postcheck reply. `visibilityState` is the pinned camelCase wire name; there is
/// deliberately no url/title/text/value field.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PostcheckReply {
    pub state: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub snap: Option<String>,
    #[serde(default)]
    pub tab: Option<u32>,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub focused: bool,
    #[serde(default, rename = "visibilityState")]
    pub visibility_state: Option<String>,
    #[serde(default)]
    pub fallback: bool,
}

#[cfg(test)]
pub(crate) const REF_ERROR_TOKENS: &[&str] = &[
    "stale_document",
    "stale_snapshot",
    "stale_element",
    "stale_identity",
    "not_visible",
    "identity_mismatch",
    "window_mismatch",
    "frame_unavailable",
    "frame_document_changed",
    "frame_not_visible",
    "duplicate_runtime_id",
    "duplicate_identity",
    "frame_link_ambiguous",
    "tab_unavailable",
    "no_tab",
    "no_content",
    "not_chrome",
    "host_down",
    "resolve_timeout",
    "protocol_error",
    "frame_unsupported",
    "window_unreadable",
    "unbound_extension",
    "provider_timeout",
    "tab_not_active",
    "window_not_focused",
    "tab_hidden",
    "frame_geometry_unavailable",
    "frame_occluded",
    "frame_probe_timeout",
    "frame_depth_exceeded",
    "frame_link_missing",
    "frame_budget_exceeded",
];

#[cfg(test)]
static POSTCHECK_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
pub(crate) fn postcheck_calls() -> usize {
    POSTCHECK_CALLS.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
thread_local! {
    static POSTCHECK_STUB: std::cell::RefCell<Option<Result<PostcheckReply, PostcheckError>>> =
        const { std::cell::RefCell::new(None) };
    static POSTCHECK_SEQUENCE: std::cell::RefCell<Vec<Result<PostcheckReply, PostcheckError>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
pub(crate) fn set_postcheck_stub(stub: Option<Result<PostcheckReply, PostcheckError>>) {
    POSTCHECK_STUB.with(|c| *c.borrow_mut() = stub);
}

#[cfg(test)]
pub(crate) fn set_postcheck_sequence(sequence: Vec<Result<PostcheckReply, PostcheckError>>) {
    POSTCHECK_SEQUENCE.with(|c| *c.borrow_mut() = sequence);
}

#[cfg(test)]
pub(crate) fn reset_postcheck_calls() {
    POSTCHECK_CALLS.store(0, std::sync::atomic::Ordering::Relaxed);
}

/// Request for the read-only `postcheck` op. Carries an explicit `frame: 0`: 0124's
/// postconditions are top-frame only and 0131 owns frame composition.
pub fn postcheck_request(
    tab: u32,
    condition: &crate::outcome::Postcondition,
    parts: Option<PostcheckRef<'_>>,
) -> serde_json::Value {
    use crate::outcome::PostconditionKind as Kind;
    let mut req = serde_json::json!({
        "op": "postcheck",
        "tab": tab,
        "frame": 0,
        "kind": condition.kind_token(),
    });
    match condition.kind {
        Kind::UrlPrefix | Kind::UrlEquals | Kind::TitleEquals => {
            if let Some(value) = condition.value.as_deref() {
                req["value"] = serde_json::Value::String(value.to_string());
            }
        }
        Kind::CountAtLeast => {
            if let Some(role) = condition.role.as_deref() {
                req["role"] = serde_json::Value::String(role.to_string());
            }
            if let Some(n) = condition.n {
                req["n"] = serde_json::Value::from(n);
            }
        }
        Kind::ElementPresent | Kind::ElementAbsent => {
            if let Some(reference) = condition.r#ref.as_deref()
                && let Ok(SnapshotRef::Chrome { doc, snap, idx, .. }) =
                    SnapshotRef::parse(reference)
            {
                req["doc"] = serde_json::Value::String(doc);
                req["snap"] = serde_json::Value::String(snap);
                req["idx"] = serde_json::Value::from(idx);
            }
        }
        Kind::UrlChanged | Kind::TitleChanged => {}
        Kind::FocusEditable | Kind::DialogPresent => {}
    }
    if matches!(condition.kind, Kind::UrlChanged | Kind::TitleChanged)
        && let Some(parts) = parts
    {
        req["doc"] = serde_json::Value::String(parts.doc.to_string());
        req["snap"] = serde_json::Value::String(parts.snap.to_string());
    }
    let _ = parts;
    req
}

/// One read-only postcheck attempt (one 400 ms named-pipe request). The bounded `wait_ms`
/// loop lives in `actuate::apply_postcondition`, outside `click_inner`.
pub fn postcheck(
    tab: u32,
    condition: &crate::outcome::Postcondition,
    parts: Option<PostcheckRef<'_>>,
) -> Result<PostcheckReply, PostcheckError> {
    #[cfg(test)]
    {
        POSTCHECK_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let next = POSTCHECK_SEQUENCE.with(|c| {
            let mut sequence = c.borrow_mut();
            if sequence.is_empty() {
                None
            } else {
                Some(sequence.remove(0))
            }
        });
        if let Some(result) = next {
            return result;
        }
        if let Some(stub) = POSTCHECK_STUB.with(|c| c.borrow().clone()) {
            return stub;
        }
    }
    if snapshot_env().is_some() {
        return Err(PostcheckError::Unavailable("fixture_snapshot".into()));
    }
    let req = postcheck_request(tab, condition, parts);
    let reply = pipe_request(&req).map_err(|err| {
        let text = err.to_string();
        if text.contains("client timed out after") {
            PostcheckError::Timeout(text)
        } else {
            PostcheckError::HostDown(text)
        }
    })?;
    if let Some(token) = error_token(&reply) {
        // Postcondition reasons are a distinct closed vocabulary (§3.5.1); a 0123 identity
        // transport token degrades instead of leaking into the postcondition reason set.
        return Err(PostcheckError::Unavailable(postcheck_error_reason(&token)));
    }
    serde_json::from_value::<PostcheckReply>(reply)
        .map_err(|_| PostcheckError::Unavailable("protocol_error".into()))
}

/// Map a raw host/extension error token onto the documented closed postcondition reasons.
fn postcheck_error_reason(token: &str) -> String {
    match token {
        "empty" | "unsupported-op" | "unsupported" => "unsupported",
        "host_down" => "host_down",
        "window_unreadable" => "window_unreadable",
        "tab_not_active" => "tab_not_active",
        "window_not_focused" => "window_not_focused",
        "tab_hidden" => "tab_hidden",
        "stale_document" => "stale_document",
        "stale_snapshot" => "stale_snapshot",
        "stale_element" => "stale_element",
        "baseline_mismatch" => "baseline_mismatch",
        "fixture_snapshot" => "fixture_snapshot",
        _ => "protocol_error",
    }
    .to_string()
}

fn pipe_snapshot(
    detail: Detail,
    hwnd: Option<isize>,
    store: bool,
) -> Result<ChromeMap, HandsError> {
    pipe_snapshot_inner(detail, hwnd, store)
}

/// Snapshot for identity/transform reads. `store=false` leaves `LAST_PIPE_DETAIL` untouched
/// (0128 point mode must not change what a later `chr:` resolves) and asks the extension not
/// to replace its agent-facing snapshot record.
fn pipe_snapshot_inner(
    detail: Detail,
    hwnd: Option<isize>,
    store: bool,
) -> Result<ChromeMap, HandsError> {
    let mut req = serde_json::json!({"op": "snapshot"});
    if detail == Detail::Dom {
        req["detail"] = serde_json::Value::String("dom".into());
    }
    if !store {
        req["store"] = serde_json::Value::Bool(false);
    }
    if crate::frames::enabled() {
        // Opt-in fan-out: only a client that understands `snapshot.frames[]` sets this.
        req["frames"] = serde_json::Value::Bool(true);
    }
    #[cfg(test)]
    {
        if let Some(map) = PIPE_SNAPSHOT_STUB.with(|c| c.borrow().clone()) {
            if store {
                store_last_pipe_detail(detail);
            }
            return Ok(map);
        }
    }
    let reply = pipe_request(&req)?;
    let map = parse_host_reply(&reply, hwnd)?;
    if store {
        store_last_pipe_detail(detail);
    }
    Ok(map)
}

/// Non-storing snapshot used by `hittest` point mode. Same path, no walk-detail mutation.
pub fn try_snapshot_identity(detail: Detail, hwnd: Option<isize>) -> SnapshotOutcome {
    #[cfg(test)]
    {
        if let Some(outcome) = SNAPSHOT_IDENTITY_STUB.with(|c| c.borrow().clone()) {
            return outcome;
        }
    }
    match snapshot_env() {
        Some(path) => match load_fixture(&path, hwnd) {
            Ok(map) => SnapshotOutcome::Map(Box::new(map)),
            Err(_) => SnapshotOutcome::HostDown,
        },
        None => match pipe_snapshot_inner(detail, hwnd, false) {
            Ok(map) => SnapshotOutcome::Map(Box::new(map)),
            Err(err) => classify_snapshot_err(&err),
        },
    }
}

fn assessment_from_reply(reply: &serde_json::Value, raw: &RawNode) -> Option<Assessment> {
    reply
        .get("assessment")
        .cloned()
        .filter(|v| !v.is_null())
        .map(|value| {
            serde_json::from_value(value).unwrap_or_else(|_| {
                Assessment::unknown(
                    raw.rect_css.center(),
                    raw.rect_css,
                    "assessment_unavailable",
                )
            })
        })
}

fn pipe_resolve_raw(
    id: &str,
    assess: bool,
    samples: Option<u32>,
) -> Result<RawSnapshot, HandsError> {
    let req = resolve_request(id, last_pipe_detail(), assess, samples);
    let reply = pipe_request(&req).map_err(|err| {
        let text = err.to_string();
        if text.contains("client timed out after") {
            HandsError::Chrome(format!("Chrome resolve timed out for {id}: {err}"))
        } else {
            HandsError::Chrome(format!(
                "Chrome host is not connected (pipe); cannot resolve {id}: {err}"
            ))
        }
    })?;
    if reply.get("error").is_some()
        && reply.get("elements").is_none()
        && reply.get("rectCss").is_none()
    {
        return Err(HandsError::Chrome(format!(
            "Chrome host/fixture did not resolve {id}"
        )));
    }
    if reply.get("rectCss").is_some() {
        let raw: RawNode = serde_json::from_value(reply.clone())
            .map_err(|err| HandsError::Chrome(format!("Chrome resolve payload for {id}: {err}")))?;
        let metrics: ChromeMetrics =
            serde_json::from_value(reply.get("metrics").cloned().ok_or_else(|| {
                HandsError::Chrome(format!("Chrome resolve {id} missing metrics"))
            })?)
            .map_err(|err| HandsError::Chrome(format!("Chrome resolve metrics: {err}")))?;
        let assessment = assessment_from_reply(&reply, &raw);
        return Ok(RawSnapshot {
            url: reply
                .get("url")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            title: reply
                .get("title")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            main_text: None,
            elements: vec![raw],
            cards: Vec::new(),
            metrics,
            result_count: None,
            local_matches: None,
            empty_state: None,
            zip: None,
            radius: None,
            cards_walked: None,
            assessment,
            assess_v: reply
                .get("assess_v")
                .and_then(|v| v.as_u64())
                .and_then(|n| u32::try_from(n).ok()),
            ..RawSnapshot::default()
        });
    }
    let parsed: RawSnapshot = serde_json::from_value(reply.clone())
        .map_err(|err| HandsError::Chrome(format!("Chrome resolve payload for {id}: {err}")))?;
    Ok(parsed)
}

fn parse_host_reply(
    reply: &serde_json::Value,
    hwnd: Option<isize>,
) -> Result<ChromeMap, HandsError> {
    if reply.get("error").is_some() && reply.get("elements").is_none() {
        return Err(HandsError::Chrome(format!(
            "Chrome host error: {}",
            reply["error"]
        )));
    }
    let raw: RawSnapshot = serde_json::from_value(reply.clone())
        .map_err(|err| HandsError::Chrome(format!("Chrome snapshot payload: {err}")))?;
    let obs = make_observe_id(&raw, hwnd, raw.elements.len());
    Ok(map_from_raw(raw, hwnd, false, obs))
}

fn pipe_request(req: &serde_json::Value) -> Result<serde_json::Value, HandsError> {
    let deadline = Instant::now() + Duration::from_millis(u64::from(CLIENT_TIMEOUT_MS));
    let name = pipe_name();
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let wait_ms = remaining_client_ms(deadline)?;
    let ready = unsafe { WaitNamedPipeW(PCWSTR(wide.as_ptr()), wait_ms) };
    if !ready.as_bool() {
        return Err(HandsError::Chrome(format!(
            "Chrome host is not connected (no pipe {name} within {CLIENT_TIMEOUT_MS} ms); chr: unavailable"
        )));
    }
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            GENERIC_READ.0 | GENERIC_WRITE.0,
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
            None,
        )
    }
    .map_err(|err| {
        HandsError::Chrome(format!(
            "Chrome host pipe connect failed ({name}): {err}; chr: unavailable"
        ))
    })?;
    let result = exchange_pipe_deadline(handle, req, deadline);
    let _ = unsafe { CloseHandle(handle) };
    result
}

/// Point-mode `assess` reply: the topmost box plus the page identity echoed for a staleness
/// check. A closed shadow/iframe answer is still a successful query.
#[derive(Debug, Clone)]
pub struct AssessReply {
    pub at_point: AtPoint,
    pub css: Option<CssPoint>,
    pub identity: PageIdentity,
    /// Top point-assess hit an iframe; the extension named its owning frame and local point.
    pub iframe_frame: Option<u32>,
    pub local_css: Option<CssPoint>,
    /// Child frame echoed by a frame-scoped assess.
    pub frame: Option<u32>,
    /// Fresh top metrics and chain carried beside a frame-scoped assess.
    pub top_metrics: Option<ChromeMetrics>,
    pub frame_chain: Vec<crate::frames::FrameLink>,
}

/// Distinguishes a real host/transport failure from a malformed/unsupported reply.
#[derive(Debug, Clone)]
pub enum AssessError {
    HostDown(String),
    Timeout(String),
    Unavailable(String),
}

impl AssessError {
    pub fn message(&self) -> &str {
        match self {
            Self::HostDown(message) | Self::Timeout(message) | Self::Unavailable(message) => {
                message
            }
        }
    }
}

#[cfg(test)]
static ASSESS_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
pub(crate) fn assess_calls() -> usize {
    ASSESS_CALLS.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
thread_local! {
    static ASSESS_SEQUENCE: std::cell::RefCell<Vec<Result<AssessReply, AssessError>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
pub(crate) fn set_assess_sequence(sequence: Vec<Result<AssessReply, AssessError>>) {
    ASSESS_SEQUENCE.with(|c| *c.borrow_mut() = sequence);
}

#[cfg(test)]
pub(crate) fn reset_assess_calls() {
    ASSESS_CALLS.store(0, std::sync::atomic::Ordering::Relaxed);
}

/// `{op:"assess", css, mode:"point", samples}`. Only called after the snapshot advertises
/// `assess_v >= 1`; an old service worker would silently drop the op and stall the host.
pub fn assess_at(css: CssPoint, samples: u32) -> Result<AssessReply, AssessError> {
    assess_with_request(serde_json::json!({
        "op": "assess",
        "css": {"x": css.x, "y": css.y},
        "mode": "point",
        "samples": samples,
    }))
}

/// Explicit `{tab, frameId}` point assess for a linked frame (0131 DoD-6). The SW routes to
/// the child, reads fresh top metrics and chain, and returns the child's local assessment.
pub fn assess_at_frame(
    tab: u32,
    frame: u32,
    css: CssPoint,
    samples: u32,
) -> Result<AssessReply, AssessError> {
    assess_with_request(serde_json::json!({
        "op": "assess",
        "tab": tab,
        "frame": frame,
        "css": {"x": css.x, "y": css.y},
        "mode": "point",
        "samples": samples,
    }))
}

fn assess_with_request(req: serde_json::Value) -> Result<AssessReply, AssessError> {
    #[cfg(test)]
    {
        ASSESS_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let next = ASSESS_SEQUENCE.with(|c| {
            let mut sequence = c.borrow_mut();
            if sequence.is_empty() {
                None
            } else {
                Some(sequence.remove(0))
            }
        });
        if let Some(result) = next {
            return result;
        }
    }
    let reply = pipe_request(&req).map_err(|err| {
        let text = err.to_string();
        if text.contains("client timed out after") {
            AssessError::Timeout(text)
        } else if text.contains("not connected") || text.contains("connect failed") {
            AssessError::HostDown(text)
        } else {
            AssessError::Unavailable(text)
        }
    })?;
    if let Some(err) = reply.get("error") {
        return Err(AssessError::Unavailable(format!(
            "Chrome assess error: {err}"
        )));
    }
    let at_point: AtPoint = serde_json::from_value(
        reply
            .get("at_point")
            .cloned()
            .ok_or_else(|| HandsError::Chrome("Chrome assess reply has no at_point".into()))
            .map_err(|err| AssessError::Unavailable(err.to_string()))?,
    )
    .map_err(|err| AssessError::Unavailable(format!("Chrome assess at_point: {err}")))?;
    let css = reply
        .get("css")
        .cloned()
        .filter(|v| !v.is_null())
        .map(serde_json::from_value)
        .transpose()
        .map_err(|err| AssessError::Unavailable(format!("Chrome assess css echo: {err}")))?;
    let metrics: ChromeMetrics = reply
        .get("metrics")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|err| AssessError::Unavailable(format!("Chrome assess metrics: {err}")))?
        .unwrap_or_default();
    let mut identity = metrics.identity();
    identity.url = reply
        .get("url")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    identity.title = reply
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    Ok(AssessReply {
        at_point,
        css,
        identity,
        iframe_frame: reply
            .get("iframe_frame")
            .and_then(|v| v.as_u64())
            .and_then(|n| u32::try_from(n).ok()),
        local_css: reply
            .get("local_css")
            .cloned()
            .filter(|v| !v.is_null())
            .map(serde_json::from_value)
            .transpose()
            .map_err(|err| AssessError::Unavailable(format!("Chrome assess local_css: {err}")))?,
        frame: reply
            .get("frame")
            .and_then(|v| v.as_u64())
            .and_then(|n| u32::try_from(n).ok()),
        top_metrics: reply
            .get("top")
            .and_then(|top| top.get("metrics"))
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|err| AssessError::Unavailable(format!("Chrome assess top: {err}")))?,
        frame_chain: reply
            .get("chain")
            .cloned()
            .filter(|v| !v.is_null())
            .map(serde_json::from_value)
            .transpose()
            .map_err(|err| AssessError::Unavailable(format!("Chrome assess chain: {err}")))?
            .unwrap_or_default(),
    })
}

fn remaining_client_ms(deadline: Instant) -> Result<u32, HandsError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(client_timeout());
    }
    Ok(u32::try_from(remaining.as_millis()).unwrap_or(u32::MAX))
}

#[cfg(test)]
pub(crate) struct EnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    prev_snap: Option<std::ffi::OsString>,
    prev_pipe: Option<std::ffi::OsString>,
}

#[cfg(test)]
impl EnvGuard {
    pub fn lock() -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        Self {
            prev_snap: std::env::var_os(SNAPSHOT_ENV),
            prev_pipe: std::env::var_os(PIPE_ENV),
            _lock: lock,
        }
    }

    pub fn set_snapshot(&self, path: Option<&std::path::Path>) {
        match path {
            Some(p) => unsafe { std::env::set_var(SNAPSHOT_ENV, p) },
            None => unsafe { std::env::remove_var(SNAPSHOT_ENV) },
        }
    }

    pub fn set_pipe(&self, name: Option<&str>) {
        match name {
            Some(n) => unsafe { std::env::set_var(PIPE_ENV, n) },
            None => unsafe { std::env::remove_var(PIPE_ENV) },
        }
    }

    pub fn fixture_path() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/chrome-snapshot.json")
    }
}

#[cfg(test)]
impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prev_snap {
            Some(v) => unsafe { std::env::set_var(SNAPSHOT_ENV, v) },
            None => unsafe { std::env::remove_var(SNAPSHOT_ENV) },
        }
        match &self.prev_pipe {
            Some(v) => unsafe { std::env::set_var(PIPE_ENV, v) },
            None => unsafe { std::env::remove_var(PIPE_ENV) },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_metrics() -> ChromeMetrics {
        ChromeMetrics {
            screen_x: 100.0,
            screen_y: 50.0,
            inner_width: 1280.0,
            inner_height: 720.0,
            device_pixel_ratio: 1.0,
            ..ChromeMetrics::default()
        }
    }

    fn fixture_rect() -> CssRect {
        CssRect {
            left: 10.0,
            top: 20.0,
            width: 200.0,
            height: 32.0,
        }
    }

    fn fixture_inputs(metrics: &ChromeMetrics, anchor: Rect) -> MapInputs {
        MapInputs {
            hwnd: Some(0x1234),
            window_rect: Some(Rect {
                x: -1920,
                y: -200,
                w: 8000,
                h: 6000,
            }),
            dpi: Some((96.0 * metrics.device_pixel_ratio).round() as u32),
            virtual_screen: Some(Rect {
                x: -1920,
                y: -200,
                w: 8000,
                h: 6000,
            }),
            page: metrics.page(),
            render_widgets: vec![anchor],
            uia_document: None,
            monitors: Vec::new(),
            observe_id: Some("f3a9c1d2e4b5a607".into()),
            page_title: None,
            fixture: false,
            identity: crate::point::WindowIdentity::Confirmed,
            fixture_origin: None,
        }
    }

    fn valid_map(metrics: &ChromeMetrics, anchor: Rect) -> ViewportMap {
        build_map(fixture_inputs(metrics, anchor))
    }

    #[test]
    fn geometry_viewport_host_1_25() {
        // Synthetic render-widget anchor (100,80,1500,1125) at dpr 1.25.
        let metrics = ChromeMetrics {
            inner_width: 1200.0,
            inner_height: 900.0,
            client_width: Some(1185.0),
            client_height: Some(900.0),
            device_pixel_ratio: 1.25,
            ..ChromeMetrics::default()
        };
        let map = valid_map(
            &metrics,
            Rect {
                x: 100,
                y: 80,
                w: 1500,
                h: 1125,
            },
        );
        assert!(map.report.valid, "{:?}", map.report.reason);
        let contract = contract_for(&map, CssPoint::new(200.0, 70.0));
        assert_eq!(
            contract.physical,
            Some(crate::point::PhysicalPoint { x: 350, y: 168 })
        );
        let rect = crate::point::map_rect(
            &contract,
            CssRect {
                left: 100.0,
                top: 50.0,
                width: 200.0,
                height: 40.0,
            },
        )
        .unwrap();
        assert_eq!(
            rect,
            Rect {
                x: 225,
                y: 143,
                w: 250,
                h: 50
            }
        );
        assert_eq!(rect.center(), (350, 168));
    }

    #[test]
    fn geometry_viewport_negative_origin() {
        let metrics = ChromeMetrics {
            inner_width: 1200.0,
            inner_height: 900.0,
            device_pixel_ratio: 1.0,
            ..ChromeMetrics::default()
        };
        let map = valid_map(
            &metrics,
            Rect {
                x: -1920,
                y: -200,
                w: 1200,
                h: 900,
            },
        );
        assert!(map.report.valid, "{:?}", map.report.reason);
        let contract = contract_for(&map, CssPoint::new(0.0, 0.0));
        assert_eq!(
            contract.physical,
            Some(crate::point::PhysicalPoint { x: -1920, y: -200 })
        );
    }

    #[test]
    fn geometry_skips_unknown_anchor() {
        let metrics = ChromeMetrics {
            inner_width: 1200.0,
            inner_height: 900.0,
            device_pixel_ratio: 1.0,
            ..ChromeMetrics::default()
        };
        let mut inputs = fixture_inputs(
            &metrics,
            Rect {
                x: 0,
                y: 0,
                w: 1200,
                h: 900,
            },
        );
        inputs.render_widgets = Vec::new();
        inputs.uia_document = None;
        let map = build_map(inputs);
        assert!(!map.report.valid);
        assert!(map.report.reason.unwrap().starts_with("no_viewport_anchor"));
    }

    #[test]
    fn metrics_new_fields_optional() {
        let old: ChromeMetrics = serde_json::from_str(
            r#"{"screenX":0,"screenY":0,"outerWidth":1280,"outerHeight":800,"innerWidth":1280,"innerHeight":720,"devicePixelRatio":1.0}"#,
        )
        .expect("old seven fields");
        assert!(old.client_width.is_none());
        assert!(old.visual_viewport_scale.is_none());
        let new: ChromeMetrics = serde_json::from_str(
            r#"{"screenX":0,"screenY":0,"outerWidth":1280,"outerHeight":800,"innerWidth":1280,"innerHeight":720,"devicePixelRatio":1.25,"clientWidth":1265,"clientHeight":720,"visualViewportWidth":1265,"visualViewportHeight":720,"visualViewportScale":1,"visualViewportOffsetLeft":0,"visualViewportOffsetTop":0,"screenAvailLeft":0,"screenAvailTop":0,"screenWidth":1920,"screenHeight":1080}"#,
        )
        .expect("new fields");
        assert_eq!(new.client_width, Some(1265.0));
        assert_eq!(new.screen_width, Some(1920.0));
        assert_eq!(new.device_pixel_ratio, 1.25);
    }

    #[test]
    fn chrome_hint_for_observe_table() {
        let hint = chrome_hint_for_observe(false, Some(CHROME_HINT_HOST_DOWN.into()));
        assert_eq!(hint.as_deref(), Some(CHROME_HINT_NOT_CHROME_TAB));
        assert!(
            !hint.as_deref().unwrap_or("").contains("native-host-doctor"),
            "non-Chrome walk must not doctor: {hint:?}"
        );
        assert_eq!(
            chrome_hint_for_observe(false, None).as_deref(),
            Some(CHROME_HINT_NOT_CHROME_TAB)
        );
        assert_eq!(
            chrome_hint_for_observe(true, Some(CHROME_HINT_TIMEOUT.into())).as_deref(),
            Some(CHROME_HINT_TIMEOUT)
        );
        assert_eq!(chrome_hint_for_observe(true, None), None);
    }

    #[test]
    fn fixture_mode_is_listing_only() {
        let g = EnvGuard::lock();
        g.set_snapshot(Some(&EnvGuard::fixture_path()));
        g.set_pipe(None);
        let map = try_snapshot(Detail::Default, None)
            .into_map()
            .expect("fixture");
        let transform = map.transform.expect("fixture transform block");
        assert!(!transform.valid);
        assert_eq!(transform.method, crate::point::TransformMethod::Fixture);
        assert_eq!(transform.reason.as_deref(), Some("fixture_snapshot"));
        // Element/card rects still list through the unbounded diagnostic map.
        assert!(!map.elements.is_empty());
    }

    #[test]
    fn fixture_geometry_dpr_1_25_still_lists() {
        let metrics: ChromeMetrics = serde_json::from_value(serde_json::json!({
            "screenX": 100.0,
            "screenY": 50.0,
            "outerWidth": 1280.0,
            "outerHeight": 800.0,
            "innerWidth": 1280.0,
            "innerHeight": 720.0,
            "devicePixelRatio": 1.25
        }))
        .expect("fixture metrics");
        let map = fixture_map(
            &RawSnapshot {
                url: None,
                title: None,
                main_text: None,
                elements: Vec::new(),
                cards: Vec::new(),
                metrics,
                result_count: None,
                local_matches: None,
                empty_state: None,
                zip: None,
                radius: None,
                cards_walked: None,
                assessment: None,
                assess_v: None,
                ..RawSnapshot::default()
            },
            None,
            "f3a9c1d2e4b5a607".into(),
        );
        let rect = map_rect_unbounded(&map, fixture_rect()).unwrap();
        assert_eq!(
            rect,
            Rect {
                x: 138,
                y: 188,
                w: 250,
                h: 40
            }
        );
    }

    #[test]
    fn geometry_skips_bad_dpr_and_zero_size() {
        let mut metrics = fixture_metrics();
        metrics.device_pixel_ratio = 0.0;
        let mut inputs = fixture_inputs(
            &metrics,
            Rect {
                x: 0,
                y: 0,
                w: 1280,
                h: 720,
            },
        );
        let map = build_map(inputs.clone());
        assert!(!map.report.valid);
        metrics.device_pixel_ratio = f64::NAN;
        inputs.page = metrics.page();
        assert!(!build_map(inputs).report.valid);
    }

    #[test]
    fn fixture_snapshot_sets_chr_and_url() {
        let g = EnvGuard::lock();
        g.set_snapshot(Some(&EnvGuard::fixture_path()));
        g.set_pipe(None);
        let map = try_snapshot(Detail::Default, None)
            .into_map()
            .expect("fixture");
        assert_eq!(map.url.as_deref(), Some("https://cars.com/search"));
        assert_eq!(map.title, "Cars.com");
        assert_eq!(map.elements[0].id, "chr:0");
        assert_eq!(
            map.elements[0].rect,
            Rect {
                x: 110,
                y: 150,
                w: 200,
                h: 32
            }
        );
        assert!(map.elements.iter().any(|e| e.text.is_none()));
        assert_eq!(map.cards.len(), 1);
        assert_eq!(map.cards[0].price, "$12,345");
        assert_eq!(map.cards[0].miles.as_deref(), Some("32,145 mi"));
        assert_eq!(map.cards[0].dealer.as_deref(), Some("Capital Toyota"));
        assert_eq!(map.cards[0].distance.as_deref(), Some("12 mi away"));
        assert!(map.cards[0].miles.is_some());
        assert!(map.cards[0].dealer.is_some());
        assert!(map.cards[0].distance.is_some());
        assert!(!map.main_text.to_ascii_lowercase().contains("hunter"));
    }

    #[test]
    fn missing_or_invalid_fixture_is_absent_for_observe() {
        let g = EnvGuard::lock();
        g.set_snapshot(Some(std::path::Path::new(
            r"C:\dev\Helping-Hands\hands\tests\fixtures\missing-chrome.json",
        )));
        assert!(matches!(
            try_snapshot(Detail::Default, None),
            SnapshotOutcome::HostDown
        ));
        let bad = std::env::temp_dir().join("hands-chrome-bad.json");
        fs::write(&bad, "{not json").unwrap();
        g.set_snapshot(Some(&bad));
        assert!(matches!(
            try_snapshot(Detail::Default, None),
            SnapshotOutcome::HostDown
        ));
        let _ = fs::remove_file(bad);
    }

    #[test]
    fn rust_caps_cards_at_eight_and_drops_javascript_href() {
        let mut cards = String::new();
        for i in 0..9 {
            cards.push_str(&format!(
                r#"{{"title":"car {i}","price":"$1.0{i}","href":"https://cars.com/{i}","rectCss":{{"left":1,"top":1,"width":10,"height":10}},"miles":"32,145 mi","dealer":"Capital Toyota","distance":"12 mi away"}},"#
            ));
        }
        cards.push_str(
            r#"{"title":"js","price":"$0.00","href":"javascript:alert(1)","rectCss":{"left":1,"top":1,"width":10,"height":10},"miles":"12,000 mi","dealer":"JS Dealer","distance":"3 mi away"}"#,
        );
        let json = format!(
            r#"{{"url":"https://cars.com/search","title":"T","main_text":"x","metrics":{{"screenX":0,"screenY":0,"outerWidth":100,"outerHeight":100,"innerWidth":100,"innerHeight":100,"devicePixelRatio":1}},"elements":[],"cards":[{cards}]}}"#
        );
        let map = parse_snapshot_bytes(json.as_bytes(), None).expect("parse");
        assert_eq!(map.cards.len(), 8);
        assert!(
            map.cards
                .iter()
                .all(|c| !c.href.to_ascii_lowercase().starts_with("javascript:"))
        );
        assert_eq!(map.cards[0].title, "car 0");
        assert_eq!(map.cards[7].title, "car 7");
        assert_eq!(map.cards[0].miles.as_deref(), Some("32,145 mi"));
        assert_eq!(map.cards[0].dealer.as_deref(), Some("Capital Toyota"));
        assert_eq!(map.cards[0].distance.as_deref(), Some("12 mi away"));
        assert!(
            map.cards
                .iter()
                .all(|c| c.miles.is_some() && c.dealer.is_some() && c.distance.is_some())
        );
        assert_eq!(map.cards_walked, 9);
        assert_eq!(map.listing.cards_walked, 9);
    }

    #[test]
    fn empty_state_snapshot_enriches_listing_meta() {
        // Top-level listing keys (not a nested `listing` object).
        let json = r#"{
            "url":"https://www.cars.com/shopping/results/?zip=32309&maximum_distance=50",
            "title":"Cars.com",
            "main_text":"Nothing fits those filters. Try a larger radius.",
            "metrics":{"screenX":0,"screenY":0,"outerWidth":100,"outerHeight":100,"innerWidth":100,"innerHeight":100,"devicePixelRatio":1},
            "elements":[],
            "cards":[]
        }"#;
        let map = parse_snapshot_bytes(json.as_bytes(), None).expect("parse");
        assert!(map.cards.is_empty());
        let extract = crate::extract::extract_fused(
            "UIA",
            &[],
            map.url.as_deref(),
            &map.title,
            &map.main_text,
            map.cards,
            map.listing,
        );
        assert!(
            extract
                .empty_state
                .as_deref()
                .unwrap()
                .to_ascii_lowercase()
                .contains("nothing fits those filters")
        );
        assert_eq!(extract.zip.as_deref(), Some("32309"));
        assert_eq!(extract.radius.as_deref(), Some("50 mi"));
        assert!(extract.cards.is_empty());
        let json_js = r#"{
            "url":"https://www.cars.com/shopping/results/?zip=32309&maximum_distance=50",
            "title":"Cars.com",
            "main_text":"Sorry, nothing fits those filters. Try a larger radius.",
            "empty_state":"Sorry, nothing fits those filters.",
            "metrics":{"screenX":0,"screenY":0,"outerWidth":100,"outerHeight":100,"innerWidth":100,"innerHeight":100,"devicePixelRatio":1},
            "elements":[],
            "cards":[]
        }"#;
        let map_js = parse_snapshot_bytes(json_js.as_bytes(), None).expect("parse");
        let extract_js = crate::extract::extract_fused(
            "UIA",
            &[],
            map_js.url.as_deref(),
            &map_js.title,
            &map_js.main_text,
            map_js.cards,
            map_js.listing,
        );
        assert_eq!(
            extract_js.empty_state.as_deref(),
            Some("Sorry, nothing fits those filters.")
        );
    }

    #[test]
    fn resolve_request_includes_dom_detail() {
        let g = EnvGuard::lock();
        g.set_snapshot(None);
        store_last_pipe_detail(Detail::Default);
        let default = resolve_request("chr:0", Detail::Default, false, None);
        assert_eq!(default["op"], "resolve");
        assert_eq!(default["id"], "chr:0");
        assert!(default.get("detail").is_none());
        assert!(default.get("assess").is_none());
        let dom = resolve_request("chr:0", Detail::Dom, false, None);
        assert_eq!(dom["detail"], "dom");
        let assessed = resolve_request("chr:0", Detail::Default, true, Some(25));
        assert_eq!(assessed["assess"], true);
        assert_eq!(assessed["samples"], 25);
        store_last_pipe_detail(Detail::Dom);
        let replay = resolve_request("chr:1", last_pipe_detail(), false, None);
        assert_eq!(replay["detail"], "dom");
        store_last_pipe_detail(Detail::Default);
        let after_reset = resolve_request("chr:0", last_pipe_detail(), false, None);
        assert!(after_reset.get("detail").is_none());
    }

    #[test]
    fn fixture_resolve_chr0_center() {
        let g = EnvGuard::lock();
        g.set_snapshot(Some(&EnvGuard::fixture_path()));
        let hit = try_resolve("chr:0", None).unwrap();
        // Fixture mode is listing-only: the CSS rect lists, but the contract refuses.
        assert_eq!(
            hit.css_rect.center(),
            crate::point::CssPoint::new(110.0, 36.0)
        );
        assert!(!hit.contract.transform.valid);
        assert_eq!(
            hit.contract.transform.reason.as_deref(),
            Some("fixture_snapshot")
        );
        assert!(hit.contract.physical.is_none());
        assert_eq!(hit.role, "Edit");
    }

    #[test]
    fn missing_fixture_resolve_is_tool_error() {
        let g = EnvGuard::lock();
        g.set_snapshot(Some(std::path::Path::new(
            r"C:\dev\Helping-Hands\hands\tests\fixtures\missing-chrome.json",
        )));
        let err = try_resolve("chr:0", None).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("chr:"), "{msg}");
        assert!(msg.contains("fixture") || msg.contains("host"), "{msg}");
        assert!(
            !msg.to_ascii_lowercase().contains("unknown prefix"),
            "{msg}"
        );
    }

    #[test]
    fn malformed_fixture_resolve_is_tool_error() {
        let g = EnvGuard::lock();
        let bad = std::env::temp_dir().join("hands-chrome-malformed.json");
        fs::write(&bad, "{\"url\":1}").unwrap();
        g.set_snapshot(Some(&bad));
        let err = try_resolve("chr:0", None).unwrap_err();
        let msg = err.to_string();
        let _ = fs::remove_file(bad);
        assert!(msg.contains("chr:") || msg.contains("fixture"), "{msg}");
        assert!(
            !msg.to_ascii_lowercase().contains("unknown prefix"),
            "{msg}"
        );
    }

    #[test]
    fn silent_host_pipe_times_out_within_budget() {
        use windows::Win32::Storage::FileSystem::{
            FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
        };
        use windows::Win32::System::Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
            PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
        };

        let name = format!(
            r"\\.\pipe\hands-chrome-silent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let server_name = name.clone();
        let server = std::thread::spawn(move || {
            let wide: Vec<u16> = server_name
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let handle = unsafe {
                CreateNamedPipeW(
                    PCWSTR(wide.as_ptr()),
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    1,
                    65_536,
                    65_536,
                    0,
                    None,
                )
            };
            if handle.is_invalid() {
                return;
            }
            let _ = ready_tx.send(());
            let _ = unsafe { ConnectNamedPipe(handle, None) };
            let _ = done_rx.recv();
            let _ = unsafe { DisconnectNamedPipe(handle) };
            let _ = unsafe { CloseHandle(handle) };
        });
        ready_rx.recv().expect("server created pipe");

        let g = EnvGuard::lock();
        g.set_snapshot(None);
        g.set_pipe(Some(&name));
        let started = Instant::now();
        let err = try_resolve("chr:0", None).unwrap_err();
        let elapsed = started.elapsed();
        drop(g);
        let _ = done_tx.send(());
        let _ = server.join();

        let msg = err.to_string();
        assert!(
            elapsed < Duration::from_millis(1_500),
            "client hung for {elapsed:?}: {msg}"
        );
        assert!(
            elapsed >= Duration::from_millis(50),
            "timeout too fast ({elapsed:?}): {msg}"
        );
        assert!(
            msg.contains("400") || msg.to_ascii_lowercase().contains("timed out"),
            "{msg}"
        );
        assert!(
            !msg.to_ascii_lowercase().contains("unknown prefix"),
            "{msg}"
        );
    }

    #[test]
    fn no_pipe_resolve_is_tool_error() {
        let g = EnvGuard::lock();
        g.set_snapshot(None);
        g.set_pipe(Some(r"\\.\pipe\hands-chrome-absent-0005-test"));
        let err = try_resolve("chr:0", None).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("chr:") || msg.contains("host"), "{msg}");
        assert!(
            !msg.to_ascii_lowercase().contains("unknown prefix"),
            "{msg}"
        );
    }

    #[test]
    fn git_ls_files_has_no_planning_paths() {
        let out = std::process::Command::new("git")
            .args(["ls-files"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .expect("git ls-files");
        let text = String::from_utf8_lossy(&out.stdout);
        for needle in ["conductor/", "planner.md", "docs/adr", "CONTEXT.md"] {
            assert!(
                !text.lines().any(|l| l.contains(needle)),
                "planning path leaked: {needle}"
            );
        }
    }

    #[test]
    fn extension_js_forbids_scrape_and_page_mutation() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("extension");
        let needles = [
            "setInterval",
            "MutationObserver",
            "eval(",
            "eval (",
            ".click(",
            "dispatchEvent",
            "innerHTML=",
            "innerHTML =",
            "chrome.scripting",
            "chrome.alarms",
            "fetch(",
            "fetch (",
            "XMLHttpRequest",
            "CarsWeb.SearchController",
        ];
        for name in ["content.js", "sw.js"] {
            let text = fs::read_to_string(root.join(name)).unwrap();
            for needle in needles {
                assert!(!text.contains(needle), "{name} must not contain {needle}");
            }
        }
    }

    #[test]
    fn sw_js_keeps_native_port_and_reconnects() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("extension");
        let text = fs::read_to_string(root.join("sw.js")).unwrap();
        assert!(
            text.contains("hostPort"),
            "sw.js must keep a global native port so MV3 does not drop connectNative"
        );
        assert!(
            text.contains("scheduleReconnect"),
            "sw.js must reconnect after onDisconnect"
        );
        assert!(
            text.contains("setTimeout"),
            "reconnect must use setTimeout (setInterval is forbidden)"
        );
        assert!(
            text.contains("onStartup") && text.contains("onInstalled"),
            "sw.js must reconnect on Chrome startup and extension reload"
        );
        assert!(
            text.contains("windowType: \"normal\"") || text.contains("windowType:\"normal\""),
            "no-tab lastFocusedWindow (DevTools) must fall back to a normal window"
        );
        assert!(
            text.contains("getPlatformInfo") && text.contains("keepWorker"),
            "MV3 must ping getPlatformInfo so the worker does not go Inactive"
        );
        let disconnect = text.find("onDisconnect").expect("onDisconnect");
        let after = &text[disconnect..];
        let end = after
            .find("function scheduleReconnect")
            .unwrap_or(after.len());
        assert!(
            after[..end].contains("chrome.runtime.lastError"),
            "onDisconnect must read lastError or Chrome shows Unchecked Native host has exited"
        );
    }

    #[test]
    fn content_js_assigns_chr_from_walk_index() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("extension");
        let text = fs::read_to_string(root.join("content.js")).unwrap();
        assert!(
            text.contains(r#""chr:" + String(index)"#),
            "content.js must assign chr: from the shared walk emit index"
        );
        assert!(
            text.contains("function walk(") && text.contains("function collect("),
            "content.js snapshot and resolve must share walk/collect"
        );
        let resolve = text.find("function buildResolve(").expect("buildResolve");
        let resolve_end = text[resolve..]
            .find("function parseChrId(")
            .expect("parseChrId after buildResolve");
        assert!(
            text[resolve..resolve + resolve_end].contains("walk("),
            "buildResolve must re-run the shared walk"
        );
        assert!(
            text.contains("node.shadowRoot"),
            "composed walk must pierce open shadow roots only"
        );
        assert!(
            !text.contains("data-hands"),
            "content.js must not grow data-hands identity"
        );
        assert!(
            !text.contains("WeakMap"),
            "content.js must not grow WeakMap identity"
        );
    }

    #[test]
    fn content_js_spa_option_walk_and_gate() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("extension");
        let text = fs::read_to_string(root.join("content.js")).unwrap();
        let default_end = text.find("const DOM_SELECTOR").expect("DOM_SELECTOR");
        let default = &text[..default_end];
        assert!(
            default.contains(r#"'[role="option"]'"#),
            "DEFAULT_SELECTOR must include role=option"
        );
        assert!(
            default.contains(r#"'[role="menuitemcheckbox"]'"#),
            "DEFAULT_SELECTOR must include role=menuitemcheckbox"
        );
        assert!(
            default.contains(r#"'[role="menuitemradio"]'"#),
            "DEFAULT_SELECTOR must include role=menuitemradio"
        );
        assert!(
            default.contains(r#"'[role="treeitem"]'"#),
            "DEFAULT_SELECTOR must include role=treeitem"
        );
        assert!(
            !default.contains(r#"'[role="listbox"]'"#)
                && !default.contains(r#"'[role="menu"]'"#)
                && !default.contains("[popover]")
                && !default.contains(r#"'[role="popover"]'"#),
            "DEFAULT_SELECTOR must not walk listbox/menu/popover containers"
        );
        assert!(
            text.contains(r#"'[aria-hidden="true"], [hidden]'"#),
            "includeNode must exclude ancestor aria-hidden/hidden"
        );
        assert!(
            text.contains("function openRowGate(")
                && text.contains("aria-expanded")
                && text.contains("aria-controls")
                && text.contains(":popover-open"),
            "open-row gate must resolve controller aria-expanded / popover-open"
        );
        assert!(
            !text.contains("querySelectorAll('*')"),
            "composed walk must not scan querySelectorAll('*')"
        );
    }

    #[test]
    fn spa_autocomplete_fixture_parses_option_rows() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/chrome-spa-autocomplete.json");
        let bytes = fs::read(&path).expect("spa autocomplete fixture");
        let map = parse_snapshot_bytes(&bytes, None).expect("parse spa autocomplete");
        let options: Vec<_> = map
            .elements
            .iter()
            .filter(|el| el.role == "ListItem" || el.role == "TreeItem")
            .collect();
        assert!(
            options.len() >= 3,
            "fixture must carry open-list option rows"
        );
        assert!(
            options
                .iter()
                .any(|el| el.text.as_deref().is_some_and(|t| t.contains("MSFT"))),
            "option text must be usable (MSFT-class)"
        );
        assert!(
            options.iter().any(|el| el.id == "chr:22"),
            "chr:22 is the open-shadow option in emit order (source-lock story)"
        );
    }

    fn mixed_fixture_path() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/chrome-srp-mixed.json")
    }

    #[test]
    fn mixed_srp_packs_five_locals_and_walked_thirteen() {
        let bytes = fs::read(mixed_fixture_path()).expect("mixed fixture");
        let map = parse_snapshot_bytes(&bytes, None).expect("parse mixed");
        assert_eq!(map.cards.len(), 8);
        assert_eq!(map.cards_walked, 13);
        assert_eq!(map.listing.cards_walked, 13);
        let locals: Vec<_> = map
            .cards
            .iter()
            .filter(|c| c.kind.as_deref() == Some("local"))
            .collect();
        assert_eq!(locals.len(), 5);
        let expected = [
            (
                "2024 Toyota Camry SE",
                "$19,999",
                "32,145 mi",
                "Capital Toyota",
                "12 mi away",
                "https://cars.com/vehicledetail/local-se",
            ),
            (
                "2024 Toyota Camry LE",
                "$18,500",
                "28,012 mi",
                "Tallahassee Toyota",
                "Tallahassee, FL (12 mi)",
                "https://cars.com/vehicledetail/local-le",
            ),
            (
                "2024 Toyota Camry XLE",
                "$21,200",
                "15,432 mi",
                "Gator Toyota",
                "(12 mi)",
                "https://cars.com/vehicledetail/local-xle",
            ),
            (
                "2024 Toyota Camry Nightshade",
                "$22,400",
                "41,200 mi",
                "Crown Toyota",
                "8 mi away",
                "https://cars.com/vehicledetail/local-night",
            ),
            (
                "2024 Toyota Camry Hybrid",
                "$24,800",
                "19,876 mi",
                "Capital Toyota",
                "5 mi away",
                "https://cars.com/vehicledetail/local-hybrid",
            ),
        ];
        for (title, price, miles, dealer, distance, href) in expected {
            let card = locals
                .iter()
                .find(|c| c.title == title)
                .unwrap_or_else(|| panic!("missing local {title}"));
            assert_eq!(card.price, price, "{title}");
            assert_eq!(card.miles.as_deref(), Some(miles), "{title}");
            assert_eq!(card.dealer.as_deref(), Some(dealer), "{title}");
            assert_eq!(card.distance.as_deref(), Some(distance), "{title}");
            assert_eq!(card.href, href, "{title}");
        }
        let ship = map
            .cards
            .iter()
            .find(|c| c.kind.as_deref() == Some("ship") && c.delivery.is_some())
            .expect("ship with delivery");
        let fee = ship.delivery.as_deref().unwrap();
        assert_ne!(ship.distance.as_deref(), Some(fee));
        assert!(
            map.cards
                .iter()
                .all(|c| c.kind.as_deref() != Some("recommended")),
            "pack must drop recommended behind locals+ships"
        );
        assert!(
            fs::read_to_string(mixed_fixture_path())
                .unwrap()
                .contains("\"kind\": \"recommended\"")
        );
        let civic = parse_snapshot_bytes(&fs::read(EnvGuard::fixture_path()).expect("civic"), None)
            .expect("civic");
        assert_eq!(civic.cards.len(), 1);
        assert_eq!(civic.cards[0].title, "2020 Honda Civic");
    }

    #[test]
    fn infer_kind_from_distance_packs_as_local() {
        let json = r#"{
            "url":"https://cars.com/search","title":"T","main_text":"x",
            "metrics":{"screenX":0,"screenY":0,"outerWidth":100,"outerHeight":100,"innerWidth":100,"innerHeight":100,"devicePixelRatio":1},
            "elements":[],
            "cards":[
                {"title":"ship a","price":"$1","href":"https://cars.com/s0","rectCss":{"left":1,"top":1,"width":10,"height":10},"kind":"ship","delivery":"Shipping $399"},
                {"title":"ship b","price":"$1","href":"https://cars.com/s1","rectCss":{"left":1,"top":1,"width":10,"height":10},"kind":"ship","delivery":"Shipping $399"},
                {"title":"inferred local","price":"$19,999","href":"https://cars.com/l0","rectCss":{"left":1,"top":1,"width":10,"height":10},"distance":"12 mi away"}
            ]
        }"#;
        let map = parse_snapshot_bytes(json.as_bytes(), None).expect("parse");
        assert_eq!(map.cards.len(), 3);
        assert_eq!(map.cards[0].title, "inferred local");
        assert_eq!(map.cards[0].kind.as_deref(), Some("local"));
        assert_eq!(map.cards_walked, 3);
    }

    #[test]
    fn old_sidecar_without_kind_delivery_deserializes() {
        let json = r#"{
            "url":"https://cars.com/search","title":"T","main_text":"x",
            "metrics":{"screenX":0,"screenY":0,"outerWidth":100,"outerHeight":100,"innerWidth":100,"innerHeight":100,"devicePixelRatio":1},
            "elements":[],
            "cards":[{"title":"civic","price":"$12,345","href":"https://cars.com/c","rectCss":{"left":1,"top":1,"width":10,"height":10}}]
        }"#;
        let map = parse_snapshot_bytes(json.as_bytes(), None).expect("parse");
        assert_eq!(map.cards.len(), 1);
        assert!(map.cards[0].kind.is_none());
        assert!(map.cards[0].delivery.is_none());
        let v = serde_json::to_value(&map.cards[0]).unwrap();
        assert!(v.get("kind").is_none());
        assert!(v.get("delivery").is_none());
    }

    #[test]
    fn content_js_extract_price_skips_monthly_and_drop_tokens() {
        let src = include_str!("../extension/content.js");
        let start = src
            .find("function extractPrice")
            .expect("function extractPrice");
        let rest = &src[start..];
        let end = rest
            .find("function prevWholeToken")
            .expect("function prevWholeToken");
        let slice = &rest[..end];
        assert!(
            slice.contains("/mo"),
            "extractPrice must skip /mo:\n{slice}"
        );
        assert!(
            slice.contains("drop") && slice.contains("est"),
            "extractPrice must whole-token skip drop/est:\n{slice}"
        );
        assert!(
            !slice.contains("substring") || slice.contains("toLowerCase"),
            "extractPrice slice present"
        );
    }

    #[test]
    fn content_js_card_dealer_junk_strips_shipping_and_deliver_to() {
        let src = include_str!("../extension/content.js");
        let start = src
            .find("function cardDealer")
            .expect("function cardDealer");
        let rest = &src[start..];
        let end = rest
            .find("function cardDistance")
            .expect("function cardDistance");
        let slice = &rest[..end];
        assert!(
            slice.contains("shipping") && slice.contains("deliver to"),
            "cardDealer must junk-strip shipping/deliver to:\n{slice}"
        );
        assert!(
            slice.contains("cardDistance("),
            "cardDealer must strip cardDistance phrase before leftover:\n{slice}"
        );
        assert!(
            !slice.contains("[A-Za-z .'-]*"),
            "cardDealer must not greedily swallow City, ST:\n{slice}"
        );
    }

    #[test]
    fn content_js_collect_cards_walks_24_packs_8() {
        let src = include_str!("../extension/content.js");
        let start = src
            .find("function collectCards")
            .expect("function collectCards");
        let rest = &src[start..];
        let end = rest
            .find("function itempropText")
            .expect("function itempropText");
        let slice = &rest[..end];
        assert!(
            slice.contains("CARD_WALK_CAP") && slice.contains("cards_walked"),
            "collectCards must walk/pack and set cards_walked:\n{slice}"
        );
        assert!(
            slice.contains("preferLocalCards") || slice.contains("CARD_CAP"),
            "collectCards must pack to emit cap:\n{slice}"
        );
    }

    #[test]
    fn classify_snapshot_err_table() {
        assert!(matches!(
            classify_snapshot_err(&HandsError::Chrome(
                "Chrome host client timed out after 400 ms; chr: unavailable".into()
            )),
            SnapshotOutcome::Timeout
        ));
        assert!(matches!(
            classify_snapshot_err(&HandsError::Chrome("Chrome host error: no-content".into())),
            SnapshotOutcome::Loading
        ));
        assert!(matches!(
            classify_snapshot_err(&HandsError::Chrome("Chrome host error: \"no-tab\"".into())),
            SnapshotOutcome::Loading
        ));
        assert!(matches!(
            classify_snapshot_err(&HandsError::Chrome("Chrome host error: empty".into())),
            SnapshotOutcome::Loading
        ));
        assert!(matches!(
            classify_snapshot_err(&HandsError::Chrome(
                "Chrome host is not connected (no pipe \\\\.\\pipe\\hands-chrome within 400 ms); chr: unavailable"
                    .into()
            )),
            SnapshotOutcome::HostDown
        ));
        assert_eq!(
            SnapshotOutcome::Map(Box::new(ChromeMap {
                frame_rows: Vec::new(),
                frames_transient: false,
                tab: None,
                url: None,
                title: String::new(),
                main_text: String::new(),
                elements: Vec::new(),
                cards: Vec::new(),
                listing: ListingMeta::default(),
                cards_walked: 0,
                transform: None,
                identity: None,
                snapshot: None,
            }))
            .chrome_hint(),
            None
        );
        assert_eq!(
            SnapshotOutcome::Loading.chrome_hint().as_deref(),
            Some(CHROME_HINT_LOADING)
        );
        assert_eq!(
            SnapshotOutcome::Timeout.chrome_hint().as_deref(),
            Some(CHROME_HINT_TIMEOUT)
        );
        assert_eq!(
            SnapshotOutcome::HostDown.chrome_hint().as_deref(),
            Some(CHROME_HINT_HOST_DOWN)
        );
        assert!(SnapshotOutcome::Loading.host_up());
        assert!(SnapshotOutcome::Timeout.host_up());
        assert!(!SnapshotOutcome::HostDown.host_up());
        assert_eq!(CLIENT_TIMEOUT_MS, 400);
    }

    fn hook_transform(_: &ChromeMetrics, _: Option<&str>, _: isize) -> MapInputs {
        let metrics = ChromeMetrics {
            inner_width: 1200.0,
            inner_height: 900.0,
            client_width: Some(1185.0),
            client_height: Some(900.0),
            device_pixel_ratio: 1.25,
            ..ChromeMetrics::default()
        };
        valid_inputs(
            &metrics,
            Rect {
                x: 100,
                y: 80,
                w: 1500,
                h: 1125,
            },
        )
    }

    fn valid_inputs(metrics: &ChromeMetrics, anchor: Rect) -> MapInputs {
        fixture_inputs(metrics, anchor)
    }

    #[test]
    fn resolve_chr_keeps_css_and_contract() {
        set_geometry_hook(Some(hook_transform));
        let raw = RawSnapshot {
            url: Some("https://example.com/".into()),
            title: Some("Example".into()),
            main_text: None,
            elements: vec![RawNode {
                frame: None,
                idx: None,
                id: Some("chr:0".into()),
                role: Some("Button".into()),
                text: Some("Go".into()),
                rect_css: CssRect {
                    left: 100.0,
                    top: 50.0,
                    width: 200.0,
                    height: 40.0,
                },
                _href: None,
            }],
            cards: Vec::new(),
            metrics: ChromeMetrics {
                inner_width: 1200.0,
                inner_height: 900.0,
                client_width: Some(1185.0),
                client_height: Some(900.0),
                device_pixel_ratio: 1.25,
                ..ChromeMetrics::default()
            },
            result_count: None,
            local_matches: None,
            empty_state: None,
            zip: None,
            radius: None,
            cards_walked: None,
            assessment: None,
            assess_v: None,
            ..RawSnapshot::default()
        };
        let hit = hit_from_raw(raw, "chr:0", Some(0x1234), false).expect("resolve");
        set_geometry_hook(None);
        assert!(hit.contract.transform.valid);
        assert_eq!(hit.css_point, CssPoint::new(200.0, 70.0));
        assert_eq!(hit.css_rect.center(), hit.css_point);
        assert_eq!(hit.contract.css, Some(hit.css_point));
        let p = crate::point::map_point(&hit.contract, hit.css_point).unwrap();
        assert_eq!(hit.contract.physical, Some(p));
        assert_eq!(hit.rect.center(), (p.x, p.y));
        assert_eq!(hit.hwnd, Some(0x1234));
    }

    #[test]
    fn transform_fixtures_parse_old_and_new_metrics() {
        let new: RawSnapshot =
            serde_json::from_str(include_str!("../tests/fixtures/chrome-transform.json"))
                .expect("new metrics fixture");
        assert_eq!(new.metrics.client_width, Some(1185.0));
        assert_eq!(new.metrics.visual_viewport_scale, Some(1.0));
        let old: RawSnapshot = serde_json::from_str(include_str!(
            "../tests/fixtures/chrome-transform-old-metrics.json"
        ))
        .expect("old metrics fixture");
        assert!(old.metrics.client_width.is_none());
        assert_eq!(old.metrics.device_pixel_ratio, 1.25);
        assert_eq!(old.elements[0].rect_css.width, 200.0);
    }

    #[test]
    fn map_from_raw_keeps_off_screen_card() {
        // C13: ingest is unbounded; an off-screen card still lists.
        let raw = RawSnapshot {
            url: None,
            title: None,
            main_text: None,
            elements: Vec::new(),
            cards: vec![RawCard {
                title: "Card".into(),
                price: "$1".into(),
                href: "https://example.com/card".into(),
                rect_css: CssRect {
                    left: 10.0,
                    top: 11000.0,
                    width: 100.0,
                    height: 30.0,
                },
                miles: None,
                dealer: None,
                distance: None,
                listing_of: None,
                kind: String::new(),
                delivery: String::new(),
            }],
            metrics: fixture_metrics(),
            result_count: None,
            local_matches: None,
            empty_state: None,
            zip: None,
            radius: None,
            cards_walked: None,
            assessment: None,
            assess_v: None,
            ..RawSnapshot::default()
        };
        let map = map_from_raw(raw, None, true, "f3a9c1d2e4b5a607".into());
        assert_eq!(map.cards.len(), 1);
        assert!(map.cards[0].rect.y > 10_000);
    }

    thread_local! {
        static REMAP_ANCHOR: std::cell::Cell<Rect> =
            const { std::cell::Cell::new(Rect { x: 100, y: 80, w: 1500, h: 1125 }) };
        static REMAP_DPI: std::cell::Cell<u32> = const { std::cell::Cell::new(120) };
    }

    fn remap_hook(page: PageMetrics, hwnd: isize) -> MapInputs {
        MapInputs {
            hwnd: Some(hwnd),
            window_rect: Some(Rect {
                x: -10,
                y: -10,
                w: 4000,
                h: 3000,
            }),
            dpi: Some(REMAP_DPI.with(|c| c.get())),
            virtual_screen: Some(Rect {
                x: -1920,
                y: -200,
                w: 8000,
                h: 6000,
            }),
            page,
            render_widgets: vec![REMAP_ANCHOR.with(|c| c.get())],
            uia_document: None,
            monitors: Vec::new(),
            observe_id: Some("f3a9c1d2e4b5a607".into()),
            page_title: Some("Owner Web Access".into()),
            fixture: false,
            identity: crate::point::WindowIdentity::Confirmed,
            fixture_origin: None,
        }
    }

    fn valid_remap_map() -> ViewportMap {
        let metrics = ChromeMetrics {
            inner_width: 1200.0,
            inner_height: 900.0,
            device_pixel_ratio: 1.25,
            ..ChromeMetrics::default()
        };
        let mut inputs = fixture_inputs(&metrics, REMAP_ANCHOR.with(|c| c.get()));
        inputs.dpi = Some(120);
        inputs.page_title = Some("Owner Web Access".into());
        let map = build_map(inputs);
        assert!(map.report.valid, "{:?}", map.report.reason);
        map
    }

    #[test]
    fn resolve_chr_id_must_match_canonical_id() {
        let raw = RawSnapshot {
            url: None,
            title: None,
            main_text: None,
            elements: vec![RawNode {
                frame: None,
                idx: None,
                id: Some("chr:7".into()),
                role: Some("Button".into()),
                text: Some("Go".into()),
                rect_css: CssRect {
                    left: 10.0,
                    top: 10.0,
                    width: 20.0,
                    height: 20.0,
                },
                _href: None,
            }],
            cards: Vec::new(),
            metrics: fixture_metrics(),
            result_count: None,
            local_matches: None,
            empty_state: None,
            zip: None,
            radius: None,
            cards_walked: None,
            assessment: None,
            assess_v: None,
            ..RawSnapshot::default()
        };
        let err = hit_from_raw(raw.clone(), "chr:0", None, true).unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
        let mut unnamed = raw;
        unnamed.elements[0].id = None;
        let err = hit_from_raw(unnamed, "chr:0", None, true).unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[test]
    fn remap_chr_uses_recorded_page_title_not_caption() {
        let old = valid_remap_map();
        REMAP_ANCHOR.with(|c| {
            c.set(Rect {
                x: 100,
                y: 80,
                w: 1500,
                h: 1125,
            })
        });
        REMAP_DPI.with(|c| c.set(120));
        set_remap_geometry_hook(Some(remap_hook));

        let ok = remap_chr_with(
            &old,
            0x1234,
            Some("Owner Web Access - Google Chrome"),
            1,
            true,
        )
        .expect("same recorded page title must remap");
        assert!(ok.report.valid);
        assert_eq!(ok.page_title.as_deref(), Some("Owner Web Access"));

        let err =
            remap_chr_with(&old, 0x1234, Some("Other Page - Google Chrome"), 1, true).unwrap_err();
        assert!(err.starts_with("window_mismatch"), "{err}");

        let err = remap_chr_with(&old, 0x1234, None, 2, true).unwrap_err();
        assert!(err.starts_with("window_identity_ambiguous"), "{err}");

        // The pre-fix bug fed the caption in as the page title; that never matches.
        assert_eq!(
            identity_from(
                "Owner Web Access - Google Chrome",
                Some("Owner Web Access - Google Chrome"),
                1
            ),
            crate::point::WindowIdentity::Mismatch
        );
        // The recorded-title path does match the caption.
        assert_eq!(
            identity_from(
                "Owner Web Access",
                Some("Owner Web Access - Google Chrome"),
                1
            ),
            crate::point::WindowIdentity::Confirmed
        );

        set_remap_geometry_hook(None);
    }

    #[test]
    fn remap_chr_move_only_ok_and_size_or_dpi_change_stale() {
        let old = valid_remap_map();
        set_remap_geometry_hook(Some(remap_hook));

        REMAP_ANCHOR.with(|c| {
            c.set(Rect {
                x: 300,
                y: 200,
                w: 1500,
                h: 1125,
            })
        });
        REMAP_DPI.with(|c| c.set(120));
        let moved = remap_chr_with(
            &old,
            0x1234,
            Some("Owner Web Access - Google Chrome"),
            1,
            true,
        )
        .expect("move-only remap");
        assert_eq!(moved.anchor.map(|r| (r.x, r.y)), Some((300, 200)));

        REMAP_ANCHOR.with(|c| {
            c.set(Rect {
                x: 300,
                y: 200,
                w: 1400,
                h: 1125,
            })
        });
        let err = remap_chr_with(
            &old,
            0x1234,
            Some("Owner Web Access - Google Chrome"),
            1,
            true,
        )
        .unwrap_err();
        assert!(err.starts_with("stale_viewport"), "{err}");

        REMAP_ANCHOR.with(|c| {
            c.set(Rect {
                x: 300,
                y: 200,
                w: 1500,
                h: 1125,
            })
        });
        REMAP_DPI.with(|c| c.set(144));
        let err = remap_chr_with(
            &old,
            0x1234,
            Some("Owner Web Access - Google Chrome"),
            1,
            true,
        )
        .unwrap_err();
        assert!(err.starts_with("stale_viewport"), "{err}");

        REMAP_DPI.with(|c| c.set(120));
        set_remap_geometry_hook(None);
    }

    fn fg_popup() -> Option<isize> {
        Some(0x5678)
    }

    fn fg_target() -> Option<isize> {
        Some(0x1234)
    }

    fn all_windows_chrome(_: Option<isize>) -> bool {
        true
    }

    fn frame_ref() -> SnapshotRef {
        SnapshotRef::Chrome {
            tab: 12,
            frame: 3,
            doc: "9f3a".into(),
            snap: "a1b2c3d4".into(),
            idx: 4,
        }
    }

    fn frame_link_json(parent: u32, width: f64, height: f64) -> serde_json::Value {
        serde_json::json!({
            "parent": parent,
            "content_left": 40.0,
            "content_top": 60.0,
            "content_width": width,
            "content_height": height,
            "clip": [{"left": 40.0, "top": 60.0, "width": width, "height": height}],
            "transform_ok": true,
            "visible": true,
            "hit_ok": true,
            "onscreen": true,
            "viewport_w": 1920.0,
            "viewport_h": 1080.0
        })
    }

    fn top_metrics_json() -> serde_json::Value {
        serde_json::json!({
            "screenX": 0.0,
            "screenY": 0.0,
            "outerWidth": 1920.0,
            "outerHeight": 1080.0,
            "innerWidth": 1920.0,
            "innerHeight": 1080.0,
            "devicePixelRatio": 1.0,
            "clientWidth": 1920.0,
            "clientHeight": 1080.0
        })
    }

    fn frame_reply(r: &SnapshotRef, child_width: f64) -> serde_json::Value {
        let SnapshotRef::Chrome {
            tab,
            frame,
            doc,
            snap,
            idx,
        } = r
        else {
            unreachable!()
        };
        serde_json::json!({
            "id": r.to_string(),
            "role": "Button",
            "text": "Sign in",
            "rectCss": {"left": 10.0, "top": 20.0, "width": 100.0, "height": 40.0},
            "metrics": {"innerWidth": child_width, "innerHeight": 250.0, "devicePixelRatio": 1.0},
            "url": "https://widget.test/",
            "title": "Widget",
            "tab": tab,
            "frame": frame,
            "doc": doc,
            "snap": snap,
            "idx": idx,
            "active": true,
            "focused": true,
            "visibilityState": "visible",
            "assess_v": 1,
            "top": {"frame": 0, "metrics": top_metrics_json(), "url": "https://top.test/", "title": "Top"},
            "chain": [frame_link_json(0, 300.0, 250.0)]
        })
    }

    #[test]
    fn frame_resolve_uses_top_metrics_and_composes_once() {
        let r = frame_ref();
        let reply = frame_reply(&r, 300.0);
        let hit = hit_from_scoped_reply(&reply, &r, Some(0x1234), true, false).expect("frame hit");
        assert_eq!(hit.css_rect.left, 50.0);
        assert_eq!(hit.css_rect.top, 80.0);
        assert_eq!(hit.css_point.x, 100.0);
        assert_eq!(
            hit.map.as_ref().and_then(|m| m.page.inner_width),
            Some(1920.0),
            "the map is built from top metrics, never the 300x250 child viewport"
        );
        assert_eq!(
            hit.identity.as_ref().map(|i| i.state),
            Some(IdentityState::Exact)
        );
    }

    #[test]
    fn frame_assessment_composes_once_into_top_space() {
        let r = frame_ref();
        let mut reply = frame_reply(&r, 300.0);
        reply["assessment"] = serde_json::json!({
            "interactable": true,
            "selected_point": {"x": 60.0, "y": 40.0},
            "target_rect_css": {"left": 10.0, "top": 20.0, "width": 100.0, "height": 40.0},
            "visible_fraction": 1.0,
            "onscreen_fraction": 1.0,
            "samples": 25,
            "samples_hit": 25,
            "confidence": "high"
        });
        let hit = hit_from_scoped_reply(&reply, &r, Some(0x1234), true, true).expect("frame hit");
        let assessment = hit.assessment.expect("assessment");
        assert_eq!(assessment.selected_point, CssPoint::new(100.0, 100.0));
        assert_eq!(assessment.target_rect_css.left, 50.0);
        assert_eq!(assessment.target_rect_css.top, 80.0);
        assert!(
            crate::hittest::validate_assessment(&hit.contract, &assessment).is_ok(),
            "the composed assessment matches the composed contract"
        );
    }

    #[test]
    fn frame_resolve_refuses_a_clipped_sliver() {
        let r = frame_ref();
        let mut reply = frame_reply(&r, 300.0);
        // Child-local top 280 translates to 340; the 250-high content box clips it.
        reply["rectCss"] = serde_json::json!({
            "left": 10.0,
            "top": 280.0,
            "width": 100.0,
            "height": 40.0
        });
        let err = hit_from_scoped_reply(&reply, &r, Some(0x1234), true, false).unwrap_err();
        assert_eq!(err.reason.as_deref(), Some("frame_not_visible"));
        assert_eq!(err.state, IdentityState::Stale);
    }

    #[test]
    fn frame_resolve_refuses_size_mismatch_and_missing_chain() {
        let r = frame_ref();
        let mut reply = frame_reply(&r, 500.0);
        let err = hit_from_scoped_reply(&reply, &r, Some(0x1234), true, false).unwrap_err();
        assert_eq!(err.reason.as_deref(), Some("frame_geometry_unavailable"));

        reply = frame_reply(&r, 300.0);
        reply.as_object_mut().unwrap().remove("chain");
        let err = hit_from_scoped_reply(&reply, &r, Some(0x1234), true, false).unwrap_err();
        assert_eq!(err.reason.as_deref(), Some("frame_geometry_unavailable"));
    }

    #[test]
    fn frame_resolve_refuses_non_top_metrics() {
        let r = frame_ref();
        let mut reply = frame_reply(&r, 300.0);
        reply["top"]["frame"] = serde_json::json!(3);
        let err = hit_from_scoped_reply(&reply, &r, Some(0x1234), true, false).unwrap_err();
        assert_eq!(err.reason.as_deref(), Some("frame_geometry_unavailable"));
    }

    #[test]
    fn map_from_raw_composes_frame_nodes_and_quarantines_chr() {
        let raw = RawSnapshot {
            tab: Some(12),
            window: Some(45),
            frame: Some(0),
            doc: Some("9f3a".into()),
            snap: Some("a1b2c3d4".into()),
            retained: Some(10),
            stored: Some(true),
            metrics: serde_json::from_value(top_metrics_json()).expect("metrics"),
            elements: vec![RawNode {
                frame: Some(3),
                idx: Some(4),
                id: Some(frame_ref().to_string()),
                role: Some("Button".into()),
                text: Some("Sign in".into()),
                rect_css: CssRect {
                    left: 10.0,
                    top: 20.0,
                    width: 100.0,
                    height: 40.0,
                },
                _href: None,
            }],
            frames: vec![crate::frames::RawFrameLink {
                frame: Some(3),
                status: Some("ok".into()),
                origin: Some("https://widget.test".into()),
                doc: Some("cafe".into()),
                snap: Some("00112233".into()),
                retained: Some(6),
                url: Some("https://widget.test/login".into()),
                link: Some(crate::frames::FrameLink {
                    parent: Some(0),
                    content_left: 40.0,
                    content_top: 60.0,
                    content_width: 300.0,
                    content_height: 250.0,
                    clip: vec![crate::frames::ClipRect {
                        left: 40.0,
                        top: 60.0,
                        width: 300.0,
                        height: 250.0,
                    }],
                    transform_ok: true,
                    visible: true,
                    hit_ok: true,
                    onscreen: true,
                    viewport_w: 1920.0,
                    viewport_h: 1080.0,
                    ..crate::frames::FrameLink::default()
                }),
                ..crate::frames::RawFrameLink::default()
            }],
            ..RawSnapshot::default()
        };
        let map = map_from_raw(raw, Some(0x1234), true, "obs".into());
        assert_eq!(map.elements.len(), 1);
        assert_eq!(map.elements[0].frame, Some(3));
        assert_eq!(map.elements[0].id, frame_ref().to_string());
        let binding = map.snapshot.expect("binding");
        assert_eq!(binding.frames.len(), 1);
        assert_eq!(binding.frames[0].status, "ok");
        assert_eq!(binding.frames[0].doc, "cafe");
        assert_eq!(
            binding.frame_binding(3).map(|f| f.snap.as_str()),
            Some("00112233")
        );
    }

    #[test]
    fn map_from_raw_omits_a_frame_node_without_a_link() {
        let raw = RawSnapshot {
            tab: Some(12),
            window: Some(45),
            frame: Some(0),
            doc: Some("9f3a".into()),
            snap: Some("a1b2c3d4".into()),
            retained: Some(1),
            stored: Some(true),
            metrics: serde_json::from_value(top_metrics_json()).expect("metrics"),
            elements: vec![RawNode {
                frame: Some(3),
                idx: Some(0),
                id: Some(frame_ref().to_string()),
                role: Some("Button".into()),
                text: Some("Sign in".into()),
                rect_css: CssRect {
                    left: 10.0,
                    top: 20.0,
                    width: 100.0,
                    height: 40.0,
                },
                _href: None,
            }],
            ..RawSnapshot::default()
        };
        let map = map_from_raw(raw, Some(0x1234), true, "obs".into());
        assert!(map.elements.is_empty(), "no fake frame rect is emitted");
    }

    #[test]
    fn fully_clipped_child_elements_name_the_frame_not_visible() {
        let raw = RawSnapshot {
            tab: Some(12),
            window: Some(45),
            frame: Some(0),
            doc: Some("9f3a".into()),
            snap: Some("a1b2c3d4".into()),
            retained: Some(10),
            stored: Some(true),
            metrics: serde_json::from_value(top_metrics_json()).expect("metrics"),
            elements: vec![RawNode {
                frame: Some(3),
                idx: Some(0),
                id: Some(frame_ref().to_string()),
                role: Some("Button".into()),
                text: Some("Outside".into()),
                rect_css: CssRect {
                    left: 400.0,
                    top: 10.0,
                    width: 50.0,
                    height: 20.0,
                },
                _href: None,
            }],
            frames: vec![crate::frames::RawFrameLink {
                frame: Some(3),
                status: Some("ok".into()),
                doc: Some("cafe".into()),
                snap: Some("00112233".into()),
                link: Some(crate::frames::FrameLink {
                    parent: Some(0),
                    content_left: 40.0,
                    content_top: 60.0,
                    content_width: 300.0,
                    content_height: 250.0,
                    clip: vec![crate::frames::ClipRect {
                        left: 40.0,
                        top: 60.0,
                        width: 300.0,
                        height: 250.0,
                    }],
                    transform_ok: true,
                    visible: true,
                    hit_ok: true,
                    onscreen: true,
                    viewport_w: 1920.0,
                    viewport_h: 1080.0,
                    ..crate::frames::FrameLink::default()
                }),
                ..crate::frames::RawFrameLink::default()
            }],
            ..RawSnapshot::default()
        };
        let map = map_from_raw(raw, Some(0x1234), true, "obs".into());
        assert!(
            map.elements.is_empty(),
            "the fully clipped element is not clickable"
        );
        let binding = map.snapshot.expect("binding");
        assert_eq!(binding.frames[0].status, "unavailable");
        assert_eq!(
            binding.frames[0].reason.as_deref(),
            Some("frame_not_visible")
        );
    }

    #[test]
    fn evil_cloudflare_path_is_not_a_frame_vendor() {
        let rows = vec![crate::frames::RawFrameLink {
            frame: Some(3),
            url: Some("https://evil.example/cdn-cgi/challenge-platform/x".into()),
            ..crate::frames::RawFrameLink::default()
        }];
        assert!(
            !frame_is_vendor(&rows, 3),
            "an arbitrary host with the Cloudflare path is not a frame vendor"
        );
        let rows = vec![crate::frames::RawFrameLink {
            frame: Some(3),
            url: Some("https://evil.example/x@google.com/recaptcha".into()),
            ..crate::frames::RawFrameLink::default()
        }];
        assert!(
            !frame_is_vendor(&rows, 3),
            "an @ in the path cannot change the vendor host"
        );
        let rows = vec![crate::frames::RawFrameLink {
            frame: Some(3),
            url: Some("https://challenges.cloudflare.com/turnstile".into()),
            ..crate::frames::RawFrameLink::default()
        }];
        assert!(frame_is_vendor(&rows, 3));
    }

    #[test]
    fn non_storing_frame_late_row_holds_the_yield_state() {
        let raw = serde_json::json!({
            "stored": false,
            "tab": 12,
            "window": 45,
            "frame": 0,
            "metrics": {},
            "frames": [
                {"status": "unavailable", "reason": "frame_late"},
                {"status": "unavailable", "reason": "frame_geometry_unavailable"}
            ]
        });
        let map = map_from_raw_json(&raw.to_string(), None, "obs".into());
        assert!(
            map.snapshot.is_none(),
            "store:false drops the snapshot binding"
        );
        assert!(
            map.frames_transient,
            "the transient row survives store:false"
        );
        assert_eq!(map.frame_rows.len(), 2);
        assert_eq!(map.frame_rows[0].reason.as_deref(), Some("frame_late"));
    }

    #[test]
    fn non_storing_frame_content_reaches_detection_and_selection() {
        let raw = RawSnapshot {
            tab: Some(12),
            window: Some(45),
            frame: Some(0),
            stored: Some(false),
            metrics: serde_json::from_value(top_metrics_json()).expect("metrics"),
            elements: vec![RawNode {
                frame: Some(3),
                idx: Some(0),
                id: Some("frame:3:0".into()),
                role: Some("Button".into()),
                text: Some("I'm not a robot".into()),
                rect_css: CssRect {
                    left: 10.0,
                    top: 20.0,
                    width: 100.0,
                    height: 40.0,
                },
                _href: None,
            }],
            frames: vec![crate::frames::RawFrameLink {
                frame: Some(3),
                status: Some("ok".into()),
                origin: Some("https://www.google.com".into()),
                url: Some("https://www.google.com/recaptcha/api2/anchor".into()),
                link: Some(crate::frames::FrameLink {
                    parent: Some(0),
                    content_left: 40.0,
                    content_top: 60.0,
                    content_width: 300.0,
                    content_height: 250.0,
                    clip: vec![crate::frames::ClipRect {
                        left: 40.0,
                        top: 60.0,
                        width: 300.0,
                        height: 250.0,
                    }],
                    transform_ok: true,
                    visible: true,
                    hit_ok: true,
                    onscreen: true,
                    viewport_w: 1920.0,
                    viewport_h: 1080.0,
                    ..crate::frames::FrameLink::default()
                }),
                ..crate::frames::RawFrameLink::default()
            }],
            ..RawSnapshot::default()
        };
        let map = map_from_raw(raw, Some(0x1234), true, "obs".into());
        assert_eq!(map.elements.len(), 1, "non-storing frame element survives");
        assert_eq!(map.elements[0].frame, Some(3));
        assert_eq!(map.elements[0].id, "frame:3:0");
        assert!(map.elements[0].sref.is_none(), "no agent ref is minted");
        assert!(map.elements[0].challenge_vendor, "vendor frame is eligible");
        let hit = crate::challenge::detect_from_extract(
            "Top",
            Some("https://top.test/"),
            "",
            &map.elements,
        );
        assert!(hit.present, "frame challenge text arms detection: {hit:?}");
    }

    #[test]
    fn fixture_frame_ref_resolves_by_frame_and_idx() {
        let r = frame_ref();
        let raw = RawSnapshot {
            tab: Some(12),
            window: Some(45),
            frame: Some(0),
            doc: Some("9f3a".into()),
            snap: Some("a1b2c3d4".into()),
            retained: Some(10),
            stored: Some(true),
            metrics: serde_json::from_value(top_metrics_json()).expect("metrics"),
            elements: vec![RawNode {
                frame: Some(3),
                idx: Some(4),
                id: Some(r.to_string()),
                role: Some("Button".into()),
                text: Some("Sign in".into()),
                rect_css: CssRect {
                    left: 10.0,
                    top: 20.0,
                    width: 100.0,
                    height: 40.0,
                },
                _href: None,
            }],
            frames: vec![crate::frames::RawFrameLink {
                frame: Some(3),
                status: Some("ok".into()),
                link: Some(crate::frames::FrameLink {
                    parent: Some(0),
                    content_left: 40.0,
                    content_top: 60.0,
                    content_width: 300.0,
                    content_height: 250.0,
                    clip: vec![crate::frames::ClipRect {
                        left: 40.0,
                        top: 60.0,
                        width: 300.0,
                        height: 250.0,
                    }],
                    transform_ok: true,
                    visible: true,
                    hit_ok: true,
                    onscreen: true,
                    viewport_w: 1920.0,
                    viewport_h: 1080.0,
                    ..crate::frames::FrameLink::default()
                }),
                ..crate::frames::RawFrameLink::default()
            }],
            ..RawSnapshot::default()
        };
        let hit =
            scoped_fixture_hit(&raw, &r, Some(0x1234), false, false).expect("fixture frame ref");
        assert_eq!(hit.css_rect.left, 50.0);
        assert_eq!(hit.css_rect.top, 80.0);
    }

    #[test]
    fn frame_chain_consistency_tolerance_is_one_px() {
        let base = crate::frames::FrameLink {
            parent: Some(0),
            content_left: 40.0,
            content_top: 60.0,
            content_width: 300.0,
            content_height: 250.0,
            clip: vec![crate::frames::ClipRect {
                left: 40.0,
                top: 60.0,
                width: 300.0,
                height: 250.0,
            }],
            transform_ok: true,
            visible: true,
            hit_ok: true,
            onscreen: true,
            viewport_w: 1920.0,
            viewport_h: 1080.0,
            ..crate::frames::FrameLink::default()
        };
        let mut fresh = base.clone();
        assert!(frame_chains_match(
            std::slice::from_ref(&base),
            std::slice::from_ref(&fresh)
        ));
        fresh.content_left += 0.9;
        assert!(frame_chains_match(
            std::slice::from_ref(&base),
            std::slice::from_ref(&fresh)
        ));
        fresh.content_left += 1.0;
        assert!(!frame_chains_match(
            std::slice::from_ref(&base),
            std::slice::from_ref(&fresh)
        ));
        let mut hidden = base.clone();
        hidden.visible = false;
        assert!(!frame_chains_match(
            std::slice::from_ref(&base),
            std::slice::from_ref(&hidden)
        ));
    }

    #[test]
    fn frame_error_tokens_map_to_stale_or_unknown() {
        assert_eq!(
            classify_ref_error("frame_not_visible"),
            (IdentityState::Stale, "frame_not_visible".into())
        );
        assert_eq!(
            classify_ref_error("frame_geometry_unavailable"),
            (IdentityState::Unknown, "frame_geometry_unavailable".into())
        );
        assert_eq!(
            classify_ref_error("frame_occluded"),
            (IdentityState::Unknown, "frame_occluded".into())
        );
        assert_eq!(
            classify_ref_error("frame_document_changed"),
            (IdentityState::Stale, "frame_document_changed".into())
        );
    }

    #[test]
    fn malformed_assessment_is_unknown_not_ok_false() {
        let raw = RawNode {
            frame: None,
            idx: None,
            id: Some("chr:0".into()),
            role: Some("Button".into()),
            text: Some("Go".into()),
            rect_css: CssRect {
                left: 10.0,
                top: 20.0,
                width: 100.0,
                height: 40.0,
            },
            _href: None,
        };
        let reply = serde_json::json!({"assessment": "not-an-object"});
        let assessment = assessment_from_reply(&reply, &raw).expect("unknown assessment");
        assert_eq!(assessment.interactable, None);
        assert_eq!(assessment.reason.as_deref(), Some("assessment_unavailable"));
        assert_eq!(assessment.selected_point, raw.rect_css.center());
    }

    #[test]
    fn focused_chrome_popup_is_window_mismatch() {
        let target = 0x1234;
        let _ = crate::foreground::take_offer_calls();
        crate::foreground::set_chrome_target_hook(Some(all_windows_chrome));
        crate::foreground::set_foreground_hwnd_hook(Some(fg_popup));
        assert!(focused_chrome_popup_owns_foreground(target));
        assert_eq!(
            observe_identity(Some("Owner Web Access"), Some(target)),
            crate::point::WindowIdentity::Mismatch
        );

        // The resolved window itself is not a popup.
        crate::foreground::set_foreground_hwnd_hook(Some(fg_target));
        assert!(!focused_chrome_popup_owns_foreground(target));

        crate::foreground::set_foreground_hwnd_hook(None);
        crate::foreground::set_chrome_target_hook(None);
    }

    // ---------------------------------------------------------- scoped identity

    fn scoped_ref() -> SnapshotRef {
        SnapshotRef::Chrome {
            tab: 12,
            frame: 0,
            doc: "9f3a".into(),
            snap: "a1b2c3d4".into(),
            idx: 7,
        }
    }

    fn scoped_reply(r: &SnapshotRef) -> serde_json::Value {
        let SnapshotRef::Chrome {
            tab,
            frame,
            doc,
            snap,
            idx,
        } = r
        else {
            panic!("chrome ref");
        };
        serde_json::json!({
            "id": format!("chr:{idx}"),
            "role": "Button",
            "text": "Sign in",
            "rectCss": {"left": 10.0, "top": 20.0, "width": 100.0, "height": 40.0},
            "metrics": {"innerWidth": 800.0, "innerHeight": 600.0, "devicePixelRatio": 1.0},
            "url": "https://example.test/",
            "title": "Example",
            "tab": tab,
            "frame": frame,
            "doc": doc,
            "snap": snap,
            "idx": idx,
            "active": true,
            "focused": true,
            "visibilityState": "visible",
            "assess_v": 1,
        })
    }

    #[test]
    fn snapshot_binding_from_reply() {
        let mut raw = RawSnapshot {
            tab: Some(12),
            window: Some(45),
            frame: Some(0),
            doc: Some("9f3a".into()),
            snap: Some("a1b2c3d4".into()),
            retained: Some(250),
            stored: Some(true),
            ..RawSnapshot::default()
        };
        raw.elements = vec![RawNode {
            frame: None,
            idx: None,
            id: Some("chr:7".into()),
            role: Some("Button".into()),
            text: Some("Go".into()),
            rect_css: CssRect {
                left: 1.0,
                top: 2.0,
                width: 3.0,
                height: 4.0,
            },
            _href: None,
        }];
        let binding = snapshot_binding(&raw, Some(0x1234), false).expect("binding");
        assert_eq!(binding.tab, Some(12));
        assert_eq!(binding.window, Some(45));
        assert_eq!(binding.doc, "9f3a");
        assert_eq!(binding.snap, "a1b2c3d4");
        assert_eq!(binding.retained, 250);
        assert_eq!(binding.source, "live");
        assert!(binding.has_scoped_refs());
    }

    #[test]
    fn legacy_reply_without_snap_has_no_binding() {
        let raw = RawSnapshot {
            tab: Some(12),
            ..RawSnapshot::default()
        };
        let binding = snapshot_binding(&raw, Some(0x1234), false).expect("unbound block");
        assert_eq!(binding.reason.as_deref(), Some("unbound_extension"));
        assert!(!binding.has_scoped_refs());
        assert!(snapshot_binding(&raw, None, false).is_none());
    }

    #[test]
    fn scoped_resolve_reply_exact() {
        let r = scoped_ref();
        let hit = hit_from_scoped_reply(&scoped_reply(&r), &r, Some(0x1234), true, false)
            .expect("exact reply");
        assert_eq!(hit.id, r.to_string());
        assert_eq!(hit.name, "Sign in");
        let identity = hit.identity.expect("identity");
        assert_eq!(identity.binding, Binding::Scoped);
        assert_eq!(identity.state, IdentityState::Exact);
        assert_eq!(identity.tab, Some(12));
        assert_eq!(identity.snapshot.as_deref(), Some("a1b2c3d4"));
    }

    fn stale_reason(error: &str) -> (IdentityState, String) {
        let r = scoped_ref();
        let report =
            hit_from_scoped_reply(&serde_json::json!({"error": error}), &r, None, false, false)
                .expect_err("must refuse");
        (report.state, report.reason.unwrap_or_default())
    }

    #[test]
    fn scoped_resolve_reply_stale_document() {
        assert_eq!(
            stale_reason("stale_document"),
            (IdentityState::Stale, "stale_document".into())
        );
    }

    #[test]
    fn scoped_resolve_reply_stale_snapshot() {
        assert_eq!(
            stale_reason("stale_snapshot"),
            (IdentityState::Stale, "stale_snapshot".into())
        );
    }

    #[test]
    fn scoped_resolve_reply_stale_element() {
        assert_eq!(
            stale_reason("stale_element"),
            (IdentityState::Stale, "stale_element".into())
        );
    }

    #[test]
    fn scoped_resolve_reply_stale_identity() {
        assert_eq!(
            stale_reason("stale_identity"),
            (IdentityState::Stale, "stale_identity".into())
        );
    }

    #[test]
    fn scoped_resolve_reply_tab_unavailable_is_unknown() {
        assert_eq!(
            stale_reason("tab_unavailable"),
            (IdentityState::Unknown, "tab_unavailable".into())
        );
    }

    #[test]
    fn scoped_resolve_reply_echo_mismatch_is_unknown() {
        let r = scoped_ref();
        let mut reply = scoped_reply(&r);
        reply["idx"] = serde_json::json!(8);
        let report = hit_from_scoped_reply(&reply, &r, None, false, false).expect_err("echo");
        assert_eq!(report.state, IdentityState::Unknown);
        assert_eq!(report.reason.as_deref(), Some("protocol_error"));
    }

    #[test]
    fn scoped_resolve_reply_tab_not_active_is_unknown() {
        let r = scoped_ref();
        let mut reply = scoped_reply(&r);
        reply["active"] = serde_json::json!(false);
        let report = hit_from_scoped_reply(&reply, &r, None, false, false).expect_err("active");
        assert_eq!(report.state, IdentityState::Unknown);
        assert_eq!(report.reason.as_deref(), Some("tab_not_active"));
    }

    #[test]
    fn scoped_resolve_does_not_gate_on_non_chrome_foreground() {
        let src = include_str!("chrome.rs");
        let start = src
            .find("pub fn try_resolve_ref(")
            .expect("try_resolve_ref");
        let end = src
            .find("#[allow(clippy::result_large_err)]\nfn scoped_fixture_hit(")
            .expect("scoped_fixture_hit");
        let body = &src[start..end];
        assert!(!body.contains("foreground_hwnd() != Some(hwnd)"));
        assert!(body.contains("Do not short-circuit on a non-Chrome foreground"));
        assert!(body.contains("window_not_focused"));
    }

    #[test]
    fn background_chrome_window_is_window_not_focused() {
        let r = scoped_ref();
        let mut reply = scoped_reply(&r);
        reply["active"] = serde_json::json!(true);
        reply["focused"] = serde_json::json!(false);
        reply["visibilityState"] = serde_json::json!("visible");
        let report = hit_from_scoped_reply(&reply, &r, None, false, false).expect_err("background");
        assert_eq!(report.state, IdentityState::Unknown);
        assert_eq!(report.reason.as_deref(), Some("window_not_focused"));
    }

    #[test]
    fn scoped_resolve_reply_tab_hidden_is_unknown() {
        let r = scoped_ref();
        let mut reply = scoped_reply(&r);
        reply["visibilityState"] = serde_json::json!("hidden");
        let report = hit_from_scoped_reply(&reply, &r, None, false, false).expect_err("hidden");
        assert_eq!(report.state, IdentityState::Unknown);
        assert_eq!(report.reason.as_deref(), Some("tab_hidden"));
    }

    #[test]
    fn frame_binding_probe_surfaces_named_failures() {
        set_frame_binding_stub(Some(Err(IdentityReport::chrome_failure(
            &frame_ref(),
            IdentityState::Stale,
            "frame_document_changed",
        ))));
        let report = verify_frame_binding(12, 3, "9f3a", "a1b2c3d4", 4).expect_err("changed");
        set_frame_binding_stub(None);
        assert_eq!(report.state, IdentityState::Stale);
        assert_eq!(report.reason.as_deref(), Some("frame_document_changed"));

        set_frame_binding_stub(Some(Err(IdentityReport::chrome_failure(
            &frame_ref(),
            IdentityState::Unknown,
            "frame_geometry_unavailable",
        ))));
        let report = verify_frame_binding(12, 3, "9f3a", "a1b2c3d4", 4).expect_err("missing chain");
        set_frame_binding_stub(None);
        assert_eq!(report.reason.as_deref(), Some("frame_geometry_unavailable"));
    }

    #[test]
    fn binding_probe_returns_active_focused() {
        set_binding_stub(Some(Ok(BindingEvidence {
            tab: 12,
            window: Some(45),
            active: true,
            focused: true,
        })));
        let evidence = verify_binding(12).expect("probe");
        set_binding_stub(None);
        assert_eq!(evidence.tab, 12);
        assert_eq!(evidence.window, Some(45));
        assert!(evidence.active);
        assert!(evidence.focused);
    }

    #[test]
    fn binding_probe_mismatch_is_unknown() {
        set_binding_stub(Some(Err(binding_report(
            12,
            IdentityState::Unknown,
            "protocol_error",
        ))));
        let report = verify_binding(13).expect_err("mismatch");
        set_binding_stub(None);
        assert_eq!(report.state, IdentityState::Unknown);
        assert_eq!(report.reason.as_deref(), Some("protocol_error"));
    }

    #[test]
    fn fixture_binding_probe_is_unknown() {
        let g = EnvGuard::lock();
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/chrome-scoped.json");
        g.set_snapshot(Some(&path));
        set_binding_stub(None);
        set_binding_sequence(Vec::new());
        let report = verify_binding(12).expect_err("fixture has no live binding proof");
        g.set_snapshot(None);
        assert_eq!(report.state, IdentityState::Unknown);
        assert_eq!(report.reason.as_deref(), Some("fixture_snapshot"));
    }

    #[test]
    fn resolve_ref_request_has_no_legacy_id() {
        let req = resolve_ref_request(&scoped_ref(), false, None);
        assert_eq!(req["op"], "resolve");
        assert_eq!(req["v"], 1);
        assert_eq!(req["tab"], 12);
        assert_eq!(req["frame"], 0);
        assert_eq!(req["doc"], "9f3a");
        assert_eq!(req["snap"], "a1b2c3d4");
        assert_eq!(req["idx"], 7);
        assert!(req.get("id").is_none());
    }

    #[test]
    fn resolve_ref_request_carries_assess_and_samples() {
        let req = resolve_ref_request(&scoped_ref(), true, Some(25));
        assert_eq!(req["assess"], true);
        assert_eq!(req["samples"], 25);
        assert!(req.get("id").is_none());
    }

    #[test]
    fn internal_snapshot_does_not_store_binding() {
        let raw = RawSnapshot {
            tab: Some(12),
            window: Some(45),
            frame: Some(0),
            doc: Some("9f3a".into()),
            snap: Some("a1b2c3d4".into()),
            retained: Some(10),
            stored: Some(false),
            ..RawSnapshot::default()
        };
        assert!(snapshot_binding(&raw, Some(0x1234), false).is_none());
    }

    #[test]
    fn snapshot_store_false_omits_binding() {
        let raw = RawSnapshot {
            tab: Some(12),
            doc: Some("9f3a".into()),
            snap: Some("a1b2c3d4".into()),
            stored: Some(false),
            ..RawSnapshot::default()
        };
        assert!(snapshot_binding(&raw, Some(0x1234), true).is_none());
    }

    #[test]
    fn sw_js_routes_named_tab_without_fallback() {
        let src = include_str!("../extension/sw.js");
        let start = src.find("if (msg.tab != null)").expect("named branch");
        let end = src[start..]
            .find("// Unscoped observe")
            .map(|i| start + i)
            .expect("unscoped boundary");
        let named = &src[start..end];
        assert!(named.contains("chrome.tabs.get"));
        assert!(named.contains("tab_unavailable"));
        assert!(named.contains("withTabEvidence"));
        assert!(!named.contains("tabs.query"));
        assert!(src.contains("windowType: \"normal\""));
        // Every content-script message goes through the one frame-routed helper.
        assert_eq!(
            src.matches("tabs.sendMessage(").count(),
            1,
            "exactly one tabs.sendMessage call"
        );
        let send_start = src.find("function sendToFrame(").expect("sendToFrame");
        let send_end = src
            .find("function withTabEvidence(")
            .expect("withTabEvidence");
        let send = &src[send_start..send_end];
        assert!(
            send.contains("chrome.tabs.sendMessage(tabId, msg, { frameId: frameId }, callback)"),
            "the single call carries an explicit frameId"
        );
    }

    #[test]
    fn sw_js_labels_unscoped_fallback() {
        let src = include_str!("../extension/sw.js");
        assert!(src.contains("fallback: fallback"));
        assert!(src.contains("focused: !!win.focused"));
        assert!(src.contains("active: !!tab.active"));
        assert!(src.contains("tab: tab.id"));
        assert!(src.contains("window: tab.windowId"));
        assert!(src.contains("chrome.tabs.query"));
    }

    #[test]
    fn sw_js_binding_probe_has_no_content_message() {
        let src = include_str!("../extension/sw.js");
        let start = src.find("if (op === \"binding\")").expect("binding branch");
        let end = src[start..]
            .find("withTabEvidence(port, tab, false, msg)")
            .map(|i| start + i)
            .expect("named content boundary");
        let binding = &src[start..end];
        assert!(binding.contains("chrome.windows.get"));
        assert!(!binding.contains("tabs.sendMessage"));
    }

    #[test]
    fn content_js_keeps_one_snapshot_and_checks_connected() {
        let src = include_str!("../extension/content.js");
        for needle in [
            "lastSnapshot",
            "store !== false",
            "isConnected",
            "stale_document",
            "stale_snapshot",
            "stale_element",
            "stale_identity",
            "visibilityState",
            "SNAPSHOT_NODE_CAP",
            "padStart(4",
            "padStart(8",
        ] {
            assert!(src.contains(needle), "missing {needle}");
        }
    }

    #[test]
    fn content_js_routes_scoped_before_legacy() {
        let src = include_str!("../extension/content.js");
        let scoped = src.find("msg.v === 1").expect("scoped predicate");
        let legacy = src.find("buildResolve(msg.id").expect("legacy call");
        assert!(scoped < legacy);
        assert!(src[scoped..legacy].contains("resolveScoped"));
    }

    #[test]
    fn content_js_scoped_resolve_does_not_walk() {
        let src = include_str!("../extension/content.js");
        let start = src.find("function resolveScoped").expect("resolveScoped");
        let end = src[start..]
            .find("function parseChrId")
            .map(|i| start + i)
            .expect("next function");
        let body = &src[start..end];
        for needle in ["walk(", "collect(", "buildResolve(", "buildSnapshot("] {
            assert!(!body.contains(needle), "scoped resolve contains {needle}");
        }
    }

    #[test]
    fn content_js_hashes_full_code_units_and_computes_payload_once() {
        let src = include_str!("../extension/content.js");
        let fnv_start = src.find("function fnv1a32").expect("fnv1a32");
        let fnv_end = src[fnv_start..]
            .find("function snapshotKey")
            .map(|i| fnv_start + i)
            .expect("snapshotKey");
        assert!(src[fnv_start..fnv_end].contains("code >>> 8"));
        let start = src.find("function buildSnapshot").expect("buildSnapshot");
        let end = src[start..]
            .find("function buildResolve")
            .map(|i| start + i)
            .expect("buildResolve");
        let body = &src[start..end];
        assert!(!body.contains("nodePayload("));
        assert!(body.contains("keyPayload("));
        assert!(body.contains("snapshotKey("));
    }

    // -------------------------------------------------------------- 0124 postcheck

    fn postcheck_condition(
        kind: crate::outcome::PostconditionKind,
    ) -> crate::outcome::Postcondition {
        crate::outcome::Postcondition {
            kind,
            value: None,
            r#ref: None,
            role: None,
            n: None,
            wait_ms: 0,
        }
    }

    #[test]
    fn postcheck_request_uses_frame_zero_and_closed_fields() {
        let req = postcheck_request(
            12,
            &postcheck_condition(crate::outcome::PostconditionKind::DialogPresent),
            None,
        );
        assert_eq!(req["op"], "postcheck");
        assert_eq!(req["tab"], 12);
        assert_eq!(req["frame"], 0);
        assert_eq!(req["kind"], "dialog_present");
        let text = req.to_string();
        assert!(!text.contains("title"));
        assert!(!text.contains("url"));
        assert!(!text.contains("value"));

        let mut condition = postcheck_condition(crate::outcome::PostconditionKind::UrlChanged);
        condition.wait_ms = 0;
        let req = postcheck_request(
            12,
            &condition,
            Some(PostcheckRef {
                doc: "9f3a",
                snap: "a1b2c3d4",
                idx: 7,
            }),
        );
        assert_eq!(req["doc"], "9f3a");
        assert_eq!(req["snap"], "a1b2c3d4");
        assert_eq!(req["frame"], 0);
    }

    #[test]
    fn postcheck_fixture_is_unavailable() {
        reset_postcheck_calls();
        set_postcheck_stub(Some(Err(PostcheckError::Unavailable(
            "fixture_snapshot".into(),
        ))));
        let reply = postcheck(
            12,
            &postcheck_condition(crate::outcome::PostconditionKind::DialogPresent),
            None,
        );
        set_postcheck_stub(None);
        assert_eq!(
            reply,
            Err(PostcheckError::Unavailable("fixture_snapshot".into()))
        );
        assert_eq!(postcheck_calls(), 1);
    }

    #[test]
    fn postcheck_timeout_is_timeout() {
        set_postcheck_stub(Some(Err(PostcheckError::Timeout(
            "client timed out after 400ms".into(),
        ))));
        let reply = postcheck(
            12,
            &postcheck_condition(crate::outcome::PostconditionKind::DialogPresent),
            None,
        );
        set_postcheck_stub(None);
        assert!(matches!(reply, Err(PostcheckError::Timeout(_))));
    }

    #[test]
    fn postcheck_old_extension_empty_is_unavailable() {
        set_postcheck_stub(Some(Err(PostcheckError::Unavailable("unsupported".into()))));
        let reply = postcheck(
            12,
            &postcheck_condition(crate::outcome::PostconditionKind::DialogPresent),
            None,
        );
        set_postcheck_stub(None);
        assert_eq!(
            reply,
            Err(PostcheckError::Unavailable("unsupported".into()))
        );
    }

    #[test]
    fn postcheck_visibility_state_camel_case() {
        let reply: PostcheckReply = serde_json::from_value(serde_json::json!({
            "state": "satisfied",
            "tab": 12,
            "active": true,
            "focused": true,
            "visibilityState": "hidden",
            "fallback": false
        }))
        .expect("reply");
        assert_eq!(reply.visibility_state.as_deref(), Some("hidden"));
        assert_eq!(reply.tab, Some(12));
        // No page text/URL/title field exists on the wire type.
        let text = serde_json::to_string(&reply).expect("json");
        assert!(!text.contains("\"url\""));
        assert!(!text.contains("\"title\""));
        assert!(!text.contains("\"value\""));
        assert!(!text.contains("\"text\""));
    }

    #[test]
    fn postcheck_no_text_url_title_in_reply() {
        let reply = PostcheckReply {
            state: "unsatisfied".into(),
            reason: Some("postcondition_not_met_yet".into()),
            doc: Some("9f3a".into()),
            snap: Some("a1b2c3d4".into()),
            tab: Some(12),
            active: true,
            focused: true,
            visibility_state: Some("visible".into()),
            fallback: false,
        };
        let json = serde_json::to_string(&reply).expect("json");
        assert!(json.contains("visibilityState"));
        assert!(!json.contains("http"));
        assert!(!json.contains("innerText"));
    }
}
