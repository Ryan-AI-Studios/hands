use serde::Serialize;

use crate::capture::{capture_virtual_screen, display_path};
use crate::error::HandsError;
use crate::extract::{Detail, Element, Extract, extract_from_nodes, filter_nodes};
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
}

#[derive(Debug, Clone, Serialize)]
pub struct ObserveSidecar {
    pub schema: &'static str,
    pub session_id: String,
    pub screenshot_path: String,
    pub observe_path: String,
    pub space: Space,
    pub extract: Extract,
    pub elements: Vec<Element>,
    pub elements_total: usize,
    pub elements_truncated: bool,
}

pub fn observe(req: ObserveRequest) -> Result<ObserveEnvelope, HandsError> {
    ensure_dpi()?;
    let session_id = resolve_session_id_from_os(req.session_id.as_deref());
    let space = virtual_screen()?;
    let paths = capture_virtual_screen(space)?;
    let screenshot_path = display_path(&paths.screenshot_path);
    let observe_path = display_path(&paths.observe_path);

    let snap = uia::collect(req.detail)?;
    let (elements, elements_total) = filter_nodes(&snap.nodes, req.detail);
    let extract = extract_from_nodes(&snap.title, &snap.nodes);
    crate::fence::note_last_url(extract.url.as_deref());

    let full = ObserveEnvelope {
        session_id,
        screenshot_path,
        observe_path,
        space,
        extract,
        elements,
        elements_total,
        elements_truncated: false,
    };
    write_sidecar(&paths.observe_path, &full)?;
    finalize_envelope(full)
}

fn write_sidecar(path: &std::path::Path, envelope: &ObserveEnvelope) -> Result<(), HandsError> {
    let sidecar = ObserveSidecar {
        schema: OBSERVE_SCHEMA,
        session_id: envelope.session_id.clone(),
        screenshot_path: envelope.screenshot_path.clone(),
        observe_path: envelope.observe_path.clone(),
        space: envelope.space,
        extract: envelope.extract.clone(),
        elements: envelope.elements.clone(),
        elements_total: envelope.elements_total,
        elements_truncated: false,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::Element;
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
        }
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
}
