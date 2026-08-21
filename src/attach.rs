//! Attach to daily Chrome, or launch it with no automation flags.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HWND, LPARAM, RECT};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, REG_EXPAND_SZ, REG_SZ,
    REG_VALUE_TYPE, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
};
use windows::Win32::System::Threading::{
    CreateProcessW, OpenProcess, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW, STARTUPINFOW, WaitForInputIdle,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
};
use windows::core::{PCWSTR, PWSTR};

use crate::error::HandsError;
use crate::foreground;
use crate::logs;
use crate::session::resolve_session_id_from_os;

pub const ATTACH_SCHEMA: &str = "hands.attach/v1";
pub const CHROME_EXE_ENV: &str = "HANDS_CHROME_EXE";
pub const CHROME_CLASS: &str = "Chrome_WidgetWin_1";
const LAUNCH_URL: &str = "about:blank";
const APP_PATHS_SUBKEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\chrome.exe";
const HWND_POLL: Duration = Duration::from_secs(5);
const INPUT_IDLE_MS: u32 = 5_000;
const PEEK_SLEEP: Duration = Duration::from_millis(50);

#[derive(Debug, Clone)]
pub struct ChromeWindow {
    pub hwnd: isize,
    pub pid: u32,
    pub exe: PathBuf,
}

#[derive(Debug, Clone)]
pub struct WindowCandidate {
    pub hwnd: isize,
    pub class: String,
    pub visible: bool,
    pub iconic: bool,
    pub pid: u32,
    pub exe: PathBuf,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttachEnvelope {
    pub schema: &'static str,
    pub session_id: String,
    pub ok: bool,
    pub attached: bool,
    /// True iff this invocation's spawn hook / `CreateProcessW` returned `Ok`.
    pub launched: bool,
    pub plan: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hwnd: Option<isize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exe: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argv: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
struct AttachOutcome {
    attached: bool,
    launched: bool,
    hwnd: Option<isize>,
    pid: Option<u32>,
    exe: Option<PathBuf>,
    argv: Option<Vec<OsString>>,
    error: Option<String>,
}

struct Hooks<'a> {
    find: &'a dyn Fn() -> Option<ChromeWindow>,
    resolve_exe: &'a dyn Fn() -> Result<PathBuf, HandsError>,
    spawn: &'a dyn Fn(&Path) -> Result<u32, HandsError>,
    offer: &'a dyn Fn(isize, (i32, i32)) -> bool,
}

impl Hooks<'static> {
    fn live() -> Self {
        Self {
            find: &find_chrome_window,
            resolve_exe: &resolve_chrome_exe,
            spawn: &spawn_chrome,
            offer: &offer_hwnd,
        }
    }
}

pub fn pick_window(cands: impl IntoIterator<Item = WindowCandidate>) -> Option<ChromeWindow> {
    cands.into_iter().find_map(|c| {
        if c.class != CHROME_CLASS {
            return None;
        }
        if !(c.visible || c.iconic) {
            return None;
        }
        if !c.iconic && (c.width <= 0 || c.height <= 0) {
            return None;
        }
        if !is_chrome_image(&c.exe) {
            return None;
        }
        Some(ChromeWindow {
            hwnd: c.hwnd,
            pid: c.pid,
            exe: c.exe,
        })
    })
}

pub fn launch_argv(exe: &Path) -> Result<Vec<OsString>, HandsError> {
    checked_argv(vec![
        exe.as_os_str().to_os_string(),
        OsString::from(LAUNCH_URL),
    ])
}

pub fn checked_argv(tokens: Vec<OsString>) -> Result<Vec<OsString>, HandsError> {
    if tokens.iter().any(|t| t.to_string_lossy().starts_with("--")) {
        return Err(HandsError::Chrome(
            "launch argv must not contain -- switches (about:blank only)".into(),
        ));
    }
    if tokens.len() != 2 || tokens[1] != OsStr::new(LAUNCH_URL) {
        return Err(HandsError::Chrome(
            "launch argv allowlist is [exe, about:blank]".into(),
        ));
    }
    Ok(tokens)
}

pub fn resolve_chrome_exe() -> Result<PathBuf, HandsError> {
    resolve_chrome_exe_inner(std::env::var_os(CHROME_EXE_ENV), read_app_paths, |p| {
        p.is_file()
    })
}

