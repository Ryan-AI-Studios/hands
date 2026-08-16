//! Confirm fence: classify evidence, check allows, refuse before SendInput.

use std::sync::{Mutex, OnceLock};

use serde::Serialize;

use crate::allows::{self, AllowHit};
use crate::classify::{self, Evidence, Verdict};
use crate::error::HandsError;
use crate::lease::{self, FreezeCause};
use crate::target::ResolvedTarget;
use crate::uia;

static INSTALLED: OnceLock<()> = OnceLock::new();
// Per-process only: CLI observe then a new CLI click process will not share this slot.
static LAST_URL: Mutex<Option<String>> = Mutex::new(None);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FenceInfo {
    pub domain: String,
    pub category: String,
    pub name: String,
    pub role: String,
    pub modes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    Free,
    Allowed(AllowHit),
    Refused { fence: FenceInfo },
}

pub fn note_last_url(url: Option<&str>) {
    if let Ok(mut slot) = LAST_URL.lock() {
        *slot = url
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
    }
}

pub fn last_url() -> Option<String> {
    LAST_URL.lock().ok().and_then(|g| g.clone())
}

pub fn ensure_installed() {
    INSTALLED.get_or_init(install_fence);
    lease::flush_notify();
}

fn install_fence() {
    allows::warmup();
    lease::subscribe(|cause| {
        if matches!(cause, FreezeCause::Pause | FreezeCause::Stop) {
            allows::clear_session_allows();
        }
    });
}

#[cfg(test)]
fn reinstall_for_test() {
    install_fence();
}

pub fn decide(session_id: &str, evidence: &Evidence, domain: &str) -> Result<Gate, HandsError> {
    lease::flush_notify();
    match classify::classify(evidence) {
        Verdict::Free => Ok(Gate::Free),
        Verdict::Gated { category } => match allows::check(session_id, domain, category)? {
            AllowHit::Miss => Ok(Gate::Refused {
                fence: FenceInfo {
                    domain: domain.to_string(),
                    category: category.to_string(),
                    name: evidence.name.clone(),
                    role: evidence.role.clone(),
                    modes: vec!["once".into(), "session".into(), "persist".into()],
                },
            }),
            hit => Ok(Gate::Allowed(hit)),
        },
    }
}

pub fn gate_click(
    session_id: &str,
    resolved: &ResolvedTarget,
) -> Result<Option<FenceInfo>, HandsError> {
    ensure_installed();
    let (evidence, address) = evidence_for_click(resolved)?;
    let domain = domain_for(&evidence, address.as_deref());
    match decide(session_id, &evidence, &domain)? {
        Gate::Refused { fence } => Ok(Some(fence)),
        Gate::Free | Gate::Allowed(_) => Ok(None),
    }
}

pub fn gate_enter(session_id: &str) -> Result<Option<FenceInfo>, HandsError> {
    ensure_installed();
    let (evidence, address) = evidence_for_focused()?;
    let domain = domain_for(&evidence, address.as_deref());
    match decide(session_id, &evidence, &domain)? {
        Gate::Refused { fence } => Ok(Some(fence)),
        Gate::Free | Gate::Allowed(_) => Ok(None),
    }
}

pub fn domain_for(evidence: &Evidence, address_value: Option<&str>) -> String {
    classify::resolve_domain(
        last_url().as_deref(),
        &evidence.window_title,
        address_value.unwrap_or(""),
        &evidence.window_class,
    )
}

fn evidence_for_click(resolved: &ResolvedTarget) -> Result<(Evidence, Option<String>), HandsError> {
    let need_hit = resolved.kind != "element" || resolved.name.is_empty();
    apply_hit_result(
        resolved,
        if need_hit {
            uia::hit_test(resolved.x, resolved.y)
        } else {
            Err(HandsError::Uia("unused".into()))
        },
        need_hit,
    )
}

fn apply_hit_result(
    resolved: &ResolvedTarget,
    hit: Result<uia::HitElement, HandsError>,
    need_hit: bool,
) -> Result<(Evidence, Option<String>), HandsError> {
    let hit = if need_hit {
        // Fail closed: a UIA error is not "unlabeled free space".
        Some(hit?)
    } else {
        None
    };
    Ok(merge_click_evidence(resolved, hit))
}

fn merge_click_evidence(
    resolved: &ResolvedTarget,
    hit: Option<uia::HitElement>,
) -> (Evidence, Option<String>) {
    let mut name = resolved.name.clone();
    let mut role = resolved.role.clone();
    let mut hwnd = resolved.hwnd;
    let mut address = None;
    if let Some(hit) = hit {
        if name.is_empty() {
            name = hit.name;
            if role.is_empty() {
                role = hit.role;
            }
        }
        if hwnd.is_none() {
            hwnd = hit.hwnd;
        }
        address = hit.value;
    }
    (
        Evidence {
            name,
            role,
            window_title: window_title(hwnd),
            window_class: window_class(hwnd),
        },
        address,
    )
}

fn evidence_for_focused() -> Result<(Evidence, Option<String>), HandsError> {
    let hit = uia::focused()?;
    Ok((
        Evidence {
            name: hit.name,
            role: hit.role.clone(),
            window_title: window_title(hit.hwnd),
            window_class: window_class(hit.hwnd),
        },
        hit.value,
    ))
}

