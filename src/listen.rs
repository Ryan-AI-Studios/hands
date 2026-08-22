//! On-demand content loopback transcript. Not observe. Not a CAPTCHA solver.
//! Refuses when challenge UI is present (any identity). Owner transcribe binary or HTTP.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine;
use serde::Serialize;
use serde_json::{Value, json};

use crate::capture::{display_path, utc_compact};
use crate::challenge;
use crate::error::HandsError;
use crate::extract::{Detail, take_chars};
use crate::logs;
use crate::observe::{self, OBSERVE_SCHEMA, ObserveRequest, ObserveSidecar};
use crate::session::resolve_session_id_from_os;

pub const LISTEN_SCHEMA: &str = "hands.listen/v1";
pub const CHALLENGE_PRESENT: &str = "challenge present";
pub const BACKEND_MISSING: &str = "listen backend not configured";
pub const DEFAULT_SECONDS: u32 = 20;
pub const MIN_SECONDS: u32 = 3;
pub const MAX_SECONDS: u32 = 60;
pub const TRANSCRIPT_CAP: usize = 4096;
pub const TARGET_HZ: u32 = 16_000;
pub const SILENCE_PEAK: u16 = 256;

pub const LISTEN_URL_ENV: &str = "HANDS_LISTEN_URL";
pub const LISTEN_BIN_ENV: &str = "HANDS_LISTEN_BIN";
pub const LISTEN_MODEL_ENV: &str = "HANDS_LISTEN_MODEL";
pub const LISTEN_KEY_ENV: &str = "HANDS_LISTEN_KEY";
pub const LISTEN_TIMEOUT_ENV: &str = "HANDS_LISTEN_TIMEOUT_MS";
pub const LISTEN_DIR_ENV: &str = "HANDS_LISTEN_DIR";

const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MIN_TIMEOUT_MS: u64 = 5_000;
const MAX_TIMEOUT_MS: u64 = 300_000;
const FORBIDDEN_GEMMA_PORT: u16 = 80 * 100 + 81;
const FORBIDDEN_EMBED_PORT: u16 = 80 * 100 + 83;

#[derive(Debug, Clone)]
pub struct ListenRequest {
    pub session_id: Option<String>,
    pub seconds: Option<u32>,
    pub observe_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListenEnvelope {
    pub schema: String,
    pub session_id: String,
    pub ok: bool,
    pub transcript: String,
    pub truncated: bool,
    pub seconds: u32,
    pub source: String,
    pub speech: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wav_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Pcm {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<i16>,
}

pub trait LoopbackCapture {
    fn capture_pcm(&self, seconds: u32) -> Result<Pcm, HandsError>;
}

pub trait TranscribeBackend {
    fn transcribe(&self, wav: &Path, pcm: &Pcm) -> Result<String, HandsError>;
    fn name(&self) -> &'static str;
}

pub fn clamp_seconds(seconds: Option<u32>) -> u32 {
    seconds
        .unwrap_or(DEFAULT_SECONDS)
        .clamp(MIN_SECONDS, MAX_SECONDS)
}

pub fn serialize_listen(env: &ListenEnvelope) -> Result<String, HandsError> {
    serde_json::to_string(env).map_err(|err| HandsError::Listen(format!("listen envelope: {err}")))
}

pub fn run_listen(req: ListenRequest) -> Result<ListenEnvelope, HandsError> {
    let capture = WasapiLoopback;
    match selected_backend() {
        Ok(Some(backend)) => run_listen_with(req, &capture, Some(&*backend)),
        Ok(None) => run_listen_with(req, &capture, None),
        Err(err) => {
            let session_id = resolve_session_id_from_os(req.session_id.as_deref());
            Err(log_listen_err(&session_id, err))
        }
    }
}

pub fn run_listen_with(
    req: ListenRequest,
    capture: &dyn LoopbackCapture,
    backend: Option<&dyn TranscribeBackend>,
) -> Result<ListenEnvelope, HandsError> {
    let session_id = resolve_session_id_from_os(req.session_id.as_deref());
    logs::check_write_id(&session_id).map_err(|err| log_listen_err(&session_id, err))?;
    let seconds = clamp_seconds(req.seconds);
    let observe_path = req.observe_path.as_deref();

    if challenge_present(observe_path).map_err(|err| log_listen_err(&session_id, err))? {
        return finish(refuse_present(&session_id, seconds, None));
    }

    let pcm = capture
        .capture_pcm(seconds)
        .map_err(|err| log_listen_err(&session_id, err))?;
    let pcm = normalize_pcm(pcm);

    if challenge_present(observe_path).map_err(|err| log_listen_err(&session_id, err))? {
        return finish(refuse_present(&session_id, seconds, None));
    }

    if !has_speech(&pcm) {
        let wav_path = write_wav_best_effort(&pcm);
        return finish(ListenEnvelope {
            schema: LISTEN_SCHEMA.into(),
            session_id,
            ok: true,
            transcript: String::new(),
            truncated: false,
            seconds,
            source: "loopback".into(),
            speech: false,
            backend: None,
            wav_path,
            error: None,
        });
    }

    let Some(backend) = backend else {
        let wav_path = write_wav_best_effort(&pcm);
        return finish(ListenEnvelope {
            schema: LISTEN_SCHEMA.into(),
            session_id,
            ok: false,
            transcript: String::new(),
            truncated: false,
            seconds,
            source: "loopback".into(),
            speech: true,
            backend: None,
            wav_path,
            error: Some(BACKEND_MISSING.into()),
        });
    };

    let wav_path = write_wav_best_effort(&pcm);
    let wav_for_backend = match &wav_path {
        Some(p) => PathBuf::from(p),
        None => {
            let tmp = std::env::temp_dir().join(format!("hands-listen-{}.wav", utc_compact()));
            let _ = std::fs::write(&tmp, pcm_to_wav_bytes(&pcm));
            tmp
        }
    };
    let raw = backend
        .transcribe(&wav_for_backend, &pcm)
        .map_err(|err| log_listen_err(&session_id, err))?;
    let (transcript, truncated) = cap_transcript(&raw);
    finish(ListenEnvelope {
        schema: LISTEN_SCHEMA.into(),
        session_id,
        ok: true,
        transcript,
        truncated,
        seconds,
        source: "loopback".into(),
        speech: true,
        backend: Some(backend.name().into()),
        wav_path,
        error: None,
    })
}

fn log_listen_err(session_id: &str, err: HandsError) -> HandsError {
    logs::ensure_installed();
    logs::remember_session(session_id);
    let msg = err.to_string();
    let _ = logs::record_actuate(
        session_id,
        "listen",
        false,
        Some(&msg),
        None,
        None,
        None,
        None,
    );
    err
}

fn finish(env: ListenEnvelope) -> Result<ListenEnvelope, HandsError> {
    logs::ensure_installed();
    logs::remember_session(&env.session_id);
    let _ = logs::record_actuate(
        &env.session_id,
        "listen",
        env.ok,
        env.error.as_deref(),
        None,
        None,
        None,
        None,
    );
    Ok(env)
}

fn refuse_present(session_id: &str, seconds: u32, wav_path: Option<String>) -> ListenEnvelope {
    ListenEnvelope {
        schema: LISTEN_SCHEMA.into(),
        session_id: session_id.into(),
        ok: false,
        transcript: String::new(),
        truncated: false,
        seconds,
        source: "loopback".into(),
        speech: false,
        backend: None,
        wav_path,
        error: Some(CHALLENGE_PRESENT.into()),
    }
}

pub fn challenge_present(observe_path: Option<&str>) -> Result<bool, HandsError> {
    match observe_path {
        Some(path) => {
            let side = load_sidecar(path)?;
            if side.challenge.present {
                return Ok(true);
            }
            Ok(challenge::detect_from_extract(
                &side.extract.title,
                side.extract.url.as_deref(),
                &side.extract.main_text,
                &side.elements,
            )
            .present)
        }
        None => {
            let env = observe::observe(ObserveRequest {
                session_id: None,
                detail: Detail::Default,
            })?;
            Ok(env.challenge.present)
        }
    }
}

fn load_sidecar(path: &str) -> Result<ObserveSidecar, HandsError> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| HandsError::Listen(format!("read observe sidecar {path}: {err}")))?;
    let sidecar: ObserveSidecar = serde_json::from_str(&text)
        .map_err(|err| HandsError::Listen(format!("observe sidecar deserialize: {err}")))?;
    if sidecar.schema != OBSERVE_SCHEMA {
        return Err(HandsError::Listen(format!(
            "observe sidecar schema is '{}' (expected {OBSERVE_SCHEMA})",
            sidecar.schema
        )));
    }
    Ok(sidecar)
}

