//! Snapshot-bound target identity: canonical `sref1:` refs and their evidence.
//!
//! A Chrome ref is bound to the answering tab, the content-script document instance and
//! the observe snapshot that produced it. A UIA ref is bound to the observation walk HWND,
//! an own-role/name fingerprint and the RuntimeId hash. Parsing is exact and canonical:
//! a malformed `sref1:` is a hard target error and never degrades to the legacy `chr:` /
//! `uia:` grammar.

use serde::{Deserialize, Serialize};

use crate::error::HandsError;

const REF_PREFIX: &str = "sref1:";
const MAX_REF_CHARS: usize = 64;
const FNV32_OFFSET: u32 = 0x811c_9dc5;
const FNV32_PRIME: u32 = 0x0100_0193;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Binding {
    Scoped,
    Legacy,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdentityState {
    Exact,
    Stale,
    Ambiguous,
    Unknown,
}

impl IdentityState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Stale => "stale",
            Self::Ambiguous => "ambiguous",
            Self::Unknown => "unknown",
        }
    }
}

/// Additive identity report for observe elements/resolve refusals and actuate envelopes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityReport {
    pub binding: Binding,
    pub state: IdentityState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<u32>,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl IdentityReport {
    fn base(binding: Binding, state: IdentityState) -> Self {
        Self {
            binding,
            state,
            reason: None,
            r#ref: None,
            snapshot: None,
            tab: None,
            window: None,
            frame: None,
            retryable: false,
            hint: None,
        }
    }

    pub fn exact_chrome(r: &SnapshotRef) -> Self {
        let SnapshotRef::Chrome {
            tab, frame, snap, ..
        } = r
        else {
            return Self::unknown(None, "protocol_error");
        };
        let mut out = Self::base(Binding::Scoped, IdentityState::Exact);
        out.r#ref = Some(r.to_string());
        out.snapshot = Some(snap.clone());
        out.tab = Some(*tab);
        out.frame = Some(*frame);
        out
    }

    pub fn chrome_failure(r: &SnapshotRef, state: IdentityState, reason: &str) -> Self {
        let mut out = Self::base(Binding::Scoped, state);
        out.r#ref = Some(r.to_string());
        if let SnapshotRef::Chrome {
            tab, frame, snap, ..
        } = r
        {
            out.snapshot = Some(snap.clone());
            out.tab = Some(*tab);
            out.frame = Some(*frame);
        }
        out.reason = Some(reason.to_string());
        out.retryable = reason == "resolve_timeout";
        out.hint = Some(hint_for(state, reason));
        out
    }

    pub fn exact_uia(r: &SnapshotRef) -> Self {
        let mut out = Self::base(Binding::Scoped, IdentityState::Exact);
        out.r#ref = Some(r.to_string());
        if let SnapshotRef::Uia { hwnd, .. } = r {
            out.tab = None;
            out.window = Some(*hwnd as u32);
            out.frame = Some(0);
        }
        out
    }

    pub fn uia_failure(r: &SnapshotRef, state: IdentityState, reason: &str) -> Self {
        let mut out = Self::base(Binding::Scoped, state);
        out.r#ref = Some(r.to_string());
        if let SnapshotRef::Uia { hwnd, .. } = r {
            out.window = Some(*hwnd as u32);
            out.frame = Some(0);
        }
        out.reason = Some(reason.to_string());
        out.retryable = reason == "resolve_timeout";
        out.hint = Some(hint_for(state, reason));
        out
    }

    /// Legacy `chr:` / `uia:` ids keep working but are never advertised as stale-safe.
    pub fn legacy() -> Self {
        Self::base(Binding::Legacy, IdentityState::Exact)
    }

    pub fn none() -> Self {
        Self::base(Binding::None, IdentityState::Exact)
    }

    pub fn unknown(r: Option<&SnapshotRef>, reason: &str) -> Self {
        let mut out = Self::base(Binding::None, IdentityState::Unknown);
        if let Some(r) = r {
            out.binding = Binding::Scoped;
            out.r#ref = Some(r.to_string());
        }
        out.reason = Some(reason.to_string());
        out.retryable = reason == "resolve_timeout";
        out.hint = Some(hint_for(IdentityState::Unknown, reason));
        out
    }

    pub fn with_window(mut self, window: Option<u32>) -> Self {
        self.window = window;
        self
    }

    pub fn with_reason(mut self, reason: &str) -> Self {
        self.reason = Some(reason.to_string());
        self.hint = Some(hint_for(self.state, reason));
        self
    }

    pub fn state_token(&self) -> &'static str {
        self.state.as_str()
    }
}

