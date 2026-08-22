//! Visible challenge detector + episode machine. Not a solver.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::classify::{contains_phrase, normalize_host};
use crate::error::HandsError;
use crate::extract::{Element, take_chars};
use crate::lease;
use crate::logs;
use crate::observe::{ENVELOPE_MAX_BYTES, ObserveSidecar};
use crate::session::resolve_session_id_from_os;

pub const CHALLENGE_SCHEMA: &str = "hands.challenge/v1";
pub const WATCH_TIMEOUT_ENV: &str = "HANDS_CHALLENGE_WATCH_TIMEOUT_MS";
pub const DEFAULT_WATCH_TIMEOUT_MS: u64 = 120_000;
pub const MIN_WATCH_TIMEOUT_MS: u64 = 1_000;
pub const MAX_WATCH_TIMEOUT_MS: u64 = 300_000;
pub const WATCH_POLL_MS: u64 = 1_000;
pub const YIELD_ERROR: &str = "yielded: challenge UI still present after two tries";
const REASON_MAX: usize = 80;
const MAX_ATTEMPTS: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChallengeKind {
    Recaptcha,
    Hcaptcha,
    Turnstile,
    Funcaptcha,
    Generic,
    Interstitial,
}

impl ChallengeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recaptcha => "recaptcha",
            Self::Hcaptcha => "hcaptcha",
            Self::Turnstile => "turnstile",
            Self::Funcaptcha => "funcaptcha",
            Self::Generic => "generic",
            Self::Interstitial => "interstitial",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DetectInput<'a> {
    pub title: &'a str,
    pub url: Option<&'a str>,
    pub main_text: &'a str,
    pub elements: &'a [(&'a str, &'a str)],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectHit {
    pub present: bool,
    pub kind: Option<ChallengeKind>,
    pub reason: Option<String>,
}