fn cap_transcript(raw: &str) -> (String, bool) {
    let trimmed = raw.trim();
    let out = take_chars(trimmed, TRANSCRIPT_CAP);
    let truncated = trimmed.chars().count() > TRANSCRIPT_CAP;
    (out, truncated)
}

pub fn has_speech(pcm: &Pcm) -> bool {
    pcm.samples
        .iter()
        .map(|s| s.unsigned_abs())
        .max()
        .unwrap_or(0)
        >= SILENCE_PEAK
}

pub fn normalize_pcm(pcm: Pcm) -> Pcm {
    let mono = if pcm.channels <= 1 {
        pcm.samples
    } else {
        downmix_to_mono(&pcm.samples, pcm.channels)
    };
    let rate = if pcm.sample_rate == 0 {
        TARGET_HZ
    } else {
        pcm.sample_rate
    };
    let samples = if rate == TARGET_HZ {
        mono
    } else {
        resample_linear_i16(&mono, rate, TARGET_HZ)
    };
    Pcm {
        sample_rate: TARGET_HZ,
        channels: 1,
        samples,
    }
}

fn downmix_to_mono(samples: &[i16], channels: u16) -> Vec<i16> {
    let ch = channels as usize;
    if ch == 0 {
        return Vec::new();
    }
    samples
        .chunks(ch)
        .map(|frame| {
            let sum: i32 = frame.iter().map(|s| i32::from(*s)).sum();
            (sum / ch as i32) as i16
        })
        .collect()
}

fn resample_linear_i16(input: &[i16], from_hz: u32, to_hz: u32) -> Vec<i16> {
    if input.is_empty() || from_hz == 0 || to_hz == 0 {
        return Vec::new();
    }
    if from_hz == to_hz {
        return input.to_vec();
    }
    let ratio = f64::from(from_hz) / f64::from(to_hz);
    let out_len = ((input.len() as f64) / ratio).round().max(1.0) as usize;
    let last = (input.len() - 1) as f64;
    (0..out_len)
        .map(|i| {
            let src = (i as f64) * ratio;
            let src = src.min(last);
            let i0 = src.floor() as usize;
            let i1 = (i0 + 1).min(input.len() - 1);
            let frac = src - i0 as f64;
            let a = f64::from(input[i0]);
            let b = f64::from(input[i1]);
            (a + (b - a) * frac).round() as i16
        })
        .collect()
}