fn hint_for(state: IdentityState, reason: &str) -> String {
    let text = match reason {
        "binding_changed_after_dispatch" => {
            "a click may already have been sent; re-observe and verify the page before any further action; do not re-click this ref blindly"
        }
        "transform_invalid" => {
            "re-observe the target; the point transform refused and nothing was sent"
        }
        "tab_not_active" | "window_not_focused" | "tab_hidden" => {
            "activate the target tab in the focused Chrome window, then retry"
        }
        "not_chrome" => "activate the target Chrome window, then retry",
        "tab_unavailable" | "no_tab" => "re-observe; the tab no longer exists",
        "no_content" | "unbound_extension" => {
            "reload the Helping Hands extension and the target tab, then re-observe"
        }
        "host_down" | "resolve_timeout" | "provider_timeout" => {
            "check the native host or retry observe"
        }
        _ => match state {
            IdentityState::Stale => "re-observe the page and use the new ref",
            IdentityState::Ambiguous => {
                "re-observe; more than one live element matches this identity"
            }
            IdentityState::Unknown => "re-observe; identity evidence is incomplete",
            IdentityState::Exact => "identity is exact",
        },
    };
    text.to_string()
}

/// Snapshot block on the observe envelope/sidecar.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SnapshotBinding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u32>,
    #[serde(default)]
    pub frame: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hwnd: Option<String>,
    #[serde(default)]
    pub doc: String,
    #[serde(default)]
    pub snap: String,
    #[serde(default)]
    pub retained: usize,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub fallback: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl SnapshotBinding {
    pub fn has_scoped_refs(&self) -> bool {
        !self.fallback
            && self.tab.is_some_and(|t| t > 0)
            && self.doc.len() == 4
            && self.snap.len() == 8
    }
}

/// The canonical new-format target reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotRef {
    Chrome {
        tab: u32,
        frame: u32,
        doc: String,
        snap: String,
        idx: u32,
    },
    Uia {
        hwnd: u64,
        fp: String,
        ridh: String,
    },
}

impl SnapshotRef {
    pub fn parse(raw: &str) -> Result<Self, HandsError> {
        let text = raw;
        if text.len() > MAX_REF_CHARS {
            return Err(malformed("ref is longer than 64 characters"));
        }
        let Some(rest) = text.strip_prefix(REF_PREFIX) else {
            if text.starts_with("sref:") || text.starts_with("sref") {
                return Err(HandsError::Target(format!(
                    "sref1: unsupported_ref_version: expected sref1: (got '{text}')"
                )));
            }
            return Err(malformed(&format!("missing sref1: prefix (got '{text}')")));
        };
        let parts: Vec<&str> = rest.split('.').collect();
        match parts.as_slice() {
            ["c", tab, frame, doc, snap, idx] => {
                let tab = parse_base36(tab, 7, false)?;
                let frame = parse_base36(frame, 7, true)?;
                let idx = parse_base36(idx, 4, true)?;
                require_lower_hex(doc, 4, "doc")?;
                require_lower_hex(snap, 8, "snap")?;
                Ok(Self::Chrome {
                    tab,
                    frame,
                    doc: (*doc).to_string(),
                    snap: (*snap).to_string(),
                    idx,
                })
            }
            ["u", hwnd, fp, ridh] => {
                require_hex_range(hwnd, 1, 16, "hwnd")?;
                if hwnd.len() > 1 && hwnd.starts_with('0') {
                    return Err(malformed("hwnd must not have leading zeros"));
                }
                let hwnd = u64::from_str_radix(hwnd, 16)
                    .map_err(|_| malformed("hwnd is not lowercase hex"))?;
                if hwnd == 0 {
                    return Err(malformed("hwnd must be > 0"));
                }
                require_lower_hex(fp, 8, "fp")?;
                require_lower_hex(ridh, 8, "ridh")?;
                Ok(Self::Uia {
                    hwnd,
                    fp: (*fp).to_string(),
                    ridh: (*ridh).to_string(),
                })
            }
            _ => Err(malformed("wrong field count or kind")),
        }
    }

