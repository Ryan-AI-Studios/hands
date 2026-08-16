//! Desk lease: physical input freezes injection; Pause/Break always halts.
//!
//! Low-level hooks run on a dedicated `hands-lease` thread that pumps messages.
//! Hook procs only store flags — no UIA, capture, file I/O, or allow-store locks.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreezeCause {
    Physical,
    Pause,
    Stop,
}

const CAUSE_NONE: u8 = 0;
const CAUSE_PHYSICAL: u8 = 1;
const CAUSE_PAUSE: u8 = 2;
const CAUSE_STOP: u8 = 3;

fn cause_code(cause: FreezeCause) -> u8 {
    match cause {
        FreezeCause::Physical => CAUSE_PHYSICAL,
        FreezeCause::Pause => CAUSE_PAUSE,
        FreezeCause::Stop => CAUSE_STOP,
    }
}

fn cause_from_code(code: u8) -> FreezeCause {
    match code {
        CAUSE_PAUSE => FreezeCause::Pause,
        CAUSE_STOP => FreezeCause::Stop,
        _ => FreezeCause::Physical,
    }
}

/// Pure state machine — unit-tested without installing hooks.
#[derive(Debug, Clone)]
pub struct LeaseMachine {
    frozen: bool,
    since: Option<Instant>,
    last_cause: Option<FreezeCause>,
}

impl LeaseMachine {
    pub fn new() -> Self {
        Self {
            frozen: false,
            since: None,
            last_cause: None,
        }
    }

    pub fn last_cause(&self) -> Option<FreezeCause> {
        self.last_cause
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
        self.freeze_now_with(now, FreezeCause::Stop);
    }

    pub fn freeze_now_with(&mut self, now: Instant, cause: FreezeCause) {
        self.frozen = true;
        self.since = Some(now);
        self.last_cause = Some(cause);
    }

    /// Mouse: freeze only when not injected.
    pub fn on_mouse(&mut self, injected: bool, now: Instant) {
        if injected {
            return;
        }
        self.freeze_now_with(now, FreezeCause::Physical);
    }

    /// Key: freeze when not injected, or always for Pause/Break.
    pub fn on_key(&mut self, vk: u32, injected: bool, now: Instant) {
        let pause = vk == u32::from(VK_PAUSE.0) || vk == u32::from(VK_CANCEL.0);
        if pause {
            self.freeze_now_with(now, FreezeCause::Pause);
        } else if !injected {
            self.freeze_now_with(now, FreezeCause::Physical);
        }
    }
}

impl Default for LeaseMachine {
    fn default() -> Self {
        Self::new()
    }
}

type FreezeListener = Arc<dyn Fn(FreezeCause) + Send + Sync>;

static FROZEN: AtomicBool = AtomicBool::new(false);
static LAST_FREEZE_MS: AtomicU64 = AtomicU64::new(0);
/// Non-zero means a notify is pending. Pause/Stop values are never overwritten
/// by Physical. `take_pending_cause` swaps this back to `CAUSE_NONE`.
static LAST_CAUSE: AtomicU8 = AtomicU8::new(CAUSE_NONE);
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
    if now_ms().saturating_sub(last) < IDLE_REARM.as_millis() as u64 {
        return;
    }
    // A Pause/Stop that lands after we sampled `last` must win: only clear
    // if the freeze clock has not moved.
    if LAST_FREEZE_MS.load(Ordering::SeqCst) == last {
        FROZEN.store(false, Ordering::SeqCst);
    }
}

/// Hook-safe: atomics only. Pause/Stop always notify; Physical is transition-only.
/// Pending cause lives in `LAST_CAUSE` alone (non-zero = pending).
fn record_freeze(cause: FreezeCause) {
    let was = FROZEN.swap(true, Ordering::SeqCst);
    LAST_FREEZE_MS.store(now_ms(), Ordering::SeqCst);
    match cause {
        FreezeCause::Pause | FreezeCause::Stop => {
            LAST_CAUSE.store(cause_code(cause), Ordering::SeqCst);
        }
        FreezeCause::Physical if !was => {
            let _ = LAST_CAUSE.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |cur| {
                if cur == CAUSE_PAUSE || cur == CAUSE_STOP {
                    None
                } else {
                    Some(CAUSE_PHYSICAL)
                }
            });
        }
        FreezeCause::Physical => {}
    }
}

/// Subscribe to freeze events. Callbacks run on `flush_notify` (pump / poll / freeze_now).
/// This module does not clear session allows.
pub fn subscribe(cb: impl Fn(FreezeCause) + Send + Sync + 'static) {
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
    freeze_now_with(FreezeCause::Stop);
}

