//! Desk lease: physical input freezes injection; Pause/Break always halts.
//!
//! Low-level hooks run on a dedicated `hands-lease` thread that pumps messages.
//! Hook procs only store flags — no UIA, capture, or sleep.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Power::{
    ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_CANCEL, VK_PAUSE};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLMHF_INJECTED,
    MSLLHOOKSTRUCT, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_QUIT,
};

use crate::error::HandsError;

pub const IDLE_REARM: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseState {
    Armed,
    Frozen,
}

/// Pure state machine — unit-tested without installing hooks.
#[derive(Debug, Clone)]
pub struct LeaseMachine {
    frozen: bool,
    since: Option<Instant>,
}

impl LeaseMachine {
    pub fn new() -> Self {
        Self {
            frozen: false,
            since: None,
        }
    }

    pub fn rearm_if_idle(&mut self, now: Instant) {
        if self.frozen
            && let Some(since) = self.since
            && now.duration_since(since) >= IDLE_REARM
        {
            self.frozen = false;
            self.since = None;
        }
    }

    pub fn is_frozen(&mut self, now: Instant) -> bool {
        self.rearm_if_idle(now);
        self.frozen
    }

    pub fn freeze_now(&mut self, now: Instant) {
        self.frozen = true;
        self.since = Some(now);
    }

    /// Mouse: freeze only when not injected.
    pub fn on_mouse(&mut self, injected: bool, now: Instant) {
        if injected {
            return;
        }
        self.freeze_now(now);
    }

    /// Key: freeze when not injected, or always for Pause/Break.
    pub fn on_key(&mut self, vk: u32, injected: bool, now: Instant) {
        let pause = vk == u32::from(VK_PAUSE.0) || vk == u32::from(VK_CANCEL.0);
        if pause || !injected {
            self.freeze_now(now);
        }
    }
}

impl Default for LeaseMachine {
    fn default() -> Self {
        Self::new()
    }
}

type FreezeListener = Arc<dyn Fn() + Send + Sync>;

static FROZEN: AtomicBool = AtomicBool::new(false);
static LAST_FREEZE_MS: AtomicU64 = AtomicU64::new(0);
static PENDING_NOTIFY: AtomicBool = AtomicBool::new(false);
static CLOCK0: OnceLock<Instant> = OnceLock::new();
static LISTENERS: Mutex<Vec<FreezeListener>> = Mutex::new(Vec::new());

fn now_ms() -> u64 {
    CLOCK0
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn rearm_if_idle() {
    if !FROZEN.load(Ordering::SeqCst) {
        return;
    }
    let last = LAST_FREEZE_MS.load(Ordering::SeqCst);
    if now_ms().saturating_sub(last) >= IDLE_REARM.as_millis() as u64 {
        FROZEN.store(false, Ordering::SeqCst);
    }
}

/// Hook-safe: atomics only, never blocks, never drops Pause/physical.
fn record_freeze() {
    let was = FROZEN.swap(true, Ordering::SeqCst);
    LAST_FREEZE_MS.store(now_ms(), Ordering::SeqCst);
    if !was {
        PENDING_NOTIFY.store(true, Ordering::SeqCst);
    }
}

/// 0004 can subscribe to freeze events (Pause/Break / physical input).
/// This track does not clear session allows.
pub fn subscribe(cb: impl Fn() + Send + Sync + 'static) {
    LISTENERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Arc::new(cb));
}

pub fn is_frozen() -> bool {
    rearm_if_idle();
    FROZEN.load(Ordering::SeqCst)
}

pub fn freeze_now() {
    record_freeze();
    flush_notify();
}

pub fn poll() -> Result<(), HandsError> {
    rearm_if_idle();
    flush_notify();
    if FROZEN.load(Ordering::SeqCst) {
        Err(HandsError::Lease(
            "desk lease frozen (physical input or Pause/Break)".into(),
        ))
    } else {
        Ok(())
    }
}

fn flush_notify() {
    if !PENDING_NOTIFY.swap(false, Ordering::SeqCst) {
        return;
    }
    let listeners = LISTENERS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for cb in listeners {
        cb();
    }
}

fn note_mouse(injected: bool) {
    if !injected {
        record_freeze();
    }
}

fn note_key(vk: u32, injected: bool) {
    let pause = vk == u32::from(VK_PAUSE.0) || vk == u32::from(VK_CANCEL.0);
    if pause || !injected {
        record_freeze();
    }
}

pub struct LeaseGuard {
    thread_id: u32,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        for _ in 0..50 {
            if unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) }.is_ok()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Install LL hooks + keep-awake on a dedicated message-pump thread.
