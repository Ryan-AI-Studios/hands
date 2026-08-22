//! Unattended challenge solver. Research identity only.

use std::time::Duration;

use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::actuate::{self, ActuateRequest};
use crate::attach::{self, Identity};
use crate::challenge::{
    self, CHALLENGE_SCHEMA, ChallengeEnvelope, ChallengeInfo, title_is_interstitial,
};
use crate::classify::contains_phrase;
use crate::error::HandsError;
use crate::lease;
use crate::logs;
use crate::observe::{self, ObserveEnvelope, ObserveRequest};

pub const RESEARCH_ONLY: &str = "research identity only";
pub const SOLVE_CAP: u8 = 8;
pub const SOLVER_URL_ENV: &str = "HANDS_SOLVER_URL";
pub const SOLVER_KEY_ENV: &str = "HANDS_SOLVER_KEY";
pub const SOLVER_TIMEOUT_ENV: &str = "HANDS_SOLVER_TIMEOUT_MS";
const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MIN_TIMEOUT_MS: u64 = 5_000;
const MAX_TIMEOUT_MS: u64 = 180_000;
const WAIT_NOT_CLICK: &str = "wait, do not click interstitial";

#[derive(Debug, Clone, Default)]
pub struct SolveActions {
    pub clicks: Vec<(i32, i32)>,
    pub text: Option<String>,
}

pub trait SolveBackend {
    fn actions(&self, envelope: &ObserveEnvelope) -> Result<SolveActions, HandsError>;
    fn name(&self) -> &'static str {
        "computer_use"
    }
}

pub struct ComputerUseBackend;

impl SolveBackend for ComputerUseBackend {
    fn actions(&self, envelope: &ObserveEnvelope) -> Result<SolveActions, HandsError> {
        computer_use_actions(envelope)
    }
}

pub fn computer_use_actions(envelope: &ObserveEnvelope) -> Result<SolveActions, HandsError> {
    let el = envelope.elements.iter().find(|e| {
        let t = e.text.as_deref().unwrap_or("");
        contains_phrase(t, "i'm not a robot") || contains_phrase(t, "i am not a robot")
    });
    let Some(el) = el else {
        return Err(HandsError::Challenge(
            "no challenge control in fused map".into(),
        ));
    };
    let (x, y) = el.rect.center();
    Ok(SolveActions {
        clicks: vec![(x, y)],
        text: None,
    })
}

pub struct SolveHooks<'a> {
    pub observe: &'a dyn Fn() -> Result<ObserveEnvelope, HandsError>,
    pub click: &'a dyn Fn(i32, i32) -> Result<(), HandsError>,
    pub type_text: &'a dyn Fn(&str) -> Result<(), HandsError>,
    pub is_frozen: &'a dyn Fn() -> bool,
}

