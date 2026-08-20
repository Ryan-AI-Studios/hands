use serde::{Deserialize, Serialize};

use crate::capture::{capture_virtual_screen, display_path};
use crate::challenge::{self, ChallengeInfo};
use crate::chrome;
use crate::error::HandsError;
use crate::extract::{
    Detail, Element, Extract, VIEWPORT_ENVELOPE_ELEMENT_CAP, extract_from_nodes, extract_fused,
    filter_nodes,
};
use crate::foreground;
use crate::logs;
use crate::session::resolve_session_id_from_os;
use crate::space::{Rect, Space, ensure_dpi, virtual_screen};
use crate::uia;

pub const ENVELOPE_MAX_BYTES: usize = 16_384;
pub const DEFAULT_ENVELOPE_MAX_BYTES: usize = 4096;
pub const OBSERVE_SCHEMA: &str = "hands.observe/v1";

#[derive(Debug, Clone)]
pub struct ObserveRequest {
    pub session_id: Option<String>,
    pub detail: Detail,
}

#[derive(Debug, Clone, Serialize)]
pub struct ObserveEnvelope {
    pub session_id: String,
    pub screenshot_path: String,
    pub observe_path: String,
    pub space: Space,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport: Option<Rect>,
    pub extract: Extract,
    pub elements: Vec<Element>,
    pub elements_total: usize,
    pub elements_truncated: bool,
    pub chrome_connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chrome_hint: Option<String>,
    pub challenge: ChallengeInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObserveSidecar {
    pub schema: String,
    pub session_id: String,
    pub screenshot_path: String,
    pub observe_path: String,
    pub space: Space,
    #[serde(default)]
    pub viewport: Option<Rect>,
    pub extract: Extract,
    pub elements: Vec<Element>,
    pub elements_total: usize,
    pub elements_truncated: bool,
    pub chrome_connected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chrome_hint: Option<String>,
    #[serde(default)]
    pub challenge: ChallengeInfo,
}

#[derive(Debug, Clone, Copy)]
pub struct FuseOpts {
    pub viewport: Option<Rect>,
    pub chrome_is_foreground: bool,
    pub virtual_screen: Option<Rect>,
    pub popup_rect: Option<Rect>,
}

pub fn observe(req: ObserveRequest) -> Result<ObserveEnvelope, HandsError> {
    ensure_dpi()?;
    let session_id = resolve_session_id_from_os(req.session_id.as_deref());
    logs::check_write_id(&session_id)?;
    let space = virtual_screen()?;
    let paths = capture_virtual_screen(space)?;
    let screenshot_path = display_path(&paths.screenshot_path);
    let observe_path = display_path(&paths.observe_path);

    let viewport = foreground::viewport_rect();
    let chrome_is_foreground = foreground::is_chrome();
    let snap = uia::collect(req.detail)?;
    let chrome = chrome::try_snapshot(req.detail);
    let (mut extract, mut elements, elements_total, chrome_connected) = fuse_maps(
        req.detail,
        &snap.title,
        &snap.nodes,
        chrome,
        FuseOpts {
            viewport,
            chrome_is_foreground,
            virtual_screen: Some(space.as_rect()),
            popup_rect: snap.popup_rect,
        },
    );
    crate::dialogs::promote(&mut extract, &mut elements);
    stamp_grid(space, &mut elements);
    crate::fence::note_last_url(extract.url.as_deref());
    let hit = challenge::detect_from_extract(
        &extract.title,
        extract.url.as_deref(),
        &extract.main_text,
        &elements,
    );
    let challenge = challenge::apply_observe(&session_id, hit);

    let full = ObserveEnvelope {
        session_id,
        screenshot_path,
        observe_path,
        space,
        viewport,
        extract,
        elements,
        elements_total,
        elements_truncated: false,
        chrome_connected,
        chrome_hint: if chrome_connected {
            None
        } else {
            Some("Chrome host down — run hands native-host-doctor (MCP: native_host_doctor)".into())
        },
        challenge,
    };
    write_sidecar(&paths.observe_path, &full)?;
    let envelope = match req.detail {
        Detail::Default => finalize_envelope(cap_default_envelope(full))?,
        Detail::Dom => finalize_envelope(full)?,
    };
    logs::ensure_installed();
    logs::remember_session(&envelope.session_id);
    let _ = logs::record_observe(
        &envelope.session_id,
        req.detail.as_str(),
        &envelope.screenshot_path,
        envelope.elements_total,
    );
    Ok(envelope)
}

fn stamp_grid(space: Space, elements: &mut [Element]) {
    for el in elements {
        el.grid = Some(space.cell_id_of_center(el.rect));
    }
}

fn write_sidecar(path: &std::path::Path, envelope: &ObserveEnvelope) -> Result<(), HandsError> {
    let sidecar = ObserveSidecar {
        schema: OBSERVE_SCHEMA.to_string(),
        session_id: envelope.session_id.clone(),
        screenshot_path: envelope.screenshot_path.clone(),
        observe_path: envelope.observe_path.clone(),
        space: envelope.space,
        viewport: envelope.viewport,
        extract: envelope.extract.clone(),
        elements: envelope.elements.clone(),
        elements_total: envelope.elements_total,
        elements_truncated: false,
        chrome_connected: envelope.chrome_connected,
        chrome_hint: envelope.chrome_hint.clone(),
        challenge: envelope.challenge.clone(),
    };
    let json = serde_json::to_string_pretty(&sidecar)
        .map_err(|err| HandsError::Observe(format!("sidecar serialize: {err}")))?;
    std::fs::write(path, json).map_err(|err| HandsError::Observe(format!("sidecar write: {err}")))
}

pub fn cap_envelope(mut envelope: ObserveEnvelope) -> ObserveEnvelope {
    if serialized_len(&envelope) <= ENVELOPE_MAX_BYTES {
        return envelope;
    }
    envelope.elements_truncated = true;
    while !envelope.elements.is_empty() && serialized_len(&envelope) > ENVELOPE_MAX_BYTES {
        envelope.elements.pop();
    }
    envelope
}

/// Default path: 20-element cap, then pop non-dialog elements from the end to
/// 4 KiB, then shrink `main_text`. Never drops cards, `challenge`,
/// `chrome_hint`, or `extract.dialogs` first. Last resort: pop extra dialogs
/// after `main_text` is empty. 16 KiB hard fail stays in `finalize_envelope`.
pub fn cap_default_envelope(mut envelope: ObserveEnvelope) -> ObserveEnvelope {
    if envelope.elements.len() > VIEWPORT_ENVELOPE_ELEMENT_CAP {
        envelope.elements.truncate(VIEWPORT_ENVELOPE_ELEMENT_CAP);
    }
    envelope.elements_truncated = envelope.elements.len() < envelope.elements_total;
    if serialized_len(&envelope) <= DEFAULT_ENVELOPE_MAX_BYTES {
        return envelope;
    }
    envelope.elements_truncated = true;
    pop_non_dialog_elements(&mut envelope);
    if serialized_len(&envelope) > DEFAULT_ENVELOPE_MAX_BYTES {
        shrink_main_text_to_fit(&mut envelope, DEFAULT_ENVELOPE_MAX_BYTES);
    }
    if serialized_len(&envelope) > DEFAULT_ENVELOPE_MAX_BYTES
        && envelope.extract.main_text.is_empty()
    {
        while envelope.extract.dialogs.len() > 1
            && serialized_len(&envelope) > DEFAULT_ENVELOPE_MAX_BYTES
        {
            let Some(dropped) = envelope.extract.dialogs.pop() else {
                break;
            };
            envelope.elements.retain(|el| el.id != dropped.id);
        }
    }
    envelope
}

fn pop_non_dialog_elements(envelope: &mut ObserveEnvelope) {
    let dialog_ids: std::collections::HashSet<String> = envelope
        .extract
        .dialogs
        .iter()
        .map(|d| d.id.clone())
        .collect();
    while serialized_len(envelope) > DEFAULT_ENVELOPE_MAX_BYTES {
        let Some(idx) = envelope
            .elements
            .iter()
            .rposition(|el| !dialog_ids.contains(&el.id))
        else {
            break;
        };
        envelope.elements.remove(idx);
    }
}

fn shrink_main_text_to_fit(envelope: &mut ObserveEnvelope, max_bytes: usize) {
    if serialized_len(envelope) <= max_bytes {
        return;
    }
    let chars: Vec<char> = envelope.extract.main_text.chars().collect();
    if chars.is_empty() {
        return;
    }
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        envelope.extract.main_text = chars[..mid].iter().collect();
        if serialized_len(envelope) <= max_bytes {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    envelope.extract.main_text = chars[..lo].iter().collect();
}

/// Drop trailing elements to fit 16 KiB. If metadata alone (session_id, paths,
/// extract, space) still exceeds the budget, fail rather than emit an oversize envelope.
pub fn finalize_envelope(envelope: ObserveEnvelope) -> Result<ObserveEnvelope, HandsError> {
    let capped = cap_envelope(envelope);
    let len = serialized_len(&capped);
    if len > ENVELOPE_MAX_BYTES {
        return Err(HandsError::Observe(format!(
            "observe envelope is {len} bytes after dropping elements (hard max {ENVELOPE_MAX_BYTES})"
        )));
    }
    Ok(capped)
}

pub fn serialize_envelope(envelope: &ObserveEnvelope) -> Result<String, HandsError> {
    serde_json::to_string(envelope)
        .map_err(|err| HandsError::Observe(format!("envelope serialize: {err}")))
}

fn serialized_len(envelope: &ObserveEnvelope) -> usize {
    serde_json::to_string(envelope)
        .map(|s| s.len())
        .unwrap_or(usize::MAX)
}

pub fn fuse_maps(
    detail: Detail,
    uia_title: &str,
    uia_nodes: &[crate::extract::RawNode],
    chrome: Option<chrome::ChromeMap>,
    opts: FuseOpts,
) -> (Extract, Vec<Element>, usize, bool) {
    match detail {
        Detail::Dom => fuse_maps_dom(uia_title, uia_nodes, chrome),
        Detail::Default => fuse_maps_default(uia_title, uia_nodes, chrome, opts),
    }
}

fn fuse_maps_dom(
    uia_title: &str,
    uia_nodes: &[crate::extract::RawNode],
    chrome: Option<chrome::ChromeMap>,
) -> (Extract, Vec<Element>, usize, bool) {
    let chrome_connected = chrome.is_some();
    let (uia_els, uia_matched) = filter_nodes(uia_nodes, Detail::Dom);
    let (chrome_els, chrome_n, extract) = match chrome {
        Some(map) => {
            let n = map.elements.len();
            let extract = extract_fused(
                uia_title,
                uia_nodes,
                map.url.as_deref(),
                &map.title,
                &map.main_text,
                map.cards,
                map.listing,
            );
            (map.elements, n, extract)
        }
        None => (Vec::new(), 0, extract_from_nodes(uia_title, uia_nodes)),
    };
    let mut elements = chrome_els;
    elements.extend(uia_els);
    let cap = Detail::Dom.element_cap();
    if elements.len() > cap {
        elements.truncate(cap);
    }
    (extract, elements, chrome_n + uia_matched, chrome_connected)
}

fn fuse_maps_default(
    uia_title: &str,
    uia_nodes: &[crate::extract::RawNode],
    chrome: Option<chrome::ChromeMap>,
    opts: FuseOpts,
) -> (Extract, Vec<Element>, usize, bool) {
    let chrome_connected = chrome.is_some();
    if opts.viewport.is_none() {
        return (
            extract_from_nodes(uia_title, uia_nodes),
            Vec::new(),
            0,
            chrome_connected,
        );
    }
    let (uia_els, uia_matched) = filter_viewport_nodes(uia_nodes, &opts);
    if opts.chrome_is_foreground
        && let Some(map) = chrome
    {
        let chrome_els = filter_viewport_elements(map.elements, &opts);
        let chrome_n = chrome_els.len();
        let extract = extract_fused(
            uia_title,
            uia_nodes,
            map.url.as_deref(),
            &map.title,
            &map.main_text,
            map.cards,
            map.listing,
        );
        let mut elements = chrome_els;
        elements.extend(uia_els);
        let cap = Detail::Default.element_cap();
        if elements.len() > cap {
            elements.truncate(cap);
        }
        return (extract, elements, chrome_n + uia_matched, true);
    }
    let extract = extract_from_nodes(uia_title, uia_nodes);
    let mut elements = uia_els;
    let cap = Detail::Default.element_cap();
    if elements.len() > cap {
        elements.truncate(cap);
    }
    (extract, elements, uia_matched, chrome_connected)
}

fn in_viewport(rect: Rect, opts: &FuseOpts) -> bool {
    match (opts.viewport, opts.virtual_screen) {
        (Some(viewport), Some(screen)) => {
            rect.intersects(screen)
                && (rect.intersects(viewport)
                    || opts.popup_rect.is_some_and(|popup| rect.intersects(popup)))
        }
        _ => false,
    }
}

fn filter_viewport_nodes(
    nodes: &[crate::extract::RawNode],
    opts: &FuseOpts,
) -> (Vec<Element>, usize) {
    let cap = Detail::Default.element_cap();
    let mut elements = Vec::new();
    let mut matched = 0usize;
    for node in nodes {
        if !node.passes_filter(Detail::Default) || !in_viewport(node.rect, opts) {
            continue;
        }
        matched += 1;
        if elements.len() < cap
            && let Some(element) = node.to_element()
        {
            elements.push(element);
        }
    }
    (elements, matched)
}

fn filter_viewport_elements(elements: Vec<Element>, opts: &FuseOpts) -> Vec<Element> {
    elements
        .into_iter()
        .filter(|el| in_viewport(el.rect, opts))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{Card, ControlKind, DialogHit, Element, RawNode};
    use crate::space::Rect;

    fn covering_opts(chrome_is_foreground: bool) -> FuseOpts {
        FuseOpts {
            viewport: Some(Rect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            }),
            chrome_is_foreground,
            virtual_screen: Some(Rect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            }),
            popup_rect: None,
        }
    }

