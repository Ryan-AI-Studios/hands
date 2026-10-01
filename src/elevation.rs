//! Process UIAccess / integrity self-report and High-IL window probe.
//!
//! Read-only: no input injection. Does not install the desk lease.

use std::path::{Path, PathBuf};

use serde::Serialize;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, POINT};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_MANDATORY_LABEL,
    TOKEN_QUERY, TokenIntegrityLevel, TokenUIAccess,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, WindowFromPoint};
use windows::core::HRESULT;

use crate::error::HandsError;
use crate::foreground;

pub const HIGH_IL_RID: u32 = 0x3000;
pub const MEDIUM_PLUS_RID: u32 = 0x2010;
pub const UNGRANTED_MSG: &str = "elevated window requires a UIAccess-granted installed copy; run elevation-status, then scripts\\uiaaccess-provision.ps1";

const E_ACCESSDENIED: HRESULT = HRESULT(0x8007_0005u32 as i32);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ElevationStatus {
    pub exe_path: String,
    pub integrity: String,
    pub integrity_rid: u32,
    pub uiaccess_granted: bool,
    pub secure_location: bool,
}

pub fn status() -> Result<ElevationStatus, HandsError> {
    #[cfg(test)]
    if let Some(hook) = status_hook() {
        return Ok(hook());
    }
    let exe_path = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "hands.exe".into());
    let integrity_rid = process_integrity_rid()?;
    Ok(ElevationStatus {
        exe_path: exe_path.clone(),
        integrity: integrity_name(integrity_rid).into(),
        integrity_rid,
        uiaccess_granted: process_uiaccess()?,
        secure_location: path_is_secure(Path::new(&exe_path)),
    })
}

pub fn serialize_status(status: &ElevationStatus) -> Result<String, HandsError> {
    serde_json::to_string(status)
        .map_err(|err| HandsError::Observe(format!("elevation-status: {err}")))
}

pub fn process_uiaccess() -> Result<bool, HandsError> {
    #[cfg(test)]
    if let Some(hook) = uiaccess_hook() {
        return Ok(hook());
    }
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) }
        .map_err(|err| HandsError::Observe(format!("OpenProcessToken: {err}")))?;
    let granted = token_uiaccess(token);
    let _ = unsafe { CloseHandle(token) };
    granted
}

pub fn process_integrity_rid() -> Result<u32, HandsError> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) }
        .map_err(|err| HandsError::Observe(format!("OpenProcessToken: {err}")))?;
    let rid = token_integrity_rid(token);
    let _ = unsafe { CloseHandle(token) };
    rid
}

pub fn window_is_high_il(hwnd: isize) -> bool {
    #[cfg(test)]
    if let Some(hook) = high_il_hook() {
        return hook(hwnd);
    }
    let root = foreground::root_hwnd(hwnd).unwrap_or(hwnd);
    let raw = foreground::raw_hwnd(root);
    if raw.is_invalid() {
        return false;
    }
    match window_integrity_rid(raw) {
        Ok(rid) => rid >= HIGH_IL_RID,
        Err(ProbeErr::Denied) => true,
        Err(ProbeErr::Other) => false,
    }
}

pub fn probe_target(resolved: Option<isize>, point: Option<(i32, i32)>) -> Option<isize> {
    if let Some(hwnd) = resolved {
        return foreground::root_hwnd(hwnd).or(Some(hwnd));
    }
    if let Some((x, y)) = point
        && let Some(hwnd) = hwnd_at_point(x, y)
    {
        return Some(hwnd);
    }
    crate::foreground::foreground_hwnd()
}

pub fn hwnd_at_point(x: i32, y: i32) -> Option<isize> {
    let hit = unsafe { WindowFromPoint(POINT { x, y }) };
    let raw = foreground::hwnd_raw(hit)?;
    foreground::root_hwnd(raw).or(Some(raw))
}

pub fn path_is_secure(path: &Path) -> bool {
    #[cfg(test)]
    if let Some(hook) = secure_hook() {
        return hook(path);
    }
    let canon = canonicalize_lossy(path);
    secure_prefixes()
        .into_iter()
        .any(|prefix| path_starts_with(&canon, &prefix))
}

pub fn integrity_name(rid: u32) -> &'static str {
    match rid {
        0x0000 => "untrusted",
        0x1000 => "low",
        0x2000 => "medium",
        MEDIUM_PLUS_RID => "medium_plus",
        HIGH_IL_RID => "high",
        0x4000 => "system",
        _ => "other",
    }
}

enum ProbeErr {
    Denied,
    Other,
}