pub fn run_solve(session_id: String) -> Result<ChallengeEnvelope, HandsError> {
    if attach::current_identity() != Identity::Research {
        return refuse_daily(&session_id);
    }
    let _lease = lease::install()?;
    let sid = session_id.clone();
    let observe = || {
        observe::observe(ObserveRequest {
            session_id: Some(sid.clone()),
            detail: crate::extract::Detail::Default,
        })
    };
    let click = |x: i32, y: i32| {
        actuate::click(ActuateRequest {
            session_id: Some(sid.clone()),
            x: Some(x),
            y: Some(y),
            ..ActuateRequest::default()
        })
        .map(|_| ())
    };
    let type_text = |text: &str| {
        actuate::type_text(ActuateRequest {
            session_id: Some(sid.clone()),
            text: Some(text.to_string()),
            ..ActuateRequest::default()
        })
        .map(|_| ())
    };
    let hooks = SolveHooks {
        observe: &observe,
        click: &click,
        type_text: &type_text,
        is_frozen: &lease::is_frozen,
    };
    let computer = ComputerUseBackend;
    let url = std::env::var(SOLVER_URL_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(url) = url {
        let http = HttpBackend::from_url(&url)?;
        run_solve_with(session_id, &http, &hooks)
    } else {
        run_solve_with(session_id, &computer, &hooks)
    }
}

pub fn run_solve_with(
    session_id: String,
    backend: &dyn SolveBackend,
    hooks: &SolveHooks<'_>,
) -> Result<ChallengeEnvelope, HandsError> {
    if attach::current_identity() != Identity::Research {
        return refuse_daily(&session_id);
    }
    let first = (hooks.observe)()?;
    if !first.challenge.present {
        let _ = logs::record_actuate(&session_id, "challenge", true, None, None, None, None, None);
        return solve_envelope(
            session_id,
            true,
            first.challenge,
            Some(true),
            Some(0),
            Some(backend.name().to_string()),
            None,
        );
    }
    if is_interstitial(&first) {
        return interstitial_refuse(session_id, first.challenge, backend.name());
    }
    let mut last = first;
    let mut cycles: u8 = 0;
    while cycles < SOLVE_CAP {
        if (hooks.is_frozen)() {
            return frozen_abort(session_id, last.challenge, cycles, backend.name());
        }
        let actions = backend.actions(&last)?;
        for (x, y) in &actions.clicks {
            if (hooks.is_frozen)() {
                return frozen_abort(session_id, last.challenge, cycles, backend.name());
            }
            (hooks.click)(*x, *y)?;
        }
        if let Some(text) = actions.text.as_deref().filter(|t| !t.is_empty()) {
            if (hooks.is_frozen)() {
                return frozen_abort(session_id, last.challenge, cycles, backend.name());
            }
            (hooks.type_text)(text)?;
        }
        cycles = cycles.saturating_add(1);
        last = (hooks.observe)()?;
        if !last.challenge.present {
            let _ =
                logs::record_actuate(&session_id, "challenge", true, None, None, None, None, None);
            return solve_envelope(
                session_id,
                true,
                last.challenge,
                Some(true),
                Some(cycles),
                Some(backend.name().to_string()),
                None,
            );
        }
        if is_interstitial(&last) {
            return interstitial_refuse(session_id, last.challenge, backend.name());
        }
    }
    let err = format!("challenge still present after {SOLVE_CAP} solve cycles");
    let _ = logs::record_actuate(
        &session_id,
        "challenge",
        false,
        Some(err.as_str()),
        None,
        None,
        None,
        None,
    );
    solve_envelope(
        session_id,
        false,
        last.challenge,
        Some(false),
        Some(cycles),
        Some(backend.name().to_string()),
        Some(err),
    )
}

fn refuse_daily(session_id: &str) -> Result<ChallengeEnvelope, HandsError> {
    let _ = logs::record_actuate(
        session_id,
        "challenge",
        false,
        Some(RESEARCH_ONLY),
        None,
        None,
        None,
        None,
    );
    solve_envelope(
        session_id.to_string(),
        false,
        challenge::snapshot(),
        Some(false),
        Some(0),
        None,
        Some(RESEARCH_ONLY.to_string()),
    )
}

fn is_interstitial(env: &ObserveEnvelope) -> bool {
    env.challenge
        .kind
        .as_deref()
        .is_some_and(|k| k == "interstitial")
        || title_is_interstitial(&env.extract.title)
}

fn interstitial_refuse(
    session_id: String,
    info: ChallengeInfo,
    backend: &str,
) -> Result<ChallengeEnvelope, HandsError> {
    let _ = logs::record_actuate(
        &session_id,
        "challenge",
        false,
        Some(WAIT_NOT_CLICK),
        None,
        None,
        None,
        None,
    );
    solve_envelope(
        session_id,
        false,
        info,
        Some(false),
        Some(0),
        Some(backend.to_string()),
        Some(WAIT_NOT_CLICK.to_string()),
    )
}

fn frozen_abort(
    session_id: String,
    info: ChallengeInfo,
    cycles: u8,
    backend: &str,
) -> Result<ChallengeEnvelope, HandsError> {
    let err = "frozen";
    let _ = logs::record_actuate(
        &session_id,
        "challenge",
        false,
        Some(err),
        None,
        None,
        None,
        None,
    );
    solve_envelope(
        session_id,
        false,
        info,
        Some(false),
        Some(cycles),
        Some(backend.to_string()),
        Some(err.into()),
    )
}

fn solve_envelope(
    session_id: String,
    ok: bool,
    info: ChallengeInfo,
    solved: Option<bool>,
    cycles: Option<u8>,
    backend: Option<String>,
    error: Option<String>,
) -> Result<ChallengeEnvelope, HandsError> {
    challenge::finalize_envelope(ChallengeEnvelope {
        schema: CHALLENGE_SCHEMA.into(),
        session_id,
        ok,
        present: info.present,
        kind: info.kind,
        attempts: info.attempts,
        yielded: info.yielded,
        reason: info.reason,
        watched: false,
        elapsed_ms: None,
        solved,
        cycles,
        backend,
        error,
    })
}

struct HttpResp {
    status: u16,
    body: String,
}

trait HttpTransport {
    fn post_json(&self, body: &Value) -> Result<HttpResp, HandsError>;
}

struct UreqTransport {
    agent: ureq::Agent,
    url: String,
    api_key: Option<String>,
}

impl UreqTransport {
    fn new(url: &str) -> Result<Self, HandsError> {
        validate_solver_url(url)?;
        let timeout_ms = solver_timeout_ms();
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .proxy(None)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_millis(timeout_ms)))
            .build();
        let api_key = std::env::var(SOLVER_KEY_ENV)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Ok(Self {
            agent: ureq::Agent::new_with_config(config),
            url: url.to_string(),
            api_key,
        })
    }
}