impl DetectHit {
    pub fn clear() -> Self {
        Self {
            present: false,
            kind: None,
            reason: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ChallengeInfo {
    pub present: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub attempts: u8,
    pub yielded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChallengeEnvelope {
    pub schema: String,
    pub session_id: String,
    pub ok: bool,
    pub present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub attempts: u8,
    pub yielded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub watched: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ChallengeRequest {
    pub session_id: Option<String>,
    pub status: bool,
    pub watch: bool,
    pub observe_path: Option<String>,
}

struct Cand {
    kind: ChallengeKind,
    reason: String,
    rank: usize,
}

#[derive(Clone, Copy)]
enum PhraseNeed {
    None,
    Cloudflare,
    CloudflareOrTurnstile,
    NamedOrRecaptcha,
}

const STRONG_PHRASES: &[(&str, ChallengeKind, PhraseNeed)] = &[
    (
        "i'm not a robot",
        ChallengeKind::Recaptcha,
        PhraseNeed::None,
    ),
    (
        "i am not a robot",
        ChallengeKind::Recaptcha,
        PhraseNeed::None,
    ),
    (
        "verifying you are human",
        ChallengeKind::Turnstile,
        PhraseNeed::None,
    ),
    (
        "verify you are human",
        ChallengeKind::Generic,
        PhraseNeed::None,
    ),
    (
        "confirm you are human",
        ChallengeKind::Generic,
        PhraseNeed::None,
    ),
    ("are you a robot", ChallengeKind::Generic, PhraseNeed::None),
    (
        "human verification",
        ChallengeKind::Generic,
        PhraseNeed::None,
    ),
    (
        "select all images with",
        ChallengeKind::Recaptcha,
        PhraseNeed::NamedOrRecaptcha,
    ),
    (
        "select all squares with",
        ChallengeKind::Recaptcha,
        PhraseNeed::NamedOrRecaptcha,
    ),
    (
        "attention required",
        ChallengeKind::Turnstile,
        PhraseNeed::CloudflareOrTurnstile,
    ),
    (
        "checking your browser",
        ChallengeKind::Turnstile,
        PhraseNeed::Cloudflare,
    ),
];

/// Title needles for Cloudflare-style interstitial pages. `just a moment` is
/// title-only in `detect` (cart toasts live in `main_text`).
const INTERSTITIAL_TITLE_PHRASES: &[&str] = &[
    "just a moment",
    "performing security verification",
    "checking if the site connection is secure",
];

const INTERSTITIAL_BODY_PHRASES: &[&str] = &[
    "performing security verification",
    "checking if the site connection is secure",
];

/// Cloudflare interstitial captions that must not report `settled: true`.
pub fn title_is_interstitial(title: &str) -> bool {
    matching_phrase(title, INTERSTITIAL_TITLE_PHRASES).is_some()
}

fn matching_phrase<'a>(hay: &str, phrases: &[&'a str]) -> Option<&'a str> {
    phrases
        .iter()
        .copied()
        .filter(|p| contains_phrase(hay, p))
        .max_by_key(|p| p.len())
}

const VENDOR_TOKENS: &[(&str, ChallengeKind)] = &[
    ("recaptcha", ChallengeKind::Recaptcha),
    ("hcaptcha", ChallengeKind::Hcaptcha),
    ("turnstile", ChallengeKind::Turnstile),
    ("funcaptcha", ChallengeKind::Funcaptcha),
    ("arkose", ChallengeKind::Funcaptcha),
    ("geetest", ChallengeKind::Generic),
];

pub fn detect(input: &DetectInput<'_>) -> DetectHit {
    let url = input.url.unwrap_or("");
    let (host, path) = host_and_path(url);
    let mut cands: Vec<Cand> = Vec::new();

    let mut host_classified = false;
    if let Some(host) = host.as_deref()
        && let Some((kind, reason)) = match_host(host, &path)
    {
        push_cand(&mut cands, kind, reason, 0);
        host_classified = true;
    }

    // Host kind wins (CF /cdn-cgi stays turnstile). Interstitial titles fill the
    // origin-URL miss (cars.com callback) when match_host is silent.
    if !host_classified
        && let Some(phrase) = matching_phrase(input.title, INTERSTITIAL_TITLE_PHRASES)
    {
        push_cand(
            &mut cands,
            ChallengeKind::Interstitial,
            phrase.to_string(),
            1,
        );
    }

    let mut element_blob = String::new();
    for &(role, text) in input.elements {
        if !element_blob.is_empty() {
            element_blob.push('\n');
        }
        element_blob.push_str(role);
        element_blob.push(' ');
        element_blob.push_str(text);
    }
    let element_blob = element_blob.as_str();
    if !host_classified {
        for &phrase in INTERSTITIAL_BODY_PHRASES {
            if contains_phrase(input.main_text, phrase) || contains_phrase(element_blob, phrase) {
                push_cand(
                    &mut cands,
                    ChallengeKind::Interstitial,
                    phrase.to_string(),
                    1,
                );
            }
        }
    }
    let all_surfaces = [input.title, url, input.main_text, element_blob];
    let token_surfaces = [input.title, url, element_blob];

    let host_hit = host_classified;
    let vendor_on_token_surface = vendor_token_hit(&token_surfaces);
    let recaptcha_chrome = vendor_on_token_surface == Some(ChallengeKind::Recaptcha)
        || host
            .as_deref()
            .is_some_and(|h| matches!(match_host(h, &path), Some((ChallengeKind::Recaptcha, _))));
    let cf_on_any = surfaces_have_token(&all_surfaces, "cloudflare");
    let turnstile_on_any = surfaces_have_token(&all_surfaces, "turnstile");

    let mut phrases: Vec<_> = STRONG_PHRASES.iter().collect();
    phrases.sort_by_key(|b| std::cmp::Reverse(b.0.len()));
    for (i, &(phrase, kind, need)) in phrases.iter().enumerate() {
        if !all_surfaces.iter().any(|s| contains_phrase(s, phrase)) {
            continue;
        }
        let allowed = match need {
            PhraseNeed::None => true,
            PhraseNeed::Cloudflare => cf_on_any || host_is_cloudflare(host.as_deref()),
            PhraseNeed::CloudflareOrTurnstile => {
                cf_on_any || turnstile_on_any || host_is_cloudflare(host.as_deref())
            }
            PhraseNeed::NamedOrRecaptcha => {
                contains_phrase(input.title, phrase)
                    || contains_phrase(url, phrase)
                    || contains_phrase(element_blob, phrase)
                    || recaptcha_chrome
            }
        };
        if allowed {
            push_cand(&mut cands, *kind, phrase.to_string(), i + 1);
        }
    }

    for &(token, kind) in VENDOR_TOKENS {
        if token_surfaces.iter().any(|s| contains_phrase(s, token)) {
            push_cand(&mut cands, kind, token.to_string(), 100);
        }
    }

    if all_surfaces
        .iter()
        .any(|s| contains_phrase(s, "i am human"))
        && (host_hit || vendor_on_token_surface.is_some() || vendor_token_hit(&[url]).is_some())
    {
        let kind = if vendor_on_token_surface.is_some_and(|k| k == ChallengeKind::Hcaptcha)
            || host
                .as_deref()
                .is_some_and(|h| h == "hcaptcha.com" || h.ends_with(".hcaptcha.com"))
        {
            ChallengeKind::Hcaptcha
        } else {
            vendor_on_token_surface.unwrap_or(ChallengeKind::Hcaptcha)
        };
        push_cand(&mut cands, kind, "i am human".into(), 50);
    }

    let Some(best) = cands.into_iter().min_by(|a, b| {
        b.reason
            .len()
            .cmp(&a.reason.len())
            .then(a.rank.cmp(&b.rank))
    }) else {
        return DetectHit::clear();
    };
    DetectHit {
        present: true,
        kind: Some(best.kind),
        reason: Some(best.reason),
    }
}

fn push_cand(cands: &mut Vec<Cand>, kind: ChallengeKind, reason: String, rank: usize) {
    cands.push(Cand {
        kind,
        reason: take_chars(&reason, REASON_MAX),
        rank,
    });
}

fn vendor_token_hit(surfaces: &[&str]) -> Option<ChallengeKind> {
    for &(token, kind) in VENDOR_TOKENS {
        if surfaces.iter().any(|s| contains_phrase(s, token)) {
            return Some(kind);
        }
    }
    None
}

fn surfaces_have_token(surfaces: &[&str], token: &str) -> bool {
    surfaces.iter().any(|s| contains_phrase(s, token))
}

fn host_is_cloudflare(host: Option<&str>) -> bool {
    host.is_some_and(|h| h == "cloudflare.com" || h.ends_with(".cloudflare.com"))
}

fn match_host(host: &str, path: &str) -> Option<(ChallengeKind, String)> {
    let host = normalize_host(host);
    if host == "recaptcha.net" || host.ends_with(".recaptcha.net") || host == "recaptcha.google.com"
    {
        return Some((ChallengeKind::Recaptcha, host));
    }
    if host == "google.com" && path.contains("/recaptcha") {
        return Some((ChallengeKind::Recaptcha, "google.com/recaptcha".into()));
    }
    if host == "hcaptcha.com" || host.ends_with(".hcaptcha.com") {
        return Some((ChallengeKind::Hcaptcha, host));
    }
    if host == "challenges.cloudflare.com" {
        return Some((ChallengeKind::Turnstile, host));
    }
    if (host == "cloudflare.com" || host.ends_with(".cloudflare.com"))
        && path.contains("/cdn-cgi/challenge-platform")
    {
        return Some((
            ChallengeKind::Turnstile,
            "/cdn-cgi/challenge-platform".into(),
        ));
    }
    if path.contains("/cdn-cgi/challenge-platform") {
        return Some((
            ChallengeKind::Interstitial,
            "/cdn-cgi/challenge-platform".into(),
        ));
    }
    if host == "funcaptcha.com" || host.ends_with(".funcaptcha.com") {
        return Some((ChallengeKind::Funcaptcha, host));
    }
    if host == "arkoselabs.com"
        || host.ends_with(".arkoselabs.com")
        || host == "client-api.arkoselabs.com"
    {
        return Some((ChallengeKind::Funcaptcha, host));
    }
    None
}

fn host_and_path(raw: &str) -> (Option<String>, String) {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return (None, String::new());
    }
    let lower = trimmed.to_ascii_lowercase();
    let after_scheme = if let Some(i) = lower.find("://") {
        &trimmed[i + 3..]
    } else {
        trimmed
    };
    let after_user = if let Some(at) = after_scheme.find('@') {
        &after_scheme[at + 1..]
    } else {
        after_scheme
    };
    let host_end = after_user
        .find(|c: char| c == '/' || c == '?' || c == '#' || c == ':' || c.is_whitespace())
        .unwrap_or(after_user.len());
    let host_raw = after_user[..host_end].trim();
    if host_raw.is_empty() || !host_raw.contains('.') {
        return (None, String::new());
    }
    // Host stops at `:`; skip `:` + ASCII digits (port) then take `/` path.
    let after_host = if host_end < after_user.len() {
        &after_user[host_end..]
    } else {
        ""
    };
    let after_port = if let Some(rest) = after_host.strip_prefix(':') {
        let digit_len = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        &rest[digit_len..]
    } else {
        after_host
    };
    let path = if after_port.starts_with('/') {
        let path_end = after_port
            .find(|c: char| c == '?' || c == '#' || c.is_whitespace())
            .unwrap_or(after_port.len());
        after_port[..path_end].to_ascii_lowercase()
    } else {
        String::new()
    };
    (Some(normalize_host(host_raw)), path)
}

/// Pure episode machine. Cap 2. Idle is not a transition.
#[derive(Debug, Clone)]
pub struct ChallengeMachine {
    present: bool,
    kind: Option<ChallengeKind>,
    reason: Option<String>,
    attempts: u8,
    yielded: bool,
    actuated_since_observe: bool,
}

impl ChallengeMachine {
    pub fn new() -> Self {
        Self {
            present: false,
            kind: None,
            reason: None,
            attempts: 0,
            yielded: false,
            actuated_since_observe: false,
        }
    }