pub fn install() -> Result<LeaseGuard, HandsError> {
    let (tx, rx) = mpsc::channel();
    let join = std::thread::Builder::new()
        .name("hands-lease".into())
        .spawn(move || lease_thread(tx))
        .map_err(|err| HandsError::Lease(format!("spawn lease thread: {err}")))?;
    let thread_id = rx
        .recv()
        .map_err(|_| HandsError::Lease("lease thread exited before ready".into()))??;
    Ok(LeaseGuard {
        thread_id,
        join: Some(join),
    })
}

fn lease_thread(ready: mpsc::Sender<Result<u32, HandsError>>) {
    unsafe {
        SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED);
    }
    let mouse = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), None, 0) };
    let key = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_hook), None, 0) };
    match (mouse, key) {
        (Ok(mh), Ok(kh)) => {
            let tid = unsafe { GetCurrentThreadId() };
            // Create the thread message queue before we publish tid so Drop's
            // PostThreadMessageW(WM_QUIT) cannot race a missing queue.
            let mut primed = empty_msg();
            unsafe {
                let _ = PeekMessageW(&mut primed, None, 0, 0, PM_NOREMOVE);
            }
            let _ = ready.send(Ok(tid));
            loop {
                let mut msg = empty_msg();
                let status = unsafe { GetMessageW(&mut msg, None, 0, 0) };
                if status.0 == 0 || status.0 == -1 {
                    break;
                }
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            let _ = unsafe { UnhookWindowsHookEx(mh) };
            let _ = unsafe { UnhookWindowsHookEx(kh) };
        }
        (Err(err), Ok(kh)) => {
            let _ = unsafe { UnhookWindowsHookEx(kh) };
            let _ = ready.send(Err(HandsError::Lease(format!(
                "SetWindowsHookExW(WH_MOUSE_LL) failed: {err}"
            ))));
        }
        (Ok(mh), Err(err)) => {
            let _ = unsafe { UnhookWindowsHookEx(mh) };
            let _ = ready.send(Err(HandsError::Lease(format!(
                "SetWindowsHookExW(WH_KEYBOARD_LL) failed: {err}"
            ))));
        }
        (Err(mouse_err), Err(key_err)) => {
            let _ = ready.send(Err(HandsError::Lease(format!(
                "SetWindowsHookExW failed: mouse={mouse_err}; key={key_err}"
            ))));
        }
    }
    unsafe {
        SetThreadExecutionState(ES_CONTINUOUS);
    }
}

fn empty_msg() -> windows::Win32::UI::WindowsAndMessaging::MSG {
    windows::Win32::UI::WindowsAndMessaging::MSG {
        hwnd: windows::Win32::Foundation::HWND::default(),
        message: 0,
        wParam: WPARAM(0),
        lParam: LPARAM(0),
        time: 0,
        pt: windows::Win32::Foundation::POINT::default(),
    }
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && lparam.0 != 0 {
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        let injected = info.flags & LLMHF_INJECTED != 0;
        note_mouse(injected);
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn key_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && lparam.0 != 0 {
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let injected = info.flags.contains(LLKHF_INJECTED);
        note_key(info.vkCode, injected);
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injected_mouse_does_not_freeze() {
        let mut m = LeaseMachine::new();
        let t0 = Instant::now();
        m.on_mouse(true, t0);
        assert!(!m.is_frozen(t0));
    }

    #[test]
    fn physical_mouse_freezes_and_idle_rearms() {
        let mut m = LeaseMachine::new();
        let t0 = Instant::now();
        m.on_mouse(false, t0);
        assert!(m.is_frozen(t0));
        assert!(m.is_frozen(t0 + Duration::from_millis(1999)));
        assert!(!m.is_frozen(t0 + Duration::from_millis(2000)));
    }

    #[test]
    fn pause_freezes_even_when_injected() {
        let mut m = LeaseMachine::new();
        let t0 = Instant::now();
        m.on_key(u32::from(VK_PAUSE.0), true, t0);
        assert!(m.is_frozen(t0));
        let mut m = LeaseMachine::new();
        m.on_key(u32::from(VK_CANCEL.0), true, t0);
        assert!(m.is_frozen(t0));
    }

    #[test]
    fn injected_letter_does_not_freeze() {
        let mut m = LeaseMachine::new();
        let t0 = Instant::now();
        m.on_key(u32::from(b'V'), true, t0);
        assert!(!m.is_frozen(t0));
        m.on_key(u32::from(b'A'), false, t0);
        assert!(m.is_frozen(t0));
    }

    #[test]
    fn freeze_now_and_physical_reset_idle() {
        let mut m = LeaseMachine::new();
        let t0 = Instant::now();
        m.freeze_now(t0);
        m.on_mouse(false, t0 + Duration::from_millis(1500));
        assert!(m.is_frozen(t0 + Duration::from_millis(3000)));
        assert!(!m.is_frozen(t0 + Duration::from_millis(3500)));
    }
}