fn resolve_chrome_exe_inner(
    env: Option<OsString>,
    app_paths: impl Fn(Hive) -> Option<PathBuf>,
    is_file: impl Fn(&Path) -> bool,
) -> Result<PathBuf, HandsError> {
    if let Some(raw) = env.filter(|v| !v.is_empty()) {
        let path = PathBuf::from(raw);
        if is_file(&path) {
            return Ok(path);
        }
        return Err(HandsError::Chrome(format!(
            "{CHROME_EXE_ENV} is set but is not a file ({})",
            path.display()
        )));
    }
    for hive in [Hive::Hkcu, Hive::Hklm] {
        if let Some(path) = app_paths(hive)
            && is_file(&path)
        {
            return Ok(path);
        }
    }
    for path in filesystem_fallbacks() {
        if is_file(&path) {
            return Ok(path);
        }
    }
    Err(HandsError::Chrome(
        "chrome.exe not found (App Paths and standard install locations are empty)".into(),
    ))
}

fn filesystem_fallbacks() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        out.push(
            PathBuf::from(local)
                .join("Google")
                .join("Chrome")
                .join("Application")
                .join("chrome.exe"),
        );
    }
    if let Some(pf) = std::env::var_os("PROGRAMFILES") {
        out.push(
            PathBuf::from(pf)
                .join("Google")
                .join("Chrome")
                .join("Application")
                .join("chrome.exe"),
        );
    }
    if let Some(pfx86) = std::env::var_os("PROGRAMFILES(X86)") {
        out.push(
            PathBuf::from(pfx86)
                .join("Google")
                .join("Chrome")
                .join("Application")
                .join("chrome.exe"),
        );
    }
    out
}

#[derive(Clone, Copy)]
enum Hive {
    Hkcu,
    Hklm,
}

fn read_app_paths(hive: Hive) -> Option<PathBuf> {
    let root = match hive {
        Hive::Hkcu => HKEY_CURRENT_USER,
        Hive::Hklm => HKEY_LOCAL_MACHINE,
    };
    let sub = to_wide(APP_PATHS_SUBKEY);
    let mut key = HKEY::default();
    let status = unsafe {
        RegOpenKeyExW(
            root,
            PCWSTR(sub.as_ptr()),
            None,
            KEY_QUERY_VALUE,
            &raw mut key,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let mut kind = REG_VALUE_TYPE::default();
    let mut nbytes = 0u32;
    let _ = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR::null(),
            None,
            Some(&raw mut kind),
            None,
            Some(&raw mut nbytes),
        )
    };
    if nbytes == 0 {
        let _ = unsafe { RegCloseKey(key) };
        return None;
    }
    let mut buf = vec![0u8; nbytes as usize];
    let status = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR::null(),
            None,
            Some(&raw mut kind),
            Some(buf.as_mut_ptr()),
            Some(&raw mut nbytes),
        )
    };
    let _ = unsafe { RegCloseKey(key) };
    if status != ERROR_SUCCESS {
        return None;
    }
    let u16s: Vec<u16> = buf
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let raw = String::from_utf16_lossy(&u16s)
        .trim_end_matches('\0')
        .trim()
        .to_string();
    if raw.is_empty() {
        return None;
    }
    let expanded = if kind == REG_EXPAND_SZ || kind == REG_SZ {
        expand_env_sz(&raw)
    } else {
        raw
    };
    let path = PathBuf::from(expanded);
    if path.as_os_str().is_empty() {
        None
    } else {
        Some(path)
    }
}

fn expand_env_sz(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(end) = after.find('%') {
            let name = &after[..end];
            if name.is_empty() {
                out.push('%');
                rest = &after[end + 1..];
                continue;
            }
            match std::env::var(name) {
                Ok(v) => out.push_str(&v),
                Err(_) => {
                    out.push('%');
                    out.push_str(name);
                    out.push('%');
                }
            }
            rest = &after[end + 1..];
        } else {
            out.push_str(&rest[start..]);
            return out;
        }
    }
    out.push_str(rest);
    out
}