    pub fn observe(&mut self, hit: &DetectHit) -> bool {
        if !hit.present {
            *self = Self::new();
            return false;
        }
        self.kind = hit.kind.or(self.kind);
        self.reason = hit.reason.clone().or_else(|| self.reason.clone());
        if !self.present {
            self.present = true;
            self.attempts = 0;
            self.yielded = false;
            self.actuated_since_observe = false;
            return false;
        }
        if self.actuated_since_observe && !self.yielded {
            self.actuated_since_observe = false;
            self.attempts = self.attempts.saturating_add(1).min(MAX_ATTEMPTS);
            if self.attempts >= MAX_ATTEMPTS {
                self.yielded = true;
                return true;
            }
            return false;
        }
        self.actuated_since_observe = false;
        false
    }

    pub fn note_actuation(&mut self) {
        if self.present && !self.yielded {
            self.actuated_since_observe = true;
        }
    }

    pub fn yielded(&self) -> bool {
        self.yielded
    }

    pub fn actuated_since_observe(&self) -> bool {
        self.actuated_since_observe
    }

    pub fn hold(&self) -> bool {
        self.present || self.yielded
    }

    pub fn info(&self) -> ChallengeInfo {
        ChallengeInfo {
            present: self.present,
            kind: self.kind.map(|k| k.as_str().to_string()),
            attempts: self.attempts,
            yielded: self.yielded,
            reason: self.reason.clone(),
        }
    }
}

impl Default for ChallengeMachine {
    fn default() -> Self {
        Self::new()
    }
}

static MACHINE: Mutex<ChallengeMachine> = Mutex::new(ChallengeMachine {
    present: false,
    kind: None,
    reason: None,
    attempts: 0,
    yielded: false,
    actuated_since_observe: false,
});

fn lock() -> std::sync::MutexGuard<'static, ChallengeMachine> {
    MACHINE.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn reset_for_test() {
    *lock() = ChallengeMachine::new();
    lease::set_challenge_hold(false);
}

pub fn apply_observe(session_id: &str, hit: DetectHit) -> ChallengeInfo {
    let mut g = lock();
    let just_yielded = g.observe(&hit);
    lease::set_challenge_hold(g.hold());
    if just_yielded {
        let reason = g.kind.map(|k| k.as_str()).unwrap_or("challenge-ui");
        let _ = logs::record_yield(session_id, reason);
    }
    g.info()
}

pub fn note_actuation() {
    lock().note_actuation();
}

/// Fence-refused click / enter did not attempt input — do not set the flag.
pub fn note_actuation_if_proceeding(fence_refused: bool) {
    if !fence_refused {
        note_actuation();
    }
}

pub fn yielded() -> bool {
    lock().yielded()
}

pub fn snapshot() -> ChallengeInfo {
    lock().info()
}

pub fn detect_from_extract(
    title: &str,
    url: Option<&str>,
    main_text: &str,
    elements: &[Element],
) -> DetectHit {
    let pairs: Vec<(&str, &str)> = elements
        .iter()
        .map(|e| (e.role.as_str(), e.text.as_deref().unwrap_or("")))
        .collect();
    detect(&DetectInput {
        title,
        url,
        main_text,
        elements: &pairs,
    })
}

pub fn watch_timeout_ms() -> u64 {
    std::env::var(WATCH_TIMEOUT_ENV)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_WATCH_TIMEOUT_MS)
        .clamp(MIN_WATCH_TIMEOUT_MS, MAX_WATCH_TIMEOUT_MS)
}

pub trait WatchClock {
    fn now(&self) -> Instant;
    fn sleep(&self, d: Duration);
}

pub struct RealClock;

impl WatchClock for RealClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

pub fn watch_until_gone<C: WatchClock>(
    clock: &C,
    timeout: Duration,
    poll: Duration,
    mut observe_once: impl FnMut() -> Result<ChallengeInfo, HandsError>,
) -> Result<(ChallengeInfo, u64), HandsError> {
    let start = clock.now();
    loop {
        let info = observe_once()?;
        let elapsed = clock.now().saturating_duration_since(start);
        if !info.present {
            return Ok((info, elapsed.as_millis() as u64));
        }
        if elapsed >= timeout {
            return Ok((info, elapsed.as_millis() as u64));
        }
        clock.sleep(poll);
    }
}

pub fn run_challenge(req: ChallengeRequest) -> Result<ChallengeEnvelope, HandsError> {
    logs::ensure_installed();
    let session_id = resolve_session_id_from_os(req.session_id.as_deref());
    logs::check_write_id(&session_id)?;
    logs::remember_session(&session_id);
    if req.watch {
        return run_watch(session_id);
    }
    run_status(session_id, req.observe_path.as_deref())
}

