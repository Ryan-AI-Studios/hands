//! Append-only session JSONL audit log. Pause/stop wipe allows; logs stay.

use std::cell::Cell;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use windows::Win32::System::SystemInformation::GetLocalTime;

use crate::error::HandsError;
use crate::lease::{self, FreezeCause};
use crate::observe::{DEFAULT_ENVELOPE_MAX_BYTES, ENVELOPE_MAX_BYTES};

pub const LOGS_SCHEMA: &str = "hands.logs/v1";
pub const DEFAULT_TAIL: usize = 50;
pub const MAX_TAIL: usize = 200;
const MAX_STEM: usize = 80;
const STEM_PREFIX: usize = 71;
const STALE: Duration = Duration::from_secs(5);

thread_local! {
    static CLOCK: Cell<Option<fn() -> String>> = const { Cell::new(None) };
}

static INSTALLED: OnceLock<()> = OnceLock::new();
static LAST_SESSION: Mutex<Option<String>> = Mutex::new(None);
static LOG_IO: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    pub schema: String,
    pub ts: String,
    pub session_id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<LogTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fence: Option<LogFence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<LogConfirm>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observe: Option<LogObserve>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_meta: Option<TypeMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, rename = "yield", skip_serializing_if = "Option::is_none")]
    pub yield_info: Option<LogYield>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogTarget {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogFence {
    pub domain: String,
    pub category: String,
    pub name: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogConfirm {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub revoke: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub list: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogObserve {
    pub detail: String,
    pub screenshot_path: String,
    pub elements_total: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TypeMeta {
    pub len: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogYield {
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionInfo {
    pub session_id: String,
    pub path: String,
    pub events: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LogsEnvelope {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub ok: bool,
    pub events: Vec<Event>,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sessions: Option<Vec<SessionInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

struct FileLock {
    path: PathBuf,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn set_clock_for_test(clock: Option<fn() -> String>) {
    CLOCK.with(|c| c.set(clock));
}

pub fn remember_session(id: &str) {
    if !is_legal_id(id) || id == "desk" {
        return;
    }
    if let Ok(mut slot) = LAST_SESSION.lock() {
        *slot = Some(id.to_string());
    }
}

pub fn last_session() -> Option<String> {
    LAST_SESSION.lock().ok().and_then(|g| g.clone())
}

pub fn ensure_installed() {
    INSTALLED.get_or_init(install_logs);
}

fn install_logs() {
    lease::subscribe(on_freeze);
}

fn on_freeze(cause: FreezeCause) {
    match cause {
        FreezeCause::Pause => {
            if let Some(id) = last_session() {
                let _ = append_event(&pause_event(&id), false);
            }
            let _ = append_event(&pause_event("desk"), true);
        }
        FreezeCause::Stop => {
            let _ = append_event(&stop_event("desk", false), true);
        }
        FreezeCause::Physical => {}
    }
}

fn pause_event(session_id: &str) -> Event {
    Event {
        schema: LOGS_SCHEMA.into(),
        ts: now_ts(),
        session_id: session_id.into(),
        kind: "pause".into(),
        tool: None,
        ok: None,
        error: None,
        target: None,
        fence: None,
        confirm: None,
        observe: None,
        type_meta: None,
        key: None,
        yield_info: None,
    }
}

fn stop_event(session_id: &str, from_tool: bool) -> Event {
    Event {
        schema: LOGS_SCHEMA.into(),
        ts: now_ts(),
        session_id: session_id.into(),
        kind: "stop".into(),
        tool: from_tool.then(|| "stop".into()),
        ok: from_tool.then_some(true),
        error: None,
        target: None,
        fence: None,
        confirm: None,
        observe: None,
        type_meta: None,
        key: None,
        yield_info: None,
    }
}

fn now_ts() -> String {
    CLOCK.with(|c| {
        if let Some(clock) = c.get() {
            clock()
        } else {
            local_now()
        }
    })
}

fn local_now() -> String {
    let st = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
    )
}

fn local_app_hands() -> Result<PathBuf, HandsError> {
    let base = std::env::var("LOCALAPPDATA")
        .map_err(|_| HandsError::Logs("LOCALAPPDATA is not set".into()))?;
    Ok(PathBuf::from(base).join("hands"))
}

pub fn logs_dir() -> Result<PathBuf, HandsError> {
    if let Ok(p) = std::env::var("HANDS_LOGS_DIR")
        && !p.trim().is_empty()
    {
        return Ok(PathBuf::from(p));
    }
    Ok(local_app_hands()?.join("logs"))
}

fn is_legal_id(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Reject reserved `desk` and illegal charset (including `.` / `..`) on tool writes.
pub fn check_write_id(id: &str) -> Result<(), HandsError> {
    if id == "desk" {
        return Err(HandsError::Logs(
            "session_id 'desk' is reserved for Pause/stop desk events".into(),
        ));
    }
    if !is_legal_id(id) {
        return Err(HandsError::Logs(format!(
            "illegal session_id '{id}' (allowed [A-Za-z0-9._-])"
        )));
    }
    Ok(())
}

/// FNV-1a 32-bit — stable across processes, not a crypto hash.
fn hash8(id: &str) -> String {
    let mut h: u32 = 2_166_136_261;
    for b in id.as_bytes() {
        h ^= u32::from(*b);
        h = h.wrapping_mul(16_777_619);
    }
    format!("{h:08x}")
}

/// File stem for a session id. `desk` is allowed (read / desk-wide write).
pub fn file_stem(session_id: &str) -> Result<String, HandsError> {
    if !is_legal_id(session_id) {
        return Err(HandsError::Logs(format!(
            "illegal session_id '{session_id}' (allowed [A-Za-z0-9._-])"
        )));
    }
    if session_id.len() > MAX_STEM {
        let prefix: String = session_id.chars().take(STEM_PREFIX).collect();
        return Ok(format!("{prefix}-{}", hash8(session_id)));
    }
    Ok(session_id.to_string())
}

fn jsonl_path(session_id: &str) -> Result<PathBuf, HandsError> {
    Ok(logs_dir()?.join(format!("{}.jsonl", file_stem(session_id)?)))
}

fn acquire_lock(jsonl: &Path) -> Result<FileLock, HandsError> {
    let mut name = jsonl.as_os_str().to_os_string();
    name.push(".lock");
    let path = PathBuf::from(name);
    if let Some(dir) = path.parent() {
        if dir.exists() && !dir.is_dir() {
            return Err(HandsError::Logs(format!(
                "create logs dir {}: not a directory",
                dir.display()
            )));
        }
        std::fs::create_dir_all(dir)
            .map_err(|err| HandsError::Logs(format!("create logs dir {}: {err}", dir.display())))?;
    }
    loop {
        if std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .is_ok()
        {
            return Ok(FileLock { path });
        }
        if let Ok(meta) = std::fs::metadata(&path)
            && let Ok(modified) = meta.modified()
            && modified.elapsed().unwrap_or(Duration::ZERO) >= STALE
        {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn with_log_io<T>(f: impl FnOnce() -> Result<T, HandsError>) -> Result<T, HandsError> {
    let _mem = LOG_IO.lock().unwrap_or_else(|e| e.into_inner());
    f()
}

/// Append one event. Tool writes reject reserved `desk`. Desk-wide writes pass `allow_desk`.
fn append_event(event: &Event, allow_desk: bool) -> Result<(), HandsError> {
    if event.session_id == "desk" && !allow_desk {
        return Err(HandsError::Logs(
            "session_id 'desk' is reserved for Pause/stop desk events".into(),
        ));
    }
    let path = jsonl_path(&event.session_id)?;
    let line = serde_json::to_string(event)
        .map_err(|err| HandsError::Logs(format!("serialize event: {err}")))?;
    with_log_io(|| {
        let _disk = acquire_lock(&path)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|err| {
                HandsError::Logs(format!("create logs dir {}: {err}", dir.display()))
            })?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|err| HandsError::Logs(format!("open {}: {err}", path.display())))?;
        file.write_all(line.as_bytes())
            .and_then(|_| file.write_all(b"\n"))
            .and_then(|_| file.flush())
            .map_err(|err| HandsError::Logs(format!("append {}: {err}", path.display())))?;
        Ok(())
    })
}

pub fn record(event: Event) -> Result<(), HandsError> {
    remember_session(&event.session_id);
    append_event(&event, false)
}

pub fn record_yield(session_id: &str, reason: &str) -> Result<(), HandsError> {
    record(Event {
        schema: LOGS_SCHEMA.into(),
        ts: now_ts(),
        session_id: session_id.into(),
        kind: "yield".into(),
        tool: None,
        ok: None,
        error: None,
        target: None,
        fence: None,
        confirm: None,
        observe: None,
        type_meta: None,
        key: None,
        yield_info: Some(LogYield {
            reason: reason.into(),
        }),
    })
}

pub fn record_observe(
    session_id: &str,
    detail: &str,
    screenshot_path: &str,
    elements_total: usize,
) -> Result<(), HandsError> {
    record(Event {
        schema: LOGS_SCHEMA.into(),
        ts: now_ts(),
        session_id: session_id.into(),
        kind: "tool".into(),
        tool: Some("observe".into()),
        ok: Some(true),
        error: None,
        target: None,
        fence: None,
        confirm: None,
        observe: Some(LogObserve {
            detail: detail.into(),
            screenshot_path: screenshot_path.into(),
            elements_total,
        }),
        type_meta: None,
        key: None,
        yield_info: None,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn record_confirm(
    session_id: &str,
    domain: Option<&str>,
    category: Option<&str>,
    mode: Option<&str>,
    revoke: bool,
    list: bool,
    ok: bool,
    error: Option<&str>,
) -> Result<(), HandsError> {
    record(Event {
        schema: LOGS_SCHEMA.into(),
        ts: now_ts(),
        session_id: session_id.into(),
        kind: "confirm".into(),
        tool: None,
        ok: Some(ok),
        error: error.map(str::to_string),
        target: None,
        fence: None,
        confirm: Some(LogConfirm {
            domain: domain.filter(|s| !s.is_empty()).map(str::to_string),
            category: category.filter(|s| !s.is_empty()).map(str::to_string),
            mode: mode.filter(|s| !s.is_empty()).map(str::to_string),
            revoke,
            list,
        }),
        observe: None,
        type_meta: None,
        key: None,
        yield_info: None,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn record_actuate(
    session_id: &str,
    tool: &str,
    ok: bool,
    error: Option<&str>,
    target: Option<LogTarget>,
    fence: Option<LogFence>,
    type_len: Option<usize>,
    key: Option<&str>,
) -> Result<(), HandsError> {
    let refuse = fence.is_some();
    let stop = tool == "stop" && !refuse;
    let kind = if refuse {
        "refuse"
    } else if stop {
        "stop"
    } else {
        "tool"
    };
    record(Event {
        schema: LOGS_SCHEMA.into(),
        ts: now_ts(),
        session_id: session_id.into(),
        kind: kind.into(),
        tool: Some(tool.into()),
        ok: Some(ok),
        error: error.map(str::to_string),
        target,
        fence,
        confirm: None,
        observe: None,
        type_meta: type_len.map(|len| TypeMeta { len }),
        key: key.map(str::to_string),
        yield_info: None,
    })
}

fn read_events(path: &Path) -> Result<Vec<Event>, HandsError> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(HandsError::Logs(format!("read {}: {err}", path.display())));
        }
    };
    let mut events = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<Event>(line) {
            events.push(event);
        }
    }
    Ok(events)
}

fn clamp_tail(tail: Option<usize>) -> usize {
    tail.unwrap_or(DEFAULT_TAIL).clamp(1, MAX_TAIL)
}

fn serialize_logs_len(envelope: &LogsEnvelope) -> usize {
    serde_json::to_string(envelope)
        .map(|s| s.len())
        .unwrap_or(usize::MAX)
}

pub fn serialize_logs(envelope: &LogsEnvelope) -> Result<String, HandsError> {
    let json = serde_json::to_string(envelope)
        .map_err(|err| HandsError::Logs(format!("logs serialize: {err}")))?;
    if json.len() > ENVELOPE_MAX_BYTES {
        return Err(HandsError::Logs(format!(
            "logs envelope is {} bytes (hard max {ENVELOPE_MAX_BYTES})",
            json.len()
        )));
    }
    Ok(json)
}

fn fit_envelope(mut envelope: LogsEnvelope, max_bytes: usize) -> Result<LogsEnvelope, HandsError> {
    while serialize_logs_len(&envelope) > max_bytes && !envelope.events.is_empty() {
        envelope.events.remove(0);
        envelope.truncated = true;
    }
    if serialize_logs_len(&envelope) > max_bytes {
        return Err(HandsError::Logs(format!(
            "logs envelope is {} bytes after dropping events (hard max {max_bytes})",
            serialize_logs_len(&envelope)
        )));
    }
    Ok(envelope)
}

pub fn read_tail(session_id: &str, tail: Option<usize>) -> Result<LogsEnvelope, HandsError> {
    let id = session_id.trim();
    if id.is_empty() {
        return Err(HandsError::Logs(
            "logs requires --session-id (or --list); will not mint".into(),
        ));
    }
    let path = jsonl_path(id)?;
    let all = with_log_io(|| {
        let _disk = acquire_lock(&path)?;
        read_events(&path)
    })?;
    let n = clamp_tail(tail);
    let truncated_by_tail = all.len() > n;
    let events = if truncated_by_tail {
        all[all.len() - n..].to_vec()
    } else {
        all
    };
    let max_bytes = if tail.is_none() {
        DEFAULT_ENVELOPE_MAX_BYTES
    } else {
        ENVELOPE_MAX_BYTES
    };
    fit_envelope(
        LogsEnvelope {
            session_id: Some(id.to_string()),
            ok: true,
            events,
            truncated: truncated_by_tail,
            sessions: None,
            error: None,
        },
        max_bytes,
    )
}

fn list_entries() -> Result<Vec<SessionInfo>, HandsError> {
    let dir = logs_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    let rd = std::fs::read_dir(&dir)
        .map_err(|err| HandsError::Logs(format!("list {}: {err}", dir.display())))?;
    for ent in rd {
        let ent = ent.map_err(|err| HandsError::Logs(format!("list {}: {err}", dir.display())))?;
        let path = ent.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        files.push(path);
    }
    files.sort();
    let mut out = Vec::new();
    for path in files {
        let events = read_events(&path)?;
        let session_id = events
            .iter()
            .rev()
            .find_map(|e| {
                if !e.session_id.is_empty() {
                    Some(e.session_id.clone())
                } else {
                    None
                }
            })
            .or_else(|| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .filter(|s| *s == "desk")
                    .map(str::to_string)
            });
        let Some(session_id) = session_id else {
            continue;
        };
        out.push(SessionInfo {
            session_id,
            path: path.to_string_lossy().into_owned(),
            events: events.len(),
        });
    }
    Ok(out)
}

fn fit_list(mut sessions: Vec<SessionInfo>) -> Result<LogsEnvelope, HandsError> {
    let mut truncated = false;
    loop {
        let envelope = LogsEnvelope {
            session_id: None,
            ok: true,
            events: Vec::new(),
            truncated,
            sessions: Some(sessions.clone()),
            error: None,
        };
        if serialize_logs_len(&envelope) <= ENVELOPE_MAX_BYTES {
            return Ok(envelope);
        }
        truncated = true;
        if sessions.is_empty() {
            return Err(HandsError::Logs(format!(
                "logs list envelope is {} bytes (hard max {ENVELOPE_MAX_BYTES})",
                serialize_logs_len(&envelope)
            )));
        }
        let drop_at = sessions
            .iter()
            .enumerate()
            .rev()
            .find(|(_, s)| s.session_id != "desk")
            .map(|(i, _)| i)
            .unwrap_or(sessions.len() - 1);
        sessions.remove(drop_at);
    }
}

pub fn list_sessions() -> Result<LogsEnvelope, HandsError> {
    let sessions = with_log_io(|| {
        let dir = logs_dir()?;
        if dir.exists() {
            let lock_path = dir.join(".list.lock");
            let _disk = acquire_lock(&lock_path)?;
            list_entries()
        } else {
            Ok(Vec::new())
        }
    })?;
    fit_list(sessions)
}

/// Read surface. Missing/empty `session_id` without `list` is a tool error (no mint).
pub fn run_logs(
    session_id: Option<&str>,
    list: bool,
    tail: Option<usize>,
) -> Result<LogsEnvelope, HandsError> {
    if list {
        return list_sessions();
    }
    let id = session_id.map(str::trim).filter(|s| !s.is_empty());
    match id {
        Some(id) => read_tail(id, tail),
        None => Err(HandsError::Logs(
            "logs requires --session-id (or --list); will not mint".into(),
        )),
    }
}

#[cfg(test)]
pub(crate) fn reinstall_for_test() {
    // Mark INSTALLED so a later ensure_installed() (e.g. stop_inner) does not
    // subscribe a second on_freeze after this explicit re-subscribe.
    let _ = INSTALLED.get_or_init(|| ());
    install_logs();
}

#[cfg(test)]
pub(crate) fn reset_last_session_for_test() {
    if let Ok(mut slot) = LAST_SESSION.lock() {
        *slot = None;
    }
}

#[cfg(test)]
pub(crate) fn with_test_env<T>(f: impl FnOnce() -> T) -> T {
    tests::with_test_env(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allows::{self, AllowMode};
    use crate::classify::Category;
    use crate::fence;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    pub fn with_test_env<T>(f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("hands-logs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let prev = std::env::var_os("HANDS_LOGS_DIR");
        unsafe { std::env::set_var("HANDS_LOGS_DIR", &dir) };
        set_clock_for_test(None);
        reset_last_session_for_test();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        set_clock_for_test(None);
        reset_last_session_for_test();
        match prev {
            Some(v) => unsafe { std::env::set_var("HANDS_LOGS_DIR", v) },
            None => unsafe { std::env::remove_var("HANDS_LOGS_DIR") },
        }
        let _ = std::fs::remove_dir_all(&dir);
        match result {
            Ok(v) => v,
            Err(p) => std::panic::resume_unwind(p),
        }
    }

    fn tool_event(session_id: &str, tool: &str) -> Event {
        Event {
            schema: LOGS_SCHEMA.into(),
            ts: now_ts(),
            session_id: session_id.into(),
            kind: "tool".into(),
            tool: Some(tool.into()),
            ok: Some(true),
            error: None,
            target: None,
            fence: None,
            confirm: None,
            observe: None,
            type_meta: None,
            key: None,
            yield_info: None,
        }
    }

    #[test]
    fn illegal_ids_are_tool_errors_and_do_not_escape() {
        with_test_env(|| {
            let dir = logs_dir().unwrap();
            for bad in [r"..\..\evil", "foo/bar", "has space", "foo\\bar", "..", "."] {
                let err = record(tool_event(bad, "click")).expect_err(bad);
                assert!(err.to_string().contains("illegal"), "{err}");
            }
            let mut names = Vec::new();
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for ent in rd.flatten() {
                    names.push(ent.file_name());
                }
            }
            assert!(
                names.is_empty(),
                "no files should be created for illegal ids: {names:?}"
            );
            let parent = dir.parent().unwrap();
            assert!(!parent.join("evil.jsonl").exists());
            assert!(!parent.join("evil").exists());
        });
    }

    #[test]
    fn illegal_or_desk_id_fails_originating_type_before_input() {
        with_test_env(|| {
            for bad in ["..", "foo/bar", "desk"] {
                let env = crate::actuate::type_text(crate::actuate::ActuateRequest {
                    session_id: Some(bad.into()),
                    text: Some("hi".into()),
                    ..crate::actuate::ActuateRequest::default()
                })
                .unwrap();
                assert!(!env.ok, "{bad}");
                let err = env.error.unwrap_or_default();
                assert!(
                    err.contains("illegal") || err.contains("reserved"),
                    "{bad}: {err}"
                );
            }
            let dir = logs_dir().unwrap();
            let names: Vec<_> = std::fs::read_dir(&dir)
                .unwrap()
                .flatten()
                .map(|e| e.file_name())
                .collect();
            assert!(names.is_empty(), "{names:?}");
        });
    }

    #[test]
    fn desk_write_is_tool_error_read_still_works() {
        with_test_env(|| {
            let err = record(tool_event("desk", "click")).expect_err("desk write");
            assert!(err.to_string().contains("reserved"), "{err}");
            append_event(&pause_event("desk"), true).unwrap();
            let env = read_tail("desk", None).unwrap();
            assert_eq!(env.events.len(), 1);
            assert_eq!(env.events[0].kind, "pause");
            assert_eq!(env.events[0].session_id, "desk");
        });
    }

    #[test]
    fn long_ids_with_same_prefix_use_distinct_files() {
        with_test_env(|| {
            let a = format!("{}AAA", "p".repeat(80));
            let b = format!("{}BBB", "p".repeat(80));
            assert!(a.len() > 80 && b.len() > 80);
            assert_eq!(&a[..80], &b[..80]);
            record(tool_event(&a, "click")).unwrap();
            record(tool_event(&b, "hover")).unwrap();
            let sa = file_stem(&a).unwrap();
            let sb = file_stem(&b).unwrap();
            assert_ne!(sa, sb);
            assert_eq!(sa.len(), 80);
            assert_eq!(&sa[..71], &a[..71]);
            let dir = logs_dir().unwrap();
            assert!(dir.join(format!("{sa}.jsonl")).exists());
            assert!(dir.join(format!("{sb}.jsonl")).exists());
            let ta = read_tail(&a, None).unwrap();
            let tb = read_tail(&b, None).unwrap();
            assert_eq!(ta.events.len(), 1);
            assert_eq!(ta.events[0].tool.as_deref(), Some("click"));
            assert_eq!(tb.events[0].tool.as_deref(), Some("hover"));
        });
    }

    #[test]
    fn write_two_events_read_back_in_order() {
        with_test_env(|| {
            record(tool_event("s-order", "observe")).unwrap();
            record(tool_event("s-order", "click")).unwrap();
            let env = read_tail("s-order", None).unwrap();
            assert_eq!(env.events.len(), 2);
            assert_eq!(env.events[0].tool.as_deref(), Some("observe"));
            assert_eq!(env.events[1].tool.as_deref(), Some("click"));
            assert_eq!(env.events[0].schema, LOGS_SCHEMA);
            assert!(env.events[0].ts.contains('T'));
        });
    }

    #[test]
    fn unknown_fields_and_unparseable_lines_are_skipped() {
        with_test_env(|| {
            record(tool_event("s-skip", "click")).unwrap();
            let path = jsonl_path("s-skip").unwrap();
            let mut raw = std::fs::read_to_string(&path).unwrap();
            raw.push_str(
                r#"{"schema":"hands.logs/v1","ts":"2026-08-16T01:02:03","session_id":"s-skip","kind":"tool","tool":"hover","ok":true,"extra":true}"#,
            );
            raw.push('\n');
            raw.push_str("{\"schema\":\"hands.logs/v1\",\"ts\":\"2026-08-16T01:02:04\"");
            raw.push('\n');
            std::fs::write(&path, raw).unwrap();
            let env = read_tail("s-skip", None).unwrap();
            assert_eq!(env.events.len(), 2);
            assert_eq!(env.events[0].tool.as_deref(), Some("click"));
            assert_eq!(env.events[1].tool.as_deref(), Some("hover"));
        });
    }

    #[test]
    fn record_yield_writes_kind_yield() {
        with_test_env(|| {
            record_yield("s-yield", "challenge-ui").unwrap();
            let env = read_tail("s-yield", None).unwrap();
            assert_eq!(env.events.len(), 1);
            assert_eq!(env.events[0].kind, "yield");
            assert_eq!(
                env.events[0].yield_info.as_ref().map(|y| y.reason.as_str()),
                Some("challenge-ui")
            );
        });
    }

    #[test]
    fn type_redaction_writes_len_not_text() {
        with_test_env(|| {
            let secret = "s3cret-password-value";
            record_actuate(
                "s-redact",
                "type",
                true,
                None,
                None,
                None,
                Some(secret.chars().count()),
                None,
            )
            .unwrap();
            let path = jsonl_path("s-redact").unwrap();
            let raw = std::fs::read_to_string(path).unwrap();
            assert!(!raw.contains(secret), "{raw}");
            assert!(raw.contains("\"len\":21"), "{raw}");
            assert!(!raw.contains("main_text"), "{raw}");
        });
    }

    #[test]
    fn observe_record_has_no_main_text_or_elements() {
        with_test_env(|| {
            record_observe("s-obs", "default", r"C:\tmp\shot.png", 12).unwrap();
            let path = jsonl_path("s-obs").unwrap();
            let raw = std::fs::read_to_string(&path).unwrap();
            assert!(!raw.contains("main_text"), "{raw}");
            assert!(!raw.contains("\"elements\""), "{raw}");
            let env = read_tail("s-obs", None).unwrap();
            let obs = env.events[0].observe.as_ref().unwrap();
            assert_eq!(obs.elements_total, 12);
            assert_eq!(obs.detail, "default");
        });
    }

    #[test]
    fn confirm_record_has_flags_not_grant_table() {
        with_test_env(|| {
            record_confirm(
                "s-cf",
                Some("linkedin.com"),
                Some("applications"),
                Some("session"),
                false,
                false,
                true,
                None,
            )
            .unwrap();
            record_confirm("s-cf", None, None, None, false, true, true, None).unwrap();
            let raw = std::fs::read_to_string(jsonl_path("s-cf").unwrap()).unwrap();
            assert!(!raw.contains("allows"), "{raw}");
            assert!(!raw.contains("persist"), "{raw}");
            let env = read_tail("s-cf", None).unwrap();
            assert_eq!(
                env.events[0].confirm.as_ref().unwrap().mode.as_deref(),
                Some("session")
            );
            assert!(env.events[1].confirm.as_ref().unwrap().list);
        });
    }

    #[test]
    fn isolation_session_a_cannot_tail_b() {
        with_test_env(|| {
            record(tool_event("sess-a", "click")).unwrap();
            record(tool_event("sess-b", "type")).unwrap();
            let a = read_tail("sess-a", None).unwrap();
            assert_eq!(a.events.len(), 1);
            assert_eq!(a.events[0].tool.as_deref(), Some("click"));
            assert!(a.events.iter().all(|e| e.session_id == "sess-a"));
        });
    }

    #[test]
    fn no_mint_on_read() {
        with_test_env(|| {
            let err = run_logs(None, false, None).expect_err("missing id");
            assert!(err.to_string().contains("will not mint"), "{err}");
            let err = run_logs(Some("   "), false, None).expect_err("blank id");
            assert!(err.to_string().contains("will not mint"), "{err}");
        });
    }

    #[test]
    fn tail_drops_oldest_to_fit_16kib() {
        with_test_env(|| {
            for i in 0..80 {
                let _ = record(Event {
                    schema: LOGS_SCHEMA.into(),
                    ts: now_ts(),
                    session_id: "s-fat".into(),
                    kind: "tool".into(),
                    tool: Some("observe".into()),
                    ok: Some(true),
                    error: None,
                    target: None,
                    fence: None,
                    confirm: None,
                    observe: Some(LogObserve {
                        detail: "default".into(),
                        screenshot_path: format!("C:\\tmp\\{}\\shot.png", "x".repeat(400)),
                        elements_total: i,
                    }),
                    type_meta: None,
                    key: None,
                    yield_info: None,
                });
            }
            let env = read_tail("s-fat", Some(200)).unwrap();
            let json = serialize_logs(&env).unwrap();
            assert!(json.len() <= ENVELOPE_MAX_BYTES, "len {}", json.len());
            assert!(
                json.len() > DEFAULT_ENVELOPE_MAX_BYTES,
                "explicit --tail must keep the 16 KiB budget, got {}",
                json.len()
            );
            assert!(env.truncated);
            assert!(!env.events.is_empty());
            let first = env.events[0].observe.as_ref().unwrap().elements_total;
            let last = env
                .events
                .last()
                .unwrap()
                .observe
                .as_ref()
                .unwrap()
                .elements_total;
            assert!(last > first, "newest last: {first}..{last}");
        });
    }

    fn fat_observe(session_id: &str, i: usize) -> Event {
        Event {
            schema: LOGS_SCHEMA.into(),
            ts: now_ts(),
            session_id: session_id.into(),
            kind: "tool".into(),
            tool: Some("observe".into()),
            ok: Some(true),
            error: None,
            target: None,
            fence: None,
            confirm: None,
            observe: Some(LogObserve {
                detail: "default".into(),
                screenshot_path: format!("C:\\tmp\\{}\\shot.png", "x".repeat(400)),
                elements_total: i,
            }),
            type_meta: None,
            key: None,
            yield_info: None,
        }
    }

    fn kind_only(session_id: &str, kind: &str) -> Event {
        Event {
            schema: LOGS_SCHEMA.into(),
            ts: now_ts(),
            session_id: session_id.into(),
            kind: kind.into(),
            tool: None,
            ok: None,
            error: None,
            target: None,
            fence: None,
            confirm: None,
            observe: None,
            type_meta: None,
            key: None,
            yield_info: None,
        }
    }

    #[test]
    fn default_none_fits_4kib_and_keeps_newest_stop() {
        with_test_env(|| {
            for i in 0..80 {
                record(fat_observe("s-4k", i)).unwrap();
            }
            record(kind_only("s-4k", "pause")).unwrap();
            record(kind_only("s-4k", "stop")).unwrap();
            let env = run_logs(Some("s-4k"), false, None).unwrap();
            let json = serialize_logs(&env).unwrap();
            assert!(
                json.len() <= DEFAULT_ENVELOPE_MAX_BYTES,
                "len {}",
                json.len()
            );
            assert!(json.len() <= ENVELOPE_MAX_BYTES, "len {}", json.len());
            assert!(env.truncated);
            assert!(!env.events.is_empty());
            assert_eq!(env.events.last().unwrap().kind, "stop");
        });
    }

    #[test]
    fn short_session_default_is_not_truncated() {
        with_test_env(|| {
            record(tool_event("s-short", "click")).unwrap();
            record(tool_event("s-short", "hover")).unwrap();
            record(tool_event("s-short", "type")).unwrap();
            let env = run_logs(Some("s-short"), false, None).unwrap();
            assert!(!env.truncated);
            assert_eq!(env.events.len(), 3);
            assert_eq!(env.events[0].tool.as_deref(), Some("click"));
            assert_eq!(env.events[2].tool.as_deref(), Some("type"));
        });
    }

    #[test]
    fn mcp_and_cli_mention_tail_budget() {
        let mcp = include_str!("mcp.rs");
        let main_prod = include_str!("main.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(
            mcp.contains("newest-last") && mcp.contains("4 KiB") && mcp.contains("truncated"),
            "mcp logs description"
        );
        assert!(
            mcp.contains("16 KiB") && mcp.contains("pause/stop"),
            "mcp logs description"
        );
        assert!(
            (main_prod.contains("newest-last") || main_prod.contains("Newest-last"))
                && main_prod.contains("4 KiB")
                && main_prod.contains("truncated"),
            "cli logs about/--tail"
        );
        assert!(
            main_prod.contains("16 KiB") && main_prod.contains("pause/stop"),
            "cli logs about/--tail"
        );
    }

    #[test]
    fn list_drops_entries_to_fit_16kib_keeps_desk() {
        with_test_env(|| {
            append_event(&pause_event("desk"), true).unwrap();
            for i in 0..200 {
                let id = format!("list-{i:03}-{}", "n".repeat(60));
                record(tool_event(&id, "click")).unwrap();
            }
            let env = list_sessions().unwrap();
            let json = serialize_logs(&env).unwrap();
            assert!(json.len() <= ENVELOPE_MAX_BYTES, "len {}", json.len());
            assert!(env.truncated);
            let sessions = env.sessions.unwrap();
            assert!(
                sessions.iter().any(|s| s.session_id == "desk"),
                "desk must be kept: {sessions:?}"
            );
        });
    }

    #[test]
    fn unwritable_dir_leaves_record_err_only() {
        with_test_env(|| {
            let dir = logs_dir().unwrap();
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::write(&dir, b"not-a-dir").unwrap();
            let err = record(tool_event("s-uw", "click")).expect_err("unwritable");
            assert!(
                err.to_string().contains("create logs dir")
                    || err.to_string().contains("not a directory")
                    || err.to_string().contains("open"),
                "{err}"
            );
        });
    }

    #[test]
    fn pause_wipes_allows_and_leaves_jsonl() {
        let _lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        allows::with_test_env(|| {
            with_test_env(|| {
                lease::reset_for_test();
                fence::reinstall_for_test();
                reinstall_for_test();
                allows::grant(
                    "s-pw",
                    "desktop",
                    Category::Applications,
                    AllowMode::Session,
                )
                .unwrap();
                record(tool_event("s-pw", "click")).unwrap();
                remember_session("s-pw");
                let before = std::fs::read_to_string(jsonl_path("s-pw").unwrap()).unwrap();
                assert!(before.contains("click"));
                lease::freeze_now_with(FreezeCause::Pause);
                assert_eq!(
                    allows::check("s-pw", "desktop", Category::Applications).unwrap(),
                    allows::AllowHit::Miss
                );
                let after = std::fs::read_to_string(jsonl_path("s-pw").unwrap()).unwrap();
                assert!(after.contains("click"));
                let env = read_tail("s-pw", None).unwrap();
                assert!(env.events.iter().any(|e| e.kind == "tool"));
                assert!(env.events.iter().any(|e| e.kind == "pause"));
                let desk = read_tail("desk", None).unwrap();
                assert!(desk.events.iter().any(|e| e.kind == "pause"));
            });
        });
    }

    #[test]
    fn midnight_drops_allows_not_logs() {
        allows::with_test_env(|| {
            with_test_env(|| {
                allows::set_clock_for_test(Some(|| "2026-08-16".into()));
                allows::grant("s-mid", "desktop", Category::Social, AllowMode::Session).unwrap();
                record(tool_event("s-mid", "click")).unwrap();
                let before = std::fs::read_to_string(jsonl_path("s-mid").unwrap()).unwrap();
                allows::set_clock_for_test(Some(|| "2026-08-17".into()));
                assert_eq!(
                    allows::check("s-mid", "desktop", Category::Social).unwrap(),
                    allows::AllowHit::Miss
                );
                let after = std::fs::read_to_string(jsonl_path("s-mid").unwrap()).unwrap();
                assert_eq!(before, after);
                assert!(after.contains("click"));
            });
        });
    }

    #[test]
    fn physical_freeze_does_not_log_or_delete() {
        let _lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        with_test_env(|| {
            lease::reset_for_test();
            reinstall_for_test();
            record(tool_event("s-phys", "click")).unwrap();
            remember_session("s-phys");
            let before = std::fs::read_to_string(jsonl_path("s-phys").unwrap()).unwrap();
            lease::freeze_now_with(FreezeCause::Physical);
            let after = std::fs::read_to_string(jsonl_path("s-phys").unwrap()).unwrap();
            assert_eq!(before, after);
            assert!(!logs_dir().unwrap().join("desk.jsonl").exists());
        });
    }

    fn with_stop_request_path<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        let dir = std::env::temp_dir().join(format!("hands-logs-stop-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stop-request.json");
        let prev = std::env::var_os("HANDS_STOP_REQUEST_PATH");
        unsafe {
            std::env::set_var("HANDS_STOP_REQUEST_PATH", &path);
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&path)));
        match prev {
            Some(v) => unsafe { std::env::set_var("HANDS_STOP_REQUEST_PATH", v) },
            None => unsafe { std::env::remove_var("HANDS_STOP_REQUEST_PATH") },
        }
        let _ = std::fs::remove_dir_all(&dir);
        match result {
            Ok(v) => v,
            Err(p) => std::panic::resume_unwind(p),
        }
    }

    #[test]
    fn one_stop_writes_one_session_line_and_one_desk_line() {
        let _lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        with_test_env(|| {
            with_stop_request_path(|_| {
                lease::reset_for_test();
                let _ingest = lease::enable_stop_ingest_for_test();
                fence::reinstall_for_test();
                reinstall_for_test();
                remember_session("s-stop");
                let env = crate::actuate::stop(crate::actuate::ActuateRequest {
                    session_id: Some("s-stop".into()),
                    ..crate::actuate::ActuateRequest::default()
                })
                .unwrap();
                assert!(env.ok);
                assert!(env.frozen);
                let session = read_tail("s-stop", None).unwrap();
                let stops: Vec<_> = session.events.iter().filter(|e| e.kind == "stop").collect();
                assert_eq!(stops.len(), 1, "{session:?}");
                assert_eq!(stops[0].tool.as_deref(), Some("stop"));
                let desk = read_tail("desk", None).unwrap();
                let desk_stops: Vec<_> = desk.events.iter().filter(|e| e.kind == "stop").collect();
                assert_eq!(desk_stops.len(), 1, "{desk:?}");
                assert_eq!(desk_stops[0].tool.as_deref(), None);
            });
        });
    }

    #[test]
    fn tail_default_clamps() {
        assert_eq!(clamp_tail(None), 50);
        assert_eq!(clamp_tail(Some(0)), 1);
        assert_eq!(clamp_tail(Some(1)), 1);
        assert_eq!(clamp_tail(Some(200)), 200);
        assert_eq!(clamp_tail(Some(500)), 200);
    }

    #[test]
    fn type_text_redacts_and_unwritable_dir_leaves_envelope() {
        with_test_env(|| {
            let secret = "s3cret-password-value\n";
            let env = crate::actuate::type_text(crate::actuate::ActuateRequest {
                session_id: Some("t-redact".into()),
                text: Some(secret.into()),
                ..crate::actuate::ActuateRequest::default()
            })
            .unwrap();
            assert!(!env.ok);
            let path = jsonl_path("t-redact").unwrap();
            let raw = std::fs::read_to_string(path).unwrap();
            assert!(!raw.contains("s3cret-password-value"), "{raw}");
            assert!(
                raw.contains("\"len\":21") || raw.contains("\"len\":22"),
                "{raw}"
            );

            let dir = logs_dir().unwrap();
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::write(&dir, b"not-a-dir").unwrap();
            let env = crate::actuate::type_text(crate::actuate::ActuateRequest {
                session_id: Some("t-uw".into()),
                text: Some("hello\n".into()),
                ..crate::actuate::ActuateRequest::default()
            })
            .unwrap();
            assert!(!env.ok);
            assert!(env.error.as_deref().unwrap_or("").contains("newline"));
        });
    }

    #[test]
    fn run_confirm_logs_flags_not_grants() {
        allows::with_test_env(|| {
            with_test_env(|| {
                allows::grant("other", "desktop", Category::Lead, AllowMode::Session).unwrap();
                let env =
                    allows::run_confirm(Some("s-list"), None, None, None, false, true).unwrap();
                assert!(env.ok);
                let raw = std::fs::read_to_string(jsonl_path("s-list").unwrap()).unwrap();
                assert!(!raw.contains("allows"), "{raw}");
                assert!(!raw.contains("\"lead\""), "{raw}");
                assert!(raw.contains("\"list\":true"), "{raw}");
            });
        });
    }
}
