//! Bounded server-side action script. Aborts on the first failed prerequisite.

use serde::Serialize;
use serde_json::Value;

use crate::actuate::{self, ActivateEnvelope, ActuateEnvelope, ActuateRequest};
use crate::challenge::{self, ChallengeInfo, YIELD_ERROR};
use crate::cooldown::{self, Snapshot};
use crate::error::HandsError;
use crate::fence::FenceInfo;
use crate::lease;
use crate::logs;
use crate::observe::{self, ENVELOPE_MAX_BYTES, ObserveRequest, ObserveView};
use crate::session::resolve_session_id_from_os;

pub const MAX_STEPS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Completed,
    PrerequisiteFailed,
    FocusLost,
    LeaseFrozen,
    FenceRefused,
    ChallengeYielded,
    UnknownKey,
    BadSteps,
    Cooldown,
}

impl StopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::PrerequisiteFailed => "prerequisite_failed",
            Self::FocusLost => "focus_lost",
            Self::LeaseFrozen => "lease_frozen",
            Self::FenceRefused => "fence_refused",
            Self::ChallengeYielded => "challenge_yielded",
            Self::UnknownKey => "unknown_key",
            Self::BadSteps => "bad_steps",
            Self::Cooldown => "cooldown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SequenceStep {
    Activate {
        window: String,
    },
    Click {
        element_id: Option<String>,
        grid: Option<String>,
        x: Option<i32>,
        y: Option<i32>,
    },
    Hover {
        element_id: Option<String>,
        grid: Option<String>,
        x: Option<i32>,
        y: Option<i32>,
    },
    Type {
        text: String,
    },
    Key {
        name: String,
    },
    Scroll {
        dy: i32,
        dx: Option<i32>,
        element_id: Option<String>,
        grid: Option<String>,
        x: Option<i32>,
        y: Option<i32>,
    },
    WaitSettle {
        x: Option<i32>,
        y: Option<i32>,
        w: Option<i32>,
        h: Option<i32>,
    },
    Observe,
}