impl HttpTransport for UreqTransport {
    fn post_json(&self, body: &Value) -> Result<HttpResp, HandsError> {
        let payload = body.to_string();
        let mut req = self
            .agent
            .post(&self.url)
            .header("Content-Type", "application/json");
        if let Some(key) = &self.api_key {
            req = req.header("Authorization", format!("Bearer {key}"));
        }
        match req.send(payload) {
            Ok(mut resp) => {
                let status = resp.status().as_u16();
                let body = resp.body_mut().read_to_string().unwrap_or_default();
                Ok(HttpResp { status, body })
            }
            Err(err) => Err(HandsError::Challenge(format!("solver http: {err}"))),
        }
    }
}

pub struct HttpBackend {
    transport: Box<dyn HttpTransport>,
}

impl HttpBackend {
    fn from_url(url: &str) -> Result<Self, HandsError> {
        Ok(Self {
            transport: Box::new(UreqTransport::new(url)?),
        })
    }

    #[cfg(test)]
    fn with_transport(transport: Box<dyn HttpTransport>) -> Self {
        Self { transport }
    }

    fn actions_http(&self, envelope: &ObserveEnvelope) -> Result<SolveActions, HandsError> {
        let png_b64 = screenshot_b64(&envelope.screenshot_path).unwrap_or_default();
        let kind = envelope
            .challenge
            .kind
            .clone()
            .unwrap_or_else(|| "generic".into());
        let instruction = envelope.challenge.reason.clone().unwrap_or_default();
        let body = json!({
            "kind": kind,
            "png_b64": png_b64,
            "instruction": instruction,
        });
        let resp = self.transport.post_json(&body)?;
        if !(200..300).contains(&resp.status) {
            return Err(HandsError::Challenge(format!(
                "solver http status {}",
                resp.status
            )));
        }
        parse_http_actions(&resp.body, image_origin(envelope))
    }
}

impl SolveBackend for HttpBackend {
    fn actions(&self, envelope: &ObserveEnvelope) -> Result<SolveActions, HandsError> {
        self.actions_http(envelope)
    }

    fn name(&self) -> &'static str {
        "http"
    }
}

fn screenshot_b64(path: &str) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(base64::engine::general_purpose::STANDARD.encode(bytes))
}

fn image_origin(envelope: &ObserveEnvelope) -> (i32, i32) {
    (envelope.space.origin_x, envelope.space.origin_y)
}

#[derive(Debug, Deserialize)]
struct HttpClick {
    x: i32,
    y: i32,
}

#[derive(Debug, Deserialize)]
struct HttpSolveBody {
    clicks: Vec<HttpClick>,
    #[serde(default)]
    text: Option<String>,
}

pub fn parse_http_actions(body: &str, origin: (i32, i32)) -> Result<SolveActions, HandsError> {
    let parsed: HttpSolveBody = serde_json::from_str(body)
        .map_err(|err| HandsError::Challenge(format!("solver http json: {err}")))?;
    Ok(SolveActions {
        clicks: parsed
            .clicks
            .into_iter()
            .map(|c| (c.x + origin.0, c.y + origin.1))
            .collect(),
        text: parsed.text.filter(|s| !s.is_empty()),
    })
}

