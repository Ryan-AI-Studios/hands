//! Session-wide consecutive-rejection cooldown. Freeze duration is unchanged.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::lease;

pub const REJECTS_BEFORE_COOLDOWN: u32 = 3;
pub const COOLDOWN_FIRST_MS: u64 = 5_000;
pub const COOLDOWN_CAP_MS: u64 = 300_000;

pub const GUIDANCE_FROZEN: &str = "Desk lease frozen (physical input or Pause/Break). Yield — do not retry this task. Physical input is a permanent yield for the task, not a 2-second wait.";
pub const GUIDANCE_LOOP: &str = "You may be in a retry loop. Self-verify: re-observe the target, check the lease state (logs tail), and if the desk lease is frozen or physical input is active, ABORT the task and report instead of retrying. Physical input outranks agent input by design.";

thread_local! {
    static CLOCK: Cell<Option<fn() -> Instant>> = const { Cell::new(None) };
}

static STATES: OnceLock<Mutex<HashMap<String, SessionState>>> = OnceLock::new();

fn states() -> std::sync::MutexGuard<'static, HashMap<String, SessionState>> {
    STATES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
pub static TEST_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    pub attempt: u32,
    pub cooldown_ms: Option<u64>,
    pub loop_suspected: bool,
}

#[derive(Debug, Clone)]
struct SessionState {
    attempt: u32,
    deadline: Option<Instant>,
}

fn now() -> Instant {
    CLOCK
        .try_with(|c| c.get().map_or_else(Instant::now, |f| f()))
        .unwrap_or_else(|_| Instant::now())
}

/// `min(5000 * 2^(n-3), 300000)` for `n >= 3`.
pub fn schedule_ms(attempt: u32) -> u64 {
    if attempt < REJECTS_BEFORE_COOLDOWN {
        return 0;
    }
    let shift = (attempt - REJECTS_BEFORE_COOLDOWN).min(16);
    COOLDOWN_FIRST_MS
        .saturating_mul(1u64 << shift)
        .min(COOLDOWN_CAP_MS)
}

fn remaining_ms(deadline: Instant, now: Instant) -> Option<u64> {
    if now < deadline {
        Some(u64::try_from(deadline.duration_since(now).as_millis()).unwrap_or(u64::MAX))
    } else {
        None
    }
}

fn snapshot_of(state: &SessionState, now: Instant) -> Snapshot {
    let cooldown_ms = state.deadline.and_then(|d| remaining_ms(d, now));
    Snapshot {
        attempt: state.attempt,
        cooldown_ms,
        loop_suspected: state.attempt >= REJECTS_BEFORE_COOLDOWN,
    }
}

pub fn note_rejection(session: &str) -> Snapshot {
    let now = now();
    let mut map = states();
    let state = map.entry(session.to_string()).or_insert(SessionState {
        attempt: 0,
        deadline: None,
    });
    state.attempt = state.attempt.saturating_add(1);
    let ms = schedule_ms(state.attempt);
    if ms > 0 {
        state.deadline = Some(now + Duration::from_millis(ms));
    }
    snapshot_of(state, now)
}

pub fn note_success(session: &str) {
    let mut map = states();
    map.remove(session);
}

pub fn note_observe(session: &str) {
    if lease::is_frozen() {
        return;
    }
    note_success(session);
}

pub fn is_cooling(session: &str) -> bool {
    snapshot(session).cooldown_ms.is_some()
}

pub fn snapshot(session: &str) -> Snapshot {
    let now = now();
    let map = states();
    match map.get(session) {
        Some(state) => snapshot_of(state, now),
        None => Snapshot {
            attempt: 0,
            cooldown_ms: None,
            loop_suspected: false,
        },
    }
}

pub fn guidance(frozen: bool, snap: Snapshot) -> Option<String> {
    if frozen {
        Some(GUIDANCE_FROZEN.into())
    } else if snap.loop_suspected {
        Some(GUIDANCE_LOOP.into())
    } else {
        None
    }
}

#[cfg(test)]
pub fn reset_for_test() {
    CLOCK.with(|c| c.set(None));
    states().clear();
}