    fn fixture_opts(chrome_is_foreground: bool) -> FuseOpts {
        FuseOpts {
            viewport: Some(Rect {
                x: 100,
                y: 50,
                w: 1280,
                h: 800,
            }),
            chrome_is_foreground,
            virtual_screen: Some(Rect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            }),
            popup_rect: None,
        }
    }

    fn uia_node(id: i32, name: &str, rect: Rect) -> RawNode {
        RawNode {
            runtime_id: vec![1, id],
            role: "Button".into(),
            name: name.to_string(),
            value: None,
            is_password: false,
            rect,
            control_kind: ControlKind::Button,
            is_control: true,
            is_offscreen: false,
            is_keyboard_focusable: true,
        }
    }

    fn fat_envelope(n: usize) -> ObserveEnvelope {
        ObserveEnvelope {
            session_id: "sess".into(),
            screenshot_path: "C:\\tmp\\observe.png".into(),
            observe_path: "C:\\tmp\\observe.json".into(),
            space: Space::new(0, 0, 1920, 1080).unwrap(),
            viewport: None,
            extract: Extract {
                title: "Title".into(),
                url: None,
                main_text: "hello".into(),
                cards: Vec::new(),
                dialogs: Vec::new(),
                ..Default::default()
            },
            elements: (0..n)
                .map(|i| Element {
                    id: format!("uia:9.{i}"),
                    role: "Button".into(),
                    text: Some("x".repeat(64)),
                    rect: Rect {
                        x: 0,
                        y: 0,
                        w: 12,
                        h: 12,
                    },
                    grid: None,
                })
                .collect(),
            elements_total: n,
            elements_truncated: false,
            chrome_connected: false,
            chrome_hint: None,
            challenge: ChallengeInfo::default(),
        }
    }

    #[test]
    fn challenge_survives_16kib_shrink() {
        let mut raw = fat_envelope(400);
        raw.challenge = ChallengeInfo {
            present: true,
            kind: Some("recaptcha".into()),
            attempts: 2,
            yielded: true,
            reason: Some("i'm not a robot".into()),
        };
        let capped = cap_envelope(raw);
        let json = serialize_envelope(&capped).unwrap();
        assert!(json.len() <= ENVELOPE_MAX_BYTES);
        assert!(capped.challenge.present);
        assert!(capped.challenge.yielded);
        assert_eq!(capped.challenge.kind.as_deref(), Some("recaptcha"));
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["challenge"]["present"], true);
        assert_eq!(parsed["challenge"]["yielded"], true);
    }

    #[test]
    fn sidecar_missing_challenge_deserializes() {
        let json = r#"{
            "schema": "hands.observe/v1",
            "session_id": "s",
            "screenshot_path": "C:\\tmp\\a.png",
            "observe_path": "C:\\tmp\\a.json",
            "space": {"origin_x":0,"origin_y":0,"width":10,"height":10,"cell_px":100},
            "extract": {"title":"T","url":null,"main_text":"","cards":[]},
            "elements": [],
            "elements_total": 0,
            "elements_truncated": false,
            "chrome_connected": false
        }"#;
        let side: ObserveSidecar = serde_json::from_str(json).unwrap();
        assert_eq!(side.challenge, ChallengeInfo::default());
    }

    #[test]
    fn envelope_truncation_fits_16kib() {
        let raw = fat_envelope(400);
        let raw_len = serialize_envelope(&raw).unwrap().len();
        assert!(raw_len > ENVELOPE_MAX_BYTES, "fixture too small: {raw_len}");
        let capped = cap_envelope(raw);
        let json = serialize_envelope(&capped).unwrap();
        assert!(capped.elements_truncated);
        assert!(json.len() <= ENVELOPE_MAX_BYTES, "len {}", json.len());
        assert_eq!(capped.elements_total, 400);
        assert!(!capped.elements.is_empty());
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["space"]["cell_px"], 100);
        assert!(parsed["url"].is_null() || parsed["extract"]["url"].is_null());
    }

    #[test]
    fn oversized_session_id_is_tool_error() {
        let mut raw = fat_envelope(0);
        raw.session_id = "s".repeat(20_000);
        let err = finalize_envelope(raw).expect_err("must not emit oversize envelope");
        assert!(err.to_string().contains("16384"), "{err}");
    }

    #[test]
    fn fusion_chrome_first_then_cap() {
        use crate::extract::{ControlKind, RawNode};

        fn uia(i: i32) -> RawNode {
            RawNode {
                runtime_id: vec![1, i],
                role: "Button".into(),
                name: format!("u{i}"),
                value: None,
                is_password: false,
                rect: Rect {
                    x: 0,
                    y: 0,
                    w: 10,
                    h: 10,
                },
                control_kind: ControlKind::Button,
                is_control: true,
                is_offscreen: false,
                is_keyboard_focusable: true,
            }
        }
        let chrome = chrome::ChromeMap {
            url: Some("https://cars.com/search".into()),
            title: "Cars.com".into(),
            main_text: "from chrome".into(),
            elements: (0..3)
                .map(|i| Element {
                    id: format!("chr:{i}"),
                    role: "Hyperlink".into(),
                    text: Some("c".into()),
                    rect: Rect {
                        x: 1,
                        y: 1,
                        w: 8,
                        h: 8,
                    },
                    grid: None,
                })
                .collect(),
            cards: vec![],
            listing: crate::extract::ListingMeta::default(),
        };
        let nodes: Vec<RawNode> = (0..5).map(uia).collect();
        let (extract, els, total, connected) = fuse_maps(
            Detail::Default,
            "UIA title",
            &nodes,
            Some(chrome),
            covering_opts(true),
        );
        assert!(connected);
        assert_eq!(extract.url.as_deref(), Some("https://cars.com/search"));
        assert_eq!(extract.title, "Cars.com");
        assert_eq!(extract.main_text, "from chrome");
        assert_eq!(total, 8);
        assert!(els[0].id.starts_with("chr:"));
        assert!(els.iter().any(|e| e.id.starts_with("uia:")));
        assert!(els.iter().take(3).all(|e| e.id.starts_with("chr:")));
    }

    #[test]
    fn fixture_fuse_sets_chrome_connected_and_url() {
        let g = chrome::EnvGuard::lock();
        g.set_snapshot(Some(&chrome::EnvGuard::fixture_path()));
        let map = chrome::try_snapshot(Detail::Default).expect("fixture");
        let (extract, els, _total, connected) = fuse_maps(
            Detail::Default,
            "UIA",
            &[],
            Some(map),
            FuseOpts {
                viewport: Some(Rect {
                    x: 100,
                    y: 50,
                    w: 1280,
                    h: 800,
                }),
                chrome_is_foreground: true,
                virtual_screen: Some(Rect {
                    x: 0,
                    y: 0,
                    w: 1920,
                    h: 1080,
                }),
                popup_rect: None,
            },
        );
        assert!(connected);
        assert_eq!(extract.url.as_deref(), Some("https://cars.com/search"));
        assert!(els.iter().any(|e| e.id == "chr:0"));
        assert_eq!(extract.cards.len(), 1);
        assert_eq!(extract.cards[0].price, "$12,345");
        assert!(extract.cards[0].miles.is_some());
        assert!(extract.cards[0].dealer.is_some());
        assert!(extract.cards[0].distance.is_some());
        crate::fence::note_last_url(extract.url.as_deref());
        assert_eq!(
            crate::fence::last_url().as_deref(),
            Some("https://cars.com/search")
        );
    }

    #[test]
    fn absent_chrome_extract_is_uia_only() {
        let (extract, els, total, connected) =
            fuse_maps(Detail::Default, "Desktop", &[], None, covering_opts(false));
        assert!(!connected);
        assert_eq!(extract.url, None);
        assert!(extract.cards.is_empty());
        assert!(els.is_empty());
        assert_eq!(total, 0);
    }

    #[test]
    fn fat_chrome_uia_mix_keeps_chr_after_shrink() {
        let mut raw = fat_envelope(400);
        raw.chrome_connected = true;
        raw.elements.insert(
            0,
            Element {
                id: "chr:0".into(),
                role: "Edit".into(),
                text: Some("search".into()),
                rect: Rect {
                    x: 110,
                    y: 150,
                    w: 200,
                    h: 32,
                },
                grid: None,
            },
        );
        raw.elements_total = 401;
        let raw_len = serialize_envelope(&raw).unwrap().len();
        assert!(raw_len > ENVELOPE_MAX_BYTES, "fixture too small: {raw_len}");
        let capped = cap_envelope(raw);
        let json = serialize_envelope(&capped).unwrap();
        assert!(json.len() <= ENVELOPE_MAX_BYTES, "len {}", json.len());
        assert!(capped.elements.iter().any(|e| e.id.starts_with("chr:")));
        assert!(capped.chrome_connected);
    }

    #[test]
    fn cap_envelope_keeps_cards() {
        let mut raw = fat_envelope(400);
        raw.extract.cards = vec![Card {
            title: "2020 Honda Civic".into(),
            price: "$12,345".into(),
            href: "https://cars.com/vehicledetail/abc".into(),
            rect: Rect {
                x: 110,
                y: 230,
                w: 300,
                h: 80,
            },
            ..Default::default()
        }];
        let raw_len = serialize_envelope(&raw).unwrap().len();
        assert!(raw_len > ENVELOPE_MAX_BYTES, "fixture too small: {raw_len}");
        let capped = cap_envelope(raw);
        assert_eq!(capped.extract.cards.len(), 1);
        assert_eq!(capped.extract.cards[0].price, "$12,345");
        assert!(serialize_envelope(&capped).unwrap().len() <= ENVELOPE_MAX_BYTES);
    }

    fn desktop_noise() -> Vec<RawNode> {
        vec![
            uia_node(
                9001,
                "Taskbar",
                Rect {
                    x: 0,
                    y: 1040,
                    w: 1920,
                    h: 40,
                },
            ),
            uia_node(
                9002,
                "Windows PowerShell",
                Rect {
                    x: 2000,
                    y: 0,
                    w: 400,
                    h: 800,
                },
            ),
            uia_node(
                9003,
                "Hidden title",
                Rect {
                    x: -31976,
                    y: 0,
                    w: 200,
                    h: 32,
                },
            ),
        ]
    }

    fn viewport_buttons(n: i32) -> Vec<RawNode> {
        (0..n)
            .map(|i| {
                uia_node(
                    i,
                    &format!("ok{i}"),
                    Rect {
                        x: 200,
                        y: 200,
                        w: 20,
                        h: 20,
                    },
                )
            })
            .collect()
    }

    fn noise_ids() -> [&'static str; 3] {
        ["uia:1.9001", "uia:1.9002", "uia:1.9003"]
    }

    #[test]
    fn default_fuse_drops_desktop_noise_keeps_fixture_chr() {
        let g = chrome::EnvGuard::lock();
        g.set_snapshot(Some(&chrome::EnvGuard::fixture_path()));
        let map = chrome::try_snapshot(Detail::Default).expect("fixture");
        let mut nodes = viewport_buttons(5);
        nodes.extend(desktop_noise());
        let (extract, els, total, connected) = fuse_maps(
            Detail::Default,
            "UIA",
            &nodes,
            Some(map),
            fixture_opts(true),
        );
        assert!(connected);
        assert_eq!(extract.url.as_deref(), Some("https://cars.com/search"));
        assert!(els.iter().any(|e| e.id == "chr:0"));
        assert!(els.iter().any(|e| e.id.starts_with("uia:1.")));
        for id in noise_ids() {
            assert!(
                !els.iter().any(|e| e.id == id),
                "noise id {id} leaked into default fuse"
            );
        }
        assert!(!els.iter().any(|e| e.text.as_deref() == Some("Taskbar")));
        assert!(
            !els.iter()
                .any(|e| e.text.as_deref() == Some("Windows PowerShell"))
        );
        assert!(!els.iter().any(|e| e.rect.x <= -30_000));
        assert_eq!(total, 3 + 5);
    }

    #[test]
    fn default_envelope_fits_4kib_and_caps_20() {
        let g = chrome::EnvGuard::lock();
        g.set_snapshot(Some(&chrome::EnvGuard::fixture_path()));
        let map = chrome::try_snapshot(Detail::Default).expect("fixture");
        let mut nodes = viewport_buttons(40);
        nodes.extend(desktop_noise());
        let (extract, mut elements, elements_total, chrome_connected) = fuse_maps(
            Detail::Default,
            "UIA",
            &nodes,
            Some(map),
            fixture_opts(true),
        );
        assert_eq!(elements_total, 3 + 40);
        assert!(elements.len() > VIEWPORT_ENVELOPE_ELEMENT_CAP);
        for id in noise_ids() {
            assert!(!elements.iter().any(|e| e.id == id));
        }
        let space = Space::new(0, 0, 1920, 1080).unwrap();
        stamp_grid(space, &mut elements);
        let raw = ObserveEnvelope {
            session_id: "sess".into(),
            screenshot_path: "C:\\tmp\\observe.png".into(),
            observe_path: "C:\\tmp\\observe.json".into(),
            space,
            viewport: fixture_opts(true).viewport,
            extract,
            elements,
            elements_total,
            elements_truncated: false,
            chrome_connected,
            chrome_hint: None,
            challenge: ChallengeInfo::default(),
        };
        assert!(raw.viewport.is_some());
        let capped = cap_default_envelope(raw);
        let json = serialize_envelope(&capped).unwrap();
        assert!(
            json.len() <= DEFAULT_ENVELOPE_MAX_BYTES,
            "len {}",
            json.len()
        );
        assert!(capped.elements.len() <= VIEWPORT_ENVELOPE_ELEMENT_CAP);
        assert!(capped.elements_truncated);
        assert_eq!(capped.elements_total, 43);
        assert!(capped.elements.iter().any(|e| e.id == "chr:0"));
        assert!(capped.elements.iter().all(|e| e.grid.is_some()));
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(!parsed["viewport"].is_null());
    }

    #[test]
    fn dom_fuse_can_include_noise_and_more_than_20() {
        let g = chrome::EnvGuard::lock();
        g.set_snapshot(Some(&chrome::EnvGuard::fixture_path()));
        let map = chrome::try_snapshot(Detail::Default).expect("fixture");
        let mut nodes = viewport_buttons(25);
        nodes.extend(desktop_noise());
        let (_extract, els, total, connected) =
            fuse_maps(Detail::Dom, "UIA", &nodes, Some(map), fixture_opts(true));
        assert!(connected);
        assert!(els.len() > VIEWPORT_ENVELOPE_ELEMENT_CAP);
        assert!(total > VIEWPORT_ENVELOPE_ELEMENT_CAP);
        assert!(els.iter().any(|e| e.id == "uia:1.9001"));
        assert!(els.iter().any(|e| e.id == "uia:1.9002"));
        assert!(els.iter().any(|e| e.id == "uia:1.9003"));
    }

    #[test]
    fn chrome_connected_but_not_foreground_has_no_chr() {
        let g = chrome::EnvGuard::lock();
        g.set_snapshot(Some(&chrome::EnvGuard::fixture_path()));
        let map = chrome::try_snapshot(Detail::Default).expect("fixture");
        let nodes = vec![uia_node(
            7,
            "Document",
            Rect {
                x: 200,
                y: 200,
                w: 40,
                h: 40,
            },
        )];
        let (extract, els, total, connected) = fuse_maps(
            Detail::Default,
            "Notepad",
            &nodes,
            Some(map),
            fixture_opts(false),
        );
        assert!(connected);
        assert!(!els.iter().any(|e| e.id.starts_with("chr:")));
        assert_eq!(extract.url, None);
        assert_eq!(extract.title, "Notepad");
        assert!(els.iter().any(|e| e.id == "uia:1.7"));
        assert_eq!(total, 1);
    }

    #[test]
    fn no_foreground_viewport_empties_default_elements() {
        let g = chrome::EnvGuard::lock();
        g.set_snapshot(Some(&chrome::EnvGuard::fixture_path()));
        let map = chrome::try_snapshot(Detail::Default).expect("fixture");
        let mut nodes = viewport_buttons(8);
        nodes.extend(desktop_noise());
        let (extract, els, total, connected) = fuse_maps(
            Detail::Default,
            "Desktop",
            &nodes,
            Some(map),
            FuseOpts {
                viewport: None,
                chrome_is_foreground: true,
                virtual_screen: Some(Rect {
                    x: 0,
                    y: 0,
                    w: 1920,
                    h: 1080,
                }),
                popup_rect: None,
            },
        );
        assert!(connected);
        assert!(els.is_empty());
        assert_eq!(total, 0);
        assert_eq!(extract.url, None);
        assert_eq!(extract.title, "Desktop");
    }

    #[test]
    fn challenge_and_cards_survive_4kib_shrink() {
        let mut raw = fat_envelope(400);
        raw.extract.main_text = "M".repeat(3000);
        raw.extract.cards = vec![Card {
            title: "2020 Honda Civic".into(),
            price: "$12,345".into(),
            href: "https://cars.com/vehicledetail/abc".into(),
            rect: Rect {
                x: 110,
                y: 230,
                w: 300,
                h: 80,
            },
            ..Default::default()
        }];
        raw.challenge = ChallengeInfo {
            present: true,
            kind: Some("recaptcha".into()),
            attempts: 2,
            yielded: true,
            reason: Some("i'm not a robot".into()),
        };
        stamp_grid(raw.space, &mut raw.elements);
        let capped = cap_default_envelope(raw);
        let json = serialize_envelope(&capped).unwrap();
        assert!(
            json.len() <= DEFAULT_ENVELOPE_MAX_BYTES,
            "len {}",
            json.len()
        );
        assert!(capped.elements.len() <= VIEWPORT_ENVELOPE_ELEMENT_CAP);
        assert!(capped.challenge.present);
        assert!(capped.challenge.yielded);
        assert_eq!(capped.challenge.kind.as_deref(), Some("recaptcha"));
        assert_eq!(capped.extract.cards.len(), 1);
        assert_eq!(capped.extract.cards[0].price, "$12,345");
        assert!(capped.elements.iter().all(|e| e.grid.is_some()));
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["challenge"]["present"], true);
        assert_eq!(parsed["extract"]["cards"][0]["price"], "$12,345");
    }

    #[test]
    fn sidecar_missing_viewport_deserializes() {
        let json = r#"{
            "schema": "hands.observe/v1",
            "session_id": "s",
            "screenshot_path": "C:\\tmp\\a.png",
            "observe_path": "C:\\tmp\\a.json",
            "space": {"origin_x":0,"origin_y":0,"width":10,"height":10,"cell_px":100},
            "extract": {"title":"T","url":null,"main_text":"","cards":[]},
            "elements": [],
            "elements_total": 0,
            "elements_truncated": false,
            "chrome_connected": false
        }"#;
        let side: ObserveSidecar = serde_json::from_str(json).unwrap();
        assert_eq!(side.viewport, None);
        assert_eq!(side.challenge, ChallengeInfo::default());
    }

    #[test]
    fn pick_still_uses_default_element_cap() {
        assert!(include_str!("pick.rs").contains("DEFAULT_ELEMENT_CAP"));
        assert_eq!(crate::extract::DEFAULT_ELEMENT_CAP, 250);
        assert_eq!(VIEWPORT_ENVELOPE_ELEMENT_CAP, 20);
    }

    #[test]
    fn mcp_and_cli_mention_viewport_and_dom() {
        let mcp = include_str!("mcp.rs");
        let main = include_str!("main.rs");
        assert!(mcp.contains("viewport"), "mcp observe description");
        assert!(mcp.contains("detail=dom") || mcp.contains("detail=dom"));
        assert!(mcp.contains("extract.dialogs") || mcp.contains("dialogs"));
        assert!(main.contains("viewport") || main.contains("foreground"));
        assert!(main.contains("dom"));
        assert!(main.contains("dialogs") || main.contains("extract.dialogs"));
        assert!(mcp.contains("grid") && mcp.contains("g:col:row"));
        assert!(main.contains("grid") && main.contains("g:col:row"));
        assert!(mcp.contains("chrome_hint"), "mcp observe description");
        assert!(
            mcp.contains("native-host-doctor") || mcp.contains("native_host_doctor"),
            "mcp observe description points at native-host-doctor"
        );
        assert!(
            mcp.contains("miles") && mcp.contains("dealer") && mcp.contains("empty_state"),
            "mcp observe description mentions listing fields"
        );
        assert!(
            main.contains("miles") && main.contains("dealer") && main.contains("empty_state"),
            "cli observe help mentions listing fields"
        );
    }

    #[test]
    fn mcp_and_cli_mention_runtime_id_and_page_local() {
        let mcp = include_str!("mcp.rs");
        let lower = mcp.to_ascii_lowercase();
        assert!(
            mcp.contains("RuntimeId") || lower.contains("runtime id"),
            "mcp.rs must mention RuntimeId"
        );
        assert!(
            lower.contains("page-local") || lower.contains("page local"),
            "mcp.rs must mention page-local"
        );
        assert!(
            lower.contains("navigation") || lower.contains("re-observe"),
            "mcp.rs must mention navigation or re-observe"
        );
    }

    #[test]
    fn disconnected_serialize_includes_chrome_hint() {
        let mut env = fat_envelope(0);
        env.chrome_connected = false;
        env.chrome_hint = Some(
            "Chrome host down — run hands native-host-doctor (MCP: native_host_doctor)".into(),
        );
        let json = serialize_envelope(&env).unwrap();
        assert!(json.contains("chrome_hint"));
        assert!(json.contains("native-host-doctor"));
    }

    #[test]
    fn connected_serialize_omits_chrome_hint() {
        let mut env = fat_envelope(0);
        env.chrome_connected = true;
        env.chrome_hint = None;
        let json = serialize_envelope(&env).unwrap();
        assert!(!json.contains("chrome_hint"));
    }

    #[test]
    fn sidecar_missing_chrome_hint_deserializes() {
        let json = r#"{
            "schema": "hands.observe/v1",
            "session_id": "s",
            "screenshot_path": "C:\\tmp\\a.png",
            "observe_path": "C:\\tmp\\a.json",
            "space": {"origin_x":0,"origin_y":0,"width":10,"height":10,"cell_px":100},
            "extract": {"title":"T","url":null,"main_text":"","cards":[]},
            "elements": [],
            "elements_total": 0,
            "elements_truncated": false,
            "chrome_connected": false
        }"#;
        let side: ObserveSidecar = serde_json::from_str(json).unwrap();
        assert_eq!(side.chrome_hint, None);
    }

    #[test]
    fn chrome_hint_survives_4kib_cap() {
        let mut raw = fat_envelope(400);
        raw.chrome_connected = false;
        raw.chrome_hint = Some(
            "Chrome host down — run hands native-host-doctor (MCP: native_host_doctor)".into(),
        );
        let capped = cap_default_envelope(raw);
        assert!(
            capped
                .chrome_hint
                .as_deref()
                .is_some_and(|h| h.contains("native-host-doctor")),
            "chrome_hint must survive 4 KiB shrink"
        );
        let json = serialize_envelope(&capped).unwrap();
        assert!(json.contains("chrome_hint"));
        assert!(json.contains("native-host-doctor"));
    }

    fn dialog_el(id: &str, text: &str, rect: Rect) -> Element {
        Element {
            id: id.into(),
            role: "Button".into(),
            text: Some(text.into()),
            rect,
            grid: None,
        }
    }

    fn dialog_hit(id: &str, text: &str, kind: &str) -> DialogHit {
        DialogHit {
            id: id.into(),
            role: "Button".into(),
            text: text.into(),
            rect: Rect {
                x: 200,
                y: 200,
                w: 80,
                h: 24,
            },
            kind: kind.into(),
        }
    }

    #[test]
    fn fixture_plus_continue_as_leads_envelope() {
        let g = chrome::EnvGuard::lock();
        g.set_snapshot(Some(&chrome::EnvGuard::fixture_path()));
        let map = chrome::try_snapshot(Detail::Default).expect("fixture");
        let mut nodes = viewport_buttons(8);
        nodes.push(uia_node(
            42,
            "Continue as Ryan",
            Rect {
                x: 220,
                y: 240,
                w: 120,
                h: 28,
            },
        ));
        nodes.extend(desktop_noise());
        let (mut extract, mut elements, elements_total, chrome_connected) = fuse_maps(
            Detail::Default,
            "UIA",
            &nodes,
            Some(map),
            fixture_opts(true),
        );
        crate::dialogs::promote(&mut extract, &mut elements);
        assert_eq!(extract.dialogs.len(), 1);
        assert_eq!(extract.dialogs[0].kind, "account");
        assert_eq!(extract.dialogs[0].id, "uia:1.42");
        assert_eq!(elements[0].id, "uia:1.42");
        let raw = ObserveEnvelope {
            session_id: "sess".into(),
            screenshot_path: "C:\\tmp\\observe.png".into(),
            observe_path: "C:\\tmp\\observe.json".into(),
            space: Space::new(0, 0, 1920, 1080).unwrap(),
            viewport: fixture_opts(true).viewport,
            extract,
            elements,
            elements_total,
            elements_truncated: false,
            chrome_connected,
            chrome_hint: None,
            challenge: ChallengeInfo::default(),
        };
        let capped = cap_default_envelope(raw);
        let json = serialize_envelope(&capped).unwrap();
        assert!(
            json.len() <= DEFAULT_ENVELOPE_MAX_BYTES,
            "len {}",
            json.len()
        );
        assert!(capped.elements.len() <= VIEWPORT_ENVELOPE_ELEMENT_CAP);
        assert_eq!(capped.extract.dialogs[0].id, "uia:1.42");
        assert_eq!(capped.elements[0].id, "uia:1.42");
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["extract"]["dialogs"][0]["kind"], "account");
    }

    #[test]
    fn chrome_cookie_chr_is_promoted() {
        let cookie = dialog_el(
            "chr:8",
            "Accept cookies",
            Rect {
                x: 200,
                y: 200,
                w: 90,
                h: 24,
            },
        );
        let chrome = chrome::ChromeMap {
            url: Some("https://cars.com/search".into()),
            title: "Cars.com".into(),
            main_text: "listings".into(),
            elements: vec![
                dialog_el(
                    "chr:0",
                    "Search",
                    Rect {
                        x: 110,
                        y: 150,
                        w: 80,
                        h: 24,
                    },
                ),
                cookie,
            ],
            cards: vec![],
            listing: crate::extract::ListingMeta::default(),
        };
        let (mut extract, mut elements, _total, connected) = fuse_maps(
            Detail::Default,
            "UIA",
            &[],
            Some(chrome),
            fixture_opts(true),
        );
        assert!(connected);
        crate::dialogs::promote(&mut extract, &mut elements);
        assert_eq!(extract.dialogs.len(), 1);
        assert_eq!(extract.dialogs[0].kind, "cookie");
        assert_eq!(extract.dialogs[0].id, "chr:8");
        assert_eq!(elements[0].id, "chr:8");
    }

    #[test]
    fn dialogs_cards_and_challenge_survive_4kib_shrink() {
        let mut raw = fat_envelope(400);
        raw.extract.main_text = "M".repeat(3000);
        raw.extract.cards = (0..8)
            .map(|i| Card {
                title: format!("2024 Toyota Camry {i}"),
                price: "$19,999".into(),
                href: format!("https://cars.com/vehicledetail/{i}"),
                rect: Rect {
                    x: 110,
                    y: 230,
                    w: 300,
                    h: 80,
                },
                miles: Some("32,145 mi".into()),
                dealer: Some("Capital Toyota".into()),
                distance: Some("12 mi away".into()),
                listing_of: None,
            })
            .collect();
        raw.extract.dialogs = vec![
            dialog_hit("uia:d.0", "Continue as Ryan", "account"),
            dialog_hit("chr:8", "Accept cookies", "cookie"),
            dialog_hit("uia:d.2", "Manage cookies", "cookie"),
            dialog_hit("uia:d.3", "Not now", "dialog"),
        ];
        raw.elements.splice(
            0..0,
            raw.extract.dialogs.iter().map(|d| Element {
                id: d.id.clone(),
                role: d.role.clone(),
                text: Some(d.text.clone()),
                rect: d.rect,
                grid: None,
            }),
        );
        raw.challenge = ChallengeInfo {
            present: true,
            kind: Some("recaptcha".into()),
            attempts: 2,
            yielded: true,
            reason: Some("i'm not a robot".into()),
        };
        stamp_grid(raw.space, &mut raw.elements);
        let capped = cap_default_envelope(raw);
        let json = serialize_envelope(&capped).unwrap();
        assert!(
            json.len() <= DEFAULT_ENVELOPE_MAX_BYTES,
            "len {}",
            json.len()
        );
        assert_eq!(capped.extract.dialogs.len(), 4);
        assert_eq!(capped.extract.cards.len(), 8);
        assert!(capped.challenge.present);
        assert!(capped.elements.iter().any(|e| e.id == "uia:d.0"));
        assert!(capped.elements.iter().all(|e| e.grid.is_some()));
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["extract"]["dialogs"].as_array().unwrap().len(), 4);
        assert_eq!(parsed["extract"]["cards"].as_array().unwrap().len(), 8);
        assert_eq!(parsed["extract"]["cards"][0]["miles"], "32,145 mi");
        assert_eq!(parsed["extract"]["cards"][0]["dealer"], "Capital Toyota");
        assert_eq!(parsed["extract"]["cards"][0]["distance"], "12 mi away");
        assert_eq!(parsed["challenge"]["present"], true);
    }

    #[test]
    fn empty_dialogs_omitted_from_json() {
        let raw = fat_envelope(2);
        assert!(raw.extract.dialogs.is_empty());
        let json = serialize_envelope(&raw).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["extract"].get("dialogs").is_none());
    }

    #[test]
    fn sidecar_missing_dialogs_deserializes() {
        let json = r#"{
            "schema": "hands.observe/v1",
            "session_id": "s",
            "screenshot_path": "C:\\tmp\\a.png",
            "observe_path": "C:\\tmp\\a.json",
            "space": {"origin_x":0,"origin_y":0,"width":10,"height":10,"cell_px":100},
            "extract": {"title":"T","url":null,"main_text":"","cards":[]},
            "elements": [],
            "elements_total": 0,
            "elements_truncated": false,
            "chrome_connected": false
        }"#;
        let side: ObserveSidecar = serde_json::from_str(json).unwrap();
        assert!(side.extract.dialogs.is_empty());
        assert_eq!(side.viewport, None);
    }

    #[test]
    fn stamp_grid_fixture_chr0_is_g21() {
        let g = chrome::EnvGuard::lock();
        g.set_snapshot(Some(&chrome::EnvGuard::fixture_path()));
        let map = chrome::try_snapshot(Detail::Default).expect("fixture");
        let (_extract, mut elements, _total, _connected) =
            fuse_maps(Detail::Default, "UIA", &[], Some(map), fixture_opts(true));
        let space = Space::new(0, 0, 1920, 1080).unwrap();
        stamp_grid(space, &mut elements);
        let chr0 = elements.iter().find(|e| e.id == "chr:0").expect("chr:0");
        assert_eq!(
            chr0.rect,
            Rect {
                x: 110,
                y: 150,
                w: 200,
                h: 32,
            }
        );
        assert_eq!(chr0.rect.center(), (210, 166));
        assert_eq!(chr0.grid.as_deref(), Some("g:2:1"));
        assert_eq!(chr0.grid.as_ref().unwrap(), &space.cell_id(210, 166));
    }

    #[test]
    fn stamp_grid_negative_origin_is_not_g00() {
        let space = Space::new(-1920, 0, 3840, 1080).unwrap();
        let mut elements = vec![Element {
            id: "chr:origin".into(),
            role: "Edit".into(),
            text: Some("origin".into()),
            rect: Rect {
                x: -1,
                y: -1,
                w: 2,
                h: 2,
            },
            grid: None,
        }];
        stamp_grid(space, &mut elements);
        assert_eq!(elements[0].rect.center(), (0, 0));
        assert_eq!(elements[0].grid.as_deref(), Some("g:19:0"));
        assert_ne!(elements[0].grid.as_deref(), Some("g:0:0"));
    }

    #[test]
    fn sidecar_missing_grid_deserializes() {
        let json = r#"{
            "schema": "hands.observe/v1",
            "session_id": "s",
            "screenshot_path": "C:\\tmp\\a.png",
            "observe_path": "C:\\tmp\\a.json",
            "space": {"origin_x":0,"origin_y":0,"width":10,"height":10,"cell_px":100},
            "extract": {"title":"T","url":null,"main_text":"","cards":[]},
            "elements": [{"id":"chr:0","role":"Edit","text":"Search","rect":{"x":110,"y":150,"w":200,"h":32}}],
            "elements_total": 1,
            "elements_truncated": false,
            "chrome_connected": false
        }"#;
        let side: ObserveSidecar = serde_json::from_str(json).unwrap();
        assert_eq!(side.elements.len(), 1);
        assert_eq!(side.elements[0].id, "chr:0");
        assert_eq!(side.elements[0].grid, None);
    }

    #[test]
    fn empty_grid_omitted_from_serialized_json() {
        let el = Element {
            id: "chr:0".into(),
            role: "Edit".into(),
            text: Some("Search".into()),
            rect: Rect {
                x: 110,
                y: 150,
                w: 200,
                h: 32,
            },
            grid: None,
        };
        let raw = serde_json::to_value(&el).unwrap();
        assert!(raw.get("grid").is_none());
        let mut elements = vec![el];
        stamp_grid(Space::new(0, 0, 1920, 1080).unwrap(), &mut elements);
        let stamped = serde_json::to_value(&elements[0]).unwrap();
        assert_eq!(stamped["grid"], "g:2:1");
    }

    #[test]
    fn popup_rect_membership_union() {
        let popup = Rect {
            x: 1400,
            y: 80,
            w: 220,
            h: 180,
        };
        let nodes = vec![uia_node(
            77,
            "Continue as Ryan",
            Rect {
                x: 1410,
                y: 100,
                w: 160,
                h: 28,
            },
        )];
        let mut opts = fixture_opts(false);
        opts.popup_rect = Some(popup);
        let (mut extract, mut elements, total, _connected) =
            fuse_maps(Detail::Default, "Chrome", &nodes, None, opts);
        assert_eq!(total, 1);
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0].id, "uia:1.77");
        crate::dialogs::promote(&mut extract, &mut elements);
        assert_eq!(extract.dialogs[0].id, "uia:1.77");
        assert_eq!(elements[0].id, "uia:1.77");
    }

    #[test]
    fn all_frames_still_false_and_no_dwm() {
        let manifest = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/extension/manifest.json"
        ));
        assert!(
            manifest.contains("\"all_frames\": false"),
            "all_frames must stay false"
        );
        assert_eq!(crate::extract::DEFAULT_ELEMENT_CAP, 250);
        assert_eq!(VIEWPORT_ENVELOPE_ELEMENT_CAP, 20);
        assert!(include_str!("pick.rs").contains("fn cap_elements"));
    }

    #[test]
    fn cargo_forbids_dwm() {
        let cargo = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
        assert!(
            !cargo.contains("Win32_Graphics_Dwm"),
            "Cargo.toml must not enable DWM"
        );
        assert!(
            !cargo.to_ascii_lowercase().contains("dwmget"),
            "Cargo.toml must not mention DwmGet"
        );
    }

    #[test]
    fn sidecar_missing_listing_fields_deserializes() {
        let json = r#"{
            "schema": "hands.observe/v1",
            "session_id": "s",
            "screenshot_path": "C:\\tmp\\a.png",
            "observe_path": "C:\\tmp\\a.json",
            "space": {"origin_x":0,"origin_y":0,"width":10,"height":10,"cell_px":100},
            "extract": {"title":"T","url":null,"main_text":"","cards":[{"title":"c","price":"$1","href":"https://example.com","rect":{"x":1,"y":1,"w":2,"h":2}}]},
            "elements": [],
            "elements_total": 0,
            "elements_truncated": false,
            "chrome_connected": false
        }"#;
        let side: ObserveSidecar = serde_json::from_str(json).unwrap();
        assert!(side.extract.result_count.is_none());
        assert!(side.extract.local_matches.is_none());
        assert!(side.extract.empty_state.is_none());
        assert!(side.extract.zip.is_none());
        assert!(side.extract.radius.is_none());
        assert_eq!(side.extract.cards.len(), 1);
        assert!(side.extract.cards[0].miles.is_none());
        assert!(side.extract.cards[0].dealer.is_none());
        assert!(side.extract.cards[0].distance.is_none());
        assert!(side.extract.cards[0].listing_of.is_none());
    }

    #[test]
    fn empty_listing_fields_omitted_from_envelope_json() {
        let raw = fat_envelope(2);
        assert!(raw.extract.result_count.is_none());
        assert!(raw.extract.empty_state.is_none());
        let json = serialize_envelope(&raw).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        for key in [
            "result_count",
            "local_matches",
            "empty_state",
            "zip",
            "radius",
        ] {
            assert!(
                parsed["extract"].get(key).is_none(),
                "{key} should be omitted"
            );
        }
    }

    #[test]
    fn empty_state_from_main_text_is_not_challenge() {
        let extract = extract_fused(
            "Results",
            &[],
            Some("https://www.cars.com/shopping/results/?zip=32309&maximum_distance=50"),
            "Cars.com",
            "Nothing fits those filters. Try a larger radius.",
            Vec::new(),
            crate::extract::ListingMeta::default(),
        );
        assert!(
            extract
                .empty_state
                .as_deref()
                .unwrap()
                .to_ascii_lowercase()
                .contains("nothing fits those filters")
        );
        assert!(extract.cards.is_empty());
        let hit = challenge::detect_from_extract(
            &extract.title,
            extract.url.as_deref(),
            &extract.main_text,
            &[],
        );
        assert!(!hit.present);
        let mut env = fat_envelope(0);
        env.extract = extract;
        env.challenge = ChallengeInfo::default();
        assert!(!env.challenge.present);
        let json = serialize_envelope(&env).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["challenge"]["present"], false);
    }

    #[test]
    fn zip_and_radius_from_cars_url_when_empty() {
        let extract = extract_fused(
            "Results",
            &[],
            Some("https://www.cars.com/shopping/results/?zip=32309&maximum_distance=50"),
            "Cars.com",
            "listings",
            Vec::new(),
            crate::extract::ListingMeta::default(),
        );
        assert_eq!(extract.zip.as_deref(), Some("32309"));
        assert_eq!(extract.radius.as_deref(), Some("50 mi"));
    }
}