pub fn validate_solver_url(raw: &str) -> Result<(), HandsError> {
    let url = raw.trim();
    if url.is_empty() {
        return Err(HandsError::Challenge("HANDS_SOLVER_URL is empty".into()));
    }
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("file:") || lower.starts_with("javascript:") {
        return Err(HandsError::Challenge(
            "solver url must be http loopback or https".into(),
        ));
    }
    if let Some(rest) = lower.strip_prefix("https://") {
        if host_of(rest).is_empty() {
            return Err(HandsError::Challenge(
                "solver https url missing host".into(),
            ));
        }
        return Ok(());
    }
    if let Some(rest) = lower.strip_prefix("http://") {
        let host = host_of(rest);
        if is_loopback_host(&host) {
            return Ok(());
        }
        return Err(HandsError::Challenge(
            "http solver url must be loopback".into(),
        ));
    }
    Err(HandsError::Challenge(
        "solver url must be http loopback or https".into(),
    ))
}

fn host_of(after_scheme: &str) -> String {
    let hostport = after_scheme.split('/').next().unwrap_or(after_scheme);
    if let Some(inner) = hostport.strip_prefix('[') {
        inner.split(']').next().unwrap_or("").to_string()
    } else {
        hostport.split(':').next().unwrap_or("").to_string()
    }
}

fn is_loopback_host(host: &str) -> bool {
    let h = host.trim();
    h == "127.0.0.1" || h == "localhost" || h == "::1"
}

fn solver_timeout_ms() -> u64 {
    std::env::var(SOLVER_TIMEOUT_ENV)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS)
}

#[cfg(test)]
struct CountingBackend {
    inner: FixtureBackend,
    calls: std::sync::atomic::AtomicU32,
}

#[cfg(test)]
impl CountingBackend {
    fn new(inner: FixtureBackend) -> Self {
        Self {
            inner,
            calls: std::sync::atomic::AtomicU32::new(0),
        }
    }

    fn calls(&self) -> u32 {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(test)]
impl SolveBackend for CountingBackend {
    fn actions(&self, envelope: &ObserveEnvelope) -> Result<SolveActions, HandsError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.actions(envelope)
    }
}

#[cfg(test)]
struct FixtureBackend {
    actions: SolveActions,
}

#[cfg(test)]
impl SolveBackend for FixtureBackend {
    fn actions(&self, _envelope: &ObserveEnvelope) -> Result<SolveActions, HandsError> {
        Ok(self.actions.clone())
    }
}

#[cfg(test)]
struct FakeHttp {
    hops: std::sync::Mutex<Vec<Result<HttpResp, HandsError>>>,
}

#[cfg(test)]
impl FakeHttp {
    fn new(hops: Vec<Result<HttpResp, HandsError>>) -> Self {
        Self {
            hops: std::sync::Mutex::new(hops),
        }
    }
}

