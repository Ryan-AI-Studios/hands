//! Persist + session/once allow store. No secrets.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use windows::Win32::System::SystemInformation::GetLocalTime;

use crate::classify::{self, Category};
use crate::error::HandsError;
use crate::logs;
use crate::observe::ENVELOPE_MAX_BYTES;
use crate::session::resolve_session_id_from_os;

pub const PERSIST_SCHEMA: &str = "hands.allows/v1";
pub const SESSION_SCHEMA: &str = "hands.session-allows/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllowMode {
    Once,
    Session,
    Persist,
}

impl AllowMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Session => "session",
            Self::Persist => "persist",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, HandsError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "once" => Ok(Self::Once),
            "session" => Ok(Self::Session),
            "persist" => Ok(Self::Persist),
            other => Err(HandsError::Fence(format!(
                "unknown mode '{other}' (expected once, session, or persist)"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllowHit {
    Once,
    Session,
    Persist,
    Miss,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PersistGrantView {
    pub domain: String,
    pub category: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionGrantView {
    pub session_id: String,
    pub domain: String,
    pub category: String,
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AllowList {
    pub session: Vec<SessionGrantView>,
    pub persist: Vec<PersistGrantView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfirmEnvelope {
    pub session_id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allows: Option<AllowList>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersistFile {
    #[serde(default)]
    schema: String,
    #[serde(default)]
    grants: Vec<PersistGrant>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersistGrant {
    #[serde(default)]
    domain: String,
    #[serde(default)]
    category: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SessionFile {
    #[serde(default)]
    schema: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    grants: Vec<SessionGrant>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SessionGrant {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    domain: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    mode: String,
}

thread_local! {
    static CLOCK: Cell<Option<fn() -> String>> = const { Cell::new(None) };
}

/// Set when Pause/Stop could not persist an empty session file. `check`/`load`
/// treat session/once as empty until a later write succeeds.
static SESSION_WIPED: AtomicBool = AtomicBool::new(false);
static SESSION_IO: Mutex<()> = Mutex::new(());

struct SessionLock {
    path: PathBuf,
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn with_session_io<T>(f: impl FnOnce() -> Result<T, HandsError>) -> Result<T, HandsError> {
    let _mem = SESSION_IO.lock().unwrap_or_else(|e| e.into_inner());
    let _disk = acquire_session_lock();
    f()
}

fn acquire_session_lock() -> SessionLock {
    let path = session_path()
        .map(|p| {
            let mut name = p.into_os_string();
            name.push(".lock");
            PathBuf::from(name)
        })
        .unwrap_or_else(|_| std::env::temp_dir().join("hands-session-allows.lock"));
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Wait for a live holder. Steal only a stale lock (crashed process).
    const STALE: Duration = Duration::from_secs(5);
    loop {
        if std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .is_ok()
        {
            return SessionLock { path };
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

fn wipe_marker_path() -> Result<PathBuf, HandsError> {
    let path = session_path()?;
    let mut name = path.into_os_string();
    name.push(".wiped");
    Ok(PathBuf::from(name))
}

fn write_wipe_marker() -> Result<(), HandsError> {
    let path = wipe_marker_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|err| HandsError::Fence(format!("create wipe dir: {err}")))?;
    }
    std::fs::write(&path, b"wiped")
        .map_err(|err| HandsError::Fence(format!("write wipe marker: {err}")))
}

fn wipe_marker_present() -> bool {
    wipe_marker_path().map(|p| p.exists()).unwrap_or(false)
}

fn clear_wipe_marker() {
    if let Ok(path) = wipe_marker_path() {
        let _ = std::fs::remove_file(path);
    }
}

pub fn set_clock_for_test(clock: Option<fn() -> String>) {
    CLOCK.with(|c| c.set(clock));
}

fn today() -> String {
    CLOCK.with(|c| {
        if let Some(clock) = c.get() {
            clock()
        } else {
            local_today()
        }
    })
}

fn local_today() -> String {
    let st = unsafe { GetLocalTime() };
    format!("{:04}-{:02}-{:02}", st.wYear, st.wMonth, st.wDay)
}

fn persist_path() -> Result<PathBuf, HandsError> {
    if let Ok(p) = std::env::var("HANDS_ALLOWS_PATH")
        && !p.trim().is_empty()
    {
        return Ok(PathBuf::from(p));
    }
    Ok(local_app_hands()?.join("allows.json"))
}

fn session_path() -> Result<PathBuf, HandsError> {
    if let Ok(p) = std::env::var("HANDS_SESSION_ALLOWS_PATH")
        && !p.trim().is_empty()
    {
        return Ok(PathBuf::from(p));
    }
    Ok(local_app_hands()?.join("session-allows.json"))
}

fn local_app_hands() -> Result<PathBuf, HandsError> {
    let base = std::env::var("LOCALAPPDATA")
        .map_err(|_| HandsError::Fence("LOCALAPPDATA is not set".into()))?;
    Ok(PathBuf::from(base).join("hands"))
}

pub fn warmup() {
    let _ = load_persist();
    let _ = load_session();
}

pub fn grant(
    session_id: &str,
    domain: &str,
    category: Category,
    mode: AllowMode,
) -> Result<(), HandsError> {
    with_session_io(|| grant_inner(session_id, domain, category, mode))
}

fn grant_inner(
    session_id: &str,
    domain: &str,
    category: Category,
    mode: AllowMode,
) -> Result<(), HandsError> {
    let domain = classify::normalize_host(domain);
    match mode {
        AllowMode::Persist => {
            let mut file = load_persist()?;
            if !file.grants.iter().any(|g| {
                classify::normalize_host(&g.domain) == domain
                    && Category::parse(&g.category).ok() == Some(category)
            }) {
                file.grants.push(PersistGrant {
                    domain,
                    category: category.to_string(),
                });
            }
            save_persist(&file)
        }
        AllowMode::Once | AllowMode::Session => {
            let mut file = load_session()?;
            drop_if_stale(&mut file);
            let mode_s = mode.as_str();
            if !file.grants.iter().any(|g| {
                g.session_id == session_id
                    && classify::normalize_host(&g.domain) == domain
                    && Category::parse(&g.category).ok() == Some(category)
                    && g.mode.eq_ignore_ascii_case(mode_s)
            }) {
                file.grants.push(SessionGrant {
                    session_id: session_id.to_string(),
                    domain,
                    category: category.to_string(),
                    mode: mode_s.to_string(),
                });
            }
            save_session(&file)
        }
    }
}

pub fn check(session_id: &str, domain: &str, category: Category) -> Result<AllowHit, HandsError> {
    with_session_io(|| check_inner(session_id, domain, category))
}

fn check_inner(session_id: &str, domain: &str, category: Category) -> Result<AllowHit, HandsError> {
    let mut session = load_session()?;
    if drop_if_stale(&mut session) {
        save_session(&session)?;
    }
    if let Some(idx) = session.grants.iter().position(|g| {
        g.session_id == session_id
            && g.mode.eq_ignore_ascii_case("once")
            && Category::parse(&g.category).ok() == Some(category)
            && classify::host_matches(&g.domain, domain)
    }) {
        session.grants.remove(idx);
        save_session(&session)?;
        return Ok(AllowHit::Once);
    }
    if session.grants.iter().any(|g| {
        g.session_id == session_id
            && g.mode.eq_ignore_ascii_case("session")
            && Category::parse(&g.category).ok() == Some(category)
            && classify::host_matches(&g.domain, domain)
    }) {
        return Ok(AllowHit::Session);
    }
    let persist = load_persist()?;
    if persist.grants.iter().any(|g| {
        Category::parse(&g.category).ok() == Some(category)
            && classify::host_matches(&g.domain, domain)
    }) {
        return Ok(AllowHit::Persist);
    }
    Ok(AllowHit::Miss)
}

pub fn revoke(
    session_id: &str,
    domain: &str,
    category: Category,
    mode: AllowMode,
) -> Result<(), HandsError> {
    with_session_io(|| revoke_inner(session_id, domain, category, mode))
}

fn revoke_inner(
    session_id: &str,
    domain: &str,
    category: Category,
    mode: AllowMode,
) -> Result<(), HandsError> {
    let domain = classify::normalize_host(domain);
    match mode {
        AllowMode::Persist => {
            let mut file = load_persist()?;
            file.grants.retain(|g| {
                !(classify::normalize_host(&g.domain) == domain
                    && Category::parse(&g.category).ok() == Some(category))
            });
            save_persist(&file)
        }
        AllowMode::Once | AllowMode::Session => {
            let mut file = load_session()?;
            drop_if_stale(&mut file);
            let mode_s = mode.as_str();
            file.grants.retain(|g| {
                !(g.session_id == session_id
                    && classify::normalize_host(&g.domain) == domain
                    && Category::parse(&g.category).ok() == Some(category)
                    && g.mode.eq_ignore_ascii_case(mode_s))
            });
            save_session(&file)
        }
    }
}

pub fn clear_session_allows() {
    SESSION_WIPED.store(true, Ordering::SeqCst);
    // Marker first, even if another process holds the lock.
    let _ = write_wipe_marker();
    let _ = with_session_io(|| {
        if let Err(err) = clear_session_allows_inner() {
            if let Ok(path) = session_path() {
                let _ = std::fs::remove_file(path);
            }
            return Err(err);
        }
        SESSION_WIPED.store(false, Ordering::SeqCst);
        clear_wipe_marker();
        Ok(())
    });
}

fn clear_session_allows_inner() -> Result<(), HandsError> {
    save_session(&SessionFile {
        schema: SESSION_SCHEMA.into(),
        date: today(),
        grants: Vec::new(),
    })
}

pub fn list(session_id: Option<&str>) -> Result<AllowList, HandsError> {
    with_session_io(|| list_inner(session_id))
}

fn list_inner(session_id: Option<&str>) -> Result<AllowList, HandsError> {
    let mut session = load_session()?;
    if drop_if_stale(&mut session) {
        save_session(&session)?;
    }
    let persist = load_persist()?;
    Ok(AllowList {
        session: session
            .grants
            .into_iter()
            .filter(|g| session_id.is_none_or(|id| g.session_id == id))
            .map(|g| SessionGrantView {
                session_id: g.session_id,
                domain: g.domain,
                category: g.category,
                mode: g.mode,
            })
            .collect(),
        persist: persist
            .grants
            .into_iter()
            .map(|g| PersistGrantView {
                domain: g.domain,
                category: g.category,
            })
            .collect(),
    })
}

pub fn run_confirm(
    session_id: Option<&str>,
    domain: Option<&str>,
    category: Option<&str>,
    mode: Option<&str>,
    revoke_flag: bool,
    list_flag: bool,
) -> Result<ConfirmEnvelope, HandsError> {
    let session_id = resolve_session_id_from_os(session_id);
    logs::check_write_id(&session_id)?;
    if list_flag {
        let env = finalize_confirm(ConfirmEnvelope {
            session_id: session_id.clone(),
            ok: true,
            domain: None,
            category: None,
            mode: None,
            allows: Some(list(Some(&session_id))?),
            error: None,
        })?;
        log_confirm(&env, false, true);
        return Ok(env);
    }
    let domain_raw = domain
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| HandsError::Fence("confirm requires --domain".into()))?;
    let category_raw = category
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| HandsError::Fence("confirm requires --category".into()))?;
    let mode_raw = mode
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| HandsError::Fence("confirm requires --mode".into()))?;
    let category = Category::parse(category_raw)?;
    let mode = AllowMode::parse(mode_raw)?;
    let domain = classify::normalize_host(domain_raw);
    if domain.is_empty() {
        return Err(HandsError::Fence("confirm domain is empty".into()));
    }
    if revoke_flag {
        revoke(&session_id, &domain, category, mode)?;
    } else {
        grant(&session_id, &domain, category, mode)?;
    }
    let env = finalize_confirm(ConfirmEnvelope {
        session_id,
        ok: true,
        domain: Some(domain),
        category: Some(category.to_string()),
        mode: Some(mode.as_str().to_string()),
        allows: None,
        error: None,
    })?;
    log_confirm(&env, revoke_flag, false);
    Ok(env)
}

fn log_confirm(env: &ConfirmEnvelope, revoke: bool, list: bool) {
    logs::ensure_installed();
    logs::remember_session(&env.session_id);
    let _ = logs::record_confirm(
        &env.session_id,
        env.domain.as_deref(),
        env.category.as_deref(),
        env.mode.as_deref(),
        revoke,
        list,
        env.ok,
        env.error.as_deref(),
    );
}

pub fn serialize_confirm(envelope: &ConfirmEnvelope) -> Result<String, HandsError> {
    serde_json::to_string(envelope)
        .map_err(|err| HandsError::Fence(format!("confirm serialize: {err}")))
}

fn finalize_confirm(envelope: ConfirmEnvelope) -> Result<ConfirmEnvelope, HandsError> {
    let json = serialize_confirm(&envelope)?;
    if json.len() > ENVELOPE_MAX_BYTES {
        return Err(HandsError::Fence(format!(
            "confirm envelope is {} bytes (hard max {ENVELOPE_MAX_BYTES})",
            json.len()
        )));
    }
    Ok(envelope)
}

fn drop_if_stale(file: &mut SessionFile) -> bool {
    let today = today();
    if file.date != today {
        file.date = today;
        file.grants.clear();
        true
    } else {
        false
    }
}

fn load_persist() -> Result<PersistFile, HandsError> {
    let path = persist_path()?;
    let mut file: PersistFile = read_json(&path)?.unwrap_or_default();
    if file.schema.is_empty() {
        file.schema = PERSIST_SCHEMA.into();
    }
    Ok(file)
}

fn load_session() -> Result<SessionFile, HandsError> {
    if SESSION_WIPED.load(Ordering::SeqCst) || wipe_marker_present() {
        return Ok(SessionFile {
            schema: SESSION_SCHEMA.into(),
            date: today(),
            grants: Vec::new(),
        });
    }
    let path = session_path()?;
    let mut file: SessionFile = read_json(&path)?.unwrap_or_default();
    if file.schema.is_empty() {
        file.schema = SESSION_SCHEMA.into();
    }
    Ok(file)
}

fn save_persist(file: &PersistFile) -> Result<(), HandsError> {
    let mut file = file.clone();
    file.schema = PERSIST_SCHEMA.into();
    write_json(&persist_path()?, &file)
}

fn save_session(file: &SessionFile) -> Result<(), HandsError> {
    let mut file = file.clone();
    file.schema = SESSION_SCHEMA.into();
    if file.date.is_empty() {
        file.date = today();
    }
    if wipe_marker_present() && !file.grants.is_empty() {
        return Err(HandsError::Fence(
            "session allows were wiped (Pause/stop); grant again after confirm".into(),
        ));
    }
    write_json(&session_path()?, &file)?;
    if file.grants.is_empty() {
        SESSION_WIPED.store(false, Ordering::SeqCst);
        clear_wipe_marker();
    }
    Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, HandsError> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(None),
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|err| HandsError::Fence(format!("read {}: {err}", path.display()))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(HandsError::Fence(format!("read {}: {err}", path.display()))),
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), HandsError> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|err| HandsError::Fence(format!("serialize: {err}")))?;
    atomic_write(path, json.as_bytes())
}

fn atomic_write(path: &Path, data: &[u8]) -> Result<(), HandsError> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)
            .map_err(|err| HandsError::Fence(format!("create dir {}: {err}", dir.display())))?;
    }
    let tmp_name = format!(
        "{}.tmp-{}",
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("allows"),
        uuid::Uuid::new_v4()
    );
    let tmp = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(&tmp_name),
        _ => PathBuf::from(&tmp_name),
    };
    std::fs::write(&tmp, data)
        .map_err(|err| HandsError::Fence(format!("write temp {}: {err}", tmp.display())))?;
    if path.exists() {
        let bak_name = format!(
            "{}.bak-{}",
            path.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("allows"),
            uuid::Uuid::new_v4()
        );
        let bak = match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir.join(&bak_name),
            _ => PathBuf::from(&bak_name),
        };
        if let Err(err) = std::fs::rename(path, &bak) {
            let _ = std::fs::remove_file(&tmp);
            return Err(HandsError::Fence(format!("park {}: {err}", path.display())));
        }
        if let Err(err) = std::fs::rename(&tmp, path) {
            let _ = std::fs::rename(&bak, path);
            let _ = std::fs::remove_file(&tmp);
            return Err(HandsError::Fence(format!(
                "rename onto {}: {err}",
                path.display()
            )));
        }
        let _ = std::fs::remove_file(&bak);
    } else if let Err(err) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(HandsError::Fence(format!(
            "rename onto {}: {err}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn with_test_env<T>(f: impl FnOnce() -> T) -> T {
    tests::with_test_env(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    pub fn with_test_env<T>(f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("hands-allows-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let persist = dir.join("allows.json");
        let session = dir.join("session-allows.json");
        let prev_p = std::env::var_os("HANDS_ALLOWS_PATH");
        let prev_s = std::env::var_os("HANDS_SESSION_ALLOWS_PATH");
        unsafe {
            std::env::set_var("HANDS_ALLOWS_PATH", &persist);
            std::env::set_var("HANDS_SESSION_ALLOWS_PATH", &session);
        }
        set_clock_for_test(None);
        SESSION_WIPED.store(false, Ordering::SeqCst);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        set_clock_for_test(None);
        SESSION_WIPED.store(false, Ordering::SeqCst);
        match prev_p {
            Some(v) => unsafe { std::env::set_var("HANDS_ALLOWS_PATH", v) },
            None => unsafe { std::env::remove_var("HANDS_ALLOWS_PATH") },
        }
        match prev_s {
            Some(v) => unsafe { std::env::set_var("HANDS_SESSION_ALLOWS_PATH", v) },
            None => unsafe { std::env::remove_var("HANDS_SESSION_ALLOWS_PATH") },
        }
        let _ = std::fs::remove_dir_all(&dir);
        match result {
            Ok(v) => v,
            Err(p) => std::panic::resume_unwind(p),
        }
    }

    fn day_a() -> String {
        "2026-08-16".into()
    }

    fn day_b() -> String {
        "2026-08-17".into()
    }

    #[test]
    fn once_is_consumed_on_hit() {
        with_test_env(|| {
            grant("s1", "desktop", Category::Applications, AllowMode::Once).unwrap();
            assert_eq!(
                check("s1", "desktop", Category::Applications).unwrap(),
                AllowHit::Once
            );
            assert_eq!(
                check("s1", "desktop", Category::Applications).unwrap(),
                AllowHit::Miss
            );
        });
    }

    #[test]
    fn session_survives_reload_same_id() {
        with_test_env(|| {
            grant(
                "s1",
                "linkedin.com",
                Category::Applications,
                AllowMode::Session,
            )
            .unwrap();
            assert_eq!(
                check("s1", "jobs.linkedin.com", Category::Applications).unwrap(),
                AllowHit::Session
            );
            assert_eq!(
                check("s1", "jobs.linkedin.com", Category::Applications).unwrap(),
                AllowHit::Session
            );
        });
    }

    #[test]
    fn new_session_id_misses_other_id() {
        with_test_env(|| {
            grant("aaa", "desktop", Category::Save, AllowMode::Session).unwrap();
            grant("aaa", "desktop", Category::Lead, AllowMode::Once).unwrap();
            assert_eq!(
                check("bbb", "desktop", Category::Save).unwrap(),
                AllowHit::Miss
            );
            assert_eq!(
                check("bbb", "desktop", Category::Lead).unwrap(),
                AllowHit::Miss
            );
        });
    }

    #[test]
    fn midnight_drops_session_once_persist_remains() {
        with_test_env(|| {
            set_clock_for_test(Some(day_a));
            grant("s1", "desktop", Category::Social, AllowMode::Session).unwrap();
            grant("s1", "desktop", Category::Lead, AllowMode::Once).unwrap();
            grant("s1", "desktop", Category::Save, AllowMode::Persist).unwrap();
            set_clock_for_test(Some(day_b));
            assert_eq!(
                check("s1", "desktop", Category::Social).unwrap(),
                AllowHit::Miss
            );
            assert_eq!(
                check("s1", "desktop", Category::Lead).unwrap(),
                AllowHit::Miss
            );
            assert_eq!(
                check("s1", "desktop", Category::Save).unwrap(),
                AllowHit::Persist
            );
        });
    }

    #[test]
    fn persist_round_trip() {
        with_test_env(|| {
            grant(
                "s1",
                "www.linkedin.com",
                Category::Applications,
                AllowMode::Persist,
            )
            .unwrap();
            assert_eq!(
                check("other", "jobs.linkedin.com", Category::Applications).unwrap(),
                AllowHit::Persist
            );
            let listed = list(None).unwrap();
            assert_eq!(
                listed.persist,
                vec![PersistGrantView {
                    domain: "linkedin.com".into(),
                    category: "applications".into(),
                }]
            );
            let path = persist_path().unwrap();
            let raw = std::fs::read_to_string(path).unwrap();
            let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(v["schema"], PERSIST_SCHEMA);
        });
    }

    #[test]
    fn mode_scoped_revoke_session_leaves_persist() {
        with_test_env(|| {
            grant("s1", "desktop", Category::Applications, AllowMode::Session).unwrap();
            grant("s1", "desktop", Category::Applications, AllowMode::Persist).unwrap();
            revoke("s1", "desktop", Category::Applications, AllowMode::Session).unwrap();
            assert_eq!(
                check("s1", "desktop", Category::Applications).unwrap(),
                AllowHit::Persist
            );
            revoke("s1", "desktop", Category::Applications, AllowMode::Persist).unwrap();
            assert_eq!(
                check("s1", "desktop", Category::Applications).unwrap(),
                AllowHit::Miss
            );
        });
    }

    #[test]
    fn clear_session_allows_leaves_persist() {
        with_test_env(|| {
            grant("s1", "desktop", Category::Lead, AllowMode::Session).unwrap();
            grant("s2", "desktop", Category::Lead, AllowMode::Once).unwrap();
            grant("s1", "desktop", Category::Lead, AllowMode::Persist).unwrap();
            clear_session_allows();
            assert_eq!(
                check("s1", "desktop", Category::Lead).unwrap(),
                AllowHit::Persist
            );
            assert_eq!(
                check("s2", "desktop", Category::Lead).unwrap(),
                AllowHit::Persist
            );
        });
    }

    #[test]
    fn grant_refused_while_wipe_marker_present() {
        with_test_env(|| {
            write_wipe_marker().unwrap();
            let err = grant("s1", "desktop", Category::Lead, AllowMode::Session)
                .expect_err("must not restore wiped session grants");
            assert!(err.to_string().contains("wiped"), "{err}");
        });
    }

    #[test]
    fn wipe_marker_hides_session_after_new_process() {
        with_test_env(|| {
            grant("s1", "desktop", Category::Lead, AllowMode::Session).unwrap();
            write_wipe_marker().unwrap();
            SESSION_WIPED.store(false, Ordering::SeqCst);
            assert_eq!(
                check("s1", "desktop", Category::Lead).unwrap(),
                AllowHit::Miss
            );
        });
    }

    #[test]
    fn list_is_scoped_to_current_session() {
        with_test_env(|| {
            grant("aaa", "desktop", Category::Save, AllowMode::Session).unwrap();
            grant("bbb", "desktop", Category::Lead, AllowMode::Session).unwrap();
            let listed = list(Some("aaa")).unwrap();
            assert_eq!(listed.session.len(), 1);
            assert_eq!(listed.session[0].session_id, "aaa");
        });
    }

    #[test]
    fn clear_write_failure_still_hides_session_grants() {
        with_test_env(|| {
            grant("s1", "desktop", Category::Lead, AllowMode::Session).unwrap();
            let path = session_path().unwrap();
            std::fs::remove_file(&path).unwrap();
            std::fs::create_dir(&path).unwrap();
            clear_session_allows();
            assert_eq!(
                check("s1", "desktop", Category::Lead).unwrap(),
                AllowHit::Miss
            );
            let _ = std::fs::remove_dir(&path);
        });
    }

    #[test]
    fn unknown_json_fields_are_ignored() {
        with_test_env(|| {
            let path = persist_path().unwrap();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                &path,
                r#"{"schema":"hands.allows/v1","extra":true,"grants":[{"domain":"desktop","category":"save","note":"x"}]}"#,
            )
            .unwrap();
            assert_eq!(
                check("s", "desktop", Category::Save).unwrap(),
                AllowHit::Persist
            );
        });
    }
}