fn window_integrity_rid(hwnd: HWND) -> Result<u32, ProbeErr> {
    let mut pid = 0u32;
    let _ = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return Err(ProbeErr::Other);
    }
    let process = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(h) => h,
        Err(err) if is_access_denied(&err) => return Err(ProbeErr::Denied),
        Err(_) => return Err(ProbeErr::Other),
    };
    let mut token = HANDLE::default();
    let opened = unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) };
    if let Err(err) = opened {
        let _ = unsafe { CloseHandle(process) };
        return if is_access_denied(&err) {
            Err(ProbeErr::Denied)
        } else {
            Err(ProbeErr::Other)
        };
    }
    let rid = token_integrity_rid(token);
    let _ = unsafe { CloseHandle(token) };
    let _ = unsafe { CloseHandle(process) };
    rid.map_err(|_| ProbeErr::Other)
}

fn token_uiaccess(token: HANDLE) -> Result<bool, HandsError> {
    let mut value = 0u32;
    let mut needed = 0u32;
    unsafe {
        GetTokenInformation(
            token,
            TokenUIAccess,
            Some((&raw mut value).cast()),
            std::mem::size_of::<u32>() as u32,
            &raw mut needed,
        )
    }
    .map_err(|err| HandsError::Observe(format!("TokenUIAccess: {err}")))?;
    Ok(value != 0)
}

fn token_integrity_rid(token: HANDLE) -> Result<u32, HandsError> {
    let mut needed = 0u32;
    let _ = unsafe { GetTokenInformation(token, TokenIntegrityLevel, None, 0, &raw mut needed) };
    if needed == 0 {
        return Err(HandsError::Observe("TokenIntegrityLevel: empty".into()));
    }
    let mut buf = vec![0u8; needed as usize];
    unsafe {
        GetTokenInformation(
            token,
            TokenIntegrityLevel,
            Some(buf.as_mut_ptr().cast()),
            needed,
            &raw mut needed,
        )
    }
    .map_err(|err| HandsError::Observe(format!("TokenIntegrityLevel: {err}")))?;
    let label = unsafe { &*buf.as_ptr().cast::<TOKEN_MANDATORY_LABEL>() };
    let sid = label.Label.Sid;
    if sid.is_invalid() {
        return Err(HandsError::Observe(
            "TokenIntegrityLevel: invalid SID".into(),
        ));
    }
    let count = unsafe { *GetSidSubAuthorityCount(sid) } as u32;
    if count == 0 {
        return Err(HandsError::Observe(
            "TokenIntegrityLevel: no sub-authority".into(),
        ));
    }
    let rid = unsafe { *GetSidSubAuthority(sid, count - 1) };
    Ok(rid)
}

fn is_access_denied(err: &windows::core::Error) -> bool {
    err.code() == E_ACCESSDENIED
}

fn canonicalize_lossy(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn secure_prefixes() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(pf) = std::env::var_os("ProgramFiles") {
        out.push(PathBuf::from(pf));
    }
    if let Some(pf86) = std::env::var_os(r"ProgramFiles(x86)") {
        out.push(PathBuf::from(pf86));
    }
    if let Some(root) = std::env::var_os("SystemRoot") {
        out.push(PathBuf::from(root).join("System32"));
    }
    out
}

fn path_starts_with(path: &Path, prefix: &Path) -> bool {
    let path = strip_verbatim(path);
    let prefix = strip_verbatim(&canonicalize_lossy(prefix));
    let path_s = path.to_string_lossy();
    let prefix_s = prefix.to_string_lossy();
    let path_n = path_s.trim_end_matches(['\\', '/']).to_ascii_lowercase();
    let prefix_n = prefix_s.trim_end_matches(['\\', '/']).to_ascii_lowercase();
    path_n == prefix_n || path_n.starts_with(&(prefix_n + "\\"))
}

