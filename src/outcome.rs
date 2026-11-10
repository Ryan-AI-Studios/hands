//! One delivery truth, one versioned outcome block, one ordered retry classifier.
//!
//! 0122 owns the semantic `delivery` vocabulary; 0123 owns identity evidence; 0128 owns the
//! hit-test refusal; 0129 owns the point contract. This module only joins their already-emitted
//! facts into a closed outcome: `delivery` (was it sent?), `effect` (did the declared end state
//! hold?), `verdict` (a pure projection) and `retry` (guidance only, never an instruction).
//!
//! Nothing here performs I/O, reads the page, or string-matches a free-text error. Every reason
//! is a closed token and every refusal constructor stamps a [`RefusalToken`].

use serde::{Deserialize, Serialize};

use crate::identity::{IdentityReport, IdentityState};

/// Single delivery truth for physical and semantic actions. Moved from `uia_actions` in 0124;
/// the wire key/values are unchanged because `SemanticInfo` was flattened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Delivered,
    Undelivered,
    Unknown,
}

impl Delivery {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Delivered => "delivered",
            Self::Undelivered => "undelivered",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Confirmed,
    Observed,
    Unconfirmed,
    Failed,
    NotEvaluated,
}

impl Effect {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Observed => "observed",
            Self::Unconfirmed => "unconfirmed",
            Self::Failed => "failed",
            Self::NotEvaluated => "not_evaluated",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Confirmed,
    Unconfirmed,
    Failed,
    Unknown,
    NotAttempted,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Unconfirmed => "unconfirmed",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
            Self::NotAttempted => "not_attempted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryDisposition {
    None,
    Safe,
    Wait,
    Reobserve,
    Unsafe,
}

impl RetryDisposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Safe => "safe",
            Self::Wait => "wait",
            Self::Reobserve => "reobserve",
            Self::Unsafe => "unsafe",
        }
    }
}

/// Guidance only. No code path auto-retries from this value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryInfo {
    pub disposition: RetryDisposition,
    pub reason: String,
}

impl RetryInfo {
    pub fn new(disposition: RetryDisposition, reason: &'static str) -> Self {
        Self {
            disposition,
            reason: reason.to_string(),
        }
    }
}

/// Closed pre-dispatch refusal vocabulary. Stamped by each constructor; never serialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalToken {
    ChallengeYielded,
    DeskFrozen,
    CooldownActive,
    FenceRequiresConfirm,
    HighIlUnavailable,
    OutsideClient,
    InteractableBlocked,
    InteractableUnknown,
    AssessmentUnavailable,
    InteractableUnavailable,
    BlindGuard,
    TransformInvalid,
    InvalidRequest,
}

impl RefusalToken {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChallengeYielded => "challenge_yielded",
            Self::DeskFrozen => "desk_frozen",
            Self::CooldownActive => "cooldown_active",
            Self::FenceRequiresConfirm => "fence_requires_confirm",
            Self::HighIlUnavailable => "high_il_unavailable",
            Self::OutsideClient => "outside_client",
            Self::InteractableBlocked => "interactable_blocked",
            Self::InteractableUnknown => "interactable_unknown",
            Self::AssessmentUnavailable => "assessment_unavailable",
            Self::InteractableUnavailable => "interactable_unavailable",
            Self::BlindGuard => "blind_guard",
            Self::TransformInvalid => "transform_invalid",
            Self::InvalidRequest => "invalid_request",
        }
    }

    /// §3.3.1 row for this token.
    pub fn retry(self) -> RetryInfo {
        match self {
            Self::ChallengeYielded => RetryInfo::new(RetryDisposition::Wait, "challenge_yielded"),
            Self::DeskFrozen => RetryInfo::new(RetryDisposition::Wait, "desk_frozen"),
            Self::CooldownActive => RetryInfo::new(RetryDisposition::Wait, "cooldown_active"),
            Self::FenceRequiresConfirm => {
                RetryInfo::new(RetryDisposition::Safe, "fence_requires_confirm")
            }
            Self::HighIlUnavailable => {
                RetryInfo::new(RetryDisposition::Safe, "high_il_unavailable")
            }
            Self::OutsideClient => RetryInfo::new(RetryDisposition::Safe, "outside_client"),
            Self::InteractableBlocked => {
                RetryInfo::new(RetryDisposition::Safe, "interactable_blocked")
            }
            Self::InteractableUnknown => {
                RetryInfo::new(RetryDisposition::Reobserve, "interactable_unknown")
            }
            Self::AssessmentUnavailable | Self::InteractableUnavailable => {
                RetryInfo::new(RetryDisposition::Reobserve, "assessment_unavailable")
            }
            Self::BlindGuard => RetryInfo::new(RetryDisposition::Safe, "blind_guard"),
            Self::TransformInvalid => {
                RetryInfo::new(RetryDisposition::Reobserve, "transform_invalid")
            }
            Self::InvalidRequest => RetryInfo::new(RetryDisposition::Safe, "invalid_request"),
        }
    }
}