fn run_status(
    session_id: String,
    observe_path: Option<&str>,
) -> Result<ChallengeEnvelope, HandsError> {
    let live = snapshot();
    let (present, kind, reason) = if let Some(path) = observe_path {
        let hit = detect_sidecar(path)?;
        (
            hit.present,
            hit.kind.map(|k| k.as_str().to_string()),
            hit.reason,
        )
    } else {
        (live.present, live.kind.clone(), live.reason.clone())
    };
    finalize_envelope(ChallengeEnvelope {
        schema: CHALLENGE_SCHEMA.into(),
        session_id,
        ok: true,
        present,
        kind,
        attempts: live.attempts,
        yielded: live.yielded,
        reason,
        watched: false,
        elapsed_ms: None,
        error: None,
    })
}

fn run_watch(session_id: String) -> Result<ChallengeEnvelope, HandsError> {
    let timeout = Duration::from_millis(watch_timeout_ms());
    let poll = Duration::from_millis(WATCH_POLL_MS);
    let sid = session_id.clone();
    match watch_until_gone(&RealClock, timeout, poll, || {
        let env = crate::observe::observe(crate::observe::ObserveRequest {
            session_id: Some(sid.clone()),
            detail: crate::extract::Detail::Default,
        })?;
        Ok(env.challenge)
    }) {
        Ok((info, elapsed_ms)) => finalize_envelope(ChallengeEnvelope {
            schema: CHALLENGE_SCHEMA.into(),
            session_id,
            ok: true,
            present: info.present,
            kind: info.kind,
            attempts: info.attempts,
            yielded: info.yielded,
            reason: info.reason,
            watched: true,
            elapsed_ms: Some(elapsed_ms),
            error: None,
        }),
        Err(err) => finalize_envelope(ChallengeEnvelope {
            schema: CHALLENGE_SCHEMA.into(),
            session_id,
            ok: false,
            present: snapshot().present,
            kind: snapshot().kind,
            attempts: snapshot().attempts,
            yielded: snapshot().yielded,
            reason: snapshot().reason,
            watched: true,
            elapsed_ms: None,
            error: Some(err.tool_message()),
        }),
    }
}

fn detect_sidecar(path: &str) -> Result<DetectHit, HandsError> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| HandsError::Challenge(format!("read observe sidecar: {err}")))?;
    let side: ObserveSidecar = serde_json::from_str(&text)
        .map_err(|err| HandsError::Challenge(format!("parse observe sidecar: {err}")))?;
    Ok(detect_from_extract(
        &side.extract.title,
        side.extract.url.as_deref(),
        &side.extract.main_text,
        &side.elements,
    ))
}

pub fn finalize_envelope(envelope: ChallengeEnvelope) -> Result<ChallengeEnvelope, HandsError> {
    let json = serialize_challenge(&envelope)?;
    if json.len() > ENVELOPE_MAX_BYTES {
        return Err(HandsError::Challenge(format!(
            "challenge envelope is {} bytes (hard max {ENVELOPE_MAX_BYTES})",
            json.len()
        )));
    }
    Ok(envelope)
}

pub fn serialize_challenge(envelope: &ChallengeEnvelope) -> Result<String, HandsError> {
    serde_json::to_string(envelope)
        .map_err(|err| HandsError::Challenge(format!("envelope serialize: {err}")))
}

