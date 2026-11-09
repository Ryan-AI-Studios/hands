//! UIA control-view walk.
//!
//! COM is initialized STA (`COINIT_APARTMENTTHREADED`) on a dedicated OS thread
//! because tokio's multi-thread runtime is the wrong apartment for IUIAutomation.

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize, SAFEARRAY,
};
use windows::Win32::System::Ole::{
    SafeArrayDestroy, SafeArrayGetElement, SafeArrayGetLBound, SafeArrayGetUBound,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTreeWalker,
    IUIAutomationValuePattern, TreeScope_Descendants, UIA_ControlTypePropertyId,
    UIA_DocumentControlTypeId, UIA_ValuePatternId,
};
use windows::Win32::UI::WindowsAndMessaging::{GW_ENABLEDPOPUP, GetWindow};

use std::collections::VecDeque;
use std::sync::mpsc;
use std::time::Duration;

use crate::error::HandsError;
use crate::extract::{ControlKind, Detail, MAIN_TEXT_MAX_CHARS, RawNode, name_fields};
use crate::identity::{IdentityState, element_fingerprint, runtime_id_hash};
use crate::space::Rect;

pub struct UiaSnapshot {
    pub title: String,
    pub nodes: Vec<RawNode>,
    pub popup_rect: Option<Rect>,
}

#[derive(Debug, Clone)]
pub struct ResolvedElement {
    pub rect: Rect,
    pub name: String,
    pub role: String,
    pub hwnd: Option<isize>,
    /// Live RuntimeId (kept for the semantic worker dispatch).
    pub runtime_id: Vec<i32>,
    /// The element's own `CurrentName`, never an ancestor-picked display name.
    pub own_name: String,
    /// The element's own localized control type (fallback: control-kind type name).
    pub own_role: String,
    pub is_password: bool,
}

impl ResolvedElement {
    pub fn own_fingerprint(&self) -> u32 {
        let (text, unnamed) = name_fields(&self.own_name, self.is_password);
        element_fingerprint(&self.own_role, text.as_deref(), unnamed)
    }
}

/// Scoped UIA failure carrying a closed state/reason pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedFailure {
    pub state: IdentityState,
    pub reason: &'static str,
}

impl ScopedFailure {
    fn stale(reason: &'static str) -> Self {
        Self {
            state: IdentityState::Stale,
            reason,
        }
    }

    fn ambiguous(reason: &'static str) -> Self {
        Self {
            state: IdentityState::Ambiguous,
            reason,
        }
    }