fn window_title(hwnd: Option<isize>) -> String {
    read_window_text(hwnd, WindowField::Title)
}

fn window_class(hwnd: Option<isize>) -> String {
    read_window_text(hwnd, WindowField::Class)
}

enum WindowField {
    Title,
    Class,
}

fn read_window_text(hwnd: Option<isize>, field: WindowField) -> String {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetForegroundWindow, GetWindowTextW,
    };

    let hwnd = hwnd
        .map(crate::foreground::raw_hwnd)
        .filter(|h| crate::foreground::hwnd_raw(*h).is_some())
        .unwrap_or_else(|| unsafe { GetForegroundWindow() });
    if hwnd.is_invalid() {
        return String::new();
    }
    let mut buf = [0u16; 512];
    let n = match field {
        WindowField::Title => unsafe { GetWindowTextW(hwnd, &mut buf) },
        WindowField::Class => unsafe { GetClassNameW(hwnd, &mut buf) },
    };
    if n <= 0 {
        String::new()
    } else {
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allows::{self, AllowHit, AllowMode};
    use crate::classify::{Category, Evidence};

    fn ev(name: &str, role: &str) -> Evidence {
        Evidence {
            name: name.into(),
            role: role.into(),
            window_title: String::new(),
            window_class: String::new(),
        }
    }

    #[test]
    fn easy_apply_refuses_without_allow() {
        allows::with_test_env(|| {
            for domain in ["desktop", "unknown"] {
                match decide("s1", &ev("Easy Apply", "button"), domain).unwrap() {
                    Gate::Refused { fence } => {
                        assert_eq!(fence.category, "applications");
                        assert_eq!(fence.domain, domain);
                        assert_eq!(fence.modes, ["once", "session", "persist"]);
                    }
                    other => panic!("expected refuse on {domain}, got {other:?}"),
                }
            }
        });
    }

    #[test]
    fn confirm_session_then_decide_allowed() {
        allows::with_test_env(|| {
            allows::grant("s1", "desktop", Category::Applications, AllowMode::Session).unwrap();
            assert_eq!(
                decide("s1", &ev("Easy Apply", "button"), "desktop").unwrap(),
                Gate::Allowed(AllowHit::Session)
            );
        });
    }

    #[test]
    fn once_consumed_on_second_decide() {
        allows::with_test_env(|| {
            allows::grant("s1", "desktop", Category::Applications, AllowMode::Once).unwrap();
            assert_eq!(
                decide("s1", &ev("Easy Apply", "button"), "desktop").unwrap(),
                Gate::Allowed(AllowHit::Once)
            );
            assert!(matches!(
                decide("s1", &ev("Easy Apply", "button"), "desktop").unwrap(),
                Gate::Refused { .. }
            ));
        });
    }

    #[test]
    fn apply_filters_is_free() {
        allows::with_test_env(|| {
            assert_eq!(
                decide("s1", &ev("Apply filters", "button"), "desktop").unwrap(),
                Gate::Free
            );
        });
    }

    #[test]
    fn pause_and_stop_clear_session_not_persist() {
        let _lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        allows::with_test_env(|| {
            lease::reset_for_test();
            reinstall_for_test();
            allows::grant("s1", "desktop", Category::Applications, AllowMode::Session).unwrap();
            allows::grant("s1", "desktop", Category::Applications, AllowMode::Persist).unwrap();
            lease::freeze_now_with(FreezeCause::Physical);
            assert_eq!(
                decide("s1", &ev("Easy Apply", "button"), "desktop").unwrap(),
                Gate::Allowed(AllowHit::Session)
            );
            lease::freeze_now_with(FreezeCause::Pause);
            assert_eq!(
                decide("s1", &ev("Easy Apply", "button"), "desktop").unwrap(),
                Gate::Allowed(AllowHit::Persist)
            );
            allows::grant("s1", "desktop", Category::Applications, AllowMode::Session).unwrap();
            lease::freeze_now_with(FreezeCause::Stop);
            assert_eq!(
                decide("s1", &ev("Easy Apply", "button"), "desktop").unwrap(),
                Gate::Allowed(AllowHit::Persist)
            );
        });
    }

    fn pixel_resolved() -> crate::target::ResolvedTarget {
        crate::target::ResolvedTarget {
            target: crate::target::Target::Pixel { x: 10, y: 10 },
            kind: "pixel",
            id: None,
            x: 10,
            y: 10,
            rect: crate::space::Rect {
                x: 10,
                y: 10,
                w: 1,
                h: 1,
            },
            hwnd: None,
            name: String::new(),
            role: String::new(),
        }
    }

    #[test]
    fn hit_test_error_is_fail_closed() {
        let err = apply_hit_result(
            &pixel_resolved(),
            Err(HandsError::Uia("ElementFromPoint: denied".into())),
            true,
        )
        .expect_err("UIA failure must not become unlabeled-free");
        assert!(err.to_string().contains("ElementFromPoint"), "{err}");
    }

    #[test]
    fn unlabeled_hit_is_free_not_an_error() {
        let (evidence, _) = apply_hit_result(
            &pixel_resolved(),
            Ok(crate::uia::HitElement {
                name: String::new(),
                role: String::new(),
                hwnd: None,
                value: None,
            }),
            true,
        )
        .unwrap();
        assert_eq!(
            classify::classify(&evidence),
            crate::classify::Verdict::Free
        );
    }
}