/// The total identity reason mapping (spec §3.3.2): reason first, then state. Every token
/// `chrome::classify_ref_error` can emit is listed; any other token falls through to
/// `protocol_error`.
pub fn identity_retry(report: &IdentityReport) -> RetryInfo {
    let reason = report
        .reason
        .as_deref()
        .unwrap_or_else(|| report.state_token());
    match reason {
        "stale_document"
        | "stale_snapshot"
        | "stale_element"
        | "stale_identity"
        | "not_visible"
        | "identity_mismatch"
        | "window_mismatch"
        | "frame_unavailable"
        | "frame_document_changed"
        | "frame_not_visible" => RetryInfo::new(RetryDisposition::Reobserve, "identity_stale"),
        "duplicate_runtime_id" | "duplicate_identity" | "frame_link_ambiguous" => {
            RetryInfo::new(RetryDisposition::Reobserve, "identity_ambiguous")
        }
        "tab_not_active" | "window_not_focused" | "tab_hidden" => {
            RetryInfo::new(RetryDisposition::Safe, "binding_precondition")
        }
        "tab_unavailable" | "no_tab" => RetryInfo::new(RetryDisposition::Reobserve, "target_gone"),
        "no_content" | "unbound_extension" => {
            RetryInfo::new(RetryDisposition::Reobserve, "extension_unavailable")
        }
        "host_down" | "resolve_timeout" | "provider_timeout" => {
            RetryInfo::new(RetryDisposition::Safe, "host_unavailable")
        }
        "not_chrome" => RetryInfo::new(RetryDisposition::Safe, "target_not_foreground"),
        "frame_unsupported" => RetryInfo::new(RetryDisposition::None, "frame_unsupported"),
        "fixture_snapshot" => RetryInfo::new(RetryDisposition::Safe, "host_unavailable"),
        _ => match report.state {
            IdentityState::Ambiguous => {
                RetryInfo::new(RetryDisposition::Reobserve, "identity_ambiguous")
            }
            IdentityState::Stale => RetryInfo::new(RetryDisposition::Reobserve, "identity_stale"),
            _ => RetryInfo::new(RetryDisposition::Reobserve, "protocol_error"),
        },
    }
}

/// Semantic facts consumed from 0122 without re-reading anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticFacts {
    pub delivery: Delivery,
    pub verified: bool,
    pub verify: Option<String>,
    /// Kept for parity; only a `worker_saturated` code changes the row.
    pub dispatched: bool,
    pub code: Option<String>,
}

impl SemanticFacts {
    pub fn new(
        delivery: Delivery,
        verified: bool,
        verify: Option<&str>,
        code: Option<&str>,
    ) -> Self {
        Self {
            delivery,
            verified,
            verify: verify.map(str::to_string),
            dispatched: false,
            code: code.map(str::to_string),
        }
    }
}

/// Closed postcondition grammar. One flat object so every provider's function-schema parser
/// accepts it; `deny_unknown_fields` keeps it closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Postcondition {
    pub kind: PostconditionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    #[serde(default)]
    pub wait_ms: u64,
}

impl Postcondition {
    pub fn kind_token(&self) -> &'static str {
        self.kind.as_str()
    }

    /// Validate the condition's own caps before the lease/fence/input. A click whose target is
    /// not a scoped Chrome ref is **not** rejected here; it dispatches and reports
    /// `unavailable` afterwards.
    pub fn validate(&self) -> Result<(), String> {
        if self.wait_ms > MAX_WAIT_MS {
            return Err(format!(
                "postcondition wait_ms must be 0..={MAX_WAIT_MS} (got {})",
                self.wait_ms
            ));
        }
        let check_value = |value: &Option<String>| -> Result<(), String> {
            match value.as_deref() {
                Some(text) if text.chars().count() > MAX_TEXT_CHARS => Err(format!(
                    "postcondition value is longer than {MAX_TEXT_CHARS} chars"
                )),
                _ => Ok(()),
            }
        };
        match self.kind {
            PostconditionKind::UrlPrefix
            | PostconditionKind::UrlEquals
            | PostconditionKind::TitleEquals => {
                if self.value.is_none() {
                    return Err(format!(
                        "postcondition {} requires value",
                        self.kind.as_str()
                    ));
                }
                check_value(&self.value)?;
            }
            PostconditionKind::ElementPresent | PostconditionKind::ElementAbsent => {
                let Some(reference) = self.r#ref.as_deref() else {
                    return Err(format!("postcondition {} requires ref", self.kind.as_str()));
                };
                // Validate the ref grammar only; eligibility is decided after dispatch.
                crate::identity::SnapshotRef::parse(reference)
                    .map_err(|err| format!("postcondition ref: {err}"))?;
            }
            PostconditionKind::CountAtLeast => {
                let role = self.role.as_deref().unwrap_or_default();
                if role.is_empty()
                    || role.len() > 64
                    || !role
                        .bytes()
                        .enumerate()
                        .all(|(i, b)| b.is_ascii_lowercase() || (i > 0 && b == b'-'))
                {
                    return Err(
                        "postcondition count_at_least role must match ^[a-z][a-z-]*$ and be ≤64"
                            .into(),
                    );
                }
                match self.n {
                    Some(n) if (1..=MAX_COUNT).contains(&n) => {}
                    _ => {
                        return Err(format!(
                            "postcondition count_at_least n must be 1..={MAX_COUNT}"
                        ));
                    }
                }
            }
            PostconditionKind::UrlChanged
            | PostconditionKind::TitleChanged
            | PostconditionKind::FocusEditable
            | PostconditionKind::DialogPresent => {}
        }
        Ok(())
    }

    pub fn is_exact(&self) -> bool {
        matches!(
            self.kind,
            PostconditionKind::UrlPrefix
                | PostconditionKind::UrlEquals
                | PostconditionKind::UrlChanged
                | PostconditionKind::TitleEquals
                | PostconditionKind::TitleChanged
                | PostconditionKind::ElementPresent
                | PostconditionKind::ElementAbsent
        )
    }
}

