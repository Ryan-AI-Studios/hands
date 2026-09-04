//! Offer a window to the foreground. Failure is not a hard error.

use std::path::Path;

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GA_ROOT, GetAncestor, GetClassNameW, GetForegroundWindow, GetWindowRect,
    GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, IsZoomed, SW_RESTORE,
    SetForegroundWindow, ShowWindow, WindowFromPoint,
};

use crate::space::Rect;

pub fn offer(hwnd: Option<isize>, point: (i32, i32)) -> bool {
    let hwnd = hwnd
        .map(raw_hwnd)
        .filter(|h| hwnd_raw(*h).is_some())
        .or_else(|| {
            let h = unsafe {
                WindowFromPoint(windows::Win32::Foundation::POINT {
                    x: point.0,
                    y: point.1,
                })
            };
            hwnd_raw(h).map(|_| h)
        });
    let Some(hwnd) = hwnd else {
        return false;
    };
    if unsafe { IsIconic(hwnd) }.as_bool() {
        unsafe {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
    }
    if unsafe { SetForegroundWindow(hwnd) }.as_bool() {
        return true;
    }
    attach_retry(hwnd)
}

fn attach_retry(hwnd: HWND) -> bool {
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() {
        return false;
    }
    let fg_tid = unsafe { GetWindowThreadProcessId(fg, None) };
    let cur = unsafe { GetCurrentThreadId() };
    if fg_tid == 0 || fg_tid == cur {
        return unsafe { SetForegroundWindow(hwnd) }.as_bool();
    }
    let _attach = AttachGuard::connect(fg_tid, cur);
    unsafe { SetForegroundWindow(hwnd) }.as_bool()
}

struct AttachGuard {
    fg_tid: u32,
    cur: u32,
    attached: bool,
}

impl AttachGuard {
    fn connect(fg_tid: u32, cur: u32) -> Self {
        let attached = unsafe { AttachThreadInput(fg_tid, cur, true) }.as_bool();
        Self {
            fg_tid,
            cur,
            attached,
        }
    }
}

impl Drop for AttachGuard {
    fn drop(&mut self) {
        if self.attached {
            unsafe {
                let _ = AttachThreadInput(self.fg_tid, self.cur, false);
            }
        }
    }
}

pub fn raw_hwnd(raw: isize) -> HWND {
    HWND(raw as *mut core::ffi::c_void)
}

pub fn hwnd_raw(hwnd: HWND) -> Option<isize> {
    if hwnd.is_invalid() {
        None
    } else {
        Some(hwnd.0 as isize)
    }
}

pub fn same_top_level(a: Option<isize>, b: Option<isize>) -> bool {
    let (Some(a), Some(b)) = (a, b) else {
        return false;
    };
    if a == b {
        return true;
    }
    let ra = hwnd_raw(unsafe { GetAncestor(raw_hwnd(a), GA_ROOT) });
    let rb = hwnd_raw(unsafe { GetAncestor(raw_hwnd(b), GA_ROOT) });
    match (ra, rb) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

pub fn foreground_hwnd() -> Option<isize> {
    hwnd_raw(unsafe { GetForegroundWindow() })
}

/// Outer rect via `GetWindowRect`. Invalid / fail / non-positive → `None`.
pub fn window_rect(hwnd: isize) -> Option<Rect> {
    let hwnd = raw_hwnd(hwnd);
    hwnd_raw(hwnd)?;
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &raw mut rect) }.is_err() {
        return None;
    }
    let w = rect.right.saturating_sub(rect.left);
    let h = rect.bottom.saturating_sub(rect.top);
    if w <= 0 || h <= 0 {
        return None;
    }
    Some(Rect {
        x: rect.left,
        y: rect.top,
        w,
        h,
    })
}

/// Foreground window outer rect via `GetWindowRect`. Invalid / fail / non-positive → `None`.
pub fn viewport_rect() -> Option<Rect> {
    foreground_hwnd().and_then(window_rect)
}