pub fn spawn_chrome(exe: &Path) -> Result<u32, HandsError> {
    let argv = launch_argv(exe)?;
    let url = argv[1].to_string_lossy();
    let exe_wide = to_wide(&exe.to_string_lossy());
    let mut cmd = to_wide(&format!("\"{}\" {url}", exe.display()));
    let si = STARTUPINFOW {
        cb: u32::try_from(std::mem::size_of::<STARTUPINFOW>()).unwrap_or(0),
        ..Default::default()
    };
    let mut pi = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(
            PCWSTR(exe_wide.as_ptr()),
            Some(PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            false,
            PROCESS_CREATION_FLAGS(0),
            None,
            PCWSTR::null(),
            &raw const si,
            &raw mut pi,
        )
    }
    .map_err(|err| HandsError::Chrome(format!("CreateProcessW chrome.exe: {err}")))?;
    let pid = pi.dwProcessId;
    let _ = unsafe { WaitForInputIdle(pi.hProcess, INPUT_IDLE_MS) };
    let _ = unsafe { CloseHandle(pi.hProcess) };
    let _ = unsafe { CloseHandle(pi.hThread) };
    Ok(pid)
}

pub fn run_attach(session_id: Option<&str>, plan: bool) -> Result<AttachEnvelope, HandsError> {
    let session_id = resolve_session_id_from_os(session_id);
    logs::check_write_id(&session_id)?;
    let outcome = ensure_daily_with(plan, &Hooks::live());
    let envelope = envelope_from(session_id, plan, outcome);
    logs::ensure_installed();
    logs::remember_session(&envelope.session_id);
    let _ = logs::record_actuate(
        &envelope.session_id,
        "attach",
        envelope.ok,
        envelope.error.as_deref(),
        None,
        None,
        None,
        None,
    );
    Ok(envelope)
}

pub fn serialize_attach(envelope: &AttachEnvelope) -> Result<String, HandsError> {
    serde_json::to_string(envelope)
        .map_err(|err| HandsError::Chrome(format!("attach envelope: {err}")))
}

fn envelope_from(session_id: String, plan: bool, outcome: AttachOutcome) -> AttachEnvelope {
    AttachEnvelope {
        schema: ATTACH_SCHEMA,
        session_id,
        ok: outcome.error.is_none(),
        attached: outcome.attached,
        launched: outcome.launched,
        plan,
        hwnd: outcome.hwnd,
        pid: outcome.pid,
        exe: outcome.exe.map(|p| p.display().to_string()),
        argv: outcome.argv.map(|a| {
            a.into_iter()
                .map(|t| t.to_string_lossy().into_owned())
                .collect()
        }),
        error: outcome.error,
    }
}

fn ensure_daily_with(plan: bool, hooks: &Hooks<'_>) -> AttachOutcome {
    let found = (hooks.find)();
    let need_exe = plan || found.is_none();
    let resolved = if need_exe {
        match (hooks.resolve_exe)() {
            Ok(exe) => match launch_argv(&exe) {
                Ok(argv) => Ok((exe, argv)),
                Err(err) => Err(err),
            },
            Err(err) => Err(err),
        }
    } else {
        Err(HandsError::Chrome("exe not requested".into()))
    };

    if let Some(win) = found {
        let center = window_center(win.hwnd);
        let _ = (hooks.offer)(win.hwnd, center);
        let (exe, argv, error) = match &resolved {
            Ok((exe, argv)) if plan => (Some(exe.clone()), Some(argv.clone()), None),
            Err(err) if plan => (None, None, Some(err.tool_message())),
            _ => (None, None, None),
        };
        return AttachOutcome {
            attached: true,
            launched: false,
            hwnd: Some(win.hwnd),
            pid: Some(win.pid),
            exe,
            argv,
            error,
        };
    }

    if plan {
        return match resolved {
            Ok((exe, argv)) => AttachOutcome {
                attached: false,
                launched: false,
                hwnd: None,
                pid: None,
                exe: Some(exe),
                argv: Some(argv),
                error: None,
            },
            Err(err) => AttachOutcome {
                attached: false,
                launched: false,
                hwnd: None,
                pid: None,
                exe: None,
                argv: None,
                error: Some(err.tool_message()),
            },
        };
    }

    let (exe, argv) = match resolved {
        Ok(pair) => pair,
        Err(err) => {
            return AttachOutcome {
                attached: false,
                launched: false,
                hwnd: None,
                pid: None,
                exe: None,
                argv: None,
                error: Some(err.tool_message()),
            };
        }
    };
    match (hooks.spawn)(&exe) {
        Ok(_) => {
            let win = poll_hwnd(hooks, HWND_POLL);
            AttachOutcome {
                attached: win.is_some(),
                launched: true,
                hwnd: win.as_ref().map(|w| w.hwnd),
                pid: win.as_ref().map(|w| w.pid),
                exe: Some(exe),
                argv: Some(argv),
                error: None,
            }
        }
        Err(err) => AttachOutcome {
            attached: false,
            launched: false,
            hwnd: None,
            pid: None,
            exe: Some(exe),
            argv: Some(argv),
            error: Some(err.tool_message()),
        },
    }
}

