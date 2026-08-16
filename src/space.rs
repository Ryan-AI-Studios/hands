use std::sync::OnceLock;

use serde::Serialize;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::core::BOOL;

use crate::error::HandsError;

pub const CELL_PX: i32 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn area(self) -> i64 {
        i64::from(self.w.max(0)) * i64::from(self.h.max(0))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Space {
    pub origin_x: i32,
    pub origin_y: i32,
    pub width: i32,
    pub height: i32,
    pub cell_px: i32,
}

impl Space {
    pub fn new(origin_x: i32, origin_y: i32, width: i32, height: i32) -> Result<Self, HandsError> {
        if width <= 0 || height <= 0 {
            return Err(HandsError::Space(format!(
                "virtual screen has non-positive size {width}x{height}"
            )));
        }
        Ok(Self {
            origin_x,
            origin_y,
            width,
            height,
            cell_px: CELL_PX,
        })
    }

    pub fn cell_id(self, x: i32, y: i32) -> String {
        let col = (x - self.origin_x).div_euclid(self.cell_px);
        let row = (y - self.origin_y).div_euclid(self.cell_px);
        format!("g:{col}:{row}")
    }

    pub fn cell_rect(self, col: i32, row: i32) -> Rect {
        let x = self.origin_x + col * self.cell_px;
        let y = self.origin_y + row * self.cell_px;
        let right = self.origin_x + self.width;
        let bottom = self.origin_y + self.height;
        Rect {
            x,
            y,
            w: (right - x).clamp(0, self.cell_px),
            h: (bottom - y).clamp(0, self.cell_px),
        }
    }

    pub fn contains(self, rect: Rect) -> bool {
        let left = self.origin_x;
        let top = self.origin_y;
        let right = self.origin_x + self.width;
        let bottom = self.origin_y + self.height;
        rect.x >= left && rect.y >= top && rect.x + rect.w <= right && rect.y + rect.h <= bottom
    }
}

static DPI: OnceLock<Result<(), String>> = OnceLock::new();

/// Set PMv2 once, before any metric or capture. Later calls reuse the first result.
pub fn ensure_dpi() -> Result<(), HandsError> {
    match DPI.get_or_init(|| {
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
            .map_err(|err| err.to_string())
    }) {
        Ok(()) => Ok(()),
        Err(err) => Err(HandsError::Dpi(err.clone())),
    }
}

/// Virtual-screen union from `EnumDisplayMonitors` + `MONITORINFO.rcMonitor`.
/// Do not use `GetSystemMetrics(SM_*VIRTUALSCREEN)` — it is not DPI-aware on PMv2.
pub fn virtual_screen() -> Result<Space, HandsError> {
    ensure_dpi()?;
    let mut acc = UnionRect::empty();
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(enum_monitor),
            LPARAM(&raw mut acc as isize),
        )
    };
    if !ok.as_bool() {
        return Err(HandsError::Space("EnumDisplayMonitors failed".to_string()));
    }
    acc.into_space()
}

#[derive(Clone, Copy)]
struct UnionRect {
    seen: bool,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl UnionRect {
    const fn empty() -> Self {
        Self {
            seen: false,
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        }
    }

    fn include(&mut self, rect: RECT) {
        if !self.seen {
            self.seen = true;
            self.left = rect.left;
            self.top = rect.top;
            self.right = rect.right;
            self.bottom = rect.bottom;
            return;
        }
        self.left = self.left.min(rect.left);
        self.top = self.top.min(rect.top);
        self.right = self.right.max(rect.right);
        self.bottom = self.bottom.max(rect.bottom);
    }

    fn into_space(self) -> Result<Space, HandsError> {
        if !self.seen {
            return Err(HandsError::Space("no monitors enumerated".to_string()));
        }
        Space::new(
            self.left,
            self.top,
            self.right.saturating_sub(self.left),
            self.bottom.saturating_sub(self.top),
        )
    }
}

unsafe extern "system" fn enum_monitor(
    monitor: HMONITOR,
    _hdc: HDC,
    _clip: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let Some(acc) = (unsafe { (data.0 as *mut UnionRect).as_mut() }) else {
        return BOOL(0);
    };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let ok = unsafe { GetMonitorInfoW(monitor, &raw mut info) };
    if ok.as_bool() {
        acc.include(info.rcMonitor);
    }
    BOOL(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_negative_origin_cell_id() {
        let space = Space::new(-1920, 0, 3840, 1080).unwrap();
        assert_eq!(space.cell_id(-1920, 0), "g:0:0");
        assert_eq!(space.cell_id(-1821, 0), "g:0:0");
        assert_eq!(space.cell_id(-1820, 0), "g:1:0");
        assert_eq!(space.cell_id(-1, 99), "g:19:0");
        assert_eq!(space.cell_id(0, 0), "g:19:0");
        assert_eq!(space.cell_id(0, 100), "g:19:1");
    }

    #[test]
    fn grid_g00_rect() {
        let space = Space::new(-1920, -200, 3840, 1280).unwrap();
        assert_eq!(
            space.cell_rect(0, 0),
            Rect {
                x: -1920,
                y: -200,
                w: 100,
                h: 100
            }
        );
    }

    #[test]
    fn grid_last_partial_cell() {
        let space = Space::new(0, 0, 250, 180).unwrap();
        assert_eq!(
            space.cell_rect(2, 0),
            Rect {
                x: 200,
                y: 0,
                w: 50,
                h: 100
            }
        );
        assert_eq!(
            space.cell_rect(0, 1),
            Rect {
                x: 0,
                y: 100,
                w: 100,
                h: 80
            }
        );
        assert_eq!(
            space.cell_rect(2, 1),
            Rect {
                x: 200,
                y: 100,
                w: 50,
                h: 80
            }
        );
        assert_eq!(space.cell_id(249, 179), "g:2:1");
    }
}