pub fn pcm_to_wav_bytes(pcm: &Pcm) -> Vec<u8> {
    let channels: u16 = 1;
    let sample_rate = TARGET_HZ;
    let bits: u16 = 16;
    let data: Vec<u8> = pcm.samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let byte_rate = sample_rate * u32::from(channels) * u32::from(bits) / 8;
    let block_align = channels * bits / 8;
    let data_len = data.len() as u32;
    let riff_size = 36 + data_len;
    let mut out = Vec::with_capacity(44 + data.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(&data);
    out
}

fn write_wav_best_effort(pcm: &Pcm) -> Option<String> {
    let dir = listen_dir()?;
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{}.wav", utc_compact()));
    let bytes = pcm_to_wav_bytes(pcm);
    std::fs::write(&path, bytes).ok()?;
    Some(display_path(&path))
}

fn listen_dir() -> Option<PathBuf> {
    if let Ok(raw) = std::env::var(LISTEN_DIR_ENV) {
        let t = raw.trim();
        if !t.is_empty() {
            return Some(PathBuf::from(t));
        }
    }
    let local = std::env::var("LOCALAPPDATA").ok()?;
    Some(PathBuf::from(local).join("hands").join("listen"))
}

fn selected_backend() -> Result<Option<Box<dyn TranscribeBackend>>, HandsError> {
    if let Some(url) = env_nonempty(LISTEN_URL_ENV) {
        return Ok(Some(Box::new(HttpBackend::from_url(&url)?)));
    }
    if let Some(bin) = env_nonempty(LISTEN_BIN_ENV) {
        return Ok(Some(Box::new(CmdBackend::from_env(bin))));
    }
    Ok(None)
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn listen_timeout_ms() -> u64 {
    std::env::var(LISTEN_TIMEOUT_ENV)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS)
}

pub fn cmd_argv(bin: &str, model: Option<&str>, wav: &Path, stem: &Path) -> Vec<String> {
    let mut argv = vec![bin.to_string()];
    if let Some(model) = model.filter(|m| !m.is_empty()) {
        argv.push("-m".into());
        argv.push(model.to_string());
    }
    argv.push("-f".into());
    argv.push(wav.to_string_lossy().into_owned());
    argv.push("-nt".into());
    argv.push("-otxt".into());
    argv.push("-of".into());
    argv.push(stem.to_string_lossy().into_owned());
    argv
}

trait ProcessRunner: Send + Sync {
    fn run(&self, program: &str, args: &[String], timeout: Duration) -> Result<i32, HandsError>;
}

struct StdProcessRunner;

impl ProcessRunner for StdProcessRunner {
    fn run(&self, program: &str, args: &[String], timeout: Duration) -> Result<i32, HandsError> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| HandsError::Listen(format!("spawn transcribe binary: {err}")))?;
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    return Ok(status.code().unwrap_or(-1));
                }
                Ok(None) => {
                    if start.elapsed() >= timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(HandsError::Listen("transcribe binary timed out".into()));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(err) => {
                    return Err(HandsError::Listen(format!("wait transcribe binary: {err}")));
                }
            }
        }
    }
}

struct CmdBackend {
    bin: String,
    model: Option<String>,
    runner: Box<dyn ProcessRunner>,
}

impl CmdBackend {
    fn from_env(bin: String) -> Self {
        Self {
            bin,
            model: env_nonempty(LISTEN_MODEL_ENV),
            runner: Box::new(StdProcessRunner),
        }
    }

    #[cfg(test)]
    fn with_runner(bin: String, model: Option<String>, runner: Box<dyn ProcessRunner>) -> Self {
        Self { bin, model, runner }
    }
}

impl TranscribeBackend for CmdBackend {
    fn name(&self) -> &'static str {
        "cmd"
    }

    fn transcribe(&self, wav: &Path, _pcm: &Pcm) -> Result<String, HandsError> {
        let stem = wav.with_extension("");
        let argv = cmd_argv(&self.bin, self.model.as_deref(), wav, &stem);
        let args = if argv.len() > 1 { &argv[1..] } else { &[] };
        let code = self
            .runner
            .run(&self.bin, args, Duration::from_millis(listen_timeout_ms()))?;
        if code != 0 {
            return Err(HandsError::Listen(format!(
                "transcribe binary exited {code}"
            )));
        }
        let txt = stem.with_extension("txt");
        let text = std::fs::read_to_string(&txt).map_err(|err| {
            HandsError::Listen(format!("missing transcribe txt {}: {err}", txt.display()))
        })?;
        Ok(text.trim().to_string())
    }
}

trait HttpTransport {
    fn post_json(&self, body: &Value) -> Result<HttpResp, HandsError>;
}

struct HttpResp {
    status: u16,
    body: String,
}

struct UreqTransport {
    agent: ureq::Agent,
    url: String,
    api_key: Option<String>,
}

impl UreqTransport {
    fn new(url: &str) -> Result<Self, HandsError> {
        validate_listen_url(url)?;
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .proxy(None)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_millis(listen_timeout_ms())))
            .build();
        Ok(Self {
            agent: ureq::Agent::new_with_config(config),
            url: url.to_string(),
            api_key: env_nonempty(LISTEN_KEY_ENV),
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
            Err(err) => Err(HandsError::Listen(format!("listen http: {err}"))),
        }
    }
}

struct HttpBackend {
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
}

impl TranscribeBackend for HttpBackend {
    fn name(&self) -> &'static str {
        "http"
    }

    fn transcribe(&self, wav: &Path, pcm: &Pcm) -> Result<String, HandsError> {
        let bytes = std::fs::read(wav).unwrap_or_else(|_| pcm_to_wav_bytes(pcm));
        let wav_b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        let body = json!({ "wav_b64": wav_b64, "format": "wav" });
        let resp = self.transport.post_json(&body)?;
        if resp.status != 200 {
            return Err(HandsError::Listen(format!(
                "listen http status {}",
                resp.status
            )));
        }
        let parsed: Value = serde_json::from_str(&resp.body)
            .map_err(|err| HandsError::Listen(format!("listen http json: {err}")))?;
        let text = parsed
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| HandsError::Listen("listen http missing text".into()))?;
        Ok(text.to_string())
    }
}