impl SequenceStep {
    pub fn tool(&self) -> &'static str {
        match self {
            Self::Activate { .. } => "activate",
            Self::Click { .. } => "click",
            Self::Hover { .. } => "hover",
            Self::Type { .. } => "type",
            Self::Key { .. } => "key",
            Self::Scroll { .. } => "scroll",
            Self::WaitSettle { .. } => "wait_settle",
            Self::Observe => "observe",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecutedStep {
    pub tool: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foregrounded: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub miss: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settled: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SequenceEnvelope {
    pub session_id: String,
    pub ok: bool,
    pub frozen: bool,
    pub steps_total: usize,
    pub steps_executed: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_step_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_step_tool: Option<String>,
    pub stop_reason: StopReason,
    pub executed_steps: Vec<ExecutedStep>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fence: Option<FenceInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge: Option<ChallengeInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observe_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elements_total: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observe_challenge_present: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cooldown_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub loop_suspected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guidance: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct StepOutcome {
    pub ok: bool,
    pub frozen: bool,
    pub foregrounded: Option<bool>,
    pub settled: Option<bool>,
    pub miss: Option<String>,
    pub error: Option<String>,
    pub fence: Option<FenceInfo>,
    pub challenge: Option<ChallengeInfo>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub window: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ObserveSummary {
    pub observe_path: String,
    pub elements_total: usize,
    pub challenge_present: bool,
}

pub struct SequenceHooks {
    pub yielded: fn() -> bool,
    pub frozen: fn() -> bool,
    pub cooling: fn(&str) -> bool,
    pub activate: fn(&str, &str) -> Result<StepOutcome, HandsError>,
    pub click: fn(&str, &SequenceStep) -> Result<StepOutcome, HandsError>,
    pub hover: fn(&str, &SequenceStep) -> Result<StepOutcome, HandsError>,
    pub type_text: fn(&str, &str) -> Result<StepOutcome, HandsError>,
    pub key: fn(&str, &str) -> Result<StepOutcome, HandsError>,
    pub scroll: fn(&str, &SequenceStep) -> Result<StepOutcome, HandsError>,
    pub wait_settle: fn(&str, &SequenceStep) -> Result<StepOutcome, HandsError>,
    pub observe: fn(&str) -> Result<ObserveSummary, HandsError>,
    /// Test hooks record steps here. Live verbs already log via `after_actuate`.
    pub record_steps: bool,
}

impl SequenceHooks {
    fn live() -> Self {
        Self {
            yielded: challenge::yielded,
            frozen: lease::is_frozen,
            cooling: cooldown::is_cooling,
            activate: live_activate,
            click: live_click,
            hover: live_hover,
            type_text: live_type,
            key: live_key,
            scroll: live_scroll,
            wait_settle: live_wait_settle,
            observe: live_observe,
            record_steps: false,
        }
    }
}

#[derive(Debug)]
struct ParseFail {
    index: Option<usize>,
    tool: Option<String>,
}

pub fn run(session_id: Option<String>, steps: Value) -> Result<SequenceEnvelope, HandsError> {
    sequence_with(session_id, steps, SequenceHooks::live())
}

pub fn sequence_with(
    session_id: Option<String>,
    steps: Value,
    hooks: SequenceHooks,
) -> Result<SequenceEnvelope, HandsError> {
    let session_id = resolve_session_id_from_os(session_id.as_deref());
    logs::check_write_id(&session_id)?;
    logs::ensure_installed();
    logs::remember_session(&session_id);

    let parsed = match parse_steps(&steps) {
        Ok(v) => v,
        Err(fail) => {
            let env = bad_steps_envelope(&session_id, count_raw_len(&steps), fail);
            finish_log(&env, None)?;
            return shrink_to_budget(env);
        }
    };

    if (hooks.cooling)(&session_id) {
        let snap = cooldown::note_rejection(&session_id);
        let mut env = abort_envelope(
            &session_id,
            parsed.len(),
            Vec::new(),
            Some(0),
            parsed.first().map(|s| s.tool().to_string()),
            StopReason::Cooldown,
            true,
            None,
            None,
        );
        stamp_sequence(&mut env, snap);
        finish_log(&env, None)?;
        return shrink_to_budget(env);
    }

    let mut executed = Vec::new();
    let mut fence = None;
    let mut challenge = None;
    let mut observe_path = None;
    let mut elements_total = None;
    let mut observe_challenge_present = None;

    for (index, step) in parsed.iter().enumerate() {
        if (hooks.yielded)() {
            let env = abort_envelope(
                &session_id,
                parsed.len(),
                executed,
                Some(index),
                Some(step.tool().into()),
                StopReason::ChallengeYielded,
                (hooks.frozen)(),
                fence,
                Some(challenge::snapshot()),
            );
            finish_log(&env, None)?;
            return shrink_to_budget(env);
        }
        if (hooks.frozen)() {
            let env = abort_envelope(
                &session_id,
                parsed.len(),
                executed,
                Some(index),
                Some(step.tool().into()),
                StopReason::LeaseFrozen,
                true,
                fence,
                challenge,
            );
            finish_log(&env, None)?;
            return shrink_to_budget(env);
        }

        if matches!(step, SequenceStep::Observe) {
            match (hooks.observe)(&session_id) {
                Ok(summary) => {
                    observe_path = Some(summary.observe_path);
                    elements_total = Some(summary.elements_total);
                    observe_challenge_present = Some(summary.challenge_present);
                    executed.push(ExecutedStep {
                        tool: "observe".into(),
                        ok: true,
                        id: None,
                        name: None,
                        window: None,
                        foregrounded: None,
                        miss: None,
                        settled: None,
                    });
                    if hooks.record_steps {
                        record_step(&session_id, "observe", true, None, None)?;
                    }
                }
                Err(err) => {
                    executed.push(ExecutedStep {
                        tool: "observe".into(),
                        ok: false,
                        id: None,
                        name: None,
                        window: None,
                        foregrounded: None,
                        miss: None,
                        settled: None,
                    });
                    if hooks.record_steps {
                        record_step(
                            &session_id,
                            "observe",
                            false,
                            Some(&err.tool_message()),
                            None,
                        )?;
                    }
                    let env = abort_envelope(
                        &session_id,
                        parsed.len(),
                        executed,
                        Some(index),
                        Some("observe".into()),
                        StopReason::PrerequisiteFailed,
                        (hooks.frozen)(),
                        fence,
                        challenge,
                    );
                    finish_log(&env, None)?;
                    return shrink_to_budget(env);
                }
            }
            continue;
        }

        let outcome = match dispatch_step(&hooks, &session_id, step) {
            Ok(v) => v,
            Err(err) => {
                let msg = err.tool_message();
                let reason = if msg.starts_with("unknown key") {
                    StopReason::UnknownKey
                } else if msg == YIELD_ERROR || msg.starts_with("yielded:") {
                    StopReason::ChallengeYielded
                } else {
                    StopReason::PrerequisiteFailed
                };
                executed.push(ExecutedStep {
                    tool: step.tool().into(),
                    ok: false,
                    id: None,
                    name: match step {
                        SequenceStep::Key { name } => Some(name.clone()),
                        _ => None,
                    },
                    window: match step {
                        SequenceStep::Activate { window } => Some(window.clone()),
                        _ => None,
                    },
                    foregrounded: None,
                    miss: None,
                    settled: None,
                });
                if hooks.record_steps {
                    record_step(&session_id, step.tool(), false, Some(&msg), type_len(step))?;
                }
                let snap = cooldown::note_rejection(&session_id);
                let mut env = abort_envelope(
                    &session_id,
                    parsed.len(),
                    executed,
                    Some(index),
                    Some(step.tool().into()),
                    reason,
                    (hooks.frozen)(),
                    fence,
                    challenge,
                );
                stamp_sequence(&mut env, snap);
                finish_log(&env, None)?;
                return shrink_to_budget(env);
            }
        };

        let row = compact_row(step, &outcome);
        let type_len = type_len(step);
        if hooks.record_steps {
            record_step(
                &session_id,
                step.tool(),
                outcome.ok && step_succeeded(step, &outcome),
                outcome.error.as_deref(),
                type_len,
            )?;
        }
        if outcome.fence.is_some() {
            fence = outcome.fence.clone();
        }
        if outcome.challenge.is_some() {
            challenge = outcome.challenge.clone();
        }
        executed.push(row);

        if let Some(reason) = classify_failure(step, &outcome) {
            let frozen = reason == StopReason::LeaseFrozen || outcome.frozen || (hooks.frozen)();
            let snap = sequence_abort_snapshot(&session_id, step, &outcome);
            let mut env = abort_envelope(
                &session_id,
                parsed.len(),
                executed,
                Some(index),
                Some(step.tool().into()),
                reason,
                frozen,
                fence.clone(),
                challenge,
            );
            if let Some(snap) = snap {
                stamp_sequence(&mut env, snap);
            }
            finish_log(&env, env.fence.as_ref())?;
            return shrink_to_budget(env);
        }
    }

    let frozen = (hooks.frozen)();
    let env = SequenceEnvelope {
        session_id: session_id.clone(),
        ok: true,
        frozen,
        steps_total: parsed.len(),
        steps_executed: executed.len(),
        failed_step_index: None,
        failed_step_tool: None,
        stop_reason: StopReason::Completed,
        executed_steps: executed,
        fence,
        challenge,
        observe_path,
        elements_total,
        observe_challenge_present,
        attempt: None,
        cooldown_ms: None,
        loop_suspected: false,
        guidance: None,
    };
    finish_log(&env, None)?;
    shrink_to_budget(env)
}

pub fn serialize_envelope(envelope: &SequenceEnvelope) -> Result<String, HandsError> {
    serde_json::to_string(envelope)
        .map_err(|err| HandsError::Input(format!("sequence envelope: {err}")))
}

fn shrink_to_budget(mut envelope: SequenceEnvelope) -> Result<SequenceEnvelope, HandsError> {
    loop {
        let json = serialize_envelope(&envelope)?;
        if json.len() <= ENVELOPE_MAX_BYTES {
            return Ok(envelope);
        }
        if envelope.observe_path.is_some()
            || envelope.elements_total.is_some()
            || envelope.observe_challenge_present.is_some()
        {
            envelope.observe_path = None;
            envelope.elements_total = None;
            envelope.observe_challenge_present = None;
            continue;
        }
        if envelope.executed_steps.pop().is_some() {
            continue;
        }
        return Err(HandsError::Input(format!(
            "sequence envelope is {} bytes (hard max {ENVELOPE_MAX_BYTES})",
            json.len()
        )));
    }
}

fn parse_steps(steps: &Value) -> Result<Vec<SequenceStep>, ParseFail> {
    let Value::Array(items) = steps else {
        return Err(ParseFail {
            index: None,
            tool: None,
        });
    };
    if items.is_empty() || items.len() > MAX_STEPS {
        return Err(ParseFail {
            index: None,
            tool: None,
        });
    }
    let mut out = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let obj = item.as_object().ok_or(ParseFail {
            index: Some(index),
            tool: None,
        })?;
        let tool = obj
            .get("tool")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or(ParseFail {
                index: Some(index),
                tool: None,
            })?;
        if tool == "observe" {
            if index + 1 != items.len() {
                return Err(ParseFail {
                    index: Some(index),
                    tool: Some("observe".into()),
                });
            }
            if obj.keys().any(|k| k != "tool") {
                return Err(ParseFail {
                    index: Some(index),
                    tool: Some("observe".into()),
                });
            }
            out.push(SequenceStep::Observe);
            continue;
        }
        let step = parse_one(tool, obj).map_err(|_| ParseFail {
            index: Some(index),
            tool: Some(tool.to_string()),
        })?;
        out.push(step);
    }
    Ok(out)
}

fn parse_one(tool: &str, obj: &serde_json::Map<String, Value>) -> Result<SequenceStep, ()> {
    match tool {
        "activate" => {
            let window = req_string(obj, "window")?;
            Ok(SequenceStep::Activate { window })
        }
        "click" => Ok(SequenceStep::Click {
            element_id: opt_string(obj, "element_id"),
            grid: opt_string(obj, "grid"),
            x: opt_i32(obj, "x"),
            y: opt_i32(obj, "y"),
        }),
        "hover" => Ok(SequenceStep::Hover {
            element_id: opt_string(obj, "element_id"),
            grid: opt_string(obj, "grid"),
            x: opt_i32(obj, "x"),
            y: opt_i32(obj, "y"),
        }),
        "type" => Ok(SequenceStep::Type {
            text: req_string(obj, "text")?,
        }),
        "key" => Ok(SequenceStep::Key {
            name: req_string(obj, "name")?,
        }),
        "scroll" => {
            let dy = obj.get("dy").and_then(as_i32).ok_or(())?;
            Ok(SequenceStep::Scroll {
                dy,
                dx: opt_i32(obj, "dx"),
                element_id: opt_string(obj, "element_id"),
                grid: opt_string(obj, "grid"),
                x: opt_i32(obj, "x"),
                y: opt_i32(obj, "y"),
            })
        }
        "wait_settle" => {
            let x = opt_i32(obj, "x");
            let y = opt_i32(obj, "y");
            let w = opt_i32(obj, "w");
            let h = opt_i32(obj, "h");
            match (x, y, w, h) {
                (None, None, None, None) | (Some(_), Some(_), Some(_), Some(_)) => {}
                _ => return Err(()),
            }
            Ok(SequenceStep::WaitSettle { x, y, w, h })
        }
        _ => Err(()),
    }
}

fn req_string(obj: &serde_json::Map<String, Value>, key: &str) -> Result<String, ()> {
    obj.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or(())
}

fn opt_string(obj: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    obj.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn opt_i32(obj: &serde_json::Map<String, Value>, key: &str) -> Option<i32> {
    obj.get(key).and_then(as_i32)
}

fn as_i32(value: &Value) -> Option<i32> {
    value
        .as_i64()
        .and_then(|n| i32::try_from(n).ok())
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn count_raw_len(steps: &Value) -> usize {
    steps.as_array().map(|a| a.len()).unwrap_or(0)
}

fn dispatch_step(
    hooks: &SequenceHooks,
    session_id: &str,
    step: &SequenceStep,
) -> Result<StepOutcome, HandsError> {
    match step {
        SequenceStep::Activate { window } => (hooks.activate)(session_id, window),
        SequenceStep::Click { .. } => (hooks.click)(session_id, step),
        SequenceStep::Hover { .. } => (hooks.hover)(session_id, step),
        SequenceStep::Type { text } => (hooks.type_text)(session_id, text),
        SequenceStep::Key { name } => (hooks.key)(session_id, name),
        SequenceStep::Scroll { .. } => (hooks.scroll)(session_id, step),
        SequenceStep::WaitSettle { .. } => (hooks.wait_settle)(session_id, step),
        SequenceStep::Observe => unreachable!("observe dispatched separately"),
    }
}

fn step_succeeded(step: &SequenceStep, outcome: &StepOutcome) -> bool {
    classify_failure(step, outcome).is_none()
}

fn classify_failure(step: &SequenceStep, outcome: &StepOutcome) -> Option<StopReason> {
    if outcome.fence.is_some() {
        return Some(StopReason::FenceRefused);
    }
    if outcome.frozen {
        return Some(StopReason::LeaseFrozen);
    }
    if outcome
        .error
        .as_deref()
        .is_some_and(|e| e == YIELD_ERROR || e.starts_with("yielded:"))
        || outcome.challenge.as_ref().is_some_and(|c| c.yielded)
    {
        return Some(StopReason::ChallengeYielded);
    }
    if outcome
        .error
        .as_deref()
        .is_some_and(|e| e.starts_with("unknown key"))
    {
        return Some(StopReason::UnknownKey);
    }
    match step {
        SequenceStep::Activate { .. } => {
            if !outcome.ok {
                Some(StopReason::PrerequisiteFailed)
            } else if outcome.foregrounded != Some(true) {
                Some(StopReason::FocusLost)
            } else {
                None
            }
        }
        SequenceStep::Click { .. } => {
            if outcome.miss.as_deref() == Some("focus_lost") {
                Some(StopReason::FocusLost)
            } else if !outcome.ok {
                Some(StopReason::PrerequisiteFailed)
            } else {
                None
            }
        }
        SequenceStep::WaitSettle { .. } => {
            if outcome.ok && outcome.settled == Some(true) {
                None
            } else {
                Some(StopReason::PrerequisiteFailed)
            }
        }
        SequenceStep::Hover { .. }
        | SequenceStep::Type { .. }
        | SequenceStep::Key { .. }
        | SequenceStep::Scroll { .. } => {
            if outcome.ok {
                None
            } else {
                Some(StopReason::PrerequisiteFailed)
            }
        }
        SequenceStep::Observe => None,
    }
}

fn compact_row(step: &SequenceStep, outcome: &StepOutcome) -> ExecutedStep {
    ExecutedStep {
        tool: step.tool().into(),
        ok: outcome.ok && step_succeeded(step, outcome),
        id: outcome.id.clone(),
        name: outcome.name.clone().or_else(|| match step {
            SequenceStep::Key { name } => Some(name.clone()),
            _ => None,
        }),
        window: outcome.window.clone().or_else(|| match step {
            SequenceStep::Activate { window } => Some(window.clone()),
            _ => None,
        }),
        foregrounded: outcome.foregrounded,
        miss: outcome.miss.clone(),
        settled: match step {
            SequenceStep::WaitSettle { .. } => outcome.settled,
            _ => None,
        },
    }
}

fn type_len(step: &SequenceStep) -> Option<usize> {
    match step {
        SequenceStep::Type { text } => Some(text.chars().count()),
        _ => None,
    }
}

fn record_step(
    session_id: &str,
    tool: &str,
    ok: bool,
    error: Option<&str>,
    type_len: Option<usize>,
) -> Result<(), HandsError> {
    logs::record_actuate(session_id, tool, ok, error, None, None, type_len, None)
}

fn finish_log(env: &SequenceEnvelope, fence: Option<&FenceInfo>) -> Result<(), HandsError> {
    let fence = fence.map(|f| logs::LogFence {
        domain: f.domain.clone(),
        category: f.category.clone(),
        name: f.name.clone(),
        role: f.role.clone(),
    });
    logs::record_actuate(
        &env.session_id,
        "sequence",
        env.ok,
        Some(env.stop_reason.as_str()),
        None,
        fence,
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn abort_envelope(
    session_id: &str,
    steps_total: usize,
    executed: Vec<ExecutedStep>,
    failed_step_index: Option<usize>,
    failed_step_tool: Option<String>,
    stop_reason: StopReason,
    frozen: bool,
    fence: Option<FenceInfo>,
    challenge: Option<ChallengeInfo>,
) -> SequenceEnvelope {
    let frozen = frozen || stop_reason == StopReason::LeaseFrozen;
    SequenceEnvelope {
        session_id: session_id.into(),
        ok: false,
        frozen,
        steps_total,
        steps_executed: executed.len(),
        failed_step_index,
        failed_step_tool,
        stop_reason,
        executed_steps: executed,
        fence,
        challenge,
        observe_path: None,
        elements_total: None,
        observe_challenge_present: None,
        attempt: None,
        cooldown_ms: None,
        loop_suspected: false,
        guidance: None,
    }
}

fn stamp_sequence(env: &mut SequenceEnvelope, snap: Snapshot) {
    env.attempt = (snap.attempt > 0).then_some(snap.attempt);
    env.cooldown_ms = snap.cooldown_ms;
    env.loop_suspected = snap.loop_suspected;
    env.guidance = cooldown::guidance(env.frozen, snap);
}

fn sequence_abort_snapshot(
    session_id: &str,
    step: &SequenceStep,
    outcome: &StepOutcome,
) -> Option<Snapshot> {
    let needs_note = match step {
        SequenceStep::Activate { .. } if outcome.ok && outcome.foregrounded != Some(true) => true,
        SequenceStep::Click { .. }
            if outcome.ok && outcome.miss.as_deref() == Some("focus_lost") =>
        {
            true
        }
        _ => false,
    };
    if needs_note {
        return Some(cooldown::note_rejection(session_id));
    }
    if !outcome.ok
        && !matches!(
            step,
            SequenceStep::WaitSettle { .. } | SequenceStep::Observe
        )
    {
        return Some(cooldown::snapshot(session_id));
    }
    None
}

fn bad_steps_envelope(session_id: &str, raw_len: usize, fail: ParseFail) -> SequenceEnvelope {
    abort_envelope(
        session_id,
        raw_len,
        Vec::new(),
        fail.index,
        fail.tool,
        StopReason::BadSteps,
        false,
        None,
        None,
    )
}

fn from_actuate(env: ActuateEnvelope) -> StepOutcome {
    StepOutcome {
        ok: env.ok,
        frozen: env.frozen,
        foregrounded: Some(env.foregrounded),
        settled: Some(env.settled),
        miss: env.miss,
        error: env.error,
        fence: env.fence,
        challenge: env.challenge,
        id: env.target.id,
        name: None,
        window: None,
    }
}

fn from_activate(env: ActivateEnvelope) -> StepOutcome {
    StepOutcome {
        ok: env.ok,
        frozen: env.frozen,
        foregrounded: Some(env.foregrounded),
        settled: None,
        miss: None,
        error: env.error,
        fence: None,
        challenge: env.challenge,
        id: None,
        name: None,
        window: env.window.map(|w| format!("hwnd:{}", w.hwnd)),
    }
}

fn live_activate(session_id: &str, window: &str) -> Result<StepOutcome, HandsError> {
    actuate::activate(Some(session_id.into()), window.to_string()).map(from_activate)
}

fn click_req(session_id: &str, step: &SequenceStep) -> ActuateRequest {
    match step {
        SequenceStep::Click {
            element_id,
            grid,
            x,
            y,
        }
        | SequenceStep::Hover {
            element_id,
            grid,
            x,
            y,
        } => ActuateRequest {
            session_id: Some(session_id.into()),
            element_id: element_id.clone(),
            grid: grid.clone(),
            x: *x,
            y: *y,
            ..ActuateRequest::default()
        },
        SequenceStep::Scroll {
            dy,
            dx,
            element_id,
            grid,
            x,
            y,
        } => ActuateRequest {
            session_id: Some(session_id.into()),
            element_id: element_id.clone(),
            grid: grid.clone(),
            x: *x,
            y: *y,
            dy: Some(*dy),
            dx: *dx,
            ..ActuateRequest::default()
        },
        SequenceStep::WaitSettle { x, y, w, h } => ActuateRequest {
            session_id: Some(session_id.into()),
            x: *x,
            y: *y,
            w: *w,
            h: *h,
            ..ActuateRequest::default()
        },
        _ => ActuateRequest {
            session_id: Some(session_id.into()),
            ..ActuateRequest::default()
        },
    }
}

fn live_click(session_id: &str, step: &SequenceStep) -> Result<StepOutcome, HandsError> {
    actuate::click(click_req(session_id, step)).map(from_actuate)
}

fn live_hover(session_id: &str, step: &SequenceStep) -> Result<StepOutcome, HandsError> {
    actuate::hover(click_req(session_id, step)).map(from_actuate)
}

fn live_type(session_id: &str, text: &str) -> Result<StepOutcome, HandsError> {
    actuate::type_text(ActuateRequest {
        session_id: Some(session_id.into()),
        text: Some(text.into()),
        ..ActuateRequest::default()
    })
    .map(from_actuate)
}

fn live_key(session_id: &str, name: &str) -> Result<StepOutcome, HandsError> {
    actuate::key(ActuateRequest {
        session_id: Some(session_id.into()),
        name: Some(name.into()),
        ..ActuateRequest::default()
    })
    .map(from_actuate)
}

fn live_scroll(session_id: &str, step: &SequenceStep) -> Result<StepOutcome, HandsError> {
    actuate::scroll(click_req(session_id, step)).map(from_actuate)
}

fn live_wait_settle(session_id: &str, step: &SequenceStep) -> Result<StepOutcome, HandsError> {
    actuate::wait_settle(click_req(session_id, step)).map(from_actuate)
}

fn live_observe(session_id: &str) -> Result<ObserveSummary, HandsError> {
    let env = observe::observe(ObserveRequest {
        session_id: Some(session_id.into()),
        detail: crate::extract::Detail::Default,
        window: None,
        view: ObserveView::Auto,
        from: None,
        card_offset: 0,
    })?;
    Ok(ObserveSummary {
        observe_path: env.observe_path,
        elements_total: env.elements_total,
        challenge_present: env.challenge.present,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    fn ok_out() -> StepOutcome {
        StepOutcome {
            ok: true,
            foregrounded: Some(true),
            settled: Some(true),
            ..StepOutcome::default()
        }
    }

    fn test_hooks() -> SequenceHooks {
        SequenceHooks {
            yielded: || false,
            frozen: || false,
            cooling: |_| false,
            activate: |_, _| Ok(ok_out()),
            click: |_, _| Ok(ok_out()),
            hover: |_, _| Ok(ok_out()),
            type_text: |_, _| Ok(ok_out()),
            key: |_, _| Ok(ok_out()),
            scroll: |_, _| Ok(ok_out()),
            wait_settle: |_, _| Ok(ok_out()),
            observe: |_| {
                Ok(ObserveSummary {
                    observe_path: "C:\\tmp\\observe.json".into(),
                    elements_total: 3,
                    challenge_present: false,
                })
            },
            record_steps: true,
        }
    }

    static LOGS: Mutex<()> = Mutex::new(());

    fn tools(env: &SequenceEnvelope) -> Vec<String> {
        env.executed_steps.iter().map(|s| s.tool.clone()).collect()
    }

    fn run_hooks(steps: Value, hooks: SequenceHooks) -> SequenceEnvelope {
        let _guard = LOGS.lock().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "hands-seq-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&dir);
        let prev = std::env::var_os("HANDS_LOGS_DIR");
        unsafe { std::env::set_var("HANDS_LOGS_DIR", &dir) };
        let env = sequence_with(Some("seq-test".into()), steps, hooks).expect("sequence");
        match prev {
            Some(v) => unsafe { std::env::set_var("HANDS_LOGS_DIR", v) },
            None => unsafe { std::env::remove_var("HANDS_LOGS_DIR") },
        }
        let _ = std::fs::remove_dir_all(&dir);
        env
    }

    #[test]
    fn empty_and_overflow_are_bad_steps_zero_hooks() {
        let empty = run_hooks(json!([]), test_hooks());
        assert_eq!(empty.stop_reason, StopReason::BadSteps);
        assert!(!empty.ok);
        assert_eq!(empty.steps_executed, 0);
        assert!(empty.failed_step_index.is_none());
        assert!(tools(&empty).is_empty());

        let too_many = json!(
            (0..9)
                .map(|_| json!({"tool":"wait_settle"}))
                .collect::<Vec<_>>()
        );
        let over = run_hooks(too_many, test_hooks());
        assert_eq!(over.stop_reason, StopReason::BadSteps);
        assert_eq!(over.steps_executed, 0);
        assert!(tools(&over).is_empty());
    }

    #[test]
    fn forbidden_and_observe_not_last_are_bad_steps() {
        let confirm = run_hooks(json!([{"tool":"confirm","domain":"x"}]), test_hooks());
        assert_eq!(confirm.stop_reason, StopReason::BadSteps);
        assert_eq!(confirm.failed_step_index, Some(0));
        assert!(tools(&confirm).is_empty());

        let mid = run_hooks(
            json!([{"tool":"observe"},{"tool":"type","text":"hi"}]),
            test_hooks(),
        );
        assert_eq!(mid.stop_reason, StopReason::BadSteps);
        assert_eq!(mid.failed_step_index, Some(0));
        assert!(tools(&mid).is_empty());
    }

    #[test]
    fn missing_required_params_are_bad_steps() {
        let typed = run_hooks(json!([{"tool":"type"}]), test_hooks());
        assert_eq!(typed.stop_reason, StopReason::BadSteps);
        assert!(tools(&typed).is_empty());

        let roi = run_hooks(json!([{"tool":"wait_settle","x":1}]), test_hooks());
        assert_eq!(roi.stop_reason, StopReason::BadSteps);
        assert!(tools(&roi).is_empty());
    }

    #[test]
    fn activate_not_foregrounded_skips_later_type() {
        let mut hooks = test_hooks();
        hooks.activate = |_, _| {
            Ok(StepOutcome {
                ok: true,
                foregrounded: Some(false),
                ..StepOutcome::default()
            })
        };
        let env = run_hooks(
            json!([
                {"tool":"activate","window":"hwnd:1"},
                {"tool":"type","text":"bleed"}
            ]),
            hooks,
        );
        assert_eq!(env.stop_reason, StopReason::FocusLost);
        assert_eq!(env.failed_step_index, Some(0));
        assert_eq!(tools(&env), vec!["activate"]);
    }

    #[test]
    fn activate_resolve_fail_is_prerequisite() {
        let mut hooks = test_hooks();
        hooks.activate = |_, _| {
            Ok(StepOutcome {
                ok: false,
                foregrounded: Some(false),
                error: Some("stale hwnd 'hwnd:dead'".into()),
                ..StepOutcome::default()
            })
        };
        let env = run_hooks(
            json!([
                {"tool":"activate","window":"hwnd:dead"},
                {"tool":"key","name":"enter"}
            ]),
            hooks,
        );
        assert_eq!(env.stop_reason, StopReason::PrerequisiteFailed);
        assert_eq!(tools(&env), vec!["activate"]);
    }

    #[test]
    fn click_focus_lost_aborts_even_when_ok() {
        let mut hooks = test_hooks();
        hooks.click = |_, _| {
            Ok(StepOutcome {
                ok: true,
                miss: Some("focus_lost".into()),
                ..StepOutcome::default()
            })
        };
        let env = run_hooks(
            json!([
                {"tool":"click","x":1,"y":1},
                {"tool":"type","text":"nope"}
            ]),
            hooks,
        );
        assert_eq!(env.stop_reason, StopReason::FocusLost);
        assert_eq!(tools(&env), vec!["click"]);
    }

    #[test]
    fn click_no_change_ok_continues() {
        let mut hooks = test_hooks();
        hooks.click = |_, _| {
            Ok(StepOutcome {
                ok: true,
                miss: Some("no_change".into()),
                ..StepOutcome::default()
            })
        };
        let env = run_hooks(
            json!([
                {"tool":"click","x":1,"y":1},
                {"tool":"type","text":"ok"}
            ]),
            hooks,
        );
        assert_eq!(env.stop_reason, StopReason::Completed);
        assert!(env.ok);
        assert_eq!(tools(&env), vec!["click", "type"]);
        assert_eq!(env.executed_steps[0].miss.as_deref(), Some("no_change"));
    }

    #[test]
    fn unknown_key_aborts_later_type() {
        let mut hooks = test_hooks();
        hooks.key = |_, name| {
            Ok(StepOutcome {
                ok: false,
                error: Some(format!("unknown key '{name}'; see key --help")),
                name: Some(name.into()),
                ..StepOutcome::default()
            })
        };
        let env = run_hooks(
            json!([
                {"tool":"key","name":"ctrl+w"},
                {"tool":"type","text":"bleed"}
            ]),
            hooks,
        );
        assert_eq!(env.stop_reason, StopReason::UnknownKey);
        assert_eq!(tools(&env), vec!["key"]);
    }

    #[test]
    fn fence_yield_freeze_cooldown_named() {
        let mut hooks = test_hooks();
        hooks.click = |_, _| {
            Ok(StepOutcome {
                ok: false,
                fence: Some(FenceInfo {
                    domain: "cars.com".into(),
                    category: "submit".into(),
                    name: "enter".into(),
                    role: "button".into(),
                    modes: vec!["once".into()],
                }),
                ..StepOutcome::default()
            })
        };
        let fence = run_hooks(
            json!([{"tool":"click","x":1,"y":1},{"tool":"type","text":"x"}]),
            hooks,
        );
        assert_eq!(fence.stop_reason, StopReason::FenceRefused);
        assert!(fence.fence.is_some());
        assert_eq!(tools(&fence), vec!["click"]);

        let mut hooks = test_hooks();
        hooks.yielded = || true;
        let yielded = run_hooks(
            json!([{"tool":"type","text":"x"},{"tool":"key","name":"enter"}]),
            hooks,
        );
        assert_eq!(yielded.stop_reason, StopReason::ChallengeYielded);
        assert!(tools(&yielded).is_empty());

        let mut hooks = test_hooks();
        hooks.frozen = || true;
        let frozen = run_hooks(json!([{"tool":"type","text":"x"}]), hooks);
        assert_eq!(frozen.stop_reason, StopReason::LeaseFrozen);
        assert!(frozen.frozen);
        assert!(tools(&frozen).is_empty());

        let mut hooks = test_hooks();
        hooks.cooling = |_| true;
        let cool = run_hooks(json!([{"tool":"type","text":"x"}]), hooks);
        assert_eq!(cool.stop_reason, StopReason::Cooldown);
        assert_eq!(cool.failed_step_index, Some(0));
        assert!(tools(&cool).is_empty());
    }

    #[test]
    fn abort_skips_trailing_observe() {
        let mut hooks = test_hooks();
        hooks.key = |_, _| {
            Ok(StepOutcome {
                ok: false,
                error: Some("unknown key 'ctrl+w'; see key --help".into()),
                ..StepOutcome::default()
            })
        };
        let env = run_hooks(
            json!([
                {"tool":"key","name":"ctrl+w"},
                {"tool":"observe"}
            ]),
            hooks,
        );
        assert_eq!(env.stop_reason, StopReason::UnknownKey);
        assert!(env.observe_path.is_none());
        assert_eq!(tools(&env), vec!["key"]);
    }

    #[test]
    fn eight_compact_rows_fit_budget() {
        let mut steps = Vec::new();
        for i in 0..7 {
            steps.push(json!({"tool":"wait_settle","x":i,"y":i,"w":10,"h":10}));
        }
        steps.push(json!({"tool":"observe"}));
        let env = run_hooks(Value::Array(steps), test_hooks());
        assert_eq!(env.stop_reason, StopReason::Completed);
        let json = serialize_envelope(&env).unwrap();
        assert!(
            json.len() <= ENVELOPE_MAX_BYTES,
            "envelope {} bytes",
            json.len()
        );
        assert!(!env.frozen);
        assert_eq!(env.executed_steps.len(), 8);
        assert!(env.observe_path.is_some());
    }

    #[test]
    fn shrink_drops_observe_then_executed_tail() {
        let env = SequenceEnvelope {
            session_id: "s".into(),
            ok: true,
            frozen: false,
            steps_total: 8,
            steps_executed: 8,
            failed_step_index: None,
            failed_step_tool: None,
            stop_reason: StopReason::Completed,
            executed_steps: (0..8)
                .map(|_| ExecutedStep {
                    tool: "wait_settle".into(),
                    ok: true,
                    id: Some("x".repeat(2000)),
                    name: Some("n".repeat(2000)),
                    window: Some("w".repeat(2000)),
                    foregrounded: Some(true),
                    miss: None,
                    settled: Some(true),
                })
                .collect(),
            fence: None,
            challenge: None,
            observe_path: Some("C:\\tmp\\observe.json".into()),
            elements_total: Some(9),
            observe_challenge_present: Some(false),
            attempt: None,
            cooldown_ms: None,
            loop_suspected: false,
            guidance: None,
        };
        let before = env.executed_steps.len();
        let shrunk = shrink_to_budget(env).expect("shrink");
        assert!(shrunk.observe_path.is_none());
        assert!(shrunk.executed_steps.len() < before);
        let json = serialize_envelope(&shrunk).unwrap();
        assert!(json.len() <= ENVELOPE_MAX_BYTES);
    }

    #[test]
    fn live_cooling_and_from_activate_source_lock() {
        let src = include_str!("sequence.rs");
        let live = src.find("fn live() -> Self").expect("live");
        let live_end = src[live..].find("\npub fn run(").expect("run follows live");
        let live_body = &src[live..live + live_end];
        assert!(
            live_body.contains("cooling: cooldown::is_cooling"),
            "live cooling must call cooldown::is_cooling:\n{live_body}"
        );
        let from = src.find("fn from_activate").expect("from_activate");
        let from_end = src[from..]
            .find("\nfn live_activate")
            .expect("live_activate follows");
        let from_body = &src[from..from + from_end];
        assert!(
            from_body.contains("frozen: env.frozen"),
            "from_activate must read env.frozen:\n{from_body}"
        );
        assert!(
            !from_body.contains("frozen: false"),
            "from_activate must not hardcode frozen false:\n{from_body}"
        );
    }

    #[test]
    fn focus_lost_ok_true_notes_once() {
        let _g = crate::cooldown::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::cooldown::reset_for_test();
        let mut hooks = test_hooks();
        hooks.click = |_, _| {
            Ok(StepOutcome {
                ok: true,
                miss: Some("focus_lost".into()),
                ..StepOutcome::default()
            })
        };
        let env = run_hooks(json!([{"tool":"click","x":1,"y":1}]), hooks);
        assert_eq!(env.stop_reason, StopReason::FocusLost);
        assert_eq!(crate::cooldown::snapshot("seq-test").attempt, 1);
        crate::cooldown::reset_for_test();
    }

    #[test]
    fn dispatch_err_notes_once() {
        let _g = crate::cooldown::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::cooldown::reset_for_test();
        let mut hooks = test_hooks();
        hooks.key = |_, _| {
            Err(HandsError::Input(
                "unknown key 'ctrl+w'; see key --help".into(),
            ))
        };
        let env = run_hooks(json!([{"tool":"key","name":"ctrl+w"}]), hooks);
        assert_eq!(env.stop_reason, StopReason::UnknownKey);
        assert_eq!(crate::cooldown::snapshot("seq-test").attempt, 1);
        crate::cooldown::reset_for_test();
    }

    #[test]
    fn abort_observe_does_not_reset() {
        let _g = crate::cooldown::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::cooldown::reset_for_test();
        crate::cooldown::note_rejection("seq-test");
        crate::cooldown::note_rejection("seq-test");
        crate::cooldown::note_rejection("seq-test");
        assert!(crate::cooldown::is_cooling("seq-test"));
        let mut hooks = test_hooks();
        hooks.key = |_, _| {
            Ok(StepOutcome {
                ok: false,
                error: Some("unknown key 'ctrl+w'; see key --help".into()),
                ..StepOutcome::default()
            })
        };
        let env = run_hooks(
            json!([
                {"tool":"key","name":"ctrl+w"},
                {"tool":"observe"}
            ]),
            hooks,
        );
        assert_eq!(env.stop_reason, StopReason::UnknownKey);
        assert!(env.observe_path.is_none());
        assert!(crate::cooldown::is_cooling("seq-test"));
        crate::cooldown::reset_for_test();
    }

    #[test]
    fn success_trailing_observe_resets_when_not_frozen() {
        let _g = crate::cooldown::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _lease = crate::lease::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::cooldown::reset_for_test();
        crate::lease::reset_for_test();
        crate::cooldown::note_rejection("seq-test");
        crate::cooldown::note_rejection("seq-test");
        crate::cooldown::note_rejection("seq-test");
        let mut hooks = test_hooks();
        hooks.observe = |_| {
            crate::cooldown::note_observe("seq-test");
            Ok(ObserveSummary {
                observe_path: "C:\\tmp\\observe.json".into(),
                elements_total: 3,
                challenge_present: false,
            })
        };
        let env = run_hooks(
            json!([
                {"tool":"wait_settle","x":1,"y":1,"w":10,"h":10},
                {"tool":"observe"}
            ]),
            hooks,
        );
        assert_eq!(env.stop_reason, StopReason::Completed);
        assert!(!crate::cooldown::is_cooling("seq-test"));
        crate::cooldown::reset_for_test();
        crate::lease::reset_for_test();
    }

    #[test]
    fn bad_steps_does_not_note() {
        let _g = crate::cooldown::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::cooldown::reset_for_test();
        let env = run_hooks(json!([]), test_hooks());
        assert_eq!(env.stop_reason, StopReason::BadSteps);
        assert_eq!(crate::cooldown::snapshot("seq-test").attempt, 0);
        crate::cooldown::reset_for_test();
    }

    #[test]
    fn step_zero_cooling_notes_once() {
        let _g = crate::cooldown::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::cooldown::reset_for_test();
        let mut hooks = test_hooks();
        hooks.cooling = |_| true;
        let env = run_hooks(json!([{"tool":"type","text":"x"}]), hooks);
        assert_eq!(env.stop_reason, StopReason::Cooldown);
        assert_eq!(env.attempt, Some(1));
        assert_eq!(crate::cooldown::snapshot("seq-test").attempt, 1);
        crate::cooldown::reset_for_test();
    }
}