#[cfg(test)]
impl HttpTransport for FakeHttp {
    fn post_json(&self, _body: &Value) -> Result<HttpResp, HandsError> {
        let mut hops = self.hops.lock().unwrap_or_else(|e| e.into_inner());
        if hops.is_empty() {
            Err(HandsError::Challenge("FakeHttp exhausted".into()))
        } else {
            hops.remove(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::challenge::{ChallengeInfo, TEST_LOCK};
    use crate::extract::{Element, Extract};
    use crate::space::{Rect, Space};
    use std::cell::Cell;
    use std::sync::Mutex;

    fn stub_observe(
        present: bool,
        interstitial: bool,
        elements: Vec<Element>,
        title: &str,
    ) -> ObserveEnvelope {
        let kind = if interstitial {
            Some("interstitial".into())
        } else if present {
            Some("recaptcha".into())
        } else {
            None
        };
        ObserveEnvelope {
            session_id: "s".into(),
            screenshot_path: r"C:\tmp\a.png".into(),
            observe_path: r"C:\tmp\a.json".into(),
            space: Space {
                origin_x: 0,
                origin_y: 0,
                width: 1000,
                height: 1000,
                cell_px: 100,
            },
            viewport: Some(Rect {
                x: 10,
                y: 20,
                w: 800,
                h: 600,
            }),
            extract: Extract {
                title: title.into(),
                url: None,
                main_text: String::new(),
                cards: Vec::new(),
                dialogs: Vec::new(),
                result_count: None,
                local_matches: None,
                empty_state: None,
                zip: None,
                radius: None,
            },
            elements,
            elements_total: 0,
            elements_truncated: false,
            chrome_connected: false,
            chrome_hint: None,
            challenge: ChallengeInfo {
                present,
                kind,
                attempts: if present { 2 } else { 0 },
                yielded: false,
                reason: if present {
                    Some("i'm not a robot".into())
                } else {
                    None
                },
            },
        }
    }

    fn robot_el() -> Element {
        Element {
            id: "chr:0".into(),
            role: "button".into(),
            text: Some("I'm not a robot".into()),
            rect: Rect {
                x: 100,
                y: 200,
                w: 40,
                h: 20,
            },
            grid: None,
        }
    }

    #[test]
    fn daily_solve_refuses_without_backend() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        challenge::reset_for_test();
        attach::set_identity_for_test(Identity::Daily);
        let backend = CountingBackend::new(FixtureBackend {
            actions: SolveActions {
                clicks: vec![(1, 2)],
                text: None,
            },
        });
        let observes = Mutex::new(0u32);
        let clicks = Mutex::new(Vec::<(i32, i32)>::new());
        let hooks = SolveHooks {
            observe: &|| {
                *observes.lock().unwrap() += 1;
                Ok(stub_observe(true, false, vec![robot_el()], "Challenge"))
            },
            click: &|x, y| {
                clicks.lock().unwrap().push((x, y));
                Ok(())
            },
            type_text: &|_| Ok(()),
            is_frozen: &|| false,
        };
        let env = run_solve_with("s-daily".into(), &backend, &hooks).unwrap();
        assert!(!env.ok);
        assert!(
            env.error
                .as_deref()
                .is_some_and(|e| e.contains(RESEARCH_ONLY)),
            "{:?}",
            env.error
        );
        assert_eq!(backend.calls(), 0);
        assert_eq!(*observes.lock().unwrap(), 0);
        assert!(clicks.lock().unwrap().is_empty());
        challenge::reset_for_test();
    }

    #[test]
    fn research_fixture_backend_solves_when_gone() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        challenge::reset_for_test();
        attach::set_identity_for_test(Identity::Research);
        let backend = FixtureBackend {
            actions: SolveActions {
                clicks: vec![(1, 2)],
                text: None,
            },
        };
        let n = Cell::new(0u32);
        let clicks = Mutex::new(Vec::<(i32, i32)>::new());
        let hooks = SolveHooks {
            observe: &|| {
                let i = n.get();
                n.set(i + 1);
                if i == 0 {
                    Ok(stub_observe(true, false, vec![robot_el()], "Challenge"))
                } else {
                    Ok(stub_observe(false, false, vec![], "Home"))
                }
            },
            click: &|x, y| {
                clicks.lock().unwrap().push((x, y));
                Ok(())
            },
            type_text: &|_| Ok(()),
            is_frozen: &|| false,
        };
        let env = run_solve_with("s-ok".into(), &backend, &hooks).unwrap();
        assert!(env.ok);
        assert_eq!(env.solved, Some(true));
        assert_eq!(env.cycles, Some(1));
        assert_eq!(env.backend.as_deref(), Some("computer_use"));
        assert_eq!(*clicks.lock().unwrap(), vec![(1, 2)]);
        challenge::reset_for_test();
    }

    #[test]
    fn research_interstitial_does_not_click() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        challenge::reset_for_test();
        attach::set_identity_for_test(Identity::Research);
        let backend = CountingBackend::new(FixtureBackend {
            actions: SolveActions {
                clicks: vec![(9, 9)],
                text: None,
            },
        });
        let clicks = Mutex::new(Vec::<(i32, i32)>::new());
        let hooks = SolveHooks {
            observe: &|| {
                Ok(stub_observe(
                    true,
                    true,
                    vec![robot_el()],
                    "Just a moment...",
                ))
            },
            click: &|x, y| {
                clicks.lock().unwrap().push((x, y));
                Ok(())
            },
            type_text: &|_| Ok(()),
            is_frozen: &|| false,
        };
        let env = run_solve_with("s-int".into(), &backend, &hooks).unwrap();
        assert!(!env.ok);
        assert!(
            env.error
                .as_deref()
                .is_some_and(|e| e.contains("wait") && e.contains("click")),
            "{:?}",
            env.error
        );
        assert_eq!(backend.calls(), 0);
        assert!(clicks.lock().unwrap().is_empty());
        challenge::reset_for_test();
    }