    fn unknown(reason: &'static str) -> Self {
        Self {
            state: IdentityState::Unknown,
            reason,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct HitElement {
    pub name: String,
    pub role: String,
    pub hwnd: Option<isize>,
    pub value: Option<String>,
}

/// Focused leaf control type + native hwnd. No ancestor name walk, no localized role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusedLeaf {
    pub kind: ControlKind,
    pub hwnd: Option<isize>,
}

pub fn collect(detail: Detail, hwnd: Option<isize>) -> Result<UiaSnapshot, HandsError> {
    let cap = detail.element_cap();
    std::thread::Builder::new()
        .name("hands-uia-sta".into())
        .spawn(move || sta_collect(detail, cap, hwnd))
        .map_err(|err| HandsError::Uia(format!("spawn STA thread: {err}")))?
        .join()
        .map_err(|_| HandsError::Uia("UIA STA thread panicked".to_string()))?
}

/// Secondary anchor: UIA `Document` bounding rectangle under `hwnd`, bounded to depth 8 /
/// 256 nodes and never using `GetRootElement` when an HWND is given. Runs on a named STA
/// worker and waits at most `deadline`; timeout/worker loss returns `None` and detaches.
pub fn document_rect(hwnd: isize, deadline: Duration) -> Option<Rect> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("hands-uia-document".into())
        .spawn(move || {
            let _ = tx.send(sta_document_rect(hwnd));
        })
        .ok()?;
    recv_document(&rx, deadline)
}

fn recv_document(rx: &mpsc::Receiver<Option<Rect>>, deadline: Duration) -> Option<Rect> {
    rx.recv_timeout(deadline).ok().flatten()
}

fn sta_document_rect(hwnd: isize) -> Option<Rect> {
    let _sta = StaGuard::enter().ok()?;
    let automation = create_automation().ok()?;
    let root = unsafe { automation.ElementFromHandle(crate::foreground::raw_hwnd(hwnd)) }.ok()?;
    let walker = unsafe { automation.ControlViewWalker() }.ok()?;
    let mut queue: VecDeque<(IUIAutomationElement, usize)> = VecDeque::new();
    queue.push_back((root, 0));
    let mut visited = 0usize;
    while let Some((element, depth)) = queue.pop_front() {
        visited += 1;
        if visited > 256 {
            break;
        }
        if let Ok(control) = unsafe { element.CurrentControlType() }
            && control.0 == UIA_DocumentControlTypeId.0
            && let Ok(bounds) = unsafe { element.CurrentBoundingRectangle() }
        {
            let rect = rect_from_uia(bounds);
            if rect.w > 0 && rect.h > 0 {
                return Some(rect);
            }
        }
        if depth >= 8 {
            continue;
        }
        if let Ok(first) = unsafe { walker.GetFirstChildElement(&element) } {
            let mut current = Some(first);
            while let Some(child) = current {
                let next = unsafe { walker.GetNextSiblingElement(&child) }.ok();
                queue.push_back((child, depth + 1));
                current = next;
            }
        }
    }
    None
}

/// Fresh control-view walk + RuntimeId compare. `ElementFromRuntimeId` is not
/// on windows 0.62.2.
pub fn resolve_runtime_id(want: &[i32]) -> Result<ResolvedElement, HandsError> {
    let want = want.to_vec();
    std::thread::Builder::new()
        .name("hands-uia-sta".into())
        .spawn(move || sta_resolve(&want))
        .map_err(|err| HandsError::Uia(format!("spawn STA thread: {err}")))?
        .join()
        .map_err(|_| HandsError::Uia("UIA STA thread panicked".to_string()))?
}

fn sta_resolve(want: &[i32]) -> Result<ResolvedElement, HandsError> {
    if want.is_empty() {
        return Err(HandsError::Target("empty RuntimeId".into()));
    }
    let _sta = StaGuard::enter()?;
    let automation = create_automation()?;
    Ok(resolve_on(&automation, want, None)?.resolved)
}

/// A resolved element plus the live COM element it came from. Both stay on the
/// caller's STA thread; COM pointers never cross it.
pub(crate) struct ResolvedComElement {
    pub element: IUIAutomationElement,
    pub resolved: ResolvedElement,
}

/// Control-view walk + RuntimeId compare on an existing automation object.
/// The caller must already be on an STA thread (the semantic worker owns it).
pub(crate) fn resolve_on(
    automation: &IUIAutomation,
    want: &[i32],
    root_hwnd: Option<isize>,
) -> Result<ResolvedComElement, HandsError> {
    if want.is_empty() {
        return Err(HandsError::Target("empty RuntimeId".into()));
    }
    let walker = unsafe { automation.ControlViewWalker() }
        .map_err(|err| HandsError::Uia(format!("ControlViewWalker: {err}")))?;
    let root = match root_hwnd {
        Some(hwnd) => unsafe { automation.ElementFromHandle(crate::foreground::raw_hwnd(hwnd)) }
            .map_err(|_| {
                HandsError::Target("window_unreadable: UIA root handle is unavailable".into())
            })?,
        None => unsafe { automation.GetRootElement() }
            .map_err(|err| HandsError::Uia(format!("GetRootElement: {err}")))?,
    };
    let (mut matches, cap_hit) = find_runtime_ids(&walker, &root, want, SCOPED_NODE_CAP);
    if cap_hit {
        return Err(HandsError::Target(
            "UIA walk cap reached before the RuntimeId was resolved; unknown".into(),
        ));
    }
    if matches.is_empty() {
        return Err(HandsError::Target(format!(
            "no UIA element with RuntimeId {}; RuntimeId does not survive navigation / a destroyed control; re-observe",
            want.iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(".")
        )));
    }
    if matches.len() > 1 {
        return Err(HandsError::Target(format!(
            "multiple UIA elements with RuntimeId {}; ambiguous; re-observe",
            want.iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(".")
        )));
    }
    let element = matches.remove(0);
    let rect = unsafe {
        element
            .CurrentBoundingRectangle()
            .map(rect_from_uia)
            .map_err(|err| HandsError::Uia(format!("CurrentBoundingRectangle: {err}")))?
    };
    let (name, role) = name_and_role(&walker, &element);
    let hwnd = native_hwnd(&walker, &element);
    let runtime_id = runtime_id(&element).unwrap_or_default();
    let own_name = unsafe { element.CurrentName() }
        .ok()
        .map(|s| s.to_string())
        .unwrap_or_default();
    let own_role = fingerprint_role(&element);
    let is_password = unsafe { element.CurrentIsPassword() }
        .map(|v| v.as_bool())
        .unwrap_or(false);
    Ok(ResolvedComElement {
        element,
        resolved: ResolvedElement {
            rect,
            name,
            role,
            hwnd,
            runtime_id,
            own_name,
            own_role,
            is_password,
        },
    })
}

#[cfg(test)]
type HitTestHook = fn(i32, i32) -> Result<HitElement, HandsError>;

#[cfg(test)]
thread_local! {
    static HIT_TEST_HOOK: std::cell::Cell<Option<HitTestHook>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_hit_test_hook(hook: Option<HitTestHook>) {
    HIT_TEST_HOOK.with(|c| c.set(hook));
}

pub fn hit_test(x: i32, y: i32) -> Result<HitElement, HandsError> {
    #[cfg(test)]
    {
        if let Some(hook) = HIT_TEST_HOOK.with(|c| c.get()) {
            return hook(x, y);
        }
    }
    std::thread::Builder::new()
        .name("hands-uia-sta".into())
        .spawn(move || sta_hit_test(x, y))
        .map_err(|err| HandsError::Uia(format!("spawn STA thread: {err}")))?
        .join()
        .map_err(|_| HandsError::Uia("UIA STA thread panicked".to_string()))?
}

pub fn focused() -> Result<HitElement, HandsError> {
    std::thread::Builder::new()
        .name("hands-uia-sta".into())
        .spawn(sta_focused)
        .map_err(|err| HandsError::Uia(format!("spawn STA thread: {err}")))?
        .join()
        .map_err(|_| HandsError::Uia("UIA STA thread panicked".to_string()))?
}

#[cfg(test)]
type FocusedLeafHook = fn() -> Result<FocusedLeaf, HandsError>;

#[cfg(test)]
thread_local! {
    static FOCUSED_LEAF_HOOK: std::cell::Cell<Option<FocusedLeafHook>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_focused_leaf_hook(hook: Option<FocusedLeafHook>) {
    FOCUSED_LEAF_HOOK.with(|c| c.set(hook));
}

#[cfg(test)]
type PageUrlHook = fn(Option<isize>) -> Option<String>;

#[cfg(test)]
thread_local! {
    static PAGE_URL_HOOK: std::cell::Cell<Option<PageUrlHook>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_page_url_hook(hook: Option<PageUrlHook>) {
    PAGE_URL_HOOK.with(|c| c.set(hook));
}

/// First descendant Document ValuePattern if it is `http://` or `https://`.
pub fn page_url(hwnd: Option<isize>) -> Option<String> {
    #[cfg(test)]
    {
        if let Some(hook) = PAGE_URL_HOOK.with(|c| c.get()) {
            return hook(hwnd);
        }
    }
    let hwnd = hwnd?;
    std::thread::Builder::new()
        .name("hands-uia-sta".into())
        .spawn(move || sta_page_url(hwnd))
        .ok()?
        .join()
        .ok()?
}

fn sta_page_url(hwnd: isize) -> Option<String> {
    let _sta = StaGuard::enter().ok()?;
    let automation = create_automation().ok()?;
    let handle = crate::foreground::raw_hwnd(hwnd);
    crate::foreground::hwnd_raw(handle)?;
    let root = unsafe { automation.ElementFromHandle(handle) }.ok()?;
    let condition = unsafe {
        automation.CreatePropertyCondition(
            UIA_ControlTypePropertyId,
            &VARIANT::from(UIA_DocumentControlTypeId.0),
        )
    }
    .ok()?;
    let document = unsafe { root.FindFirst(TreeScope_Descendants, &condition) }.ok()?;
    let value = value_pattern(&document)?;
    crate::extract::http_https_url(Some(&value))
}

/// Leaf `CurrentControlType` + hwnd. Used by `type`; fence still uses `focused()`.
pub fn focused_leaf() -> Result<FocusedLeaf, HandsError> {
    #[cfg(test)]
    {
        if let Some(hook) = FOCUSED_LEAF_HOOK.with(|c| c.get()) {
            return hook();
        }
    }
    std::thread::Builder::new()
        .name("hands-uia-sta".into())
        .spawn(sta_focused_leaf)
        .map_err(|err| HandsError::Uia(format!("spawn STA thread: {err}")))?
        .join()
        .map_err(|_| HandsError::Uia("UIA STA thread panicked".to_string()))?
}

fn sta_hit_test(x: i32, y: i32) -> Result<HitElement, HandsError> {
    let _sta = StaGuard::enter()?;
    let automation = create_automation()?;
    let element = unsafe { automation.ElementFromPoint(POINT { x, y }) }
        .map_err(|err| HandsError::Uia(format!("ElementFromPoint: {err}")))?;
    describe_element(&automation, &element)
}

fn sta_focused() -> Result<HitElement, HandsError> {
    let _sta = StaGuard::enter()?;
    let automation = create_automation()?;
    let element = unsafe { automation.GetFocusedElement() }
        .map_err(|err| HandsError::Uia(format!("GetFocusedElement: {err}")))?;
    describe_element(&automation, &element)
}

fn sta_focused_leaf() -> Result<FocusedLeaf, HandsError> {
    let _sta = StaGuard::enter()?;
    let automation = create_automation()?;
    let element = unsafe { automation.GetFocusedElement() }
        .map_err(|err| HandsError::Uia(format!("GetFocusedElement: {err}")))?;
    let control_id = unsafe { element.CurrentControlType() }
        .map_err(|err| HandsError::Uia(format!("CurrentControlType: {err}")))?
        .0;
    let walker = unsafe { automation.ControlViewWalker() }
        .map_err(|err| HandsError::Uia(format!("ControlViewWalker: {err}")))?;
    Ok(FocusedLeaf {
        kind: ControlKind::from_uia_id(control_id),
        hwnd: native_hwnd(&walker, &element),
    })
}

fn describe_element(
    automation: &IUIAutomation,
    element: &IUIAutomationElement,
) -> Result<HitElement, HandsError> {
    let walker = unsafe { automation.ControlViewWalker() }
        .map_err(|err| HandsError::Uia(format!("ControlViewWalker: {err}")))?;
    let (name, role) = name_and_role(&walker, element);
    Ok(HitElement {
        name,
        role,
        hwnd: native_hwnd(&walker, element),
        value: value_walk(&walker, element),
    })
}

fn role_of(element: &IUIAutomationElement) -> String {
    unsafe {
        element
            .CurrentLocalizedControlType()
            .ok()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "element".into())
    }
}

/// Own-role fingerprint input, mirroring `map_element`'s observe fallback. Do **not** replace
/// `role_of`: its `"element"` fallback feeds the 0122 set-value role gate.
pub(crate) fn fingerprint_role(element: &IUIAutomationElement) -> String {
    unsafe {
        let localized = element
            .CurrentLocalizedControlType()
            .ok()
            .map(|s| s.to_string())
            .unwrap_or_default();
        if !localized.is_empty() {
            return localized;
        }
        element
            .CurrentControlType()
            .ok()
            .map(|id| ControlKind::from_uia_id(id.0).type_name().to_string())
            .unwrap_or_else(|| "Other".into())
    }
}

/// First non-empty name wins, paired with **that** node's role (not the leaf).
/// A Submit button's inner `text` child must stay gated as a button.
fn pick_named(chain: &[(&str, &str)]) -> (String, String) {
    for (name, role) in chain {
        if !name.is_empty() {
            return ((*name).to_string(), (*role).to_string());
        }
    }
    (
        String::new(),
        chain
            .first()
            .map(|(_, role)| (*role).to_string())
            .unwrap_or_default(),
    )
}

fn name_and_role(
    walker: &IUIAutomationTreeWalker,
    element: &IUIAutomationElement,
) -> (String, String) {
    let mut chain = Vec::new();
    let mut current = element.clone();
    for _ in 0..8 {
        let name = unsafe { current.CurrentName() }
            .ok()
            .map(|s| s.to_string())
            .unwrap_or_default();
        chain.push((name, role_of(&current)));
        match unsafe { walker.GetParentElement(&current) } {
            Ok(parent) => current = parent,
            Err(_) => break,
        }
    }
    let refs: Vec<(&str, &str)> = chain
        .iter()
        .map(|(n, r)| (n.as_str(), r.as_str()))
        .collect();
    pick_named(&refs)
}

fn value_walk(walker: &IUIAutomationTreeWalker, element: &IUIAutomationElement) -> Option<String> {
    let mut current = element.clone();
    for _ in 0..8 {
        if let Some(value) = value_pattern(&current)
            && !value.is_empty()
        {
            return Some(value);
        }
        match unsafe { walker.GetParentElement(&current) } {
            Ok(parent) => current = parent,
            Err(_) => break,
        }
    }
    None
}

fn find_runtime_ids(
    walker: &IUIAutomationTreeWalker,
    root: &IUIAutomationElement,
    want: &[i32],
    cap: usize,
) -> (Vec<IUIAutomationElement>, bool) {
    let mut stack = vec![root.clone()];
    let mut matches = Vec::new();
    let mut visited = 0usize;
    while let Some(element) = stack.pop() {
        visited += 1;
        if visited > cap {
            return (matches, true);
        }
        if let Some(id) = runtime_id(&element)
            && id == want
        {
            matches.push(element.clone());
            if matches.len() > 1 {
                break;
            }
        }
        push_children(walker, &element, &mut stack);
    }
    (matches, false)
}

fn native_hwnd(walker: &IUIAutomationTreeWalker, element: &IUIAutomationElement) -> Option<isize> {
    let mut current = element.clone();
    for _ in 0..32 {
        if let Ok(hwnd) = unsafe { current.CurrentNativeWindowHandle() }
            && !hwnd.is_invalid()
        {
            return Some(hwnd.0 as isize);
        }
        match unsafe { walker.GetParentElement(&current) } {
            Ok(parent) => current = parent,
            Err(_) => break,
        }
    }
    None
}

pub(crate) const SCOPED_NODE_CAP: usize = 4096;
const SCOPED_MATCH_CAP: usize = 16;
const MAX_IN_FLIGHT_WORKERS: usize = 4;

static IN_FLIGHT_WORKERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Bounds detached UIA workers across scoped resolve and `uia-action` dispatch. A timed-out
/// worker keeps its slot until it actually exits, so a hung provider cannot accumulate
/// unbounded threads.
pub(crate) struct WorkerSlot;

impl WorkerSlot {
    pub(crate) fn acquire() -> Option<Self> {
        let current = IN_FLIGHT_WORKERS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if current >= MAX_IN_FLIGHT_WORKERS {
            IN_FLIGHT_WORKERS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            return None;
        }
        Some(Self)
    }
}

impl Drop for WorkerSlot {
    fn drop(&mut self) {
        IN_FLIGHT_WORKERS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
type ScopedUiaHook = fn(isize, &str, &str) -> Result<ResolvedElement, ScopedFailure>;

#[cfg(test)]
thread_local! {
    static SCOPED_UIA_HOOK: std::cell::Cell<Option<ScopedUiaHook>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_scoped_uia_hook(hook: Option<ScopedUiaHook>) {
    SCOPED_UIA_HOOK.with(|c| c.set(hook));
}

/// Scoped UIA resolve rooted at the observation walk HWND (never `GetRootElement`). The
/// caller gets a closed state/reason on every failure; nothing is dispatched.
pub fn resolve_scoped(
    hwnd: isize,
    ridh: &str,
    fp: &str,
    deadline: Duration,
) -> Result<ResolvedElement, ScopedFailure> {
    #[cfg(test)]
    {
        if let Some(hook) = SCOPED_UIA_HOOK.with(|c| c.get()) {
            return hook(hwnd, ridh, fp);
        }
    }
    let ridh = ridh.to_string();
    let fp = fp.to_string();
    let Some(slot) = WorkerSlot::acquire() else {
        return Err(ScopedFailure::unknown("provider_timeout"));
    };
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("hands-uia-scoped".into())
        .spawn(move || {
            let _slot = slot;
            let _ = tx.send(sta_resolve_scoped(hwnd, &ridh, &fp));
        })
        .map_err(|_| ScopedFailure::unknown("provider_timeout"))?;
    match rx.recv_timeout(deadline) {
        Ok(result) => result,
        Err(_) => Err(ScopedFailure::unknown("provider_timeout")),
    }
}

fn sta_resolve_scoped(hwnd: isize, ridh: &str, fp: &str) -> Result<ResolvedElement, ScopedFailure> {
    let _sta = StaGuard::enter().map_err(|_| ScopedFailure::unknown("provider_timeout"))?;
    let automation = create_automation().map_err(|_| ScopedFailure::unknown("provider_timeout"))?;
    let root = unsafe { automation.ElementFromHandle(crate::foreground::raw_hwnd(hwnd)) }
        .map_err(|_| ScopedFailure::unknown("window_unreadable"))?;
    let walker = unsafe { automation.ControlViewWalker() }
        .map_err(|_| ScopedFailure::unknown("provider_timeout"))?;
    let mut stack = vec![root];
    let mut visited = 0usize;
    let mut matches = Vec::new();
    let mut cap_hit = false;
    while let Some(element) = stack.pop() {
        visited += 1;
        if visited > SCOPED_NODE_CAP {
            cap_hit = true;
            break;
        }
        if let Some(id) = runtime_id(&element)
            && runtime_id_hash(&id) == ridh
            && let Some(candidate) = scoped_candidate(&walker, &element)
        {
            matches.push(candidate);
            if matches.len() >= SCOPED_MATCH_CAP {
                break;
            }
        }
        push_children(&walker, &element, &mut stack);
    }
    scoped_matches_outcome(matches, cap_hit, hwnd, fp)
}

fn scoped_candidate(
    walker: &IUIAutomationTreeWalker,
    element: &IUIAutomationElement,
) -> Option<ResolvedElement> {
    let rect = unsafe { element.CurrentBoundingRectangle() }
        .ok()
        .map(rect_from_uia)?;
    let runtime_id = runtime_id(element)?;
    let own_name = unsafe { element.CurrentName() }
        .ok()
        .map(|s| s.to_string())
        .unwrap_or_default();
    let own_role = fingerprint_role(element);
    let is_password = unsafe { element.CurrentIsPassword() }
        .ok()
        .map(|v| v.as_bool())
        .unwrap_or(false);
    let (name, role) = name_and_role(walker, element);
    let hwnd = native_hwnd(walker, element);
    Some(ResolvedElement {
        rect,
        name,
        role,
        hwnd,
        runtime_id,
        own_name,
        own_role,
        is_password,
    })
}

/// Pure candidate arbitration used by the live walk and tests. A cap hit with no resolved
/// hash is `unknown`, never a partial first match.
pub(crate) fn scoped_matches_outcome(
    matches: Vec<ResolvedElement>,
    cap_hit: bool,
    hwnd: isize,
    fp: &str,
) -> Result<ResolvedElement, ScopedFailure> {
    // A cap hit means the walk is incomplete even if a candidate was already collected.
    // Never accept a partial first match (DoD-7).
    if cap_hit {
        return Err(ScopedFailure::unknown("provider_timeout"));
    }
    if matches.is_empty() {
        return Err(ScopedFailure::stale("stale_element"));
    }
    let mut within = Vec::new();
    for candidate in matches {
        if candidate
            .hwnd
            .is_some_and(|native| crate::foreground::related_to_root(native, hwnd))
        {
            within.push(candidate);
        }
    }
    if within.is_empty() {
        return Err(ScopedFailure::stale("window_mismatch"));
    }
    if within.len() > 1 {
        return Err(ScopedFailure::ambiguous("duplicate_runtime_id"));
    }
    let found = within
        .into_iter()
        .next()
        .ok_or_else(|| ScopedFailure::stale("stale_element"))?;
    let actual = format!("{:08x}", found.own_fingerprint());
    if actual != fp {
        return Err(ScopedFailure::stale("identity_mismatch"));
    }
    Ok(found)
}

fn sta_collect(detail: Detail, cap: usize, hwnd: Option<isize>) -> Result<UiaSnapshot, HandsError> {
    let _sta = StaGuard::enter()?;
    let automation = create_automation()?;
    let hwnd = hwnd.and_then(|raw| {
        let h = crate::foreground::raw_hwnd(raw);
        crate::foreground::hwnd_raw(h).map(|_| h)
    });
    let title = match hwnd {
        Some(h) => window_element_name(&automation, h),
        None => String::new(),
    };
    let walker = unsafe { automation.ControlViewWalker() }
        .map_err(|err| HandsError::Uia(format!("ControlViewWalker: {err}")))?;
    let (nodes, popup_rect) = match detail {
        Detail::Dom => match hwnd {
            Some(h) => match unsafe { automation.ElementFromHandle(h) } {
                Ok(root) => (walk_control_view(&walker, &root, detail, cap)?, None),
                Err(_) => (Vec::new(), None),
            },
            None => {
                let root = unsafe { automation.GetRootElement() }
                    .map_err(|err| HandsError::Uia(format!("GetRootElement: {err}")))?;
                (walk_control_view(&walker, &root, detail, cap)?, None)
            }
        },
        Detail::Default => match hwnd {
            Some(h) => collect_default(&automation, &walker, cap, h)?,
            None => (Vec::new(), None),
        },
    };
    Ok(UiaSnapshot {
        title,
        nodes,
        popup_rect,
    })
}

/// Default walk: `ElementFromHandle(hwnd)` plus fail-closed owned popup.
/// Uses the HWND passed into `collect` — does not call `GetForegroundWindow`.
fn collect_default(
    automation: &IUIAutomation,
    walker: &IUIAutomationTreeWalker,
    cap: usize,
    hwnd: HWND,
) -> Result<(Vec<RawNode>, Option<Rect>), HandsError> {
    if hwnd.is_invalid() {
        return Ok((Vec::new(), None));
    }
    let mut nodes = match unsafe { automation.ElementFromHandle(hwnd) } {
        Ok(root) => walk_control_view(walker, &root, Detail::Default, cap)?,
        Err(_) => Vec::new(),
    };
    let popup_rect = prepend_owned_popup(automation, walker, hwnd, cap, &mut nodes);
    Ok((nodes, popup_rect))
}

/// `GW_ENABLEDPOPUP` on the walk HWND already in hand. If the popup HWND equals
/// the walk root (Microsoft: none) or the handle fails, skip — never `GetRootElement`.
fn prepend_owned_popup(
    automation: &IUIAutomation,
    walker: &IUIAutomationTreeWalker,
    fg: HWND,
    cap: usize,
    nodes: &mut Vec<RawNode>,
) -> Option<Rect> {
    let popup = unsafe { GetWindow(fg, GW_ENABLEDPOPUP) }.ok()?;
    if popup.is_invalid() || popup == fg {
        return None;
    }
    let extra = match unsafe { automation.ElementFromHandle(popup) } {
        Ok(root) => walk_control_view(walker, &root, Detail::Default, cap).ok()?,
        Err(_) => return None,
    };
    let rect = crate::foreground::hwnd_raw(popup).and_then(crate::foreground::client_rect)?;
    let mut combined = extra;
    combined.append(nodes);
    *nodes = combined;
    Some(rect)
}

pub(crate) fn create_automation() -> Result<IUIAutomation, HandsError> {
    unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
        .map_err(|err| HandsError::Uia(format!("CoCreateInstance(CUIAutomation): {err}")))
}

fn window_element_name(automation: &IUIAutomation, hwnd: HWND) -> String {
    if hwnd.is_invalid() {
        return String::new();
    }
    match unsafe { automation.ElementFromHandle(hwnd) } {
        Ok(element) => unsafe { element.CurrentName() }
            .map(|n| n.to_string())
            .unwrap_or_default(),
        Err(_) => String::new(),
    }
}

fn walk_control_view(
    walker: &IUIAutomationTreeWalker,
    root: &IUIAutomationElement,
    detail: Detail,
    cap: usize,
) -> Result<Vec<RawNode>, HandsError> {
    let mut out = Vec::new();
    let mut matched = 0usize;
    let mut text_chars = 0usize;
    let mut stack = vec![root.clone()];
    while let Some(element) = stack.pop() {
        if let Some(node) = map_element(&element) {
            let passes = node.passes_filter(detail);
            let piece_len = node
                .main_text_piece()
                .map(|p| p.chars().count())
                .unwrap_or(0);
            let keep_element = passes && matched < cap;
            let keep_text = piece_len > 0 && text_chars < MAIN_TEXT_MAX_CHARS;
            if keep_element || keep_text {
                if keep_element {
                    matched += 1;
                }
                if keep_text {
                    text_chars = text_chars.saturating_add(piece_len);
                }
                out.push(node);
            }
        }
        if matched >= cap && text_chars >= MAIN_TEXT_MAX_CHARS {
            break;
        }
        push_children(walker, &element, &mut stack);
    }
    Ok(out)
}

fn push_children(
    walker: &IUIAutomationTreeWalker,
    parent: &IUIAutomationElement,
    stack: &mut Vec<IUIAutomationElement>,
) {
    let Ok(first) = (unsafe { walker.GetFirstChildElement(parent) }) else {
        return;
    };
    let mut current = Some(first);
    let mut children = Vec::new();
    while let Some(el) = current {
        let next = unsafe { walker.GetNextSiblingElement(&el) }.ok();
        children.push(el);
        current = next;
    }
    stack.extend(children.into_iter().rev());
}

fn map_element(element: &IUIAutomationElement) -> Option<RawNode> {
    unsafe {
        let is_control = element.CurrentIsControlElement().ok()?.as_bool();
        let is_offscreen = element.CurrentIsOffscreen().ok()?.as_bool();
        let bounds = element.CurrentBoundingRectangle().ok()?;
        let rect = rect_from_uia(bounds);
        let control_id = element.CurrentControlType().ok()?.0;
        let control_kind = ControlKind::from_uia_id(control_id);
        let is_keyboard_focusable = element.CurrentIsKeyboardFocusable().ok()?.as_bool();
        let is_password = element.CurrentIsPassword().ok()?.as_bool();
        let name = element.CurrentName().ok()?.to_string();
        let localized = element
            .CurrentLocalizedControlType()
            .ok()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let role = if localized.is_empty() {
            control_kind.type_name().to_string()
        } else {
            localized
        };
        let runtime_id = runtime_id(element)?;
        if runtime_id.is_empty() {
            return None;
        }
        let value = if is_password {
            None
        } else {
            value_pattern(element)
        };
        Some(RawNode {
            runtime_id,
            role,
            name,
            value,
            is_password,
            rect,
            control_kind,
            is_control,
            is_offscreen,
            is_keyboard_focusable,
        })
    }
}

fn rect_from_uia(bounds: RECT) -> Rect {
    Rect {
        x: bounds.left,
        y: bounds.top,
        w: bounds.right.saturating_sub(bounds.left),
        h: bounds.bottom.saturating_sub(bounds.top),
    }
}

fn runtime_id(element: &IUIAutomationElement) -> Option<Vec<i32>> {
    let psa = unsafe { element.GetRuntimeId().ok()? };
    if psa.is_null() {
        return None;
    }
    let result = unsafe { read_i32_array(psa) };
    unsafe {
        let _ = SafeArrayDestroy(psa);
    }
    result.ok()
}

unsafe fn read_i32_array(psa: *mut SAFEARRAY) -> Result<Vec<i32>, ()> {
    let lbound = unsafe { SafeArrayGetLBound(psa, 1) }.map_err(|_| ())?;
    let ubound = unsafe { SafeArrayGetUBound(psa, 1) }.map_err(|_| ())?;
    let mut ids = Vec::new();
    let mut index = lbound;
    while index <= ubound {
        let mut value: i32 = 0;
        unsafe { SafeArrayGetElement(psa, &raw const index, (&raw mut value).cast()) }
            .map_err(|_| ())?;
        ids.push(value);
        index += 1;
    }
    Ok(ids)
}

fn value_pattern(element: &IUIAutomationElement) -> Option<String> {
    let pattern: IUIAutomationValuePattern =
        unsafe { element.GetCurrentPatternAs(UIA_ValuePatternId) }.ok()?;
    unsafe { pattern.CurrentValue() }
        .ok()
        .map(|s| s.to_string())
}

pub(crate) struct StaGuard;

impl StaGuard {
    pub(crate) fn enter() -> Result<Self, HandsError> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .map_err(|err| HandsError::Uia(format!("CoInitializeEx(STA): {err}")))?;
        Ok(Self)
    }
}

impl Drop for StaGuard {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::Detail;

    #[test]
    #[ignore = "live desktop; not a CI gate. Run: cargo test -- --ignored"]
    fn live_notepad_not_required_in_ci() {
        let snap =
            collect(Detail::Default, crate::foreground::foreground_hwnd()).expect("live UIA");
        let found = snap.nodes.iter().any(|n| {
            let role = n.role.to_ascii_lowercase();
            let name = n.name.to_ascii_lowercase();
            (role.contains("document") || role.contains("edit") || name.contains("notepad"))
                && n.rect.w > 0
                && n.rect.h > 0
        });
        assert!(
            found,
            "expected a Notepad Document/Edit (or window) with positive area"
        );
    }

    #[test]
    #[ignore = "live desktop; not a CI gate. Run: cargo test -- --ignored"]
    fn live_resolve_notepad_runtime_id() {
        let snap = collect(Detail::Dom, crate::foreground::foreground_hwnd()).expect("live UIA");
        let node = snap.nodes.iter().find(|n| {
            let role = n.role.to_ascii_lowercase();
            let name = n.name.to_ascii_lowercase();
            (role.contains("document") || role.contains("edit") || name.contains("notepad"))
                && n.rect.area() > 0
                && !n.runtime_id.is_empty()
        });
        let Some(node) = node else {
            // No Notepad on this desktop; still prove resolve errors on a missing id.
            let err = resolve_runtime_id(&[9_999_999, 1]).expect_err("missing id");
            assert!(err.to_string().contains("no UIA element"), "{err}");
            return;
        };
        let found = resolve_runtime_id(&node.runtime_id).expect("resolve");
        assert!(found.rect.area() > 0);
    }

    #[test]
    #[ignore = "live desktop; not a CI gate. Run: cargo test -- --ignored"]
    fn live_two_observe_notepad_same_runtime_id() {
        let first = collect(Detail::Dom, crate::foreground::foreground_hwnd()).expect("live UIA");
        let second = collect(Detail::Dom, crate::foreground::foreground_hwnd()).expect("live UIA");
        let finder = |n: &crate::extract::RawNode| {
            let role = n.role.to_ascii_lowercase();
            let name = n.name.to_ascii_lowercase();
            (role.contains("document") || role.contains("edit") || name.contains("notepad"))
                && n.rect.area() > 0
                && !n.runtime_id.is_empty()
        };
        let node_a = first.nodes.iter().find(|n| finder(n));
        let node_b = second.nodes.iter().find(|n| finder(n));
        let (Some(node_a), Some(node_b)) = (node_a, node_b) else {
            // No Notepad on this desktop; still prove resolve errors on a missing id.
            let err = resolve_runtime_id(&[9_999_999, 1]).expect_err("missing id");
            let msg = err.to_string();
            assert!(msg.contains("no UIA element"), "{msg}");
            let lower = msg.to_ascii_lowercase();
            assert!(
                lower.contains("re-observe") || lower.contains("navigation"),
                "{msg}"
            );
            return;
        };
        assert_eq!(node_a.runtime_id, node_b.runtime_id);
        assert_eq!(node_a.element_id(), node_b.element_id());
    }

    #[test]
    fn first_named_ancestor_role_wins() {
        assert_eq!(
            pick_named(&[("", "text"), ("Submit", "button")]),
            ("Submit".into(), "button".into())
        );
        assert_eq!(
            pick_named(&[("Send", "text")]),
            ("Send".into(), "text".into())
        );
        assert_eq!(
            pick_named(&[("", "text"), ("Reply on LinkedIn", "document")]),
            ("Reply on LinkedIn".into(), "document".into())
        );
    }

    #[test]
    fn hwnd_null_is_empty_handle() {
        let hwnd = windows::Win32::Foundation::HWND::default();
        assert!(hwnd.is_invalid());
    }

    #[test]
    fn prepend_owned_popup_uses_true_client() {
        let src = include_str!("uia.rs");
        let start = src
            .find("fn prepend_owned_popup(")
            .expect("prepend_owned_popup");
        let end = src
            .find("fn create_automation(")
            .expect("create_automation");
        let slice = &src[start..end];
        assert!(
            slice.contains("client_rect"),
            "owned popup membership must use the true client:\n{slice}"
        );
        assert!(
            !slice.contains("GetWindowRect") && !slice.contains("hwnd_outer_rect"),
            "owned popup membership must not use the outer rect:\n{slice}"
        );
    }

    #[test]
    fn collect_default_and_title_do_not_call_get_foreground_window() {
        let src = include_str!("uia.rs");
        let start = src.find("fn collect_default(").expect("collect_default");
        let end = src.find("fn prepend_owned_popup(").expect("prepend");
        let def = &src[start..end];
        assert!(
            !def.contains("GetForegroundWindow"),
            "collect_default must use the passed HWND:\n{def}"
        );
        let t_start = src
            .find("fn window_element_name(")
            .expect("window_element_name");
        let t_end = src.find("fn walk_control_view(").expect("walk");
        let title = &src[t_start..t_end];
        assert!(
            !title.contains("GetForegroundWindow"),
            "window_element_name must use the passed HWND:\n{title}"
        );
    }

    #[test]
    fn detail_dom_with_hwnd_does_not_use_desktop_root() {
        let src = include_str!("uia.rs");
        let start = src.find("Detail::Dom =>").expect("Detail::Dom");
        let slice = &src[start..start + 450];
        assert!(
            slice.contains("ElementFromHandle"),
            "Dom + hwnd must ElementFromHandle:\n{slice}"
        );
        assert!(
            slice.contains("GetRootElement"),
            "Dom + None still uses GetRootElement:\n{slice}"
        );
        assert!(
            slice.contains("Some(h)"),
            "Dom must branch on hwnd:\n{slice}"
        );
    }

    #[test]
    fn document_rect_bounded_no_root() {
        let src = include_str!("uia.rs");
        let start = src
            .find("fn sta_document_rect(")
            .expect("sta_document_rect");
        let end = src
            .find("pub fn resolve_runtime_id(")
            .expect("resolve_runtime_id");
        let slice = &src[start..end];
        assert!(slice.contains("ElementFromHandle"));
        assert!(!slice.contains("GetRootElement"));
        assert!(slice.contains("depth >= 8"));
        assert!(slice.contains("visited > 256"));
    }

    #[test]
    fn document_rect_deadline_returns_none() {
        let (tx, rx) = mpsc::channel();
        let start = std::time::Instant::now();
        let out = recv_document(&rx, Duration::from_millis(10));
        assert!(out.is_none());
        assert!(start.elapsed() >= Duration::from_millis(10));
        drop(tx);
    }

    #[test]
    fn document_rect_runs_on_named_sta_worker() {
        let src = include_str!("uia.rs");
        let start = src.find("pub fn document_rect(").expect("document_rect");
        let end = src.find("fn recv_document(").expect("recv_document");
        let slice = &src[start..end];
        assert!(slice.contains("\"hands-uia-document\""));
        assert!(slice.contains("recv_document("));
        let worker = &src[src.find("fn sta_document_rect(").unwrap()
            ..src.find("pub fn resolve_runtime_id(").unwrap()];
        assert!(worker.contains("StaGuard::enter"));
    }

    fn found(own_name: &str, own_role: &str) -> ResolvedElement {
        ResolvedElement {
            rect: Rect {
                x: 0,
                y: 0,
                w: 10,
                h: 10,
            },
            name: "ancestor display".into(),
            role: "ancestor role".into(),
            hwnd: Some(0x1234),
            runtime_id: vec![1, 2],
            own_name: own_name.into(),
            own_role: own_role.into(),
            is_password: false,
        }
    }

    #[test]
    fn fingerprint_uses_own_name_and_role_not_ancestor() {
        let mut a = found("Send", "Button");
        let mut b = found("Send", "Button");
        b.name = "different ancestor".into();
        b.role = "different ancestor role".into();
        assert_eq!(a.own_fingerprint(), b.own_fingerprint());
        b.own_name = "Cancel".into();
        assert_ne!(a.own_fingerprint(), b.own_fingerprint());
        a.own_role = "Hyperlink".into();
        assert_ne!(a.own_fingerprint(), b.own_fingerprint());
    }

    #[test]
    fn fingerprint_role_matches_map_element_fallback() {
        assert_eq!(ControlKind::from_uia_id(50_000).type_name(), "Button");
        assert_eq!(ControlKind::from_uia_id(50_004).type_name(), "Edit");
        let src = include_str!("uia.rs");
        let start = src
            .find("pub(crate) fn fingerprint_role")
            .expect("fingerprint_role");
        let end = src
            .find("/// First non-empty name wins")
            .expect("name_and_role doc");
        let slice = &src[start..end];
        assert!(slice.contains("CurrentLocalizedControlType"));
        assert!(slice.contains("ControlKind::from_uia_id"));
    }

    #[test]
    fn role_of_fallback_is_unchanged() {
        let src = include_str!("uia.rs");
        let start = src.find("fn role_of(").expect("role_of");
        let end = src
            .find("pub(crate) fn fingerprint_role")
            .expect("fingerprint_role");
        let slice = &src[start..end];
        assert!(slice.contains("unwrap_or_else(|| \"element\".into())"));
    }

    #[test]
    fn scoped_walk_roots_at_element_from_handle() {
        let src = include_str!("uia.rs");
        let start = src
            .find("fn sta_resolve_scoped(")
            .expect("sta_resolve_scoped");
        let end = src.find("fn scoped_candidate(").expect("scoped_candidate");
        let body = &src[start..end];
        assert!(body.contains("ElementFromHandle"));
        assert!(!body.contains("GetRootElement"));
        assert!(body.contains("SCOPED_NODE_CAP"));
    }

    #[test]
    fn dispatch_resolve_on_roots_at_hwnd_and_bounds_walk() {
        let src = include_str!("uia.rs");
        let start = src.find("pub(crate) fn resolve_on(").expect("resolve_on");
        let end = src.find("fn pick_named(").expect("pick_named");
        let body = &src[start..end];
        assert!(body.contains("ElementFromHandle"));
        assert!(body.contains("find_runtime_ids"));
        assert!(body.contains("SCOPED_NODE_CAP"));
        assert!(body.contains("ambiguous"));
        let actions = include_str!("uia_actions.rs");
        assert!(actions.contains("resolve_on(&automation, &want, outer_hwnd)"));
    }

    #[test]
    fn uia_workers_are_bounded() {
        let src = include_str!("uia.rs");
        assert!(src.contains("MAX_IN_FLIGHT_WORKERS"));
        assert!(src.contains("WorkerSlot::acquire"));
        let actions = include_str!("uia_actions.rs");
        assert!(actions.contains("uia::WorkerSlot::acquire"));
    }

    #[test]
    fn scoped_walk_cap_is_unknown_not_first_match() {
        let err =
            scoped_matches_outcome(Vec::new(), true, 0x1234, "00000000").expect_err("cap hit");
        assert_eq!(err.state, IdentityState::Unknown);
        assert_eq!(err.reason, "provider_timeout");

        let err = scoped_matches_outcome(vec![found("Send", "Button")], true, 0x1234, "00000000")
            .expect_err("cap hit after one match");
        assert_eq!(err.state, IdentityState::Unknown);
        assert_eq!(err.reason, "provider_timeout");
    }

    #[test]
    fn scoped_matches_duplicate_is_ambiguous() {
        let err = scoped_matches_outcome(
            vec![found("Send", "Button"), found("Send", "Button")],
            false,
            0x1234,
            "00000000",
        )
        .expect_err("duplicate");
        assert_eq!(err.state, IdentityState::Ambiguous);
        assert_eq!(err.reason, "duplicate_runtime_id");
    }

    #[test]
    #[ignore = "live desktop; H5 owner evidence"]
    fn live_scoped_uia_ref_notepad() {
        // Owner HITL: resolve a scoped ref against a real Notepad window.
    }
}
