//! 0130 `changes`: a read-only, bounded poll against the extension's stored baseline.
//!
//! The extension keeps a per-document content revision (`rev`) with each agent-facing Chrome
//! observe. This module composes that revision into a tab-scoped public token
//! (`<tab>.<doc>.<rev>.<fp16>.<shape4>`), validates tokens, and maps one `snapshot` poll reply to a
//! bounded envelope. It never observes, never mints refs, never replaces the stored baseline, and
//! takes no lease, cooldown, screenshot, UIA walk or JSONL row. Page text is untrusted: it is capped
//! and never logged.

use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::chrome;
use crate::extract::Detail;

pub const CHANGES_SCHEMA: &str = "hands.changes/v1";
/// Content-revision capability level this client understands (`rev_v` on the wire).
pub const REV_V: u32 = 1;
const ENTRY_TEXT_CAP: usize = 80;
const CTX_URL_CAP: usize = 256;
const CTX_TITLE_CAP: usize = 120;

/// A parsed public token: the tab it names and the 4-part extension core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicToken {
    pub tab: u32,
    /// `<doc>.<rev>.<fp16>.<shape4>`, lowercase, digits and hex only.
    pub core: String,
}

fn decimal_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 10 && s.bytes().all(|b| b.is_ascii_digit())
}

/// `[1-9][0-9]{0,9}`: a decimal id/revision that never starts with zero.
fn positive_decimal_ok(s: &str) -> bool {
    decimal_ok(s) && !s.starts_with('0')
}

