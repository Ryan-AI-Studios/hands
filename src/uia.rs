//! UIA control-view walk.
//!
//! COM is initialized STA (`COINIT_APARTMENTTHREADED`) on a dedicated OS thread
//! because tokio's multi-thread runtime is the wrong apartment for IUIAutomation.

use windows::Win32::Foundation::RECT;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize, SAFEARRAY,
};
use windows::Win32::System::Ole::{
    SafeArrayDestroy, SafeArrayGetElement, SafeArrayGetLBound, SafeArrayGetUBound,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTreeWalker,
    IUIAutomationValuePattern, UIA_ValuePatternId,
};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

use crate::error::HandsError;
use crate::extract::{ControlKind, Detail, MAIN_TEXT_MAX_CHARS, RawNode};
use crate::space::Rect;

pub struct UiaSnapshot {
    pub title: String,
    pub nodes: Vec<RawNode>,
}

#[derive(Debug, Clone)]
pub struct ResolvedElement {
    pub rect: Rect,
    pub role: String,
    pub hwnd: Option<isize>,
}

pub fn collect(detail: Detail) -> Result<UiaSnapshot, HandsError> {
    let cap = detail.element_cap();
    std::thread::Builder::new()
        .name("hands-uia-sta".into())
        .spawn(move || sta_collect(detail, cap))
        .map_err(|err| HandsError::Uia(format!("spawn STA thread: {err}")))?
        .join()
        .map_err(|_| HandsError::Uia("UIA STA thread panicked".to_string()))?
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
    let walker = unsafe { automation.ControlViewWalker() }
        .map_err(|err| HandsError::Uia(format!("ControlViewWalker: {err}")))?;
    let root = unsafe { automation.GetRootElement() }
        .map_err(|err| HandsError::Uia(format!("GetRootElement: {err}")))?;
    let Some(element) = find_runtime_id(&walker, &root, want) else {
        return Err(HandsError::Target(format!(
            "no UIA element with RuntimeId {}",
            want.iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(".")
        )));
    };
    let rect = unsafe {
        element
            .CurrentBoundingRectangle()
            .map(rect_from_uia)
            .map_err(|err| HandsError::Uia(format!("CurrentBoundingRectangle: {err}")))?
    };
    let role = unsafe {
        element
            .CurrentLocalizedControlType()
            .ok()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "element".into())
    };
    let hwnd = native_hwnd(&walker, &element);
    Ok(ResolvedElement { rect, role, hwnd })
}

fn find_runtime_id(
    walker: &IUIAutomationTreeWalker,
    root: &IUIAutomationElement,
    want: &[i32],
) -> Option<IUIAutomationElement> {
    let mut stack = vec![root.clone()];
    while let Some(element) = stack.pop() {
        if let Some(id) = runtime_id(&element)
            && id == want
        {
            return Some(element);
        }
        push_children(walker, &element, &mut stack);
    }
    None
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

fn sta_collect(detail: Detail, cap: usize) -> Result<UiaSnapshot, HandsError> {
    let _sta = StaGuard::enter()?;
    let automation = create_automation()?;
    let title = foreground_title(&automation);
    let walker = unsafe { automation.ControlViewWalker() }
        .map_err(|err| HandsError::Uia(format!("ControlViewWalker: {err}")))?;
    let root = unsafe { automation.GetRootElement() }
        .map_err(|err| HandsError::Uia(format!("GetRootElement: {err}")))?;
    let nodes = walk_control_view(&walker, &root, detail, cap)?;
    Ok(UiaSnapshot { title, nodes })
}

fn create_automation() -> Result<IUIAutomation, HandsError> {
    unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
        .map_err(|err| HandsError::Uia(format!("CoCreateInstance(CUIAutomation): {err}")))
}

fn foreground_title(automation: &IUIAutomation) -> String {
    let hwnd = unsafe { GetForegroundWindow() };
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

struct StaGuard;

impl StaGuard {
    fn enter() -> Result<Self, HandsError> {
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
        let snap = collect(Detail::Default).expect("live UIA");
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
        let snap = collect(Detail::Dom).expect("live UIA");
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
    fn hwnd_null_is_empty_handle() {
        let hwnd = windows::Win32::Foundation::HWND::default();
        assert!(hwnd.is_invalid());
    }
}
