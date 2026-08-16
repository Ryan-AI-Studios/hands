//! Offer a window to the foreground. Failure is not a hard error.

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, IsIconic, SW_RESTORE, SetForegroundWindow,
    ShowWindow, WindowFromPoint,
};

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