fn hex_ok(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `<doc4>.<rev>.<fp16>.<shape4>` — the extension core, validated before anything is composed.
pub fn core_ok(core: &str) -> bool {
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 4
        && hex_ok(parts[0], 4)
        && positive_decimal_ok(parts[1])
        && hex_ok(parts[2], 16)
        && hex_ok(parts[3], 4)
}

/// Parse `<tab>.<doc>.<rev>.<fp16>.<shape4>`. `None` for anything else (never a partial parse).
pub fn parse_public_token(token: &str) -> Option<PublicToken> {
    let (tab_part, rest) = token.split_once('.')?;
    if !positive_decimal_ok(tab_part) {
        return None;
    }
    let tab: u32 = tab_part.parse().ok()?;
    if !core_ok(rest) {
        return None;
    }
    Some(PublicToken {
        tab,
        core: rest.to_string(),
    })
}

/// Compose the public token from the reply's tab and the extension core. `None` when either is
/// absent or malformed: an unkeyed token is never emitted.
pub fn compose(tab: Option<u32>, core: Option<&str>) -> Option<String> {
    let tab = tab.filter(|t| *t > 0)?;
    let core = core?;
    if !core_ok(core) {
        return None;
    }
    Some(format!("{tab}.{core}"))
}

/// The observe-side `rev`: only when the reply carried `rev_v:1` and a tab.
pub fn observe_rev(rev_v: Option<u32>, tab: Option<u32>, core: Option<&str>) -> Option<String> {
    if rev_v != Some(REV_V) {
        return None;
    }
    compose(tab, core)
}

fn cap_chars(value: &str, cap: usize) -> String {
    value.chars().take(cap).collect()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Counts {
    #[serde(default)]
    pub added: usize,
    #[serde(default)]
    pub removed: usize,
    #[serde(default)]
    pub changed: usize,
    #[serde(default)]
    pub viewport: usize,
    #[serde(default)]
    pub context: usize,
    #[serde(default)]
    pub total: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeltaEntry {
    pub idx: u64,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangedEntry {
    pub idx: u64,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Viewport {
    pub fields: Vec<String>,
    #[serde(default)]
    pub from: Value,
    #[serde(default)]
    pub to: Value,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ChangesBody {
    pub counts: Counts,
    pub added: Vec<DeltaEntry>,
    pub removed: Vec<DeltaEntry>,
    pub changed: Vec<ChangedEntry>,
    pub viewport: Option<Viewport>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Pair {
    pub from: Option<String>,
    pub to: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ContextBody {
    pub changed: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<Pair>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<Pair>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Coverage {
    pub scope: String,
    pub page_complete: bool,
    pub elements_walked: usize,
    pub elements_retained: usize,
    pub excluded: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Resync {
    pub required: bool,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counts: Option<Counts>,
}

/// The `changes` envelope. It has no `elements`, `ref`, `screenshot_path`, `windows` or `extract`
/// by construction: a poll is not an observe replacement.
#[derive(Debug, Clone, Serialize)]
pub struct ChangesEnvelope {
    pub ok: bool,
    pub schema: &'static str,
    pub session_id: String,
    pub chrome_walk: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<u32>,
    pub fallback: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rev_v: Option<u32>,
    /// The stored baseline token (a poll never advances it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// The token the caller sent, echoed.
    pub baseline: String,
    pub unchanged: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changes: Option<ChangesBody>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextBody>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<Coverage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resync: Option<Resync>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timing_ms: Option<u64>,
}

impl ChangesEnvelope {
    fn base(session_id: &str, baseline: &str) -> Self {
        Self {
            ok: true,
            schema: CHANGES_SCHEMA,
            session_id: session_id.to_string(),
            chrome_walk: true,
            error: None,
            tab: None,
            window: None,
            fallback: false,
            rev_v: None,
            revision: None,
            baseline: baseline.to_string(),
            unchanged: false,
            changes: None,
            context: None,
            coverage: None,
            resync: None,
            timing_ms: None,
        }
    }

    /// `ok:false` with no request sent (usage) or a transport failure. Never a resync.
    fn refusal(session_id: &str, baseline: &str, error: String) -> Self {
        let mut env = Self::base(session_id, baseline);
        env.ok = false;
        env.chrome_walk = false;
        env.error = Some(error);
        env
    }

    fn resync(
        session_id: &str,
        baseline: &str,
        reason: &str,
        tab: Option<u32>,
        window: Option<u32>,
    ) -> Self {
        let mut env = Self::base(session_id, baseline);
        env.tab = tab;
        env.window = window;
        env.resync = Some(Resync {
            required: true,
            reason: reason.to_string(),
            counts: None,
        });
        env
    }
}

fn reply_u32(value: &Value, key: &str) -> Option<u32> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
}

fn reply_text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn parse_counts(value: &Value) -> Counts {
    serde_json::from_value(value.clone()).unwrap_or_default()
}

fn parse_entries(value: Option<&Value>) -> Vec<DeltaEntry> {
    value
        .and_then(|v| serde_json::from_value::<Vec<DeltaEntry>>(v.clone()).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|mut e| {
            e.text = e.text.map(|t| cap_chars(&t, ENTRY_TEXT_CAP));
            e
        })
        .collect()
}

fn parse_changed(value: Option<&Value>) -> Vec<ChangedEntry> {
    value
        .and_then(|v| serde_json::from_value::<Vec<ChangedEntry>>(v.clone()).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|mut e| {
            e.text = e.text.map(|t| cap_chars(&t, ENTRY_TEXT_CAP));
            e
        })
        .collect()
}

fn parse_pair(value: Option<&Value>, cap: usize) -> Option<Pair> {
    let v = value?;
    Some(Pair {
        from: v
            .get("from")
            .and_then(Value::as_str)
            .map(|s| cap_chars(s, cap)),
        to: v
            .get("to")
            .and_then(Value::as_str)
            .map(|s| cap_chars(s, cap)),
    })
}

fn parse_context(value: Option<&Value>) -> ContextBody {
    let changed = value
        .and_then(|v| v.get("changed"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    ContextBody {
        changed,
        url: parse_pair(value.and_then(|v| v.get("url")), CTX_URL_CAP),
        title: parse_pair(value.and_then(|v| v.get("title")), CTX_TITLE_CAP),
    }
}

fn parse_coverage(value: Option<&Value>) -> Option<Coverage> {
    value.and_then(|v| serde_json::from_value::<Coverage>(v.clone()).ok())
}

/// Map one poll reply to the envelope. Pure: the transport is the only thing `run` adds.
///
/// Check order (fixed): transport/`error` → `rev_unavailable` (no `rev_v` or no `tab`) → tab
/// mismatch (`document_changed`) → extension `resync` → unchanged → delta.
pub fn map_reply(
    token: &PublicToken,
    baseline: &str,
    session_id: &str,
    reply: &Value,
) -> ChangesEnvelope {
    if let Some(name) = reply.get("error").and_then(Value::as_str) {
        let tab = reply_u32(reply, "tab").or(Some(token.tab));
        return match name {
            // The token's tab is gone, unreadable or has no content: the caller re-observes.
            "tab_unavailable" | "no-tab" | "no_content" | "empty" | "window_unreadable" => {
                ChangesEnvelope::resync(
                    session_id,
                    baseline,
                    "document_changed",
                    tab,
                    reply_u32(reply, "window"),
                )
            }
            other => ChangesEnvelope::refusal(
                session_id,
                baseline,
                format!("Chrome host error: {other}"),
            ),
        };
    }
    let reply_tab = reply_u32(reply, "tab");
    let reply_window = reply_u32(reply, "window");
    let fallback = reply
        .get("fallback")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if reply.get("rev_v").and_then(Value::as_u64) != Some(u64::from(REV_V)) || reply_tab.is_none() {
        // Old extension (no content revision) or no tab evidence: fail closed, loudly.
        let mut env = ChangesEnvelope::resync(
            session_id,
            baseline,
            "rev_unavailable",
            reply_tab,
            reply_window,
        );
        env.fallback = fallback;
        return env;
    }
    if reply_tab != Some(token.tab) {
        return ChangesEnvelope::resync(
            session_id,
            baseline,
            "document_changed",
            reply_tab,
            reply_window,
        );
    }
    let mut env = ChangesEnvelope::base(session_id, baseline);
    env.tab = reply_tab;
    env.window = reply_window;
    env.fallback = fallback;
    env.rev_v = Some(REV_V);
    env.revision = compose(reply_tab, reply_text(reply, "rev_core").as_deref());
    env.coverage = parse_coverage(reply.get("coverage"));
    if let Some(resync) = reply.get("resync") {
        let reason = resync
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("document_changed")
            .to_string();
        env.resync = Some(Resync {
            required: true,
            reason,
            counts: resync.get("counts").map(parse_counts),
        });
        return env;
    }
    if reply.get("unchanged").and_then(Value::as_bool) == Some(true) {
        env.unchanged = true;
        env.changes = Some(ChangesBody {
            counts: Counts::default(),
            added: Vec::new(),
            removed: Vec::new(),
            changed: Vec::new(),
            viewport: None,
        });
        env.context = Some(ContextBody {
            changed: Vec::new(),
            url: None,
            title: None,
        });
        return env;
    }
    let changes = reply.get("changes");
    env.changes = Some(ChangesBody {
        counts: changes
            .and_then(|c| c.get("counts"))
            .map(parse_counts)
            .unwrap_or_default(),
        added: parse_entries(changes.and_then(|c| c.get("added"))),
        removed: parse_entries(changes.and_then(|c| c.get("removed"))),
        changed: parse_changed(changes.and_then(|c| c.get("changed"))),
        viewport: changes
            .and_then(|c| c.get("viewport"))
            .filter(|v| !v.is_null())
            .and_then(|v| serde_json::from_value::<Viewport>(v.clone()).ok()),
    });
    env.context = Some(parse_context(reply.get("context")));
    env
}

/// Run one poll. A malformed `--since` is a usage refusal before any request is sent; a transport
/// failure is `ok:false`. Neither is ever a resync.
pub fn run(since: &str, detail: Detail, session_id: Option<&str>, timing: bool) -> ChangesEnvelope {
    let started = Instant::now();
    let session = crate::session::resolve_session_id_from_os(session_id);
    let Some(token) = parse_public_token(since) else {
        return ChangesEnvelope::refusal(
            &session,
            since,
            "malformed --since: expected <tab>.<doc>.<rev>.<fp16>.<shape4> from a Chrome observe"
                .to_string(),
        );
    };
    let mut env = match chrome::poll_changes(token.tab, since, detail) {
        Ok(reply) => map_reply(&token, since, &session, &reply),
        Err(err) => ChangesEnvelope::refusal(&session, since, err.tool_message()),
    };
    if timing {
        env.timing_ms = Some(started.elapsed().as_millis() as u64);
    }
    env
}

/// One-line JSON, the same contract the CLI and MCP tool print.
pub fn serialize(envelope: &ChangesEnvelope) -> Result<String, crate::error::HandsError> {
    serde_json::to_string(envelope).map_err(|err| {
        crate::error::HandsError::Chrome(format!("changes envelope serialize: {err}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CORE: &str = "5678.2.0123456789abcdef.0fa0";

    fn token() -> PublicToken {
        parse_public_token(&format!("12.{CORE}")).expect("valid token")
    }

    #[test]
    fn public_token_round_trips_and_rejects_anything_else() {
        let t = parse_public_token("12.5678.2.0123456789abcdef.0fa0").expect("parse");
        assert_eq!(t.tab, 12);
        assert_eq!(t.core, CORE);
        assert_eq!(
            compose(Some(12), Some(CORE)).as_deref(),
            Some("12.5678.2.0123456789abcdef.0fa0")
        );
        // Uppercase, page-like text, zero-leading, wrong arity and missing tab all refuse.
        assert!(parse_public_token("12.5678.2.0123456789ABCDEF.0fa0").is_none());
        assert!(parse_public_token("0.5678.2.0123456789abcdef.0fa0").is_none());
        assert!(parse_public_token("12.5678.02.0123456789abcdef.0fa0").is_none());
        assert!(parse_public_token("12.5678.2.0123456789abcdef").is_none());
        assert!(parse_public_token("12.5678.2.0123456789abcdef.0fa0.extra").is_none());
        assert!(parse_public_token("not-a-token").is_none());
        assert!(parse_public_token("12.5678.2.0123456789abcdef.0fa0 <script>").is_none());
        assert!(
            compose(None, Some(CORE)).is_none(),
            "no tab is no unkeyed token"
        );
        assert!(compose(Some(12), None).is_none());
    }

    #[test]
    fn observe_rev_needs_rev_v_and_tab() {
        assert!(
            observe_rev(None, Some(12), Some(CORE)).is_none(),
            "old extension"
        );
        assert!(
            observe_rev(Some(1), None, Some(CORE)).is_none(),
            "no tab evidence"
        );
        assert_eq!(
            observe_rev(Some(1), Some(12), Some(CORE)).as_deref(),
            Some("12.5678.2.0123456789abcdef.0fa0")
        );
    }

    #[test]
    fn unchanged_reply_is_zero_counts_with_no_arrays() {
        let reply = json!({
            "rev_v": 1, "tab": 12, "window": 3, "fallback": false,
            "rev_core": CORE, "unchanged": true,
            "coverage": {"scope":"retained_walk","page_complete":false,"elements_walked":2,"elements_retained":2,"excluded":["closed_shadow"]}
        });
        let env = map_reply(&token(), "12.5678.2.0123456789abcdef.0fa0", "s", &reply);
        assert!(env.ok && env.unchanged);
        let body = env.changes.expect("changes");
        assert_eq!(body.counts.total, 0);
        assert!(body.added.is_empty() && body.removed.is_empty() && body.changed.is_empty());
        assert_eq!(
            env.revision.as_deref(),
            Some("12.5678.2.0123456789abcdef.0fa0")
        );
        assert!(!env.coverage.expect("coverage").page_complete);
    }

    #[test]
    fn delta_reply_maps_entries_and_caps_text() {
        let long = "x".repeat(200);
        let reply = json!({
            "rev_v": 1, "tab": 12, "rev_core": CORE, "unchanged": false,
            "changes": {
                "counts": {"added":1,"removed":0,"changed":1,"viewport":0,"context":1,"total":3},
                "added": [{"idx": 0, "role": "Button", "text": long}],
                "removed": [],
                "changed": [{"idx": 7, "role": "button", "text": "Submitted", "fields": ["text"]}],
                "viewport": null
            },
            "context": {"changed": ["url"], "url": {"from": "https://a.test/l", "to": "https://a.test/i"}}
        });
        let env = map_reply(&token(), "12.5678.2.0123456789abcdef.0fa0", "s", &reply);
        let body = env.changes.expect("changes");
        assert_eq!(body.counts.total, 3);
        assert_eq!(body.added[0].text.as_ref().map(String::len), Some(80));
        assert_eq!(body.changed[0].fields, vec!["text".to_string()]);
        let ctx = env.context.expect("context");
        assert_eq!(ctx.changed, vec!["url".to_string()]);
        assert_eq!(
            ctx.url.expect("url").to.as_deref(),
            Some("https://a.test/i")
        );
        assert!(env.resync.is_none());
    }

    #[test]
    fn every_resync_reason_is_named_and_carries_no_arrays() {
        for reason in [
            "no_baseline",
            "document_changed",
            "stale_revision",
            "changed_request",
        ] {
            let reply = json!({
                "rev_v": 1, "tab": 12, "rev_core": null,
                "resync": {"required": true, "reason": reason}
            });
            let env = map_reply(&token(), "12.5678.2.0123456789abcdef.0fa0", "s", &reply);
            let resync = env.resync.expect("resync");
            assert_eq!(resync.reason, reason);
            assert!(env.changes.is_none() && env.context.is_none());
        }
        let overflow = json!({
            "rev_v": 1, "tab": 12, "rev_core": CORE,
            "resync": {"required": true, "reason": "delta_overflow",
                "counts": {"added":0,"removed":0,"changed":25,"viewport":0,"context":0,"total":25}}
        });
        let env = map_reply(&token(), "12.5678.2.0123456789abcdef.0fa0", "s", &overflow);
        assert_eq!(
            env.resync
                .expect("overflow")
                .counts
                .expect("counts")
                .changed,
            25
        );
        assert!(env.changes.is_none());
    }

    #[test]
    fn no_baseline_is_not_rev_unavailable() {
        let reply = json!({"rev_v": 1, "tab": 12, "rev_core": null,
            "resync": {"required": true, "reason": "no_baseline"}});
        let env = map_reply(&token(), "12.5678.2.0123456789abcdef.0fa0", "s", &reply);
        assert_eq!(env.resync.expect("resync").reason, "no_baseline");
    }

    #[test]
    fn old_extension_fails_closed_as_rev_unavailable() {
        let reply = json!({"tab": 12, "unchanged": true});
        let env = map_reply(&token(), "12.5678.2.0123456789abcdef.0fa0", "s", &reply);
        assert_eq!(env.resync.expect("resync").reason, "rev_unavailable");
        assert!(env.rev_v.is_none());
        assert!(env.changes.is_none());
    }

    #[test]
    fn reply_tab_mismatch_is_document_changed_never_another_pages_delta() {
        let reply = json!({"rev_v": 1, "tab": 99, "rev_core": CORE, "unchanged": true});
        let env = map_reply(&token(), "12.5678.2.0123456789abcdef.0fa0", "s", &reply);
        assert_eq!(env.resync.expect("resync").reason, "document_changed");
        assert!(env.changes.is_none());
    }

    #[test]
    fn tab_gone_maps_to_document_changed() {
        for name in ["tab_unavailable", "no-tab", "no_content", "empty"] {
            let reply = json!({"error": name, "tab": 12});
            let env = map_reply(&token(), "12.5678.2.0123456789abcdef.0fa0", "s", &reply);
            assert_eq!(
                env.resync.expect("resync").reason,
                "document_changed",
                "{name}"
            );
        }
    }

    #[test]
    fn envelope_has_no_map_refs_screenshot_or_windows() {
        let reply = json!({"rev_v": 1, "tab": 12, "rev_core": CORE, "unchanged": true});
        let env = map_reply(&token(), "12.5678.2.0123456789abcdef.0fa0", "s", &reply);
        let text = serialize(&env).expect("serialize");
        for banned in [
            "\"elements\"",
            "\"ref\"",
            "screenshot_path",
            "\"windows\"",
            "\"extract\"",
            "\"snap\"",
        ] {
            assert!(
                !text.contains(banned),
                "poll envelope must not contain {banned}"
            );
        }
        assert!(text.contains("\"schema\":\"hands.changes/v1\""));
    }

    #[test]
    fn poll_path_takes_no_capture_uia_lease_cooldown_fence_or_log() {
        // Source lock: the poll module's code (not its tests) never reaches the observe pipeline.
        let src = include_str!("changes.rs");
        let code = src.split("#[cfg(test)]").next().expect("code section");
        for banned in [
            "observe(",
            "capture_virtual_screen",
            "uia::",
            "plan_observe_target",
            "is_chrome_hwnd",
            "foreground::",
            "lease::",
            "cooldown::",
            "fence::",
            "logs::",
            "outcome::",
            "postcondition",
            "identity::",
            "find::",
            "frames",
        ] {
            assert!(
                !code.contains(banned),
                "changes.rs must not reference {banned}"
            );
        }
        // The envelope has no page map, refs or screenshot fields.
        for field in [
            "pub elements:",
            "pub windows:",
            "pub extract:",
            "pub screenshot_path:",
            "pub snapshot:",
        ] {
            assert!(
                !code.contains(field),
                "poll envelope must not carry {field}"
            );
        }
    }

    #[test]
    fn malformed_since_is_a_usage_refusal_before_any_request() {
        let env = run("not-a-token", Detail::Default, Some("sess"), false);
        assert!(!env.ok);
        assert!(
            env.error
                .as_deref()
                .is_some_and(|e| e.starts_with("malformed --since"))
        );
        assert!(env.resync.is_none(), "never a resync");
    }
}