#[cfg(test)]
pub fn set_clock_for_test(clock: Option<fn() -> Instant>) {
    CLOCK.with(|c| c.set(clock));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    static OFFSET_MS: AtomicU64 = AtomicU64::new(0);
    static BASE: Mutex<Option<Instant>> = Mutex::new(None);

    fn test_now() -> Instant {
        let base = *BASE.lock().unwrap_or_else(|e| e.into_inner());
        let base = base.expect("BASE");
        base + Duration::from_millis(OFFSET_MS.load(Ordering::SeqCst))
    }

    fn install_clock() {
        let mut slot = BASE.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(Instant::now());
        }
        OFFSET_MS.store(0, Ordering::SeqCst);
        set_clock_for_test(Some(test_now));
    }

    fn advance(ms: u64) {
        OFFSET_MS.fetch_add(ms, Ordering::SeqCst);
    }

    #[test]
    fn schedule_table() {
        assert_eq!(schedule_ms(1), 0);
        assert_eq!(schedule_ms(2), 0);
        assert_eq!(schedule_ms(3), 5_000);
        assert_eq!(schedule_ms(4), 10_000);
        assert_eq!(schedule_ms(5), 20_000);
        assert_eq!(schedule_ms(6), 40_000);
        assert_eq!(schedule_ms(7), 80_000);
        assert_eq!(schedule_ms(8), 160_000);
        assert_eq!(schedule_ms(9), 300_000);
        assert_eq!(schedule_ms(10), 300_000);
    }

    #[test]
    fn schedule_slice_is_not_whole_file() {
        let src = include_str!("cooldown.rs");
        let start = src.find("pub fn schedule_ms").expect("schedule_ms");
        let slice = src[start..].split("fn remaining_ms").next().expect("slice");
        assert!(slice.contains("COOLDOWN_FIRST_MS"));
        assert!(slice.contains("COOLDOWN_CAP_MS"));
        assert!(!src[..start].contains("pub fn schedule_ms("));
    }

    #[test]
    fn guidance_consts_source_lock() {
        let src = include_str!("cooldown.rs");
        let start = src
            .find("pub const GUIDANCE_FROZEN")
            .expect("GUIDANCE_FROZEN");
        let slice = src[start..]
            .split("thread_local!")
            .next()
            .expect("guidance slice");
        assert!(slice.contains(GUIDANCE_FROZEN));
        assert!(slice.contains(GUIDANCE_LOOP));
        assert_eq!(
            GUIDANCE_FROZEN,
            "Desk lease frozen (physical input or Pause/Break). Yield — do not retry this task. Physical input is a permanent yield for the task, not a 2-second wait."
        );
        assert_eq!(
            GUIDANCE_LOOP,
            "You may be in a retry loop. Self-verify: re-observe the target, check the lease state (logs tail), and if the desk lease is frozen or physical input is active, ABORT the task and report instead of retrying. Physical input outranks agent input by design."
        );
    }

    #[test]
    fn session_isolation_and_spam_replaces_deadline() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        install_clock();
        let a = note_rejection("s-a");
        assert_eq!(a.attempt, 1);
        assert!(a.cooldown_ms.is_none());
        note_rejection("s-a");
        let third = note_rejection("s-a");
        assert_eq!(third.attempt, 3);
        assert_eq!(third.cooldown_ms, Some(5_000));
        assert!(third.loop_suspected);
        assert!(is_cooling("s-a"));
        assert!(!is_cooling("s-b"));
        let fourth = note_rejection("s-a");
        assert_eq!(fourth.attempt, 4);
        assert_eq!(fourth.cooldown_ms, Some(10_000));
        advance(3_000);
        let mid = snapshot("s-a");
        assert_eq!(mid.attempt, 4);
        assert_eq!(mid.cooldown_ms, Some(7_000));
        let fifth = note_rejection("s-a");
        assert_eq!(fifth.attempt, 5);
        assert_eq!(fifth.cooldown_ms, Some(20_000));
        note_success("s-a");
        assert!(!is_cooling("s-a"));
        assert_eq!(snapshot("s-a").attempt, 0);
    }

    #[test]
    fn observe_resets_only_when_not_frozen() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _lease = crate::lease::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        crate::lease::reset_for_test();
        install_clock();
        note_rejection("s-o");
        note_rejection("s-o");
        note_rejection("s-o");
        assert!(is_cooling("s-o"));
        crate::lease::freeze_now_with(crate::lease::FreezeCause::Physical);
        note_observe("s-o");
        assert!(is_cooling("s-o"));
        crate::lease::reset_for_test();
        note_observe("s-o");
        assert!(!is_cooling("s-o"));
    }
}
