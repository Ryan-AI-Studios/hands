use serde::{Deserialize, Serialize};

use crate::capture::{capture_virtual_screen, display_path};
use crate::challenge::{self, ChallengeInfo};
use crate::chrome;
use crate::error::HandsError;
use crate::extract::{Detail, Element, Extract, extract_from_nodes, extract_fused, filter_nodes};
use crate::logs;
use crate::session::resolve_session_id_from_os;
use crate::space::{Space, ensure_dpi, virtual_screen};
use crate::uia;

pub const ENVELOPE_MAX_BYTES: usize = 16_384;
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
    pub extract: Extract,
    pub elements: Vec<Element>,
    pub elements_total: usize,
    pub elements_truncated: bool,
    pub chrome_connected: bool,
    pub challenge: ChallengeInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObserveSidecar {
    pub schema: String,
    pub session_id: String,
    pub screenshot_path: String,
    pub observe_path: String,
    pub space: Space,
    pub extract: Extract,
    pub elements: Vec<Element>,
    pub elements_total: usize,
    pub elements_truncated: bool,
    pub chrome_connected: bool,
    #[serde(default)]
    pub challenge: ChallengeInfo,
}

pub fn observe(req: ObserveRequest) -> Result<ObserveEnvelope, HandsError> {
    ensure_dpi()?;
    let session_id = resolve_session_id_from_os(req.session_id.as_deref());
    logs::check_write_id(&session_id)?;
    let space = virtual_screen()?;
    let paths = capture_virtual_screen(space)?;
    let screenshot_path = display_path(&paths.screenshot_path);
    let observe_path = display_path(&paths.observe_path);

    let snap = uia::collect(req.detail)?;
    let chrome = chrome::try_snapshot(req.detail);
    let (extract, elements, elements_total, chrome_connected) =
        fuse_maps(req.detail, &snap.title, &snap.nodes, chrome);
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
        extract,
        elements,
        elements_total,
        elements_truncated: false,
        chrome_connected,
        challenge,
    };
    write_sidecar(&paths.observe_path, &full)?;
    let envelope = finalize_envelope(full)?;
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

fn write_sidecar(path: &std::path::Path, envelope: &ObserveEnvelope) -> Result<(), HandsError> {
    let sidecar = ObserveSidecar {
        schema: OBSERVE_SCHEMA.to_string(),
        session_id: envelope.session_id.clone(),
        screenshot_path: envelope.screenshot_path.clone(),
        observe_path: envelope.observe_path.clone(),
        space: envelope.space,
        extract: envelope.extract.clone(),
        elements: envelope.elements.clone(),
        elements_total: envelope.elements_total,
        elements_truncated: false,
        chrome_connected: envelope.chrome_connected,
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
) -> (Extract, Vec<Element>, usize, bool) {
    let chrome_connected = chrome.is_some();
    let (uia_els, uia_matched) = filter_nodes(uia_nodes, detail);
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
            );
            (map.elements, n, extract)
        }
        None => (Vec::new(), 0, extract_from_nodes(uia_title, uia_nodes)),
    };
    let mut elements = chrome_els;
    elements.extend(uia_els);
    let cap = detail.element_cap();
    if elements.len() > cap {
        elements.truncate(cap);
    }
    (extract, elements, chrome_n + uia_matched, chrome_connected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{Card, Element};
    use crate::space::Rect;

    fn fat_envelope(n: usize) -> ObserveEnvelope {
        ObserveEnvelope {
            session_id: "sess".into(),
            screenshot_path: "C:\\tmp\\observe.png".into(),
            observe_path: "C:\\tmp\\observe.json".into(),
            space: Space::new(0, 0, 1920, 1080).unwrap(),
            extract: Extract {
                title: "Title".into(),
                url: None,
                main_text: "hello".into(),
                cards: Vec::new(),
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
                })
                .collect(),
            elements_total: n,
            elements_truncated: false,
            chrome_connected: false,
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
                })
                .collect(),
            cards: vec![],
        };
        let nodes: Vec<RawNode> = (0..5).map(uia).collect();
        let (extract, els, total, connected) =
            fuse_maps(Detail::Default, "UIA title", &nodes, Some(chrome));
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
        let (extract, els, _total, connected) = fuse_maps(Detail::Default, "UIA", &[], Some(map));
        assert!(connected);
        assert_eq!(extract.url.as_deref(), Some("https://cars.com/search"));
        assert!(els.iter().any(|e| e.id == "chr:0"));
        assert_eq!(extract.cards.len(), 1);
        assert_eq!(extract.cards[0].price, "$12,345");
        crate::fence::note_last_url(extract.url.as_deref());
        assert_eq!(
            crate::fence::last_url().as_deref(),
            Some("https://cars.com/search")
        );
    }

    #[test]
    fn absent_chrome_extract_is_uia_only() {
        let (extract, els, total, connected) = fuse_maps(Detail::Default, "Desktop", &[], None);
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
        }];
        let raw_len = serialize_envelope(&raw).unwrap().len();
        assert!(raw_len > ENVELOPE_MAX_BYTES, "fixture too small: {raw_len}");
        let capped = cap_envelope(raw);
        assert_eq!(capped.extract.cards.len(), 1);
        assert_eq!(capped.extract.cards[0].price, "$12,345");
        assert!(serialize_envelope(&capped).unwrap().len() <= ENVELOPE_MAX_BYTES);
    }
}