#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::Extract;
    use crate::observe::ObserveSidecar;
    use crate::space::Space;
    use std::cell::Cell;
    use std::path::PathBuf;

    fn hit(
        title: &str,
        url: Option<&str>,
        main_text: &str,
        elements: &[(&str, &str)],
    ) -> DetectHit {
        detect(&DetectInput {
            title,
            url,
            main_text,
            elements,
        })
    }

    fn assert_kind(h: DetectHit, kind: ChallengeKind) {
        assert!(h.present, "expected present: {h:?}");
        assert_eq!(h.kind, Some(kind), "{h:?}");
        assert!(h.reason.as_ref().is_some_and(|r| r.len() <= REASON_MAX));
    }

    fn assert_clear(h: DetectHit) {
        assert!(!h.present, "expected not present: {h:?}");
        assert!(h.kind.is_none(), "{h:?}");
    }

    #[test]
    fn positive_recaptcha_phrase_and_iframe() {
        assert_kind(
            hit("I'm not a robot", None, "", &[]),
            ChallengeKind::Recaptcha,
        );
        assert_kind(
            hit("I am not a robot", None, "", &[]),
            ChallengeKind::Recaptcha,
        );
        assert_kind(
            hit("page", None, "", &[("iframe", "reCAPTCHA")]),
            ChallengeKind::Recaptcha,
        );
        assert_kind(
            hit("Select all images with traffic lights", None, "", &[]),
            ChallengeKind::Recaptcha,
        );
        assert_kind(
            hit("Select all squares with buses", None, "", &[]),
            ChallengeKind::Recaptcha,
        );
    }

    #[test]
    fn grid_phrase_in_main_text_alone_is_not_present() {
        assert_clear(hit(
            "Fingerprint: Bot Detection",
            Some("https://fingerprint.com/try/bot-detection/"),
            "Old CAPTCHAs asked you to select all squares with traffic lights. Fingerprint is invisible.",
            &[],
        ));
        assert_clear(hit(
            "CAPTCHA docs",
            Some("https://example.com/docs/captcha"),
            "Old CAPTCHAs asked you to select all images with traffic lights.",
            &[],
        ));
    }

    #[test]
    fn grid_phrase_on_named_widget_or_recaptcha_chrome_is_present() {
        assert_kind(
            hit(
                "page",
                None,
                "",
                &[("document", "Select all squares with buses")],
            ),
            ChallengeKind::Recaptcha,
        );
        assert_kind(
            hit(
                "page",
                None,
                "",
                &[("iframe", "Select all images with traffic lights")],
            ),
            ChallengeKind::Recaptcha,
        );
        assert_kind(
            hit(
                "t",
                Some("https://www.google.com/recaptcha/api2/bframe"),
                "select all squares with traffic lights",
                &[],
            ),
            ChallengeKind::Recaptcha,
        );
    }

    #[test]
    fn positive_hosts() {
        assert_kind(
            hit(
                "t",
                Some("https://www.google.com/recaptcha/api2/anchor"),
                "",
                &[],
            ),
            ChallengeKind::Recaptcha,
        );
        assert_kind(
            hit(
                "t",
                Some("https://www.recaptcha.net/recaptcha/api.js"),
                "",
                &[],
            ),
            ChallengeKind::Recaptcha,
        );
        assert_kind(
            hit(
                "t",
                Some("https://recaptcha.google.com/recaptcha/api.js"),
                "",
                &[],
            ),
            ChallengeKind::Recaptcha,
        );
        assert_kind(
            hit(
                "t",
                Some("https://newassets.hcaptcha.com/1/api.js"),
                "",
                &[],
            ),
            ChallengeKind::Hcaptcha,
        );
        assert_kind(
            hit("t", Some("https://hcaptcha.com/1/api.js"), "", &[]),
            ChallengeKind::Hcaptcha,
        );
        assert_kind(
            hit(
                "t",
                Some("https://challenges.cloudflare.com/turnstile/v0/api.js"),
                "",
                &[],
            ),
            ChallengeKind::Turnstile,
        );
        assert_kind(
            hit(
                "t",
                Some("https://example.com/cdn-cgi/challenge-platform/h/b/orchestrate"),
                "",
                &[],
            ),
            ChallengeKind::Interstitial,
        );
        assert_kind(
            hit(
                "t",
                Some("https://www.cloudflare.com/cdn-cgi/challenge-platform/h/b"),
                "",
                &[],
            ),
            ChallengeKind::Turnstile,
        );
        assert_kind(
            hit(
                "t",
                Some("https://www.google.com:443/recaptcha/api2/anchor"),
                "",
                &[],
            ),
            ChallengeKind::Recaptcha,
        );
        assert_kind(
            hit(
                "t",
                Some("https://user:pass@www.google.com/recaptcha/api2/anchor"),
                "",
                &[],
            ),
            ChallengeKind::Recaptcha,
        );
        assert_clear(hit("t", Some("https://example.com:443/products"), "", &[]));
        assert_kind(
            hit(
                "t",
                Some("https://client-api.arkoselabs.com/v2/key/api.js"),
                "",
                &[],
            ),
            ChallengeKind::Funcaptcha,
        );
        assert_kind(
            hit("t", Some("https://funcaptcha.com/fc/api/"), "", &[]),
            ChallengeKind::Funcaptcha,
        );
        assert_kind(
            hit(
                "t",
                Some("https://company-api.arkoselabs.com/v2/x"),
                "",
                &[],
            ),
            ChallengeKind::Funcaptcha,
        );
    }

    #[test]
    fn positive_cloudflare_copy_and_weak_phrases() {
        assert_kind(
            hit(
                "Verifying you are human",
                None,
                "This security check helps us protect example.com from automated traffic. It will only take a moment.",
                &[],
            ),
            ChallengeKind::Turnstile,
        );
        assert_kind(
            hit("Verify you are human", None, "", &[]),
            ChallengeKind::Generic,
        );
        assert_kind(
            hit("Confirm you are human", None, "", &[]),
            ChallengeKind::Generic,
        );
        assert_kind(
            hit("Are you a robot?", None, "", &[]),
            ChallengeKind::Generic,
        );
        assert_kind(
            hit("Human verification", None, "", &[]),
            ChallengeKind::Generic,
        );
        assert_kind(
            hit(
                "Attention Required",
                Some("https://challenges.cloudflare.com/cdn-cgi/challenge-platform/x"),
                "",
                &[],
            ),
            ChallengeKind::Turnstile,
        );
        assert_kind(
            hit(
                "Checking your browser",
                None,
                "",
                &[("document", "cloudflare")],
            ),
            ChallengeKind::Turnstile,
        );
        assert_kind(
            hit("t", None, "", &[("document", "turnstile widget")]),
            ChallengeKind::Turnstile,
        );
        assert_kind(
            hit("t", None, "", &[("document", "geetest puzzle")]),
            ChallengeKind::Generic,
        );
    }

    #[test]
    fn positive_interstitials() {
        let callback = Some("https://www.cars.com/signin/google_callback/");
        assert_kind(
            hit("Just a moment...", callback, "", &[]),
            ChallengeKind::Interstitial,
        );
        assert_kind(
            hit("Performing security verification", callback, "", &[]),
            ChallengeKind::Interstitial,
        );
        assert_kind(
            hit("Checking if the site connection is secure", None, "", &[]),
            ChallengeKind::Interstitial,
        );
        assert_clear(hit("cars.com", callback, "", &[]));
        assert_kind(
            hit(
                "t",
                Some("https://www.cars.com/cdn-cgi/challenge-platform/h/b"),
                "",
                &[],
            ),
            ChallengeKind::Interstitial,
        );
        assert_kind(
            hit(
                "t",
                Some("https://www.cars.com:443/cdn-cgi/challenge-platform/h/b"),
                "",
                &[],
            ),
            ChallengeKind::Interstitial,
        );
        assert_kind(
            hit(
                "t",
                Some("https://example.com:443/cdn-cgi/challenge-platform/h/b"),
                "",
                &[],
            ),
            ChallengeKind::Interstitial,
        );
        assert_kind(
            hit(
                "Checking if the site connection is secure",
                Some("https://www.cloudflare.com/cdn-cgi/challenge-platform/h/b"),
                "",
                &[],
            ),
            ChallengeKind::Turnstile,
        );
        assert_kind(
            hit("cars.com", None, "Performing security verification", &[]),
            ChallengeKind::Interstitial,
        );
        assert_kind(
            hit(
                "cars.com",
                None,
                "",
                &[("document", "Checking if the site connection is secure")],
            ),
            ChallengeKind::Interstitial,
        );
        assert_clear(hit(
            "cars.com",
            None,
            "please wait just a moment while we load your cart",
            &[],
        ));
        assert_clear(hit(
            "t",
            Some("https://www.cars.com/cdn-cgi/scripts/rocket"),
            "",
            &[],
        ));
    }

    #[test]
    fn title_is_interstitial_matches_settle_table() {
        let cases = [
            ("Just a moment...", true),
            ("Performing security verification", true),
            ("Checking if the site connection is secure", true),
            ("cars.com: Camry", false),
            ("Continue as Ryan", false),
            ("Accept cookies", false),
            ("", false),
        ];
        for (title, blocked) in cases {
            assert_eq!(title_is_interstitial(title), blocked, "title {title:?}");
        }
    }

    #[test]
    fn strong_phrases_does_not_include_just_a_moment() {
        let src = include_str!("challenge.rs");
        let start = src.find("const STRONG_PHRASES").expect("STRONG_PHRASES");
        let rest = &src[start..];
        let end = rest.find("];").expect("STRONG_PHRASES table end") + 2;
        let table = &rest[..end];
        assert!(
            !table.contains("just a moment"),
            "STRONG_PHRASES must not contain just a moment:\n{table}"
        );
        assert!(
            !table.contains("it will only take a moment"),
            "STRONG_PHRASES must not contain it will only take a moment:\n{table}"
        );
    }

    fn strong_phrases_table() -> &'static str {
        let src = include_str!("challenge.rs");
        let start = src.find("const STRONG_PHRASES").expect("STRONG_PHRASES");
        let rest = &src[start..];
        let end = rest.find("];").expect("STRONG_PHRASES table end");
        &rest[..end]
    }

    fn phrase_need_of<'a>(table: &'a str, needle: &str) -> &'a str {
        let i = table.find(needle).unwrap_or_else(|| panic!("{needle}"));
        let after = &table[i + needle.len()..];
        let need_i = after
            .find("PhraseNeed::")
            .unwrap_or_else(|| panic!("PhraseNeed after {needle}"));
        let need = &after[need_i..];
        let end = need.find([',', '\n', ')']).unwrap_or(need.len());
        need[..end].trim()
    }

    #[test]
    fn strong_phrases_grid_rows_are_named_or_recaptcha() {
        let table = strong_phrases_table();
        assert!(
            !table.contains("#[cfg(test)]") && !table.contains("mod tests"),
            "STRONG_PHRASES slice must stay production-only:\n{table}"
        );
        assert!(
            table.contains("select all images with"),
            "missing select all images with:\n{table}"
        );
        assert!(
            table.contains("select all squares with"),
            "missing select all squares with:\n{table}"
        );
        assert_eq!(
            table.matches("PhraseNeed::NamedOrRecaptcha").count(),
            2,
            "exactly two NamedOrRecaptcha rows:\n{table}"
        );
        assert_eq!(
            phrase_need_of(table, "select all images with"),
            "PhraseNeed::NamedOrRecaptcha"
        );
        assert_eq!(
            phrase_need_of(table, "select all squares with"),
            "PhraseNeed::NamedOrRecaptcha"
        );
        assert_ne!(
            phrase_need_of(table, "select all images with"),
            "PhraseNeed::None"
        );
        assert_ne!(
            phrase_need_of(table, "select all squares with"),
            "PhraseNeed::None"
        );
        assert_eq!(phrase_need_of(table, "i'm not a robot"), "PhraseNeed::None");
        assert!(
            !table.contains("just a moment"),
            "STRONG_PHRASES must not contain just a moment:\n{table}"
        );
        assert!(
            !table.contains("it will only take a moment"),
            "STRONG_PHRASES must not contain it will only take a moment:\n{table}"
        );
    }

    #[test]
    fn mcp_challenge_tool_mentions_interstitial_and_wait() {
        let src = include_str!("mcp.rs");
        let start = src.find("fn challenge(").expect("mcp challenge tool");
        let window = &src[start.saturating_sub(700)..start];
        let lower = window.to_ascii_lowercase();
        assert!(
            lower.contains("interstitial") || lower.contains("just a moment"),
            "mcp challenge description should name interstitial or Just a moment:\n{window}"
        );
        assert!(
            lower.contains("wait"),
            "mcp challenge description should mention wait:\n{window}"
        );
        assert!(
            lower.contains("grid copy in page body"),
            "mcp challenge description should name grid copy in page body:\n{window}"
        );
    }

    #[test]
    fn agents_and_readme_name_grid_copy_in_page_body() {
        let agents = include_str!("../AGENTS.md").to_ascii_lowercase();
        let readme = include_str!("../README.md").to_ascii_lowercase();
        assert!(
            agents.contains("grid copy in page body"),
            "AGENTS.md challenge paragraph should name grid copy in page body"
        );
        assert!(
            readme.contains("grid copy in page body"),
            "README.md challenge paragraph should name grid copy in page body"
        );
    }

    #[test]
    fn hcaptcha_i_am_human_needs_vendor() {
        assert_clear(hit(
            "I am human",
            None,
            "a blog post about being human",
            &[],
        ));
        assert_kind(
            hit("I am human", Some("https://hcaptcha.com/captcha"), "", &[]),
            ChallengeKind::Hcaptcha,
        );
        assert_kind(
            hit("I am human", None, "", &[("iframe", "hCaptcha")]),
            ChallengeKind::Hcaptcha,
        );
    }

    #[test]
    fn negative_lexicon() {
        assert_clear(hit("Human Resources", None, "join our HR team", &[]));
        assert_clear(hit("Verify your email", None, "we sent a code", &[]));
        assert_clear(hit("Verify your phone", None, "sms code", &[]));
        assert_clear(hit("Robot vacuum on sale", None, "roomba", &[]));
        assert_clear(hit("", None, "", &[]));
        assert_clear(hit(
            "2020 Honda Civic for sale",
            Some("https://www.cars.com/vehicledetail/abc"),
            "price $12,345",
            &[("hyperlink", "2020 Honda Civic")],
        ));
        assert_clear(hit(
            "Software Engineer - LinkedIn",
            Some("https://www.linkedin.com/jobs/view/123"),
            "Easy Apply",
            &[("button", "Easy Apply")],
        ));
        assert_clear(hit(
            "Blog",
            None,
            "A long article about how recaptcha changed the web and why I am human according to philosophers.",
            &[],
        ));
        assert_clear(hit(
            "Attention Required",
            Some("https://intranet.example.com/hr"),
            "",
            &[],
        ));
        assert_clear(hit("Checking your browser settings", None, "", &[]));
        assert_clear(hit("Successful", Some("https://cars.com/"), "", &[]));
        assert_clear(hit(
            "t",
            Some("https://www.google.com/search?q=hello"),
            "",
            &[],
        ));
        assert_clear(hit(
            "t",
            Some("https://www.cloudflare.com/products/"),
            "",
            &[],
        ));
        assert_clear(hit(
            "cars.com",
            Some("https://www.cars.com/"),
            "",
            &[("button", "Continue as Ryan")],
        ));
        assert_clear(hit(
            "cars.com",
            Some("https://www.cars.com/"),
            "",
            &[("button", "Accept cookies")],
        ));
        assert_clear(hit(
            "cars.com",
            Some("https://www.cars.com/"),
            "",
            &[("button", "Accept all cookies")],
        ));
    }

    #[test]
    fn reason_capped_80() {
        let h = hit("I'm not a robot — please wait", None, "", &[]);
        assert_kind(h.clone(), ChallengeKind::Recaptcha);
        assert!(h.reason.as_ref().is_some_and(|r| r.len() <= REASON_MAX));
        assert_eq!(
            crate::extract::take_chars(&"x".repeat(200), REASON_MAX)
                .chars()
                .count(),
            REASON_MAX
        );
    }

    fn present_hit() -> DetectHit {
        DetectHit {
            present: true,
            kind: Some(ChallengeKind::Recaptcha),
            reason: Some("i'm not a robot".into()),
        }
    }

    #[test]
    fn machine_first_present_is_zero_attempts() {
        let mut m = ChallengeMachine::new();
        assert!(!m.observe(&present_hit()));
        assert_eq!(m.info().attempts, 0);
        assert!(!m.yielded());
        assert!(m.hold());
    }

    #[test]
    fn machine_two_cycles_then_yield() {
        let mut m = ChallengeMachine::new();
        m.observe(&present_hit());
        m.note_actuation();
        assert!(!m.observe(&present_hit()));
        assert_eq!(m.info().attempts, 1);
        assert!(!m.yielded());
        m.note_actuation();
        assert!(m.observe(&present_hit()));
        assert_eq!(m.info().attempts, 2);
        assert!(m.yielded());
        m.note_actuation();
        assert!(!m.observe(&present_hit()));
        assert_eq!(m.info().attempts, 2);
        assert!(m.yielded());
    }

    #[test]
    fn extra_observe_without_actuate_does_not_increment() {
        let mut m = ChallengeMachine::new();
        m.observe(&present_hit());
        m.observe(&present_hit());
        m.observe(&present_hit());
        assert_eq!(m.info().attempts, 0);
        assert!(!m.yielded());
    }

    #[test]
    fn hover_does_not_increment() {
        let mut m = ChallengeMachine::new();
        m.observe(&present_hit());
        // hover never calls note_actuation
        m.observe(&present_hit());
        assert_eq!(m.info().attempts, 0);
    }

    #[test]
    fn note_actuation_if_proceeding_skips_fence_refuse() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        apply_observe("s-fence", present_hit());
        note_actuation_if_proceeding(true);
        assert!(!lock().actuated_since_observe());
        apply_observe("s-fence", present_hit());
        assert_eq!(snapshot().attempts, 0);
        note_actuation_if_proceeding(false);
        assert!(lock().actuated_since_observe());
        apply_observe("s-fence", present_hit());
        assert_eq!(snapshot().attempts, 1);
        reset_for_test();
    }

    #[test]
    fn ui_gone_clears_yield_and_hold() {
        let mut m = ChallengeMachine::new();
        m.observe(&present_hit());
        m.note_actuation();
        m.observe(&present_hit());
        m.note_actuation();
        m.observe(&present_hit());
        assert!(m.yielded());
        m.observe(&DetectHit::clear());
        assert!(!m.yielded());
        assert_eq!(m.info().attempts, 0);
        assert!(!m.hold());
        assert!(!m.info().present);
    }

    #[test]
    fn yield_log_once() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        logs::with_test_env(|| {
            reset_for_test();
            apply_observe("s-y", present_hit());
            note_actuation();
            apply_observe("s-y", present_hit());
            note_actuation();
            apply_observe("s-y", present_hit());
            assert!(snapshot().yielded);
            apply_observe("s-y", present_hit());
            apply_observe("s-y", present_hit());
            let env = logs::run_logs(Some("s-y"), false, None).unwrap();
            let yields: Vec<_> = env.events.iter().filter(|e| e.kind == "yield").collect();
            assert_eq!(yields.len(), 1, "{:?}", env.events);
            assert_eq!(
                yields[0].yield_info.as_ref().map(|y| y.reason.as_str()),
                Some("recaptcha")
            );
            reset_for_test();
        });
    }

    #[test]
    fn yielded_third_actuate_refuses_without_input() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        apply_observe("s-ref", present_hit());
        note_actuation();
        apply_observe("s-ref", present_hit());
        note_actuation();
        apply_observe("s-ref", present_hit());
        assert!(yielded());
        assert_eq!(snapshot().attempts, 2);
        reset_for_test();
    }

    struct FakeClock {
        now: Cell<Instant>,
    }

    impl WatchClock for FakeClock {
        fn now(&self) -> Instant {
            self.now.get()
        }
        fn sleep(&self, d: Duration) {
            self.now.set(self.now.get() + d);
        }
    }

    #[test]
    fn watch_timeout_still_present_is_ok() {
        let clock = FakeClock {
            now: Cell::new(Instant::now()),
        };
        let calls = Cell::new(0u32);
        let (info, elapsed) = watch_until_gone(
            &clock,
            Duration::from_millis(1_000),
            Duration::from_millis(1_000),
            || {
                calls.set(calls.get() + 1);
                Ok(ChallengeInfo {
                    present: true,
                    kind: Some("recaptcha".into()),
                    attempts: 2,
                    yielded: true,
                    reason: Some("i'm not a robot".into()),
                })
            },
        )
        .unwrap();
        assert!(info.present);
        assert!(info.yielded);
        assert!(elapsed >= 1_000);
        assert!(calls.get() >= 2);
        assert!(calls.get() < 10, "must not poll for 120s: {}", calls.get());
    }

    #[test]
    fn watch_stops_when_ui_gone() {
        let clock = FakeClock {
            now: Cell::new(Instant::now()),
        };
        let calls = Cell::new(0u32);
        let (info, _) = watch_until_gone(
            &clock,
            Duration::from_millis(120_000),
            Duration::from_millis(1_000),
            || {
                let n = calls.get() + 1;
                calls.set(n);
                Ok(ChallengeInfo {
                    present: n < 2,
                    kind: None,
                    attempts: 0,
                    yielded: false,
                    reason: None,
                })
            },
        )
        .unwrap();
        assert!(!info.present);
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn watch_timeout_env_clamped() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os(WATCH_TIMEOUT_ENV);
        unsafe { std::env::set_var(WATCH_TIMEOUT_ENV, "50") };
        assert_eq!(watch_timeout_ms(), MIN_WATCH_TIMEOUT_MS);
        unsafe { std::env::set_var(WATCH_TIMEOUT_ENV, "999999") };
        assert_eq!(watch_timeout_ms(), MAX_WATCH_TIMEOUT_MS);
        unsafe { std::env::set_var(WATCH_TIMEOUT_ENV, "45000") };
        assert_eq!(watch_timeout_ms(), 45_000);
        match prev {
            Some(v) => unsafe { std::env::set_var(WATCH_TIMEOUT_ENV, v) },
            None => unsafe { std::env::remove_var(WATCH_TIMEOUT_ENV) },
        }
    }

    #[test]
    fn observe_path_status_does_not_mutate_episode() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        apply_observe("s-st", present_hit());
        note_actuation();
        apply_observe("s-st", present_hit());
        assert_eq!(snapshot().attempts, 1);
        let dir = std::env::temp_dir().join(format!("hands-ch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_fixture_sidecar(&dir, "I'm not a robot", None);
        let env = run_challenge(ChallengeRequest {
            session_id: Some("s-st".into()),
            status: true,
            watch: false,
            observe_path: Some(path.to_string_lossy().into()),
        })
        .unwrap();
        assert!(env.ok);
        assert!(env.present);
        assert_eq!(env.attempts, 1);
        assert!(!env.yielded);
        assert!(!env.watched);
        assert_eq!(snapshot().attempts, 1);
        assert!(!yielded());
        let _ = std::fs::remove_dir_all(&dir);
        reset_for_test();
    }

    fn write_fixture_sidecar(dir: &std::path::Path, title: &str, url: Option<&str>) -> PathBuf {
        let path = dir.join("observe.json");
        let side = ObserveSidecar {
            schema: crate::observe::OBSERVE_SCHEMA.to_string(),
            session_id: "sid".into(),
            screenshot_path: "C:\\tmp\\x.png".into(),
            observe_path: path.to_string_lossy().into(),
            space: Space::new(0, 0, 100, 100).unwrap(),
            viewport: None,
            extract: Extract {
                title: title.into(),
                url: url.map(str::to_string),
                main_text: String::new(),
                cards: Vec::new(),
                dialogs: Vec::new(),
                ..Default::default()
            },
            elements: Vec::new(),
            elements_total: 0,
            elements_truncated: false,
            chrome_connected: false,
            chrome_hint: None,
            challenge: ChallengeInfo::default(),
        };
        std::fs::write(&path, serde_json::to_string_pretty(&side).unwrap()).unwrap();
        path
    }

    #[test]
    fn old_sidecar_without_challenge_deserializes() {
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
    fn cargo_and_source_forbid_solver_tokens() {
        let cargo = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
        let cargo_l = cargo.to_ascii_lowercase();
        for needle in ["onnx", "whisper", "2captcha", "anticaptcha"] {
            assert!(
                !cargo_l.contains(needle),
                "Cargo.toml must not mention {needle}"
            );
        }
        let recaptcha_model = ["recaptcha", "-"].concat();
        let yolo = ["yolo", "v8"].concat();
        assert!(
            !cargo_l.contains(&recaptcha_model),
            "Cargo.toml must not mention recaptcha model prefix"
        );
        assert!(
            !cargo_l.contains(&yolo),
            "Cargo.toml must not mention yolo detector"
        );

        let src = include_str!("challenge.rs");
        let port = ["80", "81"].concat();
        let pick_mod = ["pick", "::"].concat();
        assert!(
            !src.contains(&port),
            "challenge.rs must not mention the Gemma loopback port"
        );
        assert!(
            !src.contains(&pick_mod),
            "challenge.rs must not import the pick module"
        );

        let manifest = include_str!("../extension/manifest.json");
        assert!(
            manifest.contains("\"all_frames\": false"),
            "all_frames must stay false"
        );
    }

    #[test]
    fn pause_mid_hold_still_wipes_session_allows() {
        let _ch = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::allows::with_test_env(|| {
            lease::reset_for_test();
            crate::fence::reinstall_for_test();
            reset_for_test();
            crate::allows::grant(
                "s1",
                "desktop",
                crate::classify::Category::Applications,
                crate::allows::AllowMode::Session,
            )
            .unwrap();
            apply_observe("s1", present_hit());
            assert!(lease::challenge_hold());
            lease::freeze_now_with(crate::lease::FreezeCause::Pause);
            let ev = crate::classify::Evidence {
                name: "Easy Apply".into(),
                role: "button".into(),
                window_title: String::new(),
                window_class: String::new(),
            };
            assert!(matches!(
                crate::fence::decide("s1", &ev, "desktop").unwrap(),
                crate::fence::Gate::Refused { .. }
            ));
            assert!(snapshot().present);
            reset_for_test();
        });
    }

    #[test]
    fn yield_does_not_wipe_session_allows() {
        let _ch = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::allows::with_test_env(|| {
            lease::reset_for_test();
            crate::fence::reinstall_for_test();
            reset_for_test();
            crate::allows::grant(
                "s1",
                "desktop",
                crate::classify::Category::Applications,
                crate::allows::AllowMode::Session,
            )
            .unwrap();
            apply_observe("s1", present_hit());
            note_actuation();
            apply_observe("s1", present_hit());
            note_actuation();
            apply_observe("s1", present_hit());
            assert!(yielded());
            let ev = crate::classify::Evidence {
                name: "Easy Apply".into(),
                role: "button".into(),
                window_title: String::new(),
                window_class: String::new(),
            };
            assert_eq!(
                crate::fence::decide("s1", &ev, "desktop").unwrap(),
                crate::fence::Gate::Allowed(crate::allows::AllowHit::Session)
            );
            reset_for_test();
        });
    }

    #[test]
    fn challenge_envelope_fits_16kib() {
        let env = ChallengeEnvelope {
            schema: CHALLENGE_SCHEMA.into(),
            session_id: "s".into(),
            ok: true,
            present: true,
            kind: Some("recaptcha".into()),
            attempts: 2,
            yielded: true,
            reason: Some("i'm not a robot".into()),
            watched: true,
            elapsed_ms: Some(120_000),
            error: None,
        };
        let json = serialize_challenge(&env).unwrap();
        assert!(json.len() <= ENVELOPE_MAX_BYTES);
        assert!(json.contains(CHALLENGE_SCHEMA));
    }
}