fn poll_hwnd(hooks: &Hooks<'_>, budget: Duration) -> Option<ChromeWindow> {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(win) = (hooks.find)() {
            return Some(win);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(PEEK_SLEEP.min(deadline.saturating_duration_since(Instant::now())));
    }
}

fn find_chrome_window() -> Option<ChromeWindow> {
    pick_window(enum_candidates())
}

fn enum_candidates() -> Vec<WindowCandidate> {
    let mut hwnds: Vec<HWND> = Vec::new();
    let _ = unsafe { EnumWindows(Some(collect_hwnds), LPARAM(&raw mut hwnds as isize)) };
    let mut out = Vec::new();
    for hwnd in hwnds {
        let Some(raw) = foreground::hwnd_raw(hwnd) else {
            continue;
        };
        let class = class_name(hwnd);
        let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
        let iconic = unsafe { IsIconic(hwnd) }.as_bool();
        let mut pid = 0u32;
        let _ = unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut pid)) };
        let Some(exe) = process_image(pid) else {
            continue;
        };
        let (width, height) = window_size(hwnd);
        out.push(WindowCandidate {
            hwnd: raw,
            class,
            visible,
            iconic,
            pid,
            exe,
            width,
            height,
        });
    }
    out
}

unsafe extern "system" fn collect_hwnds(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
    let list = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
    list.push(hwnd);
    true.into()
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

fn process_image(pid: u32) -> Option<PathBuf> {
    if pid == 0 {
        return None;
    }
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut n = 32768u32;
    let mut buf = vec![0u16; n as usize];
    let ok = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &raw mut n,
        )
    };
    let _ = unsafe { CloseHandle(handle) };
    ok.ok()?;
    let len = n as usize;
    if len == 0 || len > buf.len() {
        return None;
    }
    Some(PathBuf::from(OsString::from_wide_lossy(&buf[..len])))
}

fn is_chrome_image(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case("chrome.exe"))
}

fn offer_hwnd(hwnd: isize, center: (i32, i32)) -> bool {
    foreground::offer(Some(hwnd), center)
}

fn window_size(hwnd: HWND) -> (i32, i32) {
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &raw mut rect) }.is_err() {
        return (0, 0);
    }
    (
        rect.right.saturating_sub(rect.left),
        rect.bottom.saturating_sub(rect.top),
    )
}

fn window_center(hwnd: isize) -> (i32, i32) {
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(foreground::raw_hwnd(hwnd), &raw mut rect) }.is_err() {
        return (0, 0);
    }
    (
        rect.left.saturating_add(rect.right) / 2,
        rect.top.saturating_add(rect.bottom) / 2,
    )
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

trait FromWideLossy {
    fn from_wide_lossy(wide: &[u16]) -> OsString;
}