    pub fn tab(&self) -> Option<u32> {
        match self {
            Self::Chrome { tab, .. } => Some(*tab),
            Self::Uia { .. } => None,
        }
    }

    pub fn frame(&self) -> u32 {
        match self {
            Self::Chrome { frame, .. } => *frame,
            Self::Uia { .. } => 0,
        }
    }
}

impl std::fmt::Display for SnapshotRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Chrome {
                tab,
                frame,
                doc,
                snap,
                idx,
            } => write!(
                f,
                "sref1:c.{}.{}.{}.{}.{}",
                base36(*tab),
                base36(*frame),
                doc,
                snap,
                base36(*idx)
            ),
            Self::Uia { hwnd, fp, ridh } => {
                write!(f, "sref1:u.{hwnd:x}.{fp}.{ridh}")
            }
        }
    }
}

/// Legacy id classification. A `sref1:` token is never returned here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyKind {
    Chr(u32),
    Uia(Vec<i32>),
    Grid { col: i32, row: i32 },
    Pixel { x: i32, y: i32 },
}

pub fn parse_legacy(id: &str) -> Option<LegacyKind> {
    if id.starts_with("sref") {
        return None;
    }
    if let Some(rest) = id.strip_prefix("chr:")
        && canonical_u32(rest).is_some()
    {
        return canonical_u32(rest).map(LegacyKind::Chr);
    }
    if let Some(rest) = id.strip_prefix("uia:") {
        let ids = rest
            .split('.')
            .map(str::parse::<i32>)
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        if !ids.is_empty() {
            return Some(LegacyKind::Uia(ids));
        }
    }
    if let Some(rest) = id.strip_prefix("g:") {
        let mut parts = rest.split(':');
        if let (Some(col), Some(row), None) = (parts.next(), parts.next(), parts.next())
            && let (Ok(col), Ok(row)) = (col.parse(), row.parse())
        {
            return Some(LegacyKind::Grid { col, row });
        }
    }
    if let Some((x, y)) = id.split_once(',')
        && let (Ok(x), Ok(y)) = (x.trim().parse(), y.trim().parse())
    {
        return Some(LegacyKind::Pixel { x, y });
    }
    None
}

fn canonical_u32(rest: &str) -> Option<u32> {
    if rest.is_empty()
        || rest.starts_with('+')
        || rest.starts_with('-')
        || (rest.len() > 1 && rest.starts_with('0'))
        || !rest.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    rest.parse::<u32>().ok()
}

fn malformed(detail: &str) -> HandsError {
    HandsError::Target(format!("sref1: malformed_ref: {detail}"))
}

fn parse_base36(raw: &str, max_len: usize, allow_zero: bool) -> Result<u32, HandsError> {
    let bytes = raw.as_bytes();
    if bytes.is_empty() || bytes.len() > max_len {
        return Err(malformed("base36 field length is out of range"));
    }
    if bytes.len() > 1 && bytes[0] == b'0' {
        return Err(malformed("base36 field must not have leading zeros"));
    }
    let mut value: u64 = 0;
    for b in bytes {
        let digit = match b {
            b'0'..=b'9' => u64::from(b - b'0'),
            b'a'..=b'z' => u64::from(b - b'a') + 10,
            _ => return Err(malformed("base36 field is not lowercase base36")),
        };
        value = value * 36 + digit;
        if value > u64::from(u32::MAX) {
            return Err(malformed("base36 field overflows u32"));
        }
    }
    let value = value as u32;
    if value == 0 && !allow_zero {
        return Err(malformed("field must be > 0"));
    }
    Ok(value)
}

fn require_lower_hex(raw: &str, len: usize, field: &str) -> Result<(), HandsError> {
    if raw.len() != len
        || !raw
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(malformed(&format!(
            "{field} must be exactly {len} lowercase hex characters"
        )));
    }
    Ok(())
}

fn require_hex_range(raw: &str, min: usize, max: usize, field: &str) -> Result<(), HandsError> {
    if raw.len() < min
        || raw.len() > max
        || !raw
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(malformed(&format!(
            "{field} must be {min}–{max} lowercase hex characters"
        )));
    }
    Ok(())
}