/// Caption via `GetWindowTextW` (256 wchar, same as `class_name`). `None` = current FG.
/// Invalid HWND / empty caption → empty string (title gate does not fire).
pub fn title(hwnd: Option<isize>) -> String {
    let hwnd = match hwnd {
        Some(raw) => {
            let h = raw_hwnd(raw);
            if hwnd_raw(h).is_none() {
                return String::new();
            }
            h
        }
        None => {
            let h = unsafe { GetForegroundWindow() };
            if h.is_invalid() {
                return String::new();
            }
            h
        }
    };
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if n <= 0 {
        String::new()
    } else {
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

/// True when the foreground window is daily Chrome: class `Chrome_WidgetWin_1`
/// **and** process image `chrome.exe` (not Cursor / VS Code / Electron).
pub fn is_chrome() -> bool {
    match foreground_hwnd() {
        Some(hwnd) => is_chrome_hwnd(hwnd),
        None => false,
    }
}

/// Class `Chrome_WidgetWin_1` **and** process image `chrome.exe`.
pub fn is_chrome_hwnd(hwnd: isize) -> bool {
    let h = raw_hwnd(hwnd);
    if hwnd_raw(h).is_none() {
        return false;
    }
    is_chrome_class_and_image(
        &class_name(h),
        crate::attach::process_image(window_pid(hwnd)).as_deref(),
    )
}

pub(crate) fn is_chrome_class_and_image(class: &str, image: Option<&Path>) -> bool {
    class == crate::attach::CHROME_CLASS && image.is_some_and(crate::attach::is_chrome_image)
}

pub fn window_pid(hwnd: isize) -> u32 {
    let h = raw_hwnd(hwnd);
    if hwnd_raw(h).is_none() {
        return 0;
    }
    let mut pid = 0u32;
    let _ = unsafe { GetWindowThreadProcessId(h, Some(&raw mut pid)) };
    pid
}

pub(crate) fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

pub(crate) fn class_name_of(hwnd: isize) -> String {
    let h = raw_hwnd(hwnd);
    if hwnd_raw(h).is_none() {
        String::new()
    } else {
        class_name(h)
    }
}

pub(crate) const WINDOW_TITLE_CAP: usize = 40;
pub(crate) const WINDOW_LIST_CAP: usize = 12;

#[derive(Debug, Clone)]
pub(crate) struct TitledWindow {
    pub hwnd: isize,
    pub pid: u32,
    pub title: String,
    pub class: String,
    pub iconic: bool,
    pub zoomed: bool,
    pub rect: Option<crate::space::Rect>,
}

pub(crate) fn inventory_rect(
    iconic: bool,
    rect: Option<crate::space::Rect>,
) -> Option<crate::space::Rect> {
    let r = rect?;
    if iconic || r.w <= 0 || r.x <= -32000 {
        None
    } else {
        Some(r)
    }
}

pub(crate) fn cap_window_title(title: &str) -> String {
    title.chars().take(WINDOW_TITLE_CAP).collect()
}

pub(crate) fn sort_titled_windows(windows: &mut [TitledWindow]) {
    windows.sort_by(|a, b| a.pid.cmp(&b.pid).then_with(|| a.title.cmp(&b.title)));
}

#[cfg(test)]
thread_local! {
    static TITLED_HOOK: std::cell::RefCell<Option<Vec<TitledWindow>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_titled_windows_hook(windows: Option<Vec<TitledWindow>>) {
    TITLED_HOOK.with(|c| *c.borrow_mut() = windows);
}

pub(crate) fn titled_windows() -> Vec<TitledWindow> {
    #[cfg(test)]
    {
        if let Some(list) = TITLED_HOOK.with(|c| c.borrow().clone()) {
            return list;
        }
    }
    titled_windows_live()
}

fn titled_windows_live() -> Vec<TitledWindow> {
    let mut hwnds: Vec<HWND> = Vec::new();
    let _ = unsafe { EnumWindows(Some(collect_top_level), LPARAM(&raw mut hwnds as isize)) };
    let mut out = Vec::new();
    for hwnd in hwnds {
        if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
            continue;
        }
        let Some(raw) = hwnd_raw(hwnd) else {
            continue;
        };
        let title = title(Some(raw));
        if title.is_empty() {
            continue;
        }
        let iconic = unsafe { IsIconic(hwnd) }.as_bool();
        let zoomed = unsafe { IsZoomed(hwnd) }.as_bool();
        out.push(TitledWindow {
            hwnd: raw,
            pid: window_pid(raw),
            title,
            class: class_name(hwnd),
            iconic,
            zoomed,
            rect: inventory_rect(iconic, window_rect(raw)),
        });
    }
    sort_titled_windows(&mut out);
    out
}

unsafe extern "system" fn collect_top_level(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
    let list = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
    list.push(hwnd);
    true.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_class_is_widget_win_1_not_zero() {
        assert_eq!(crate::attach::CHROME_CLASS, "Chrome_WidgetWin_1");
        assert_ne!(crate::attach::CHROME_CLASS, "Chrome_WidgetWin_0");
        let _ = is_chrome();
        let _ = viewport_rect();
        assert_eq!(title(Some(0)), "");
        let _ = title(None);
    }

    #[test]
    fn same_top_level_none_and_equal_hwnd() {
        assert!(!same_top_level(None, Some(1)));
        assert!(!same_top_level(None, None));
        assert!(same_top_level(Some(7), Some(7)));
    }

    #[test]
    fn is_chrome_requires_class_and_chrome_exe() {
        assert!(is_chrome_class_and_image(
            crate::attach::CHROME_CLASS,
            Some(Path::new(
                r"C:\Program Files\Google\Chrome\Application\chrome.exe"
            )),
        ));
        assert!(
            !is_chrome_class_and_image(
                crate::attach::CHROME_CLASS,
                Some(Path::new(
                    r"C:\Users\me\AppData\Local\Programs\cursor\Cursor.exe"
                )),
            ),
            "Cursor.exe with Chrome_WidgetWin_1 is not daily Chrome"
        );
        assert!(!is_chrome_class_and_image(
            "Chrome_WidgetWin_0",
            Some(Path::new(r"C:\chrome.exe")),
        ));
        assert!(!is_chrome_class_and_image(
            crate::attach::CHROME_CLASS,
            None,
        ));
        assert!(!is_chrome_hwnd(0));
    }

    #[test]
    fn inventory_rect_omits_iconic_zero_width_and_offscreen_sentinel() {
        let ok = crate::space::Rect {
            x: 10,
            y: 20,
            w: 800,
            h: 600,
        };
        assert_eq!(inventory_rect(false, Some(ok)), Some(ok));
        assert_eq!(inventory_rect(true, Some(ok)), None);
        assert_eq!(
            inventory_rect(
                false,
                Some(crate::space::Rect {
                    x: 10,
                    y: 20,
                    w: 0,
                    h: 600,
                })
            ),
            None
        );
        assert_eq!(
            inventory_rect(
                false,
                Some(crate::space::Rect {
                    x: -32000,
                    y: -32000,
                    w: 160,
                    h: 28,
                })
            ),
            None
        );
        assert_eq!(inventory_rect(false, None), None);
    }

    #[test]
    fn titled_windows_sort_pid_then_title_and_cap_title() {
        let mut rows = vec![
            TitledWindow {
                hwnd: 3,
                pid: 20,
                title: "B".into(),
                class: "X".into(),
                iconic: false,
                zoomed: false,
                rect: None,
            },
            TitledWindow {
                hwnd: 1,
                pid: 10,
                title: "Z".into(),
                class: "X".into(),
                iconic: false,
                zoomed: false,
                rect: None,
            },
            TitledWindow {
                hwnd: 2,
                pid: 10,
                title: "A".into(),
                class: "X".into(),
                iconic: false,
                zoomed: false,
                rect: None,
            },
        ];
        sort_titled_windows(&mut rows);
        assert_eq!(
            rows.iter()
                .map(|w| (w.pid, w.title.as_str()))
                .collect::<Vec<_>>(),
            vec![(10, "A"), (10, "Z"), (20, "B")]
        );
        let long = "a".repeat(50);
        assert_eq!(cap_window_title(&long).chars().count(), WINDOW_TITLE_CAP);
        assert_eq!(WINDOW_TITLE_CAP, 40);
        assert_eq!(WINDOW_LIST_CAP, 12);
    }

    #[test]
    fn titled_windows_hook_avoids_live_enum() {
        set_titled_windows_hook(Some(vec![TitledWindow {
            hwnd: 7,
            pid: 3,
            title: "Hooked".into(),
            class: "X".into(),
            iconic: false,
            zoomed: true,
            rect: None,
        }]));
        let list = titled_windows();
        set_titled_windows_hook(None);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "Hooked");
        assert!(list[0].zoomed);
    }

    #[test]
    fn titled_windows_live_does_not_raise() {
        let src = include_str!("foreground.rs");
        let start = src
            .find("fn titled_windows_live(")
            .expect("titled_windows_live");
        let end = src
            .find("fn collect_top_level(")
            .expect("collect_top_level");
        let slice = &src[start..end];
        assert!(
            !slice.contains("ShowWindow") && !slice.contains("SetForegroundWindow"),
            "inventory must not raise:\n{slice}"
        );
    }
}