pub const MAX_WAIT_MS: u64 = 2000;
pub const MAX_TEXT_CHARS: usize = 512;
pub const MAX_COUNT: u32 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostconditionKind {
    UrlPrefix,
    UrlEquals,
    UrlChanged,
    TitleEquals,
    TitleChanged,
    ElementPresent,
    ElementAbsent,
    FocusEditable,
    DialogPresent,
    CountAtLeast,
}

impl PostconditionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UrlPrefix => "url_prefix",
            Self::UrlEquals => "url_equals",
            Self::UrlChanged => "url_changed",
            Self::TitleEquals => "title_equals",
            Self::TitleChanged => "title_changed",
            Self::ElementPresent => "element_present",
            Self::ElementAbsent => "element_absent",
            Self::FocusEditable => "focus_editable",
            Self::DialogPresent => "dialog_present",
            Self::CountAtLeast => "count_at_least",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostconditionState {
    Satisfied,
    Unsatisfied,
    Unavailable,
    Timeout,
    NotEvaluated,
}

impl PostconditionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Satisfied => "satisfied",
            Self::Unsatisfied => "unsatisfied",
            Self::Unavailable => "unavailable",
            Self::Timeout => "timeout",
            Self::NotEvaluated => "not_evaluated",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostconditionReport {
    pub kind: String,
    pub state: PostconditionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub source: String,
}