    #[test]
    fn http_200_parses_clicks_with_viewport_origin() {
        let body = r#"{"clicks":[{"x":1,"y":2}],"text":"ok"}"#;
        let actions = parse_http_actions(body, (10, 20)).unwrap();
        assert_eq!(actions.clicks, vec![(11, 22)]);
        assert_eq!(actions.text.as_deref(), Some("ok"));
    }

    #[test]
    fn http_fake_200_actions_no_sendinput() {
        let transport = FakeHttp::new(vec![Ok(HttpResp {
            status: 200,
            body: r#"{"clicks":[{"x":1,"y":2}]}"#.into(),
        })]);
        let http = HttpBackend::with_transport(Box::new(transport));
        let env = stub_observe(true, false, vec![robot_el()], "Challenge");
        let actions = http.actions(&env).unwrap();
        assert_eq!(actions.clicks, vec![(1, 2)]);
    }

    #[test]
    fn http_500_is_error_without_clicks() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        challenge::reset_for_test();
        attach::set_identity_for_test(Identity::Research);
        let transport = FakeHttp::new(vec![Ok(HttpResp {
            status: 500,
            body: "nope".into(),
        })]);
        let http = HttpBackend::with_transport(Box::new(transport));
        let clicks = Mutex::new(Vec::<(i32, i32)>::new());
        let hooks = SolveHooks {
            observe: &|| Ok(stub_observe(true, false, vec![robot_el()], "Challenge")),
            click: &|x, y| {
                clicks.lock().unwrap().push((x, y));
                Ok(())
            },
            type_text: &|_| Ok(()),
            is_frozen: &|| false,
        };
        let err = run_solve_with("s-500".into(), &http, &hooks).unwrap_err();
        assert!(err.to_string().contains("500"), "{err}");
        assert!(clicks.lock().unwrap().is_empty());
        challenge::reset_for_test();
    }

    #[test]
    fn solver_url_allowlist() {
        assert!(validate_solver_url("https://solver.example").is_ok());
        assert!(validate_solver_url("http://127.0.0.1:9/solve").is_ok());
        assert!(validate_solver_url("http://localhost/s").is_ok());
        assert!(validate_solver_url("file:///tmp/x").is_err());
        assert!(validate_solver_url("http://example.com/s").is_err());
        assert!(validate_solver_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn solver_source_forbids_gemma_and_pick() {
        let src = include_str!("solver.rs");
        let port = ["80", "81"].concat();
        let pick_mod = ["pick", "::"].concat();
        assert!(
            !src.contains(&port),
            "solver.rs must not mention Gemma port"
        );
        assert!(!src.contains(&pick_mod), "solver.rs must not import pick");
        let confirm = ["run", "_confirm"].concat();
        let send = ["Send", "Input"].concat();
        assert!(!src.contains(&confirm));
        assert!(!src.contains(&send));
    }

    #[test]
    fn cargo_still_forbids_vendor_solver_crates() {
        let cargo =
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).to_ascii_lowercase();
        for needle in ["onnx", "whisper", "2captcha", "anticaptcha"] {
            assert!(
                !cargo.contains(needle),
                "Cargo.toml must not mention {needle}"
            );
        }
    }

    #[test]
    fn frozen_aborts_without_further_clicks() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        challenge::reset_for_test();
        attach::set_identity_for_test(Identity::Research);
        let backend = FixtureBackend {
            actions: SolveActions {
                clicks: vec![(1, 2)],
                text: None,
            },
        };
        let frozen = Cell::new(true);
        let clicks = Mutex::new(Vec::<(i32, i32)>::new());
        let hooks = SolveHooks {
            observe: &|| Ok(stub_observe(true, false, vec![robot_el()], "Challenge")),
            click: &|x, y| {
                clicks.lock().unwrap().push((x, y));
                Ok(())
            },
            type_text: &|_| Ok(()),
            is_frozen: &|| frozen.get(),
        };
        let env = run_solve_with("s-fr".into(), &backend, &hooks).unwrap();
        assert!(!env.ok);
        assert_eq!(env.error.as_deref(), Some("frozen"));
        assert!(clicks.lock().unwrap().is_empty());
        challenge::reset_for_test();
    }
}