fn base36(mut value: u32) -> String {
    if value == 0 {
        return "0".to_string();
    }
    let mut digits = Vec::new();
    while value > 0 {
        let d = (value % 36) as u8;
        digits.push(if d < 10 {
            char::from(b'0' + d)
        } else {
            char::from(b'a' + d - 10)
        });
        value /= 36;
    }
    digits.iter().rev().collect()
}

fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut hash = FNV32_OFFSET;
    for b in bytes {
        hash ^= u32::from(*b);
        hash = hash.wrapping_mul(FNV32_PRIME);
    }
    hash
}

/// Own-role/name shape fingerprint. Values, rects and ancestor display names are excluded.
/// `(None, None)` is the password shape; `(None, Some(true))` is the unnamed shape.
pub fn element_fingerprint(role: &str, text: Option<&str>, unnamed: Option<bool>) -> u32 {
    let (marker, name) = match (text, unnamed) {
        (_, Some(true)) => ('u', ""),
        (None, None) => ('p', ""),
        (None, Some(false)) | (Some(""), _) => ('e', ""),
        (Some(value), _) => ('n', value),
    };
    let mut input = String::with_capacity(role.len() + name.len() + 4);
    input.push(marker);
    input.push('\u{1f}');
    input.push_str(role);
    input.push('\u{1f}');
    input.push_str(name);
    fnv1a32(input.as_bytes())
}

pub fn fingerprint_hex(role: &str, text: Option<&str>, unnamed: Option<bool>) -> String {
    format!("{:08x}", element_fingerprint(role, text, unnamed))
}