pub fn validate_listen_url(raw: &str) -> Result<(), HandsError> {
    let url = raw.trim();
    if url.is_empty() {
        return Err(HandsError::Listen("HANDS_LISTEN_URL is empty".into()));
    }
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("file:") || lower.starts_with("javascript:") {
        return Err(HandsError::Listen(
            "listen url must be http loopback or https".into(),
        ));
    }
    if let Some(rest) = lower.strip_prefix("https://") {
        if host_of(rest).is_empty() {
            return Err(HandsError::Listen("listen https url missing host".into()));
        }
        refuse_forbidden_loopback_port(rest, 443)?;
        return Ok(());
    }
    if let Some(rest) = lower.strip_prefix("http://") {
        let host = host_of(rest);
        if !is_loopback_host(&host) {
            return Err(HandsError::Listen(
                "http listen url must be loopback".into(),
            ));
        }
        refuse_forbidden_loopback_port(rest, 80)?;
        return Ok(());
    }
    Err(HandsError::Listen(
        "listen url must be http loopback or https".into(),
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

fn refuse_forbidden_loopback_port(rest: &str, default_port: u16) -> Result<(), HandsError> {
    if !is_loopback_host(&host_of(rest)) {
        return Ok(());
    }
    let port = port_of(rest, default_port);
    if port == FORBIDDEN_GEMMA_PORT || port == FORBIDDEN_EMBED_PORT {
        return Err(HandsError::Listen(
            "listen url must not target the local vision or embed ports".into(),
        ));
    }
    Ok(())
}

fn port_of(after_scheme: &str, default_port: u16) -> u16 {
    let hostport = after_scheme.split('/').next().unwrap_or(after_scheme);
    if let Some(inner) = hostport.strip_prefix('[') {
        return inner
            .split(']')
            .nth(1)
            .and_then(|s| s.strip_prefix(':'))
            .and_then(|p| p.parse().ok())
            .unwrap_or(default_port);
    }
    hostport
        .split_once(':')
        .and_then(|(_, p)| p.parse().ok())
        .unwrap_or(default_port)
}

fn is_loopback_host(host: &str) -> bool {
    let h = host.trim();
    h == "127.0.0.1" || h == "localhost" || h == "::1"
}

struct WasapiLoopback;

impl LoopbackCapture for WasapiLoopback {
    fn capture_pcm(&self, seconds: u32) -> Result<Pcm, HandsError> {
        std::thread::Builder::new()
            .name("hands-listen-sta".into())
            .spawn(move || sta_capture(seconds))
            .map_err(|err| HandsError::Listen(format!("spawn STA thread: {err}")))?
            .join()
            .map_err(|_| HandsError::Listen("listen STA thread panicked".into()))?
    }
}

struct StaGuard;

impl StaGuard {
    fn enter() -> Result<Self, HandsError> {
        unsafe {
            windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
            )
        }
        .ok()
        .map_err(|err| HandsError::Listen(format!("CoInitializeEx(STA): {err}")))?;
        Ok(Self)
    }
}

impl Drop for StaGuard {
    fn drop(&mut self) {
        unsafe {
            windows::Win32::System::Com::CoUninitialize();
        }
    }
}

fn sta_capture(seconds: u32) -> Result<Pcm, HandsError> {
    use windows::Win32::Media::Audio::{
        AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
        AUDCLNT_STREAMFLAGS_LOOPBACK, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator,
        MMDeviceEnumerator, WAVEFORMATEX, eConsole, eRender,
    };
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};

    let _sta = StaGuard::enter()?;
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_INPROC_SERVER) }
            .map_err(|err| HandsError::Listen(format!("MMDeviceEnumerator: {err}")))?;
    let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
        .map_err(|err| HandsError::Listen(format!("default render endpoint: {err}")))?;
    let client: IAudioClient = unsafe { device.Activate(CLSCTX_INPROC_SERVER, None) }
        .map_err(|err| HandsError::Listen(format!("IAudioClient activate: {err}")))?;
    let mix_ptr = unsafe { client.GetMixFormat() }
        .map_err(|err| HandsError::Listen(format!("GetMixFormat: {err}")))?;
    if mix_ptr.is_null() {
        return Err(HandsError::Listen("GetMixFormat returned null".into()));
    }
    struct MixFree(*mut WAVEFORMATEX);
    impl Drop for MixFree {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CoTaskMemFree(Some(self.0.cast())) };
            }
        }
    }
    let _free = MixFree(mix_ptr);
    let flags = AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM;
    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            flags,
            10_000_000,
            0,
            mix_ptr,
            None,
        )
    }
    .map_err(|err| HandsError::Listen(format!("IAudioClient::Initialize loopback: {err}")))?;
    let capture: IAudioCaptureClient = unsafe { client.GetService() }
        .map_err(|err| HandsError::Listen(format!("IAudioCaptureClient: {err}")))?;
    let fmt: WAVEFORMATEX = unsafe { std::ptr::read_unaligned(mix_ptr) };
    let channels = fmt.nChannels.max(1);
    let rate = if fmt.nSamplesPerSec == 0 {
        TARGET_HZ
    } else {
        fmt.nSamplesPerSec
    };
    let kind = mix_sample_kind(&fmt, mix_ptr);
    let frame_nbytes = mix_frame_bytes(channels, kind);
    unsafe { client.Start() }
        .map_err(|err| HandsError::Listen(format!("IAudioClient::Start: {err}")))?;
    let deadline = Instant::now() + Duration::from_secs(u64::from(seconds.max(1)));
    let mut raw: Vec<u8> = Vec::new();
    while Instant::now() < deadline {
        let packet = unsafe { capture.GetNextPacketSize() }.unwrap_or(0);
        if packet == 0 {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        let mut data: *mut u8 = std::ptr::null_mut();
        let mut frames: u32 = 0;
        let mut flags: u32 = 0;
        let got = unsafe { capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None) };
        if got.is_err() || data.is_null() || frames == 0 {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        let nbytes = frames as usize * frame_nbytes;
        if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 {
            raw.extend(std::iter::repeat_n(0u8, nbytes));
        } else {
            let slice = unsafe { std::slice::from_raw_parts(data, nbytes) };
            raw.extend_from_slice(slice);
        }
        let _ = unsafe { capture.ReleaseBuffer(frames) };
    }
    let _ = unsafe { client.Stop() };
    let samples = decode_mix(&raw, kind);
    Ok(normalize_pcm(Pcm {
        sample_rate: rate,
        channels,
        samples,
    }))
}

#[derive(Clone, Copy)]
enum MixKind {
    Pcm16,
    Pcm32,
    Float32,
}