impl FromWideLossy for OsString {
    fn from_wide_lossy(wide: &[u16]) -> OsString {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(wide)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn chrome_cand(
        hwnd: isize,
        class: &str,
        exe: &str,
        visible: bool,
        iconic: bool,
    ) -> WindowCandidate {
        WindowCandidate {
            hwnd,
            class: class.into(),
            visible,
            iconic,
            pid: 42,
            exe: PathBuf::from(exe),
            width: 800,
            height: 600,
        }
    }

    #[test]
    fn picker_accepts_chrome_widget_win_1() {
        let win = pick_window([chrome_cand(
            11,
            CHROME_CLASS,
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            true,
            false,
        )])
        .expect("chrome");
        assert_eq!(win.hwnd, 11);
        assert_eq!(win.pid, 42);
    }

    #[test]
    fn picker_accepts_minimized() {
        let win = pick_window([chrome_cand(12, CHROME_CLASS, r"C:\chrome.exe", false, true)]);
        assert!(win.is_some());
        let mut zero = chrome_cand(13, CHROME_CLASS, r"C:\chrome.exe", false, true);
        zero.width = 0;
        zero.height = 0;
        assert!(pick_window([zero]).is_some(), "minimized zero-area is open");
    }

    #[test]
    fn picker_rejects_zero_area_visible_helper() {
        let mut helper = chrome_cand(
            14,
            CHROME_CLASS,
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            true,
            false,
        );
        helper.width = 0;
        helper.height = 0;
        assert!(pick_window([helper]).is_none());
    }

    #[test]
    fn picker_rejects_edge_and_widget_win_0() {
        assert!(
            pick_window([chrome_cand(
                1,
                CHROME_CLASS,
                r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
                true,
                false,
            )])
            .is_none()
        );
        assert!(
            pick_window([chrome_cand(
                2,
                "Chrome_WidgetWin_0",
                r"C:\Program Files\Google\Chrome\Application\chrome.exe",
                true,
                false,
            )])
            .is_none()
        );
        assert!(
            pick_window([chrome_cand(
                3,
                CHROME_CLASS,
                r"C:\Program Files\Google\Chrome\Application\chrome.exe",
                false,
                false,
            )])
            .is_none()
        );
    }

    #[test]
    fn launch_argv_is_about_blank_only() {
        let exe = Path::new(r"C:\Program Files\Google\Chrome\Application\chrome.exe");
        let argv = launch_argv(exe).unwrap();
        assert_eq!(argv.len(), 2);
        assert_eq!(argv[1], OsStr::new("about:blank"));
        assert!(!argv.iter().any(|t| t.to_string_lossy().starts_with("--")));
    }

    #[test]
    fn launch_argv_rejects_dash_tokens() {
        let err = checked_argv(vec![
            OsString::from(r"C:\chrome.exe"),
            OsString::from("--enable-automation"),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("--"), "{err}");
        let err = checked_argv(vec![
            OsString::from(r"C:\chrome.exe"),
            OsString::from("about:blank"),
            OsString::from("--remote-debugging-port=9222"),
        ])
        .unwrap_err();
        assert!(
            err.to_string().contains("allowlist") || err.to_string().contains("--"),
            "{err}"
        );
    }

    #[test]
    fn hands_chrome_exe_set_existing_wins() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let path = PathBuf::from(r"C:\forced\chrome.exe");
        let got = resolve_chrome_exe_inner(
            Some(path.clone().into_os_string()),
            |_| Some(PathBuf::from(r"C:\app-paths\chrome.exe")),
            |p| p == path,
        )
        .unwrap();
        assert_eq!(got, path);
    }

    #[test]
    fn hands_chrome_exe_set_missing_is_hard_error() {
        let err = resolve_chrome_exe_inner(
            Some(OsString::from(r"C:\missing\chrome.exe")),
            |_| Some(PathBuf::from(r"C:\app-paths\chrome.exe")),
            |_| false,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(CHROME_EXE_ENV), "{msg}");
        assert!(!msg.to_ascii_lowercase().contains("app paths"), "{msg}");
    }

    #[test]
    fn unset_env_uses_app_paths_then_files() {
        let hkcu = PathBuf::from(r"C:\hkcu\chrome.exe");
        let got = resolve_chrome_exe_inner(
            None,
            |hive| match hive {
                Hive::Hkcu => Some(hkcu.clone()),
                Hive::Hklm => Some(PathBuf::from(r"C:\hklm\chrome.exe")),
            },
            |p| p == hkcu,
        )
        .unwrap();
        assert_eq!(got, hkcu);

        let hklm = PathBuf::from(r"C:\hklm\chrome.exe");
        let got = resolve_chrome_exe_inner(
            None,
            |hive| match hive {
                Hive::Hkcu => Some(PathBuf::from(r"C:\hkcu-missing\chrome.exe")),
                Hive::Hklm => Some(hklm.clone()),
            },
            |p| p == hklm,
        )
        .unwrap();
        assert_eq!(got, hklm);

        let err = resolve_chrome_exe_inner(Some(OsString::new()), |_| None, |_| false).unwrap_err();
        assert!(!err.to_string().contains(CHROME_EXE_ENV), "{}", err);
    }

    fn dummy_exe() -> PathBuf {
        PathBuf::from(r"C:\Program Files\Google\Chrome\Application\chrome.exe")
    }

    fn dummy_win() -> ChromeWindow {
        ChromeWindow {
            hwnd: 99,
            pid: 7,
            exe: dummy_exe(),
        }
    }

    #[test]
    fn plan_with_window_does_not_spawn() {
        let spawned = AtomicU32::new(0);
        let offered = AtomicU32::new(0);
        let hooks = Hooks {
            find: &|| Some(dummy_win()),
            resolve_exe: &|| Ok(dummy_exe()),
            spawn: &|_| {
                spawned.fetch_add(1, Ordering::SeqCst);
                Ok(1)
            },
            offer: &|hwnd, _| {
                offered.store(hwnd as u32, Ordering::SeqCst);
                true
            },
        };
        let out = ensure_daily_with(true, &hooks);
        assert!(out.attached);
        assert!(!out.launched);
        assert_eq!(out.hwnd, Some(99));
        assert_eq!(spawned.load(Ordering::SeqCst), 0);
        assert_eq!(offered.load(Ordering::SeqCst), 99);
        assert!(out.exe.is_some());
        assert_eq!(
            out.argv
                .as_ref()
                .map(|a| a[1].to_string_lossy().into_owned()),
            Some("about:blank".into())
        );
    }

    #[test]
    fn attach_existing_does_not_spawn() {
        let spawned = AtomicU32::new(0);
        let hooks = Hooks {
            find: &|| Some(dummy_win()),
            resolve_exe: &|| Ok(dummy_exe()),
            spawn: &|_| {
                spawned.fetch_add(1, Ordering::SeqCst);
                Ok(1)
            },
            offer: &|_, _| true,
        };
        let out = ensure_daily_with(false, &hooks);
        assert!(out.attached);
        assert!(!out.launched);
        assert_eq!(spawned.load(Ordering::SeqCst), 0);
        assert!(out.exe.is_none());
        assert!(out.argv.is_none());
    }

    #[test]
    fn launch_without_window_calls_spawn_once() {
        let spawned = AtomicU32::new(0);
        let finds = AtomicU32::new(0);
        let hooks = Hooks {
            find: &|| {
                if finds.fetch_add(1, Ordering::SeqCst) == 0 {
                    None
                } else {
                    Some(dummy_win())
                }
            },
            resolve_exe: &|| Ok(dummy_exe()),
            spawn: &|_| {
                spawned.fetch_add(1, Ordering::SeqCst);
                Ok(77)
            },
            offer: &|_, _| true,
        };
        let out = ensure_daily_with(false, &hooks);
        assert_eq!(spawned.load(Ordering::SeqCst), 1);
        assert!(out.launched);
        assert!(out.attached);
        assert!(out.error.is_none());
        assert!(out.exe.is_some());
        assert_eq!(
            out.argv
                .as_ref()
                .map(|a| a[1].to_string_lossy().into_owned()),
            Some("about:blank".into())
        );
    }

    #[test]
    fn spawn_err_reports_launched_false() {
        let spawned = AtomicU32::new(0);
        let hooks = Hooks {
            find: &|| None,
            resolve_exe: &|| Ok(dummy_exe()),
            spawn: &|_| {
                spawned.fetch_add(1, Ordering::SeqCst);
                Err(HandsError::Chrome(
                    "CreateProcessW chrome.exe: access denied".into(),
                ))
            },
            offer: &|_, _| true,
        };
        let out = ensure_daily_with(false, &hooks);
        assert_eq!(spawned.load(Ordering::SeqCst), 1);
        assert!(!out.launched);
        assert!(!out.attached);
        assert!(out.hwnd.is_none());
        assert!(out.pid.is_none());
        assert!(
            out.error
                .as_deref()
                .is_some_and(|e| e.contains("CreateProcessW")),
            "{:?}",
            out.error
        );
        assert!(out.exe.is_some());
        let argv = out.argv.as_ref().expect("argv");
        assert_eq!(argv[1], OsStr::new("about:blank"));
        assert!(!argv.iter().any(|t| t.to_string_lossy().starts_with("--")));
        let env = envelope_from("sid".into(), false, out);
        assert!(!env.ok);
        assert!(!env.launched);
        assert!(!env.attached);
        assert_eq!(env.schema, ATTACH_SCHEMA);
    }

    #[test]
    fn spawn_ok_arm_launched_is_literal_true() {
        let src = include_str!("attach.rs");
        let production = src.split("#[cfg(test)]").next().expect("production prefix");
        assert!(
            production.contains("launched: true"),
            "spawn Ok arm must set launched: true as a literal"
        );
        let forbidden = concat!("launched: win", ".is_some()");
        assert!(
            !production.contains(forbidden),
            "spawn Ok arm must not set launched from win.is_some()"
        );
    }

    #[test]
    fn plan_with_window_reports_resolve_error() {
        let spawned = AtomicU32::new(0);
        let hooks = Hooks {
            find: &|| Some(dummy_win()),
            resolve_exe: &|| {
                Err(HandsError::Chrome(format!(
                    "{CHROME_EXE_ENV} is set but is not a file (C:\\missing\\chrome.exe)"
                )))
            },
            spawn: &|_| {
                spawned.fetch_add(1, Ordering::SeqCst);
                Ok(1)
            },
            offer: &|_, _| true,
        };
        let out = ensure_daily_with(true, &hooks);
        assert!(out.attached);
        assert!(!out.launched);
        assert_eq!(spawned.load(Ordering::SeqCst), 0);
        assert!(
            out.error
                .as_deref()
                .is_some_and(|e| e.contains(CHROME_EXE_ENV))
        );
        assert!(out.exe.is_none());
        assert!(out.argv.is_none());
        let env = envelope_from("sid".into(), true, out);
        assert!(!env.ok);
    }

    #[test]
    fn plan_without_window_does_not_spawn() {
        let spawned = AtomicU32::new(0);
        let hooks = Hooks {
            find: &|| None,
            resolve_exe: &|| Ok(dummy_exe()),
            spawn: &|_| {
                spawned.fetch_add(1, Ordering::SeqCst);
                Ok(1)
            },
            offer: &|_, _| true,
        };
        let out = ensure_daily_with(true, &hooks);
        assert!(!out.attached);
        assert!(!out.launched);
        assert_eq!(spawned.load(Ordering::SeqCst), 0);
        assert!(out.exe.is_some());
        assert!(out.argv.is_some());
        assert!(out.error.is_none());
    }

    #[test]
    fn attach_source_forbids_kill() {
        let src = include_str!("attach.rs");
        let kill = concat!("Terminate", "Process");
        let task = concat!("task", "kill");
        assert!(!src.contains(kill), "attach.rs must not {kill}");
        assert!(
            !src.to_ascii_lowercase().contains(task),
            "attach.rs must not {task}"
        );
    }

    #[test]
    fn observe_does_not_call_attach() {
        let src = include_str!("observe.rs");
        assert!(!src.contains("attach::"));
        assert!(!src.contains("crate::attach"));
        assert!(!src.contains("ensure_daily"));
        assert!(!src.contains("spawn_chrome"));
        assert!(!src.contains("HANDS_CHROME_EXE"));
    }

    #[test]
    fn envelope_schema_and_plan_fields() {
        let env = envelope_from(
            "sid".into(),
            true,
            AttachOutcome {
                attached: false,
                launched: false,
                hwnd: None,
                pid: None,
                exe: Some(dummy_exe()),
                argv: Some(launch_argv(&dummy_exe()).unwrap()),
                error: None,
            },
        );
        let json = serialize_attach(&env).unwrap();
        assert!(json.contains(ATTACH_SCHEMA));
        assert!(json.contains("about:blank"));
        assert!(!json.contains("hwnd"));
    }
}