/// FNV-1a32 over the dotted RuntimeId, formatted as exactly 8 lowercase hex.
pub fn runtime_id_hash(ids: &[i32]) -> String {
    let dotted = ids.iter().map(i32::to_string).collect::<Vec<_>>().join(".");
    format!("{:08x}", fnv1a32(dotted.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sref_grammar_accept_table() {
        let chrome = "sref1:c.c.0.9f3a.a1b2c3d4.7";
        let parsed = SnapshotRef::parse(chrome).expect("chrome ref");
        assert_eq!(parsed.to_string(), chrome);
        assert_eq!(parsed.tab(), Some(12));

        let uia = "sref1:u.1a2b3c.7d0e1f22.42a1b2c3";
        let parsed = SnapshotRef::parse(uia).expect("uia ref");
        assert_eq!(parsed.to_string(), uia);
        assert_eq!(parsed.tab(), None);

        for (raw, canonical) in [
            ("sref1:c.1.0.0000.00000000.0", "sref1:c.1.0.0000.00000000.0"),
            (
                "sref1:c.zz.1.ffff.ffffffff.zz",
                "sref1:c.zz.1.ffff.ffffffff.zz",
            ),
        ] {
            assert_eq!(SnapshotRef::parse(raw).unwrap().to_string(), canonical);
        }
    }

    #[test]
    fn sref_grammar_reject_table() {
        for raw in [
            "sref:1",
            "sref2:c.1.0.0000.00000000.0",
            "SREF1:c.1.0.0000.00000000.0",
            "sref1:",
            "sref1:x.1",
            "sref1:c",
            "sref1:c.1.0.0000.00000000",
            "sref1:c.1.0.0000.00000000.0.1",
            "sref1:c.0.0.0000.00000000.0",
            "sref1:c.01.0.0000.00000000.0",
            "sref1:c.1.0.9F3A.00000000.0",
            "sref1:c.1.0.0000.A1B2C3D4.0",
            "sref1:c.1.0.000.00000000.0",
            "sref1:c.1.0.0000.0000000.0",
            "sref1:c.1.0.0000.00000000.00000",
            "sref1:c.1.0.0000.00000000.+1",
            "sref1:u.0.7d0e1f22.42a1b2c3",
            "sref1:u.01a.7d0e1f22.42a1b2c3",
            "sref1:u.1a2b3c.7D0E1F22.42a1b2c3",
            "sref1:u.1a2b3c.7d0e1f22",
            "  sref1:c.1.0.0000.00000000.0",
        ] {
            let err = SnapshotRef::parse(raw).expect_err(raw);
            let msg = err.to_string();
            assert!(msg.contains("sref1:"), "{raw}: {msg}");
        }
    }

    #[test]
    fn chrome_ref_round_trip() {
        let r = SnapshotRef::Chrome {
            tab: 7,
            frame: 0,
            doc: "00ff".into(),
            snap: "deadbeef".into(),
            idx: 35,
        };
        assert_eq!(r.to_string(), "sref1:c.7.0.00ff.deadbeef.z");
        assert_eq!(SnapshotRef::parse(&r.to_string()).unwrap(), r);
    }

    #[test]
    fn uia_ref_round_trip() {
        let r = SnapshotRef::Uia {
            hwnd: 0x1a2b3c,
            fp: "7d0e1f22".into(),
            ridh: "42a1b2c3".into(),
        };
        assert_eq!(r.to_string(), "sref1:u.1a2b3c.7d0e1f22.42a1b2c3");
        assert_eq!(SnapshotRef::parse(&r.to_string()).unwrap(), r);
    }

    #[test]
    fn legacy_ids_are_not_scoped() {
        assert!(matches!(parse_legacy("chr:0"), Some(LegacyKind::Chr(0))));
        assert!(matches!(
            parse_legacy("uia:42.1"),
            Some(LegacyKind::Uia(ids)) if ids == vec![42, 1]
        ));
        assert!(matches!(
            parse_legacy("g:1:2"),
            Some(LegacyKind::Grid { col: 1, row: 2 })
        ));
        assert!(parse_legacy("10,20").is_some());
        assert!(parse_legacy("sref1:c.1.0.0000.00000000.0").is_none());
        assert!(parse_legacy("sref:1").is_none());
    }

    #[test]
    fn fingerprint_ignores_value_and_rect() {
        let a = element_fingerprint("Edit", Some("Name"), None);
        let b = element_fingerprint("Edit", Some("Name"), None);
        assert_eq!(a, b);
        assert_ne!(a, element_fingerprint("Edit", Some("Name2"), None));
        assert_ne!(a, element_fingerprint("Button", Some("Name"), None));
        assert_eq!(
            element_fingerprint("Edit", None, None),
            element_fingerprint("Edit", None, None)
        );
        assert_ne!(
            element_fingerprint("Edit", None, None),
            element_fingerprint("Edit", None, Some(true))
        );
        assert_ne!(
            element_fingerprint("Edit", Some("unnamed"), None),
            element_fingerprint("Edit", None, Some(true))
        );
        assert_ne!(
            element_fingerprint("Edit", Some("password"), None),
            element_fingerprint("Edit", None, None)
        );
        assert_eq!(format!("{a:08x}").len(), 8);
    }

    #[test]
    fn runtime_hash_stable() {
        assert_eq!(runtime_id_hash(&[42, 1]), runtime_id_hash(&[42, 1]));
        assert_ne!(runtime_id_hash(&[42, 1]), runtime_id_hash(&[42, 2]));
        assert_eq!(runtime_id_hash(&[42, 1]).len(), 8);
        assert!(
            runtime_id_hash(&[42, 1])
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
    }

    #[test]
    fn ref_charset_carries_no_page_content() {
        for raw in [
            "sref1:c.c.0.9f3a.a1b2c3d4.7",
            "sref1:u.1a2b3c.7d0e1f22.42a1b2c3",
        ] {
            assert!(
                raw.bytes().all(|b| b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || b == b':'
                    || b == b'.')
            );
        }
    }

    #[test]
    fn maximal_emitted_ref_json_length_is_bounded() {
        // 0123 emits frame 0 only; nonzero frames are 0131 and must use the compact encoding
        // before they are emitted. Pin the emitted frame-0 maximum against the DoD-4 45-byte
        // `"ref":` key+value bound.
        let r = SnapshotRef::Chrome {
            tab: u32::MAX,
            frame: 0,
            doc: "ffff".into(),
            snap: "ffffffff".into(),
            idx: 1_679_615,
        };
        let canonical = r.to_string();
        assert!(canonical.len() <= 64, "{canonical}");
        let json_key_len = "\"ref\":".len() + serde_json::to_string(&canonical).unwrap().len();
        assert!(
            json_key_len <= 45,
            "emitted frame-0 ref key+value is {json_key_len} bytes ({canonical})"
        );
    }
}