fn mix_sample_kind(
    fmt: &windows::Win32::Media::Audio::WAVEFORMATEX,
    mix_ptr: *mut windows::Win32::Media::Audio::WAVEFORMATEX,
) -> MixKind {
    const WAVE_TAG_IEEE_FLOAT: u16 = 3;
    const WAVE_TAG_EXTENSIBLE: u16 = 0xFFFE;
    if fmt.wFormatTag == WAVE_TAG_IEEE_FLOAT {
        return MixKind::Float32;
    }
    if fmt.wFormatTag == WAVE_TAG_EXTENSIBLE {
        let ext = unsafe {
            std::ptr::read_unaligned(
                mix_ptr.cast::<windows::Win32::Media::Audio::WAVEFORMATEXTENSIBLE>(),
            )
        };
        if ext.SubFormat.data1 == 3 {
            return MixKind::Float32;
        }
    }
    if fmt.wBitsPerSample == 32 {
        MixKind::Pcm32
    } else {
        MixKind::Pcm16
    }
}

fn mix_frame_bytes(channels: u16, kind: MixKind) -> usize {
    let ch = channels.max(1) as usize;
    match kind {
        MixKind::Pcm16 => ch * 2,
        MixKind::Pcm32 | MixKind::Float32 => ch * 4,
    }
}

fn decode_mix(raw: &[u8], kind: MixKind) -> Vec<i16> {
    match kind {
        MixKind::Pcm16 => raw
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect(),
        MixKind::Pcm32 => raw
            .chunks_exact(4)
            .map(|b| {
                let v = i32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                (v >> 16) as i16
            })
            .collect(),
        MixKind::Float32 => raw
            .chunks_exact(4)
            .map(|b| {
                let v = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                (v.clamp(-1.0, 1.0) * 32767.0).round() as i16
            })
            .collect(),
    }
}

#[cfg(test)]
struct ImmediatePcm {
    pcm: Pcm,
    calls: std::sync::atomic::AtomicU32,
}

#[cfg(test)]
impl LoopbackCapture for ImmediatePcm {
    fn capture_pcm(&self, _seconds: u32) -> Result<Pcm, HandsError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(self.pcm.clone())
    }
}

#[cfg(test)]
struct StampPresentCapture {
    path: PathBuf,
    pcm: Pcm,
    kind: &'static str,
    calls: std::sync::atomic::AtomicU32,
}

#[cfg(test)]
impl LoopbackCapture for StampPresentCapture {
    fn capture_pcm(&self, _seconds: u32) -> Result<Pcm, HandsError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        write_sidecar(&self.path, self.kind, true);
        Ok(self.pcm.clone())
    }
}

#[cfg(test)]
struct CannedBackend {
    text: String,
    calls: std::sync::atomic::AtomicU32,
}

#[cfg(test)]
impl TranscribeBackend for CannedBackend {
    fn name(&self) -> &'static str {
        "cmd"
    }

    fn transcribe(&self, _wav: &Path, _pcm: &Pcm) -> Result<String, HandsError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(self.text.clone())
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
            Err(HandsError::Listen("FakeHttp exhausted".into()))
        } else {
            hops.remove(0)
        }
    }
}

#[cfg(test)]
struct FakeCmd {
    text: Option<String>,
    code: i32,
    last_args: std::sync::Mutex<Vec<String>>,
}

#[cfg(test)]
impl ProcessRunner for FakeCmd {
    fn run(&self, _program: &str, args: &[String], _timeout: Duration) -> Result<i32, HandsError> {
        *self.last_args.lock().unwrap_or_else(|e| e.into_inner()) = args.to_vec();
        if let Some(text) = &self.text {
            let mut stem = None;
            for pair in args.windows(2) {
                if pair[0] == "-of" {
                    stem = Some(PathBuf::from(&pair[1]));
                }
            }
            if let Some(stem) = stem {
                let txt = stem.with_extension("txt");
                let _ = std::fs::write(txt, text);
            }
        }
        Ok(self.code)
    }
}

#[cfg(test)]
fn loud_pcm() -> Pcm {
    Pcm {
        sample_rate: TARGET_HZ,
        channels: 1,
        samples: vec![0, 1000, -1000, 800],
    }
}

#[cfg(test)]
fn silent_pcm() -> Pcm {
    Pcm {
        sample_rate: TARGET_HZ,
        channels: 1,
        samples: vec![0, 10, -10, 20],
    }
}

