//! Offer a window to the foreground. Failure is not a hard error.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId,
    IsIconic, SW_RESTORE, SetForegroundWindow, ShowWindow, WindowFromPoint,
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

/// True when the foreground class is `Chrome_WidgetWin_1` (not `_0`).
pub fn is_chrome() -> bool {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        return false;
    }
    class_name(hwnd) == crate::attach::CHROME_CLASS
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
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
}