pub fn freeze_now_with(cause: FreezeCause) {
    record_freeze(cause);
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

fn take_pending_cause() -> Option<FreezeCause> {
    let code = LAST_CAUSE.swap(CAUSE_NONE, Ordering::SeqCst);
    if code == CAUSE_NONE {
        None
    } else {
        Some(cause_from_code(code))
    }
}

pub(crate) fn flush_notify() {
    let Some(cause) = take_pending_cause() else {
        return;
    };
    let listeners = LISTENERS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for cb in listeners {
        cb(cause);
    }
}

fn note_mouse(injected: bool) {
    if !injected {
        record_freeze(FreezeCause::Physical);
    }
}

fn note_key(vk: u32, injected: bool) {
    let pause = vk == u32::from(VK_PAUSE.0) || vk == u32::from(VK_CANCEL.0);
    if pause {
        record_freeze(FreezeCause::Pause);
    } else if !injected {
        record_freeze(FreezeCause::Physical);
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
                flush_notify();
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                flush_notify();
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
pub(crate) fn reset_for_test() {
    FROZEN.store(false, Ordering::SeqCst);
    LAST_FREEZE_MS.store(0, Ordering::SeqCst);
    LAST_CAUSE.store(CAUSE_NONE, Ordering::SeqCst);
    LISTENERS.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

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
        assert_eq!(m.last_cause(), Some(FreezeCause::Stop));
        m.on_mouse(false, t0 + Duration::from_millis(1500));
        assert_eq!(m.last_cause(), Some(FreezeCause::Physical));
        assert!(m.is_frozen(t0 + Duration::from_millis(3000)));
        assert!(!m.is_frozen(t0 + Duration::from_millis(3500)));
    }

    #[test]
    fn machine_pause_while_physically_frozen_is_pause() {
        let mut m = LeaseMachine::new();
        let t0 = Instant::now();
        m.on_mouse(false, t0);
        assert_eq!(m.last_cause(), Some(FreezeCause::Physical));
        m.on_key(u32::from(VK_PAUSE.0), true, t0);
        assert_eq!(m.last_cause(), Some(FreezeCause::Pause));
        assert!(m.is_frozen(t0));
    }

    fn collect_causes() -> (Arc<Mutex<Vec<FreezeCause>>>, impl Fn(FreezeCause)) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let slot = seen.clone();
        (seen, move |cause| {
            slot.lock().unwrap_or_else(|e| e.into_inner()).push(cause);
        })
    }

    #[test]
    fn injected_letter_does_not_freeze_atomics() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        let (seen, cb) = collect_causes();
        subscribe(cb);
        note_key(u32::from(b'V'), true);
        flush_notify();
        assert!(!is_frozen());
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn pause_injected_is_pause() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        let (seen, cb) = collect_causes();
        subscribe(cb);
        note_key(u32::from(VK_PAUSE.0), true);
        flush_notify();
        assert_eq!(*seen.lock().unwrap(), vec![FreezeCause::Pause]);
        reset_for_test();
        let (seen, cb) = collect_causes();
        subscribe(cb);
        note_key(u32::from(VK_CANCEL.0), true);
        flush_notify();
        assert_eq!(*seen.lock().unwrap(), vec![FreezeCause::Pause]);
    }

    #[test]
    fn physical_mouse_is_physical() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        let (seen, cb) = collect_causes();
        subscribe(cb);
        note_mouse(false);
        flush_notify();
        assert_eq!(*seen.lock().unwrap(), vec![FreezeCause::Physical]);
        note_mouse(false);
        flush_notify();
        assert_eq!(*seen.lock().unwrap(), vec![FreezeCause::Physical]);
    }

    #[test]
    fn freeze_now_with_stop_is_stop() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        let (seen, cb) = collect_causes();
        subscribe(cb);
        freeze_now_with(FreezeCause::Stop);
        assert_eq!(*seen.lock().unwrap(), vec![FreezeCause::Stop]);
    }

    #[test]
    fn pause_while_already_physically_frozen_delivers_pause() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        let (seen, cb) = collect_causes();
        subscribe(cb);
        note_mouse(false);
        flush_notify();
        assert_eq!(*seen.lock().unwrap(), vec![FreezeCause::Physical]);
        note_key(u32::from(VK_PAUSE.0), true);
        flush_notify();
        assert_eq!(
            *seen.lock().unwrap(),
            vec![FreezeCause::Physical, FreezeCause::Pause]
        );
    }

    #[test]
    fn physical_does_not_clobber_pending_pause() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        let (seen, cb) = collect_causes();
        subscribe(cb);
        note_key(u32::from(VK_PAUSE.0), true);
        note_mouse(false);
        flush_notify();
        assert_eq!(*seen.lock().unwrap(), vec![FreezeCause::Pause]);
    }

    #[test]
    fn take_pending_cause_snapshots_before_clearing_pending() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        note_key(u32::from(VK_PAUSE.0), true);
        assert_eq!(take_pending_cause(), Some(FreezeCause::Pause));
        assert_eq!(take_pending_cause(), None);
    }
}
