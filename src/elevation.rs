//! Process UIAccess / integrity self-report and High-IL window probe.
//!
//! Read-only: no input injection. Does not install the desk lease.

use std::path::{Path, PathBuf};

use crate::desk_pipe::{DeskRole, get_role};
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
pub const UNGRANTED_MSG: &str = "elevated window is above this process integrity; the privileged server is hands serve --pipe \\\\.\\pipe\\hands-elev (task HandsElevated). UIAccess is not the path on this machine.";

const E_ACCESSDENIED: HRESULT = HRESULT(0x8007_0005u32 as i32);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ElevationStatus {
    pub exe_path: String,
    pub integrity: String,
    pub integrity_rid: u32,
    pub uiaccess_granted: bool,
    pub secure_location: bool,
    pub role: String,
    pub privileged: bool,
    pub pipe: Option<String>,
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

    // Get role and pipe from the process-global slot
    let (role, pipe) = get_role();
    let role_str = match role {
        DeskRole::Cli => "cli",
        DeskRole::StdioDesk => "stdio-desk",
        DeskRole::PipeDesk => "pipe-desk",
    };
    let privileged = integrity_rid >= HIGH_IL_RID;

    Ok(ElevationStatus {
        exe_path: exe_path.clone(),
        integrity: integrity_name(integrity_rid).into(),
        integrity_rid,
        uiaccess_granted: process_uiaccess()?,
        secure_location: path_is_secure(Path::new(&exe_path)),
        role: role_str.into(),
        privileged,
        pipe,
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
    #[cfg(test)]
    if let Some(hook) = caller_rid_hook() {
        return Ok(hook());
    }
    #[cfg(test)]
    if let Some(hook) = integrity_rid_hook() {
        return hook();
    }
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
    #[cfg(test)]
    if let Some(hook) = window_rid_hook() {
        return match hook(hwnd) {
            Ok(rid) => rid >= HIGH_IL_RID,
            Err(ProbeErr::Denied) => true,
            Err(ProbeErr::Other) => false,
        };
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

/// High-IL reach decision for fence/activate.
/// Returns Allow (same-or-higher IL), ConfirmElevated (target higher and UIAccess granted), or Refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighIlReach {
    Allow,
    ConfirmElevated,
    Refuse,
}

impl HighIlReach {
    pub fn is_allow_or_confirm(&self) -> bool {
        matches!(self, HighIlReach::Allow | HighIlReach::ConfirmElevated)
    }
}

/// Determine if the caller can reach the target window based on integrity levels.
/// - Allow: caller RID >= target RID (same or higher IL)
/// - ConfirmElevated: target higher than caller OR probe denied, AND UIAccess granted
/// - Refuse: otherwise (target higher/denied without UIAccess)
pub fn high_il_reach(target_hwnd: Option<isize>) -> Result<HighIlReach, HandsError> {
    let Some(hwnd) = target_hwnd else {
        return Ok(HighIlReach::Allow);
    };

    let caller_rid = process_integrity_rid()?;

    // Probe target RID
    let target_rid_result = window_integrity_rid(foreground::raw_hwnd(hwnd));

    match target_rid_result {
        Ok(target_rid) => {
            if target_rid < HIGH_IL_RID || caller_rid >= target_rid {
                Ok(HighIlReach::Allow)
            } else if process_uiaccess()? {
                Ok(HighIlReach::ConfirmElevated)
            } else {
                Ok(HighIlReach::Refuse)
            }
        }
        Err(ProbeErr::Denied) => {
            // Access denied probing target - fail closed unless UIAccess granted
            if process_uiaccess()? {
                Ok(HighIlReach::ConfirmElevated)
            } else {
                Ok(HighIlReach::Refuse)
            }
        }
        Err(ProbeErr::Other) => Ok(HighIlReach::Refuse),
    }
}

pub(crate) enum ProbeErr {
    Denied,
    Other,
}

fn window_integrity_rid(hwnd: HWND) -> Result<u32, ProbeErr> {
    #[cfg(test)]
    if let Some(hook) = window_rid_hook() {
        return hook(hwnd.0 as isize);
    }
    #[cfg(test)]
    if let Some(hook) = high_il_hook() {
        return Ok(if hook(hwnd.0 as isize) {
            HIGH_IL_RID
        } else {
            0x2000
        });
    }
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

/// Integrity RID of another process (read-only token query). The bridge uses this to check
/// that the server end of the desk pipe really is the elevated server.
pub fn pid_integrity_rid(pid: u32) -> Result<u32, HandsError> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .map_err(|err| HandsError::Observe(format!("OpenProcess({pid}): {err}")))?;
    let mut token = HANDLE::default();
    let opened = unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) };
    let rid = match opened {
        Ok(()) => {
            let rid = token_integrity_rid(token);
            let _ = unsafe { CloseHandle(token) };
            rid
        }
        Err(err) => Err(HandsError::Observe(format!(
            "OpenProcessToken({pid}): {err}"
        ))),
    };
    let _ = unsafe { CloseHandle(process) };
    rid
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
type CallerRidHook = fn() -> u32;
#[cfg(test)]
type WindowRidHook = fn(isize) -> Result<u32, ProbeErr>;
#[cfg(test)]
type IntegrityRidHook = fn() -> Result<u32, HandsError>;

#[cfg(test)]
static CALLER_RID_HOOK: Mutex<Option<CallerRidHook>> = Mutex::new(None);
#[cfg(test)]
static WINDOW_RID_HOOK: Mutex<Option<WindowRidHook>> = Mutex::new(None);
#[cfg(test)]
static INTEGRITY_RID_HOOK: Mutex<Option<IntegrityRidHook>> = Mutex::new(None);

#[cfg(test)]
fn caller_rid_hook() -> Option<CallerRidHook> {
    *CALLER_RID_HOOK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
fn window_rid_hook() -> Option<WindowRidHook> {
    *WINDOW_RID_HOOK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
fn integrity_rid_hook() -> Option<IntegrityRidHook> {
    *INTEGRITY_RID_HOOK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
pub(crate) fn set_caller_rid_hook(hook: Option<CallerRidHook>) {
    *CALLER_RID_HOOK.lock().unwrap_or_else(|e| e.into_inner()) = hook;
}

#[cfg(test)]
pub(crate) fn set_window_rid_hook(hook: Option<WindowRidHook>) {
    *WINDOW_RID_HOOK.lock().unwrap_or_else(|e| e.into_inner()) = hook;
}

#[cfg(test)]
pub(crate) fn set_integrity_rid_hook(hook: Option<IntegrityRidHook>) {
    *INTEGRITY_RID_HOOK.lock().unwrap_or_else(|e| e.into_inner()) = hook;
}

#[cfg(test)]
pub(crate) fn set_status_hook(hook: Option<StatusHook>) {
    *STATUS_HOOK.lock().unwrap_or_else(|e| e.into_inner()) = hook;
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
pub(crate) fn set_secure_hook(hook: Option<SecureHook>) {
    *SECURE_HOOK.lock().unwrap_or_else(|e| e.into_inner()) = hook;
}

#[cfg(test)]
pub(crate) struct HookGuard;

#[cfg(test)]
impl Drop for HookGuard {
    fn drop(&mut self) {
        set_high_il_hook(None);
        set_uiaccess_hook(None);
        set_status_hook(None);
        set_secure_hook(None);
        set_caller_rid_hook(None);
        set_window_rid_hook(None);
        set_integrity_rid_hook(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_il_reach_integrity_and_denied_table() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _hooks = HookGuard;
        set_uiaccess_hook(Some(|| false));
        set_caller_rid_hook(Some(|| HIGH_IL_RID));
        set_window_rid_hook(Some(|_| Ok(HIGH_IL_RID)));
        assert_eq!(high_il_reach(Some(1)).unwrap(), HighIlReach::Allow);
        set_window_rid_hook(Some(|_| Ok(0x2000)));
        assert_eq!(high_il_reach(Some(1)).unwrap(), HighIlReach::Allow);
        set_window_rid_hook(Some(|_| Ok(0x4000)));
        assert_eq!(high_il_reach(Some(1)).unwrap(), HighIlReach::Refuse);
        set_window_rid_hook(Some(|_| Err(ProbeErr::Denied)));
        assert_eq!(high_il_reach(Some(1)).unwrap(), HighIlReach::Refuse);
        set_uiaccess_hook(Some(|| true));
        assert_eq!(
            high_il_reach(Some(1)).unwrap(),
            HighIlReach::ConfirmElevated
        );
        set_caller_rid_hook(Some(|| 0x2000));
        set_window_rid_hook(Some(|_| Ok(HIGH_IL_RID)));
        assert_eq!(
            high_il_reach(Some(1)).unwrap(),
            HighIlReach::ConfirmElevated
        );
        set_uiaccess_hook(Some(|| false));
        assert_eq!(high_il_reach(Some(1)).unwrap(), HighIlReach::Refuse);
        assert_eq!(high_il_reach(None).unwrap(), HighIlReach::Allow);
    }

    #[test]
    fn status_role_integrity_table_uses_real_status_path() {
        use crate::desk_pipe::{set_role, take_role_slot_for_test};
        for (role, pipe) in [
            (DeskRole::Cli, None),
            (DeskRole::StdioDesk, None),
            (
                DeskRole::PipeDesk,
                Some(crate::desk_pipe::PIPE_NAME.to_string()),
            ),
        ] {
            for high in [false, true] {
                let _slot = take_role_slot_for_test();
                let _hooks = HookGuard;
                set_integrity_rid_hook(Some(if high {
                    || Ok(HIGH_IL_RID)
                } else {
                    || Ok(0x2000)
                }));
                set_uiaccess_hook(Some(|| false));
                set_role(role, pipe.clone());
                let report = status().unwrap();
                assert_eq!(report.privileged, high);
                assert_eq!(report.pipe, pipe);
                assert_eq!(
                    report.role,
                    match role {
                        DeskRole::Cli => "cli",
                        DeskRole::StdioDesk => "stdio-desk",
                        DeskRole::PipeDesk => "pipe-desk",
                    }
                );
            }
        }
        let _slot = take_role_slot_for_test();
        let _hooks = HookGuard;
        set_integrity_rid_hook(Some(|| Ok(0x2000)));
        assert_eq!(status().unwrap().role, "cli");
        assert_eq!(status().unwrap().pipe, None);
    }

    #[test]
    fn high_il_callers_share_reach_helper() {
        let fence = include_str!("fence.rs").split("mod tests").next().unwrap();
        let actuate = include_str!("actuate.rs")
            .split("mod tests")
            .next()
            .unwrap();
        assert!(fence.contains("elevation::high_il_reach("));
        let activate = actuate
            .split("fn activate_with(")
            .nth(1)
            .unwrap()
            .split("fn classify_activate_reason")
            .next()
            .unwrap();
        assert!(activate.contains("elevation::high_il_reach("));
        assert!(!activate.contains("window_is_high_il"));
    }

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
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _hooks = HookGuard;
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
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _hooks = HookGuard;
        set_status_hook(Some(|| ElevationStatus {
            exe_path: r"C:\dev\Helping-Hands\hands\target\debug\hands.exe".into(),
            integrity: "medium".into(),
            integrity_rid: 0x2000,
            uiaccess_granted: false,
            secure_location: false,
            role: "cli".into(),
            privileged: false,
            pipe: None,
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
            role: "stdio-desk".into(),
            privileged: false,
            pipe: None,
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
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _hooks = HookGuard;
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