#[cfg(test)]
fn write_sidecar(path: &Path, kind: &str, present: bool) {
    use crate::challenge::ChallengeInfo;
    use crate::extract::{Element, Extract};
    use crate::space::{Rect, Space};
    let (title, url, elements) = match kind {
        "interstitial" => ("Just a moment...".to_string(), None, Vec::new()),
        _ => (
            "Challenge".to_string(),
            Some("https://www.google.com/recaptcha/api2/anchor".to_string()),
            vec![Element {
                id: "chr:0".into(),
                role: "iframe".into(),
                text: Some("recaptcha".into()),
                rect: Rect {
                    x: 0,
                    y: 0,
                    w: 10,
                    h: 10,
                },
                grid: None,
            }],
        ),
    };
    let side = ObserveSidecar {
        schema: OBSERVE_SCHEMA.to_string(),
        session_id: "sid".into(),
        screenshot_path: "C:\\tmp\\x.png".into(),
        observe_path: path.to_string_lossy().into(),
        space: Space::new(0, 0, 100, 100).unwrap(),
        viewport: None,
        extract: Extract {
            title,
            url,
            main_text: String::new(),
            cards: Vec::new(),
            dialogs: Vec::new(),
            ..Default::default()
        },
        elements,
        elements_total: 0,
        elements_truncated: false,
        chrome_connected: false,
        chrome_hint: None,
        challenge: ChallengeInfo {
            present,
            kind: if present { Some(kind.into()) } else { None },
            attempts: 0,
            yielded: false,
            reason: None,
        },
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(path, serde_json::to_string_pretty(&side).unwrap()).unwrap();
}

#[cfg(test)]
fn tmp_side(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hands-listen-{}", utc_compact()));
    let _ = std::fs::create_dir_all(&dir);
    dir.join(name)
}

#[cfg(test)]
fn req(path: &Path) -> ListenRequest {
    ListenRequest {
        session_id: Some("listen-test".into()),
        seconds: Some(5),
        observe_path: Some(path.to_string_lossy().into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attach::{self, Identity};
    use std::sync::atomic::Ordering;

    #[test]
    fn recaptcha_sidecar_refuses_before_capture() {
        let _g = crate::challenge::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::challenge::reset_for_test();
        let path = tmp_side("recaptcha.json");
        write_sidecar(&path, "recaptcha", false);
        let capture = ImmediatePcm {
            pcm: loud_pcm(),
            calls: std::sync::atomic::AtomicU32::new(0),
        };
        let backend = CannedBackend {
            text: "nope".into(),
            calls: std::sync::atomic::AtomicU32::new(0),
        };
        let env = run_listen_with(req(&path), &capture, Some(&backend)).unwrap();
        assert!(!env.ok, "{env:?}");
        assert!(
            env.error
                .as_deref()
                .unwrap_or("")
                .contains(CHALLENGE_PRESENT),
            "{:?}",
            env.error
        );
        assert_eq!(capture.calls.load(Ordering::SeqCst), 0);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
        assert!(env.wav_path.is_none());
        crate::challenge::reset_for_test();
    }

    #[test]
    fn interstitial_sidecar_refuses_before_capture() {
        let _g = crate::challenge::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::challenge::reset_for_test();
        let path = tmp_side("interstitial.json");
        write_sidecar(&path, "interstitial", false);
        let capture = ImmediatePcm {
            pcm: loud_pcm(),
            calls: std::sync::atomic::AtomicU32::new(0),
        };
        let backend = CannedBackend {
            text: "nope".into(),
            calls: std::sync::atomic::AtomicU32::new(0),
        };
        let env = run_listen_with(req(&path), &capture, Some(&backend)).unwrap();
        assert!(!env.ok);
        assert!(
            env.error
                .as_deref()
                .unwrap_or("")
                .contains(CHALLENGE_PRESENT)
        );
        assert_eq!(capture.calls.load(Ordering::SeqCst), 0);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
        crate::challenge::reset_for_test();
    }

    #[test]
    fn daily_and_research_both_refuse() {
        let _g = crate::challenge::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::challenge::reset_for_test();
        let path = tmp_side("both.json");
        write_sidecar(&path, "recaptcha", true);
        let capture = ImmediatePcm {
            pcm: loud_pcm(),
            calls: std::sync::atomic::AtomicU32::new(0),
        };
        let backend = CannedBackend {
            text: "nope".into(),
            calls: std::sync::atomic::AtomicU32::new(0),
        };
        for id in [Identity::Daily, Identity::Research] {
            attach::set_identity_for_test(id);
            capture.calls.store(0, Ordering::SeqCst);
            backend.calls.store(0, Ordering::SeqCst);
            let env = run_listen_with(req(&path), &capture, Some(&backend)).unwrap();
            assert!(!env.ok, "{id:?}");
            assert!(
                env.error
                    .as_deref()
                    .unwrap_or("")
                    .contains(CHALLENGE_PRESENT),
                "{id:?}"
            );
            assert_eq!(capture.calls.load(Ordering::SeqCst), 0);
            assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
        }
        crate::challenge::reset_for_test();
    }

    #[test]
    fn capture_then_present_skips_backend() {
        let _g = crate::challenge::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::challenge::reset_for_test();
        let path = tmp_side("recheck.json");
        write_sidecar(&path, "recaptcha", false);
        // start not present: stamp extract without recaptcha
        {
            use crate::challenge::ChallengeInfo;
            use crate::extract::Extract;
            use crate::space::Space;
            let side = ObserveSidecar {
                schema: OBSERVE_SCHEMA.to_string(),
                session_id: "sid".into(),
                screenshot_path: "C:\\tmp\\x.png".into(),
                observe_path: path.to_string_lossy().into(),
                space: Space::new(0, 0, 100, 100).unwrap(),
                viewport: None,
                extract: Extract {
                    title: "YouTube".into(),
                    url: Some("https://www.youtube.com/watch?v=1".into()),
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
        }
        let capture = StampPresentCapture {
            path: path.clone(),
            pcm: loud_pcm(),
            kind: "recaptcha",
            calls: std::sync::atomic::AtomicU32::new(0),
        };
        let backend = CannedBackend {
            text: "nope".into(),
            calls: std::sync::atomic::AtomicU32::new(0),
        };
        let env = run_listen_with(req(&path), &capture, Some(&backend)).unwrap();
        assert!(!env.ok);
        assert!(
            env.error
                .as_deref()
                .unwrap_or("")
                .contains(CHALLENGE_PRESENT)
        );
        assert_eq!(capture.calls.load(Ordering::SeqCst), 1);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
        crate::challenge::reset_for_test();
    }

    #[test]
    fn fixture_transcript_envelope() {
        let _g = crate::challenge::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::challenge::reset_for_test();
        let path = tmp_side("ok.json");
        {
            use crate::challenge::ChallengeInfo;
            use crate::extract::Extract;
            use crate::space::Space;
            let side = ObserveSidecar {
                schema: OBSERVE_SCHEMA.to_string(),
                session_id: "sid".into(),
                screenshot_path: "C:\\tmp\\x.png".into(),
                observe_path: path.to_string_lossy().into(),
                space: Space::new(0, 0, 100, 100).unwrap(),
                viewport: None,
                extract: Extract {
                    title: "YouTube".into(),
                    url: Some("https://www.youtube.com/watch?v=1".into()),
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
        }
        let dir = path.parent().unwrap().join("wavs");
        unsafe { std::env::set_var(LISTEN_DIR_ENV, dir.as_os_str()) };
        let capture = ImmediatePcm {
            pcm: loud_pcm(),
            calls: std::sync::atomic::AtomicU32::new(0),
        };
        let backend = CannedBackend {
            text: "hello from the tab".into(),
            calls: std::sync::atomic::AtomicU32::new(0),
        };
        let env = run_listen_with(req(&path), &capture, Some(&backend)).unwrap();
        assert!(env.ok, "{:?}", env.error);
        assert_eq!(env.schema, LISTEN_SCHEMA);
        assert_eq!(env.source, "loopback");
        assert!(env.speech);
        assert_eq!(env.transcript, "hello from the tab");
        assert!(!env.truncated);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
        let json = serialize_listen(&env).unwrap();
        assert!(!json.contains("wav_b64"));
        unsafe { std::env::remove_var(LISTEN_DIR_ENV) };
        crate::challenge::reset_for_test();
    }

    #[test]
    fn silence_skips_backend() {
        let _g = crate::challenge::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::challenge::reset_for_test();
        let path = tmp_side("silence.json");
        {
            use crate::challenge::ChallengeInfo;
            use crate::extract::Extract;
            use crate::space::Space;
            let side = ObserveSidecar {
                schema: OBSERVE_SCHEMA.to_string(),
                session_id: "sid".into(),
                screenshot_path: "C:\\tmp\\x.png".into(),
                observe_path: path.to_string_lossy().into(),
                space: Space::new(0, 0, 100, 100).unwrap(),
                viewport: None,
                extract: Extract::default(),
                elements: Vec::new(),
                elements_total: 0,
                elements_truncated: false,
                chrome_connected: false,
                chrome_hint: None,
                challenge: ChallengeInfo::default(),
            };
            std::fs::write(&path, serde_json::to_string_pretty(&side).unwrap()).unwrap();
        }
        let capture = ImmediatePcm {
            pcm: silent_pcm(),
            calls: std::sync::atomic::AtomicU32::new(0),
        };
        let backend = CannedBackend {
            text: "nope".into(),
            calls: std::sync::atomic::AtomicU32::new(0),
        };
        let env = run_listen_with(req(&path), &capture, Some(&backend)).unwrap();
        assert!(env.ok);
        assert!(!env.speech);
        assert!(env.transcript.is_empty());
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
        crate::challenge::reset_for_test();
    }

    #[test]
    fn missing_backend_is_configured_error() {
        let _g = crate::challenge::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::challenge::reset_for_test();
        let path = tmp_side("nobackend.json");
        {
            use crate::challenge::ChallengeInfo;
            use crate::extract::Extract;
            use crate::space::Space;
            let side = ObserveSidecar {
                schema: OBSERVE_SCHEMA.to_string(),
                session_id: "sid".into(),
                screenshot_path: "C:\\tmp\\x.png".into(),
                observe_path: path.to_string_lossy().into(),
                space: Space::new(0, 0, 100, 100).unwrap(),
                viewport: None,
                extract: Extract::default(),
                elements: Vec::new(),
                elements_total: 0,
                elements_truncated: false,
                chrome_connected: false,
                chrome_hint: None,
                challenge: ChallengeInfo::default(),
            };
            std::fs::write(&path, serde_json::to_string_pretty(&side).unwrap()).unwrap();
        }
        let capture = ImmediatePcm {
            pcm: loud_pcm(),
            calls: std::sync::atomic::AtomicU32::new(0),
        };
        let env = run_listen_with(req(&path), &capture, None).unwrap();
        assert!(!env.ok);
        assert!(env.error.as_deref().unwrap_or("").contains(BACKEND_MISSING));
        crate::challenge::reset_for_test();
    }

    #[test]
    fn wav_header_is_pcm16_16k_mono() {
        let pcm = loud_pcm();
        let bytes = pcm_to_wav_bytes(&pcm);
        assert!(bytes.len() >= 44);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[12..16], b"fmt ");
        let fmt_size = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
        assert_eq!(fmt_size, 16);
        let audio_format = u16::from_le_bytes(bytes[20..22].try_into().unwrap());
        assert_eq!(audio_format, 1);
        let channels = u16::from_le_bytes(bytes[22..24].try_into().unwrap());
        assert_eq!(channels, 1);
        let rate = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
        assert_eq!(rate, TARGET_HZ);
        let bits = u16::from_le_bytes(bytes[34..36].try_into().unwrap());
        assert_eq!(bits, 16);
        assert_eq!(&bytes[36..40], b"data");
    }

    #[test]
    fn seconds_clamp() {
        assert_eq!(clamp_seconds(None), 20);
        assert_eq!(clamp_seconds(Some(2)), 3);
        assert_eq!(clamp_seconds(Some(90)), 60);
        assert_eq!(clamp_seconds(Some(20)), 20);
    }

    #[test]
    fn cmd_argv_lock() {
        let argv = cmd_argv(
            "transcribe-cli",
            Some("model.bin"),
            Path::new("a.wav"),
            Path::new("a"),
        );
        let joined = argv.join(" ");
        assert!(joined.contains("-f"));
        assert!(joined.contains("-otxt"));
        assert!(!joined.contains("-ngl"));
        assert!(!joined.to_ascii_lowercase().contains("vulkan"));
        assert_eq!(argv[0], "transcribe-cli");
    }

    #[test]
    fn cmd_fake_runner_reads_txt() {
        let dir = std::env::temp_dir().join(format!("hands-listen-cmd-{}", utc_compact()));
        let _ = std::fs::create_dir_all(&dir);
        let wav = dir.join("clip.wav");
        std::fs::write(&wav, pcm_to_wav_bytes(&loud_pcm())).unwrap();
        let fake = FakeCmd {
            text: Some("voicemail body".into()),
            code: 0,
            last_args: std::sync::Mutex::new(Vec::new()),
        };
        let backend = CmdBackend::with_runner("transcribe-cli".into(), None, Box::new(fake));
        let text = backend.transcribe(&wav, &loud_pcm()).unwrap();
        assert_eq!(text, "voicemail body");
    }

    #[test]
    fn http_200_parses_text() {
        let transport = FakeHttp::new(vec![Ok(HttpResp {
            status: 200,
            body: r#"{"text":"hello"}"#.into(),
        })]);
        let http = HttpBackend::with_transport(Box::new(transport));
        let dir = std::env::temp_dir().join(format!("hands-listen-http-{}", utc_compact()));
        let _ = std::fs::create_dir_all(&dir);
        let wav = dir.join("c.wav");
        std::fs::write(&wav, pcm_to_wav_bytes(&loud_pcm())).unwrap();
        let text = http.transcribe(&wav, &loud_pcm()).unwrap();
        assert_eq!(text, "hello");
    }

    #[test]
    fn http_500_is_error() {
        let transport = FakeHttp::new(vec![Ok(HttpResp {
            status: 500,
            body: "nope".into(),
        })]);
        let http = HttpBackend::with_transport(Box::new(transport));
        let dir = std::env::temp_dir().join(format!("hands-listen-http5-{}", utc_compact()));
        let _ = std::fs::create_dir_all(&dir);
        let wav = dir.join("c.wav");
        std::fs::write(&wav, pcm_to_wav_bytes(&loud_pcm())).unwrap();
        let err = http.transcribe(&wav, &loud_pcm()).unwrap_err();
        assert!(err.to_string().contains("500"), "{err}");
    }

    #[test]
    fn listen_url_allowlist() {
        assert!(validate_listen_url("https://transcribe.example").is_ok());
        assert!(validate_listen_url("http://127.0.0.1:9/t").is_ok());
        assert!(validate_listen_url("http://localhost/t").is_ok());
        assert!(validate_listen_url("file:///tmp/x").is_err());
        assert!(validate_listen_url("http://example.com/t").is_err());
        assert!(validate_listen_url("javascript:alert(1)").is_err());
        let gemma = 80 * 100 + 81;
        let embed = 80 * 100 + 83;
        assert!(validate_listen_url(&format!("http://127.0.0.1:{gemma}/t")).is_err());
        assert!(validate_listen_url(&format!("http://127.0.0.1:{embed}/t")).is_err());
        assert!(validate_listen_url(&format!("https://127.0.0.1:{gemma}/t")).is_err());
        assert!(validate_listen_url(&format!("https://localhost:{embed}/t")).is_err());
    }

    #[test]
    fn transcript_truncates_at_4kib() {
        let long = "a".repeat(5000);
        let (out, truncated) = cap_transcript(&long);
        assert!(truncated);
        assert_eq!(out.chars().count(), TRANSCRIPT_CAP);
    }

    #[test]
    fn source_locks_and_forbids() {
        let src = include_str!("listen.rs");
        assert!(src.contains("eRender"));
        assert!(src.contains("AUDCLNT_STREAMFLAGS_LOOPBACK"));
        assert!(src.contains("CoInitializeEx"));
        assert!(src.contains("COINIT_APARTMENTTHREADED"));
        assert!(src.contains("CoUninitialize"));
        assert!(src.contains("80 * 100 + 81"));
        assert!(src.contains("80 * 100 + 83"));
        let port = ["80", "81"].concat();
        assert!(
            !src.contains(&port),
            "listen.rs must not contain the decimal Gemma port"
        );
        let pick_mod = ["pick", "::"].concat();
        assert!(!src.contains(&pick_mod));
        let send = ["Send", "Input"].concat();
        assert!(!src.contains(&send));
        let mic = ["eCap", "ture"].concat();
        assert!(!src.contains(&mic));
        let cargo =
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).to_ascii_lowercase();
        for needle in [
            ["on", "nx"].concat(),
            ["whis", "per"].concat(),
            ["2cap", "tcha"].concat(),
        ] {
            assert!(
                !cargo.contains(&needle),
                "Cargo.toml must not mention {needle}"
            );
        }
        assert!(!include_str!("observe.rs").contains("listen::"));
        let main = include_str!("main.rs");
        assert!(
            main.contains("listen_main("),
            "CLI listen must dispatch via listen_main (no desk lease)"
        );
        let input = mcp_fn_slice(main, "fn input_main(");
        assert!(
            input.contains("Command::Listen"),
            "input_main must treat Listen as unreachable, not actuate:\n{input}"
        );
        assert!(
            !input.contains("listen::run_listen"),
            "input_main must not run listen:\n{input}"
        );
    }

    #[test]
    fn mcp_and_docs_name_the_sense() {
        let mcp = include_str!("mcp.rs");
        assert!(mcp.contains("not a CAPTCHA solver"));
        assert!(mcp.contains("never a CAPTCHA solver on any identity"));
        let agents = include_str!("../AGENTS.md");
        let readme = include_str!("../README.md");
        assert!(agents.to_ascii_lowercase().contains("listen"));
        assert!(readme.to_ascii_lowercase().contains("listen"));
        assert!(
            agents.contains("not a CAPTCHA solver")
                || agents.to_ascii_lowercase().contains("not a captcha solver")
        );
        assert!(
            readme.contains("not a CAPTCHA solver")
                || readme.to_ascii_lowercase().contains("not a captcha solver")
        );
        let slice = mcp_fn_slice(mcp, "fn run_listen_tool(");
        assert!(slice.contains("ContentBlock::text"));
        assert!(!slice.contains("ContentBlock::image"));
        assert!(!slice.contains("ContentBlock::audio"));
    }

    fn mcp_fn_slice<'a>(src: &'a str, needle: &str) -> &'a str {
        let start = src.find(needle).unwrap_or_else(|| panic!("{needle}"));
        let rest = &src[start..];
        let rel = rest.find("\nfn ").unwrap_or(rest.len());
        &rest[..rel]
    }

    #[test]
    fn downmix_and_resample() {
        let stereo = Pcm {
            sample_rate: 8_000,
            channels: 2,
            samples: vec![1000, 3000, 1000, 3000],
        };
        let out = normalize_pcm(stereo);
        assert_eq!(out.channels, 1);
        assert_eq!(out.sample_rate, TARGET_HZ);
        assert!(!out.samples.is_empty());
    }

    #[test]
    #[ignore = "live loopback; not a CI gate"]
    fn live_loopback_not_a_ci_gate() {
        let pcm = WasapiLoopback.capture_pcm(3).expect("live loopback");
        assert_eq!(pcm.sample_rate, TARGET_HZ);
        assert_eq!(pcm.channels, 1);
    }
}