fn strip_verbatim(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

#[cfg(test)]
use std::sync::Mutex;

#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
type HighIlHook = fn(isize) -> bool;
#[cfg(test)]
type UiAccessHook = fn() -> bool;
#[cfg(test)]
type StatusHook = fn() -> ElevationStatus;
#[cfg(test)]
type SecureHook = fn(&Path) -> bool;

#[cfg(test)]
static HIGH_IL_HOOK: Mutex<Option<HighIlHook>> = Mutex::new(None);
#[cfg(test)]
static UIACCESS_HOOK: Mutex<Option<UiAccessHook>> = Mutex::new(None);
#[cfg(test)]
static STATUS_HOOK: Mutex<Option<StatusHook>> = Mutex::new(None);
#[cfg(test)]
static SECURE_HOOK: Mutex<Option<SecureHook>> = Mutex::new(None);

#[cfg(test)]
fn high_il_hook() -> Option<HighIlHook> {
    *HIGH_IL_HOOK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
fn uiaccess_hook() -> Option<UiAccessHook> {
    *UIACCESS_HOOK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
fn status_hook() -> Option<StatusHook> {
    *STATUS_HOOK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
fn secure_hook() -> Option<SecureHook> {
    *SECURE_HOOK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
pub(crate) fn set_high_il_hook(hook: Option<HighIlHook>) {
    *HIGH_IL_HOOK.lock().unwrap_or_else(|e| e.into_inner()) = hook;
}

#[cfg(test)]
pub(crate) fn set_uiaccess_hook(hook: Option<UiAccessHook>) {
    *UIACCESS_HOOK.lock().unwrap_or_else(|e| e.into_inner()) = hook;
}

#[cfg(test)]
pub(crate) fn set_status_hook(hook: Option<StatusHook>) {
    *STATUS_HOOK.lock().unwrap_or_else(|e| e.into_inner()) = hook;
}

#[cfg(test)]
pub(crate) fn set_secure_hook(hook: Option<SecureHook>) {
    *SECURE_HOOK.lock().unwrap_or_else(|e| e.into_inner()) = hook;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integrity_name_table() {
        assert_eq!(integrity_name(0x2000), "medium");
        assert_eq!(integrity_name(MEDIUM_PLUS_RID), "medium_plus");
        assert_eq!(integrity_name(HIGH_IL_RID), "high");
        assert_eq!(integrity_name(0x4000), "system");
        assert_eq!(integrity_name(0x1234), "other");
    }

    #[test]
    fn high_il_hook_and_medium_plus_not_high() {
        set_high_il_hook(Some(|_| false));
        assert!(!window_is_high_il(0x10));
        set_high_il_hook(Some(|hwnd| hwnd == 0x22));
        assert!(window_is_high_il(0x22));
        assert!(!window_is_high_il(0x21));
        set_high_il_hook(None);
        const { assert!(HIGH_IL_RID > MEDIUM_PLUS_RID) };
        const { assert!(MEDIUM_PLUS_RID < HIGH_IL_RID) };
    }

    #[test]
    fn status_json_omits_input_and_is_hookable() {
        set_status_hook(Some(|| ElevationStatus {
            exe_path: r"C:\dev\Helping-Hands\hands\target\debug\hands.exe".into(),
            integrity: "medium".into(),
            integrity_rid: 0x2000,
            uiaccess_granted: false,
            secure_location: false,
        }));
        let json = serialize_status(&status().expect("status")).expect("json");
        assert!(json.contains("\"uiaccess_granted\":false"));
        assert!(json.contains("\"secure_location\":false"));
        assert!(!json.contains("SendInput"));
        set_status_hook(Some(|| ElevationStatus {
            exe_path: r"C:\Program Files\HelpingHands\hands.exe".into(),
            integrity: "medium_plus".into(),
            integrity_rid: MEDIUM_PLUS_RID,
            uiaccess_granted: true,
            secure_location: true,
        }));
        let installed = serialize_status(&status().expect("status")).expect("json");
        assert!(installed.contains("\"uiaccess_granted\":true"));
        assert!(installed.contains("\"secure_location\":true"));
        set_status_hook(None);
    }

    #[test]
    fn elevation_rs_has_no_sendinput() {
        let src = include_str!("elevation.rs");
        let needle = ["Send", "Input"].concat();
        let prod = src.split("mod tests").next().unwrap_or(src);
        assert!(
            !prod.contains(&needle),
            "elevation.rs must not {needle}:\n{prod}"
        );
    }

    #[test]
    fn access_denied_is_high_il_and_manifest_source_lock() {
        let src = include_str!("elevation.rs");
        assert!(src.contains("is_access_denied"));
        assert!(src.contains("ProbeErr::Denied"));
        assert!(src.contains("PROCESS_QUERY_LIMITED_INFORMATION"));
        let build = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/build.rs"));
        assert!(build.contains("ui_access(cfg!(feature = \"uiaccess\"))"));
        assert!(build.contains("AsInvoker"));
        let cargo = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
        assert!(cargo.contains("uiaccess = []"));
        assert!(!cargo.contains("default = [\"uiaccess\"]"));
        assert!(cargo.contains("embed-manifest = \"1.5.1\""));
        let hid = include_str!("hid.rs");
        let input = include_str!("input.rs");
        assert!(input.contains("fn send_os_inputs"));
        assert!(input.contains("SendInput"));
        assert!(include_str!("../AGENTS.md").contains("LLMHF_INJECTED"));
        assert!(!hid.contains("SendInput"));
    }

    #[test]
    fn program_files_is_secure_system32_is_secure_target_is_not() {
        set_secure_hook(None);
        let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
        assert!(path_is_secure(
            &PathBuf::from(pf).join("HelpingHands").join("hands.exe")
        ));
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        assert!(path_is_secure(
            &PathBuf::from(root).join("System32").join("hands.exe")
        ));
        assert!(!path_is_secure(Path::new(
            r"C:\dev\Helping-Hands\hands\target\debug\hands.exe"
        )));
    }
}