impl PostconditionReport {
    pub fn new(kind: &str, state: PostconditionState, reason: Option<&str>, source: &str) -> Self {
        Self {
            kind: kind.to_string(),
            state,
            reason: reason.map(str::to_string),
            source: source.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeInfo {
    pub v: u8,
    pub effect: Effect,
    pub verdict: Verdict,
    pub retry: RetryInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postcondition: Option<PostconditionReport>,
    /// Positive marker that this binary understood the `postcondition` flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postcondition_checked: Option<bool>,
}

impl OutcomeInfo {
    pub const VERSION: u8 = 1;
}

/// One classification: exactly one disposition, exactly one verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub delivery: Delivery,
    pub effect: Effect,
    pub verdict: Verdict,
    pub retry: RetryInfo,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OutcomeInputs {
    pub tool: String,
    pub ok: bool,
    pub post_dispatch: bool,
    pub miss: Option<String>,
    pub navigated: bool,
    pub retried: bool,
    pub blind: bool,
    pub identity: Option<IdentityReport>,
    pub semantic: Option<SemanticFacts>,
    pub postcondition: Option<PostconditionReport>,
    pub postcondition_wait_ms: u64,
    pub refusal: Option<RefusalToken>,
}

/// Pure projection of `delivery` + `effect` (D-8). Exhaustive over every pair.
pub fn derive_verdict(delivery: Delivery, effect: Effect) -> Verdict {
    match delivery {
        Delivery::Unknown => Verdict::Unknown,
        Delivery::Undelivered => Verdict::NotAttempted,
        Delivery::Delivered => match effect {
            Effect::Confirmed => Verdict::Confirmed,
            Effect::Failed => Verdict::Failed,
            Effect::Observed | Effect::Unconfirmed | Effect::NotEvaluated => Verdict::Unconfirmed,
        },
    }
}

/// Ordered, first-match-wins classifier (spec §3.3). Never reads `error` text.
pub fn classify_outcome(input: &OutcomeInputs) -> Classification {
    // 1. Pre-dispatch refusal: nothing was sent, never `unsafe`. A `post_dispatch` flag means
    // it is no longer a pre-dispatch refusal, so it falls through to delivery uncertainty.
    if !input.ok
        && !input.post_dispatch
        && let Some(token) = input.refusal
    {
        return finish(Delivery::Undelivered, Effect::NotEvaluated, token.retry());
    }
    // 1b. Identity refusal: total reason mapping (reason before state).
    if !input.ok
        && !input.post_dispatch
        && let Some(report) = input.identity.as_ref()
        && report.state != IdentityState::Exact
    {
        return finish(
            Delivery::Undelivered,
            Effect::NotEvaluated,
            identity_retry(report),
        );
    }
    // 2. Delivery uncertainty dominates every effect.
    let semantic_unknown = input
        .semantic
        .as_ref()
        .is_some_and(|facts| facts.delivery == Delivery::Unknown);
    if input.post_dispatch || semantic_unknown {
        let effect = if input
            .postcondition
            .as_ref()
            .is_some_and(|report| report.state == PostconditionState::Satisfied)
        {
            Effect::Confirmed
        } else if input
            .postcondition
            .as_ref()
            .is_some_and(|report| report.state == PostconditionState::NotEvaluated)
        {
            Effect::NotEvaluated
        } else if input.navigated {
            Effect::Observed
        } else if matches!(input.tool.as_str(), "type" | "key") {
            Effect::NotEvaluated
        } else {
            Effect::Unconfirmed
        };
        let reason = if input
            .identity
            .as_ref()
            .and_then(|report| report.reason.as_deref())
            .is_some_and(|reason| reason == "binding_changed_after_dispatch")
        {
            "post_dispatch_binding_changed"
        } else if semantic_unknown {
            "semantic_unknown"
        } else if matches!(input.tool.as_str(), "type" | "key") {
            "partial_input_unproven"
        } else {
            "post_dispatch_unverified"
        };
        let disposition = if input.tool == "hover" {
            RetryDisposition::Reobserve
        } else {
            RetryDisposition::Unsafe
        };
        return finish(
            Delivery::Unknown,
            effect,
            RetryInfo::new(disposition, reason),
        );
    }
    // 3. A second dispatch already happened.
    if input.retried {
        let effect = postcondition_effect(
            input.postcondition.as_ref(),
            input.postcondition_wait_ms,
            input.navigated,
        );
        let reason = if input.miss.is_some() {
            "auto_retry_exhausted"
        } else {
            "second_dispatch_sent"
        };
        return finish(
            Delivery::Delivered,
            effect,
            RetryInfo::new(RetryDisposition::Unsafe, reason),
        );
    }
    // 4. A declared postcondition was evaluated.
    if let Some(report) = input.postcondition.as_ref() {
        return finish_postcondition(report, input.postcondition_wait_ms, input.navigated);
    }
    // 5-8. Semantic facts.
    if let Some(facts) = input.semantic.as_ref() {
        if facts.code.as_deref() == Some("worker_saturated") {
            return finish(
                Delivery::Undelivered,
                Effect::NotEvaluated,
                RetryInfo::new(RetryDisposition::Wait, "worker_saturated"),
            );
        }
        if facts.delivery == Delivery::Undelivered {
            let retry = input
                .identity
                .as_ref()
                .map(identity_retry)
                .unwrap_or_else(|| RetryInfo::new(RetryDisposition::Safe, "pre_dispatch_refusal"));
            return finish(Delivery::Undelivered, Effect::NotEvaluated, retry);
        }
        if facts.delivery == Delivery::Delivered {
            if facts.verify.as_deref() == Some("state_readback") && facts.verified {
                return finish(
                    Delivery::Delivered,
                    Effect::Confirmed,
                    RetryInfo::new(RetryDisposition::None, "semantic_readback"),
                );
            }
            return finish(
                Delivery::Delivered,
                Effect::Unconfirmed,
                RetryInfo::new(RetryDisposition::Reobserve, "semantic_dispatch_only"),
            );
        }
    }
    // 5. Scoped `sref1:` click miss without a retry.
    if input.miss.is_some() && is_scoped_click(input) {
        return finish(
            Delivery::Delivered,
            Effect::Unconfirmed,
            RetryInfo::new(RetryDisposition::Reobserve, "scoped_ref_no_auto_retry"),
        );
    }
    // 6. Navigation: an effect happened, but not the declared one.
    if input.navigated {
        return finish(
            Delivery::Delivered,
            Effect::Observed,
            RetryInfo::new(RetryDisposition::Reobserve, "navigated_refs_stale"),
        );
    }
    // 7. Blind click: dispatch without an effect contract.
    if input.blind {
        return finish(
            Delivery::Delivered,
            Effect::Unconfirmed,
            RetryInfo::new(RetryDisposition::Reobserve, "blind_no_effect_contract"),
        );
    }
    // 9. No-effect tools.
    match input.tool.as_str() {
        "hover" => finish(
            Delivery::Delivered,
            Effect::NotEvaluated,
            RetryInfo::new(RetryDisposition::None, "no_effect_contract"),
        ),
        "type" | "key" | "scroll" => finish(
            Delivery::Delivered,
            Effect::NotEvaluated,
            RetryInfo::new(RetryDisposition::Reobserve, "no_effect_contract"),
        ),
        _ => finish(
            Delivery::Delivered,
            Effect::Unconfirmed,
            RetryInfo::new(RetryDisposition::Reobserve, "effect_unproven"),
        ),
    }
}

fn is_scoped_click(input: &OutcomeInputs) -> bool {
    input.tool == "click"
        && input
            .identity
            .as_ref()
            .is_some_and(|report| report.binding == crate::identity::Binding::Scoped)
}

fn postcondition_effect(
    report: Option<&PostconditionReport>,
    wait_ms: u64,
    navigated: bool,
) -> Effect {
    match report {
        Some(report) => match report.state {
            PostconditionState::Satisfied => Effect::Confirmed,
            PostconditionState::Unsatisfied => {
                if wait_ms > 0 {
                    Effect::Failed
                } else {
                    Effect::Unconfirmed
                }
            }
            PostconditionState::Unavailable | PostconditionState::Timeout => {
                if navigated {
                    Effect::Observed
                } else {
                    Effect::Unconfirmed
                }
            }
            PostconditionState::NotEvaluated => Effect::NotEvaluated,
        },
        None => Effect::Unconfirmed,
    }
}

fn finish_postcondition(
    report: &PostconditionReport,
    wait_ms: u64,
    navigated: bool,
) -> Classification {
    let effect = postcondition_effect(Some(report), wait_ms, navigated);
    let (delivery, retry) = match report.state {
        PostconditionState::Satisfied => (
            Delivery::Delivered,
            RetryInfo::new(RetryDisposition::None, "effect_confirmed"),
        ),
        PostconditionState::Unsatisfied => {
            if wait_ms > 0 {
                (
                    Delivery::Delivered,
                    RetryInfo::new(RetryDisposition::Reobserve, "postcondition_failed"),
                )
            } else {
                (
                    Delivery::Delivered,
                    RetryInfo::new(RetryDisposition::Reobserve, "postcondition_not_met_yet"),
                )
            }
        }
        PostconditionState::Unavailable => (
            Delivery::Delivered,
            RetryInfo::new(RetryDisposition::Reobserve, "postcondition_unavailable"),
        ),
        PostconditionState::Timeout => (
            Delivery::Delivered,
            RetryInfo::new(RetryDisposition::Reobserve, "postcondition_timeout"),
        ),
        PostconditionState::NotEvaluated => (
            Delivery::Undelivered,
            RetryInfo::new(RetryDisposition::Safe, "not_dispatched"),
        ),
    };
    finish(delivery, effect, retry)
}

fn finish(delivery: Delivery, effect: Effect, retry: RetryInfo) -> Classification {
    Classification {
        delivery,
        effect,
        verdict: derive_verdict(delivery, effect),
        retry,
    }
}

/// Closed postcondition-reason vocabulary (spec §3.5.1). Any reason outside this set degrades
/// to `protocol_error` in the Rust mapper and is never echoed from page/extension text.
pub const POSTCONDITION_REASONS: &[&str] = &[
    "not_dispatched",
    "requires_scoped_chrome_ref",
    "dom_only",
    "frame_postcondition_out_of_scope",
    "invalid_condition",
    "stale_document",
    "stale_snapshot",
    "stale_element",
    "baseline_mismatch",
    "tab_not_active",
    "window_not_focused",
    "tab_hidden",
    "window_unreadable",
    "host_down",
    "protocol_error",
    "fixture_snapshot",
    "unsupported",
    "count_capped",
    "not_observed",
    "postcondition_not_met_yet",
    "postcondition_failed",
    "postcondition_unavailable",
    "postcondition_timeout",
    "evaluation_failed",
];

/// Closed retry-reason vocabulary (§3.3.4). Every emitted reason must be here.
pub const RETRY_REASONS: &[&str] = &[
    "effect_confirmed",
    "no_effect_contract",
    "pre_dispatch_refusal",
    "fence_requires_confirm",
    "high_il_unavailable",
    "challenge_yielded",
    "cooldown_active",
    "desk_frozen",
    "outside_client",
    "transform_invalid",
    "binding_precondition",
    "host_unavailable",
    "target_not_foreground",
    "identity_stale",
    "identity_ambiguous",
    "target_gone",
    "extension_unavailable",
    "interactable_blocked",
    "interactable_unknown",
    "assessment_unavailable",
    "blind_guard",
    "invalid_request",
    "scoped_ref_no_auto_retry",
    "auto_retry_exhausted",
    "second_dispatch_sent",
    "post_dispatch_unverified",
    "partial_input_unproven",
    "post_dispatch_binding_changed",
    "navigated_refs_stale",
    "postcondition_failed",
    "postcondition_not_met_yet",
    "postcondition_unavailable",
    "postcondition_timeout",
    "effect_unproven",
    "semantic_readback",
    "semantic_dispatch_only",
    "semantic_unknown",
    "worker_saturated",
    "frame_unsupported",
    "protocol_error",
    "not_dispatched",
    "blind_no_effect_contract",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{Binding, IdentityReport};

    fn base_input(tool: &str) -> OutcomeInputs {
        OutcomeInputs {
            tool: tool.into(),
            ok: true,
            post_dispatch: false,
            miss: None,
            navigated: false,
            retried: false,
            blind: false,
            identity: None,
            semantic: None,
            postcondition: None,
            postcondition_wait_ms: 0,
            refusal: None,
        }
    }

    fn identity(state: IdentityState, reason: &str) -> IdentityReport {
        let mut report = IdentityReport::unknown(None, reason);
        report.state = state;
        report.binding = Binding::Scoped;
        report
    }

    #[test]
    fn verdict_mapping_table_is_exhaustive() {
        for delivery in [
            Delivery::Delivered,
            Delivery::Undelivered,
            Delivery::Unknown,
        ] {
            for effect in [
                Effect::Confirmed,
                Effect::Observed,
                Effect::Unconfirmed,
                Effect::Failed,
                Effect::NotEvaluated,
            ] {
                let verdict = derive_verdict(delivery, effect);
                match (delivery, effect) {
                    (Delivery::Unknown, _) => assert_eq!(verdict, Verdict::Unknown),
                    (Delivery::Undelivered, _) => assert_eq!(verdict, Verdict::NotAttempted),
                    (Delivery::Delivered, Effect::Confirmed) => {
                        assert_eq!(verdict, Verdict::Confirmed)
                    }
                    (Delivery::Delivered, Effect::Failed) => assert_eq!(verdict, Verdict::Failed),
                    (Delivery::Delivered, _) => assert_eq!(verdict, Verdict::Unconfirmed),
                }
            }
        }
    }

    #[test]
    fn classifier_is_total_and_one_disposition() {
        let postconditions = [
            None,
            Some(PostconditionReport::new(
                "dialog_present",
                PostconditionState::Satisfied,
                None,
                "dom",
            )),
            Some(PostconditionReport::new(
                "dialog_present",
                PostconditionState::Unsatisfied,
                None,
                "dom",
            )),
            Some(PostconditionReport::new(
                "dialog_present",
                PostconditionState::Unavailable,
                Some("not_observed"),
                "dom",
            )),
            Some(PostconditionReport::new(
                "dialog_present",
                PostconditionState::Timeout,
                Some("postcondition_timeout"),
                "dom",
            )),
            Some(PostconditionReport::new(
                "dialog_present",
                PostconditionState::NotEvaluated,
                Some("not_dispatched"),
                "dom",
            )),
        ];
        for tool in ["click", "hover", "type", "key", "scroll", "uia-action"] {
            for post_dispatch in [false, true] {
                for retried in [false, true] {
                    for miss in [None, Some("no_change")] {
                        for navigated in [false, true] {
                            for blind in [false, true] {
                                for refusal in [None, Some(RefusalToken::DeskFrozen)] {
                                    for postcondition in postconditions.clone() {
                                        for semantic in [
                                            None,
                                            Some(SemanticFacts::new(
                                                Delivery::Delivered,
                                                false,
                                                Some("unavailable"),
                                                None,
                                            )),
                                            Some(SemanticFacts::new(
                                                Delivery::Unknown,
                                                false,
                                                Some("unavailable"),
                                                Some("provider_timeout"),
                                            )),
                                            Some(SemanticFacts::new(
                                                Delivery::Undelivered,
                                                false,
                                                Some("unavailable"),
                                                Some("stale_target"),
                                            )),
                                        ] {
                                            let mut input = base_input(tool);
                                            input.post_dispatch = post_dispatch;
                                            input.retried = retried;
                                            input.miss = miss.map(str::to_string);
                                            input.navigated = navigated;
                                            input.blind = blind;
                                            input.refusal = refusal;
                                            input.postcondition = postcondition.clone();
                                            input.semantic = semantic.clone();
                                            input.identity = Some({
                                                let mut r =
                                                    identity(IdentityState::Exact, "not_visible");
                                                r.binding = Binding::Scoped;
                                                r
                                            });
                                            // Clear a refusal's identity so the refusal row is
                                            // what is exercised.
                                            if refusal.is_some() {
                                                input.identity = None;
                                            }
                                            let out = classify_outcome(&input);
                                            assert_eq!(
                                                out.verdict,
                                                derive_verdict(out.delivery, out.effect),
                                                "verdict must be a pure projection"
                                            );
                                            if post_dispatch || retried {
                                                assert_ne!(
                                                    out.retry.disposition,
                                                    RetryDisposition::Safe,
                                                    "no safe row after dispatch/retry: {out:?}"
                                                );
                                            }
                                            if refusal.is_none() && !post_dispatch {
                                                assert!(
                                                    RETRY_REASONS
                                                        .contains(&out.retry.reason.as_str()),
                                                    "unclosed reason {}",
                                                    out.retry.reason
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn every_refusal_token_maps_once() {
        let tokens = [
            RefusalToken::ChallengeYielded,
            RefusalToken::DeskFrozen,
            RefusalToken::CooldownActive,
            RefusalToken::FenceRequiresConfirm,
            RefusalToken::HighIlUnavailable,
            RefusalToken::OutsideClient,
            RefusalToken::InteractableBlocked,
            RefusalToken::InteractableUnknown,
            RefusalToken::AssessmentUnavailable,
            RefusalToken::InteractableUnavailable,
            RefusalToken::BlindGuard,
            RefusalToken::TransformInvalid,
            RefusalToken::InvalidRequest,
        ];
        for token in tokens {
            let mut input = base_input("click");
            input.ok = false;
            input.refusal = Some(token);
            let out = classify_outcome(&input);
            assert_eq!(out.delivery, Delivery::Undelivered);
            assert_eq!(out.effect, Effect::NotEvaluated);
            assert_eq!(out.verdict, Verdict::NotAttempted);
            assert_eq!(out.retry, token.retry());
            assert!(RETRY_REASONS.contains(&out.retry.reason.as_str()));
        }
    }

    #[test]
    fn identity_reason_map_is_total() {
        for reason in crate::chrome::REF_ERROR_TOKENS {
            let report = identity(IdentityState::Unknown, reason);
            let mut input = base_input("click");
            input.ok = false;
            input.identity = Some(report);
            let out = classify_outcome(&input);
            assert_eq!(out.delivery, Delivery::Undelivered, "{reason}");
            assert_eq!(out.effect, Effect::NotEvaluated, "{reason}");
            assert!(
                RETRY_REASONS.contains(&out.retry.reason.as_str()),
                "unclosed reason for {reason}: {}",
                out.retry.reason
            );
        }
    }

    #[test]
    fn binding_precondition_is_safe_not_reobserve() {
        for reason in ["tab_not_active", "window_not_focused", "tab_hidden"] {
            let retry = identity_retry(&identity(IdentityState::Unknown, reason));
            assert_eq!(retry.disposition, RetryDisposition::Safe, "{reason}");
            assert_eq!(retry.reason, "binding_precondition");
        }
    }

    #[test]
    fn window_mismatch_is_reobserve() {
        let retry = identity_retry(&identity(IdentityState::Stale, "window_mismatch"));
        assert_eq!(retry.disposition, RetryDisposition::Reobserve);
        assert_eq!(retry.reason, "identity_stale");
    }

    #[test]
    fn missing_postcondition_never_failed_or_confirmed() {
        for state in [
            PostconditionState::Unavailable,
            PostconditionState::Timeout,
            PostconditionState::NotEvaluated,
        ] {
            let mut input = base_input("click");
            input.postcondition = Some(PostconditionReport::new(
                "dialog_present",
                state,
                Some("not_observed"),
                "dom",
            ));
            let out = classify_outcome(&input);
            assert_ne!(out.effect, Effect::Failed, "{state:?}");
            assert_ne!(out.effect, Effect::Confirmed, "{state:?}");
        }
    }

    #[test]
    fn unsatisfied_without_wait_is_not_met_yet() {
        let mut input = base_input("click");
        input.postcondition = Some(PostconditionReport::new(
            "url_equals",
            PostconditionState::Unsatisfied,
            None,
            "dom",
        ));
        input.postcondition_wait_ms = 0;
        let out = classify_outcome(&input);
        assert_eq!(out.effect, Effect::Unconfirmed);
        assert_eq!(out.retry.reason, "postcondition_not_met_yet");
    }

    #[test]
    fn unsatisfied_after_wait_is_failed() {
        let mut input = base_input("click");
        input.postcondition = Some(PostconditionReport::new(
            "url_equals",
            PostconditionState::Unsatisfied,
            None,
            "dom",
        ));
        input.postcondition_wait_ms = 500;
        let out = classify_outcome(&input);
        assert_eq!(out.effect, Effect::Failed);
        assert_eq!(out.verdict, Verdict::Failed);
        assert_eq!(out.retry.disposition, RetryDisposition::Reobserve);
        assert_eq!(out.retry.reason, "postcondition_failed");
    }

    #[test]
    fn timeout_and_unavailable_stay_unconfirmed() {
        for (state, reason) in [
            (PostconditionState::Timeout, "postcondition_timeout"),
            (PostconditionState::Unavailable, "postcondition_unavailable"),
        ] {
            let mut input = base_input("click");
            input.postcondition = Some(PostconditionReport::new(
                "element_absent",
                state,
                Some("protocol_error"),
                "dom",
            ));
            let out = classify_outcome(&input);
            assert_eq!(out.effect, Effect::Unconfirmed);
            assert_eq!(out.retry.disposition, RetryDisposition::Reobserve);
            assert_eq!(out.retry.reason, reason);
        }
    }

    #[test]
    fn positive_only_not_observed_is_unavailable_not_failed() {
        let mut input = base_input("click");
        input.postcondition = Some(PostconditionReport::new(
            "dialog_present",
            PostconditionState::Unavailable,
            Some("not_observed"),
            "dom",
        ));
        let out = classify_outcome(&input);
        assert_eq!(out.effect, Effect::Unconfirmed);
        assert_eq!(out.verdict, Verdict::Unconfirmed);
        let condition: Postcondition =
            serde_json::from_str("{\"kind\":\"dialog_present\"}").expect("dialog");
        assert!(!condition.is_exact());
    }

    #[test]
    fn confirmed_effect_does_not_downgrade_unknown_delivery() {
        let mut input = base_input("click");
        input.post_dispatch = true;
        input.postcondition = Some(PostconditionReport::new(
            "dialog_present",
            PostconditionState::Satisfied,
            None,
            "dom",
        ));
        let out = classify_outcome(&input);
        assert_eq!(out.delivery, Delivery::Unknown);
        assert_eq!(out.effect, Effect::Confirmed);
        assert_eq!(out.verdict, Verdict::Unknown);
        assert_eq!(out.retry.disposition, RetryDisposition::Unsafe);
        assert_eq!(out.retry.reason, "post_dispatch_unverified");
    }

    #[test]
    fn retried_dominates_satisfied_postcondition() {
        let mut input = base_input("click");
        input.retried = true;
        input.miss = Some("no_change".into());
        input.postcondition = Some(PostconditionReport::new(
            "dialog_present",
            PostconditionState::Satisfied,
            None,
            "dom",
        ));
        let out = classify_outcome(&input);
        assert_eq!(out.retry.disposition, RetryDisposition::Unsafe);
        assert_eq!(out.retry.reason, "auto_retry_exhausted");
        assert_eq!(out.effect, Effect::Confirmed);
    }

    #[test]
    fn binding_changed_after_dispatch_unknown_unsafe() {
        let mut report = identity(IdentityState::Stale, "binding_changed_after_dispatch");
        report.binding = Binding::Scoped;
        let mut input = base_input("click");
        input.post_dispatch = true;
        input.identity = Some(report);
        let out = classify_outcome(&input);
        assert_eq!(out.delivery, Delivery::Unknown);
        assert_eq!(out.retry.reason, "post_dispatch_binding_changed");
        assert_eq!(out.retry.disposition, RetryDisposition::Unsafe);
    }

    #[test]
    fn retry_reason_closed_vocabulary() {
        for reason in RETRY_REASONS {
            assert!(!reason.is_empty());
            assert_eq!(*reason, reason.trim());
        }
        // Every disposition/reason pair emitted by the token table is in the closed set.
        for token in [
            RefusalToken::ChallengeYielded,
            RefusalToken::DeskFrozen,
            RefusalToken::CooldownActive,
            RefusalToken::FenceRequiresConfirm,
            RefusalToken::HighIlUnavailable,
            RefusalToken::OutsideClient,
            RefusalToken::InteractableBlocked,
            RefusalToken::InteractableUnknown,
            RefusalToken::AssessmentUnavailable,
            RefusalToken::InteractableUnavailable,
            RefusalToken::BlindGuard,
            RefusalToken::TransformInvalid,
            RefusalToken::InvalidRequest,
        ] {
            assert!(RETRY_REASONS.contains(&token.retry().reason.as_str()));
        }
    }

    #[test]
    fn postcondition_value_caps_reject_before_dispatch() {
        let long = "a".repeat(MAX_TEXT_CHARS + 1);
        let condition: Postcondition = serde_json::from_value(serde_json::json!({
            "kind": "url_prefix",
            "value": long,
        }))
        .expect("parse");
        assert!(condition.validate().is_err());

        let bad_role: Postcondition = serde_json::from_value(serde_json::json!({
            "kind": "count_at_least",
            "role": "Button",
            "n": 3,
        }))
        .expect("parse");
        assert!(bad_role.validate().is_err());

        let big_n: Postcondition = serde_json::from_value(serde_json::json!({
            "kind": "count_at_least",
            "role": "button",
            "n": 51,
        }))
        .expect("parse");
        assert!(big_n.validate().is_err());

        let big_wait: Postcondition = serde_json::from_value(serde_json::json!({
            "kind": "dialog_present",
            "wait_ms": 2001,
        }))
        .expect("parse");
        assert!(big_wait.validate().is_err());

        let unknown: Result<Postcondition, _> = serde_json::from_value(serde_json::json!({
            "kind": "dialog_present",
            "selector": "#x",
        }));
        assert!(unknown.is_err(), "closed grammar");
    }

    #[test]
    fn count_at_least_rejects_n_over_50() {
        let condition: Postcondition = serde_json::from_value(serde_json::json!({
            "kind": "count_at_least",
            "role": "listitem",
            "n": 51,
        }))
        .expect("parse");
        assert!(condition.validate().is_err());
        let ok: Postcondition = serde_json::from_value(serde_json::json!({
            "kind": "count_at_least",
            "role": "listitem",
            "n": 50,
        }))
        .expect("parse");
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn url_changed_requires_matching_snap() {
        let condition: Postcondition = serde_json::from_str("{\"kind\":\"url_changed\"}")
            .expect("url_changed parses without a caller baseline");
        assert!(condition.validate().is_ok());
        assert!(condition.is_exact());
    }

    #[test]
    fn frame_ref_postcondition_out_of_scope() {
        // The ref parses; eligibility is decided by the caller (`frame>0` -> unavailable).
        let reference = "sref1:c.1.1.0a0a.0000a0a0.1";
        let condition: Postcondition = serde_json::from_value(serde_json::json!({
            "kind": "element_present",
            "ref": reference,
        }))
        .expect("parse");
        assert!(condition.validate().is_ok());
        let parsed = crate::identity::SnapshotRef::parse(reference).expect("ref");
        match parsed {
            crate::identity::SnapshotRef::Chrome { frame, .. } => assert!(frame > 0),
            _ => panic!("chrome ref"),
        }
    }

    #[test]
    fn uia_ref_condition_validates_before_input() {
        // The condition's own grammar is checked; a UIA ref is still valid input and the
        // click dispatches, reporting `dom_only` afterwards (Phase 5 matrix).
        let condition: Postcondition = serde_json::from_value(serde_json::json!({
            "kind": "element_present",
            "ref": "sref1:u.1a2b3c.7d0e1f22.42a1b2c3",
        }))
        .expect("parse");
        assert!(condition.validate().is_ok());
        match crate::identity::SnapshotRef::parse(condition.r#ref.as_deref().expect("ref")) {
            Ok(crate::identity::SnapshotRef::Uia { .. }) => {}
            other => panic!("uia ref: {other:?}"),
        }
    }

    #[test]
    fn postcondition_never_changes_delivery_or_authority() {
        // A satisfied postcondition with a pre-dispatch refusal stays undelivered.
        let mut input = base_input("click");
        input.ok = false;
        input.refusal = Some(RefusalToken::FenceRequiresConfirm);
        input.postcondition = Some(PostconditionReport::new(
            "dialog_present",
            PostconditionState::Satisfied,
            None,
            "dom",
        ));
        let out = classify_outcome(&input);
        assert_eq!(out.delivery, Delivery::Undelivered);
        assert_eq!(out.effect, Effect::NotEvaluated);
        assert_eq!(out.retry.disposition, RetryDisposition::Safe);
    }

    #[test]
    fn semantic_delivery_wire_shape_unchanged() {
        let json = serde_json::json!({
            "action": "invoke",
            "delivery": "delivered",
            "verify": "state_readback",
            "foreground_changed": false,
            "pointer_moved": false
        });
        let facts = SemanticFacts::new(Delivery::Delivered, true, Some("state_readback"), None);
        assert_eq!(json["delivery"], Delivery::Delivered.as_str());
        let out = classify_outcome(&{
            let mut input = base_input("uia-action");
            input.semantic = Some(facts);
            input
        });
        assert_eq!(out.delivery, Delivery::Delivered);
        assert_eq!(out.effect, Effect::Confirmed);
        assert_eq!(out.retry.reason, "semantic_readback");
    }

    #[test]
    fn worker_saturated_is_undelivered_not_unsafe() {
        let mut input = base_input("uia-action");
        input.semantic = Some(SemanticFacts::new(
            Delivery::Undelivered,
            false,
            Some("unavailable"),
            Some("worker_saturated"),
        ));
        // Nothing was dispatched, so slot exhaustion is a known non-delivery.
        let out = classify_outcome(&input);
        assert_eq!(out.delivery, Delivery::Undelivered);
        assert_eq!(out.effect, Effect::NotEvaluated);
        assert_eq!(out.retry.disposition, RetryDisposition::Wait);
        assert_eq!(out.retry.reason, "worker_saturated");
    }

    #[test]
    fn agent_contract_names_outcome() {
        let doc = include_str!("../docs/AGENT-CONTRACT.md");
        assert!(doc.contains("## Outcome and retry (0124)"));
        assert!(doc.contains("confirmed` means the declared\n  end state held at check time, not that the click caused it"));
        assert!(doc.contains("verify_outcome` is deliberately **not** a tool"));
        assert!(doc.contains("Scoped refs never auto-retry"));
        assert!(doc.contains("Bridge honesty"));
    }

    #[test]
    fn bridge_disconnect_is_error_not_success() {
        // A lost bridge response is a transport error; no envelope is synthesized.
        let src = include_str!("desk_pipe.rs");
        assert!(!src.contains("\"delivered\""));
        assert!(!src.contains("Ok(ActuateEnvelope"));
    }

    #[test]
    fn worker_timeout_stays_unknown_unsafe() {
        let mut input = base_input("uia-action");
        input.semantic = Some(SemanticFacts::new(
            Delivery::Unknown,
            false,
            Some("unavailable"),
            Some("provider_timeout"),
        ));
        let out = classify_outcome(&input);
        assert_eq!(out.delivery, Delivery::Unknown);
        assert_eq!(out.retry.disposition, RetryDisposition::Unsafe);
        assert_eq!(out.retry.reason, "semantic_unknown");
    }
}
