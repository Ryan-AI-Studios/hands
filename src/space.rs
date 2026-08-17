use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

    pub fn center(self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Space {
    pub origin_x: i32,
    pub origin_y: i32,
    pub width: i32,
    pub height: i32,
    #[serde(default = "default_cell_px")]
    pub cell_px: i32,
}

fn default_cell_px() -> i32 {
    CELL_PX
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
        let x = self
            .origin_x
            .saturating_add(col.saturating_mul(self.cell_px));
        let y = self
            .origin_y
            .saturating_add(row.saturating_mul(self.cell_px));
        let right = self.origin_x.saturating_add(self.width);
        let bottom = self.origin_y.saturating_add(self.height);
        Rect {
            x,
            y,
            w: right.saturating_sub(x).clamp(0, self.cell_px),
            h: bottom.saturating_sub(y).clamp(0, self.cell_px),
        }
    }

    pub fn contains(self, rect: Rect) -> bool {
        let left = self.origin_x;
        let top = self.origin_y;
        let right = self.origin_x + self.width;
        let bottom = self.origin_y + self.height;
        rect.x >= left && rect.y >= top && rect.x + rect.w <= right && rect.y + rect.h <= bottom
    }

    pub fn contains_point(self, x: i32, y: i32) -> bool {
        x >= self.origin_x
            && y >= self.origin_y
            && x < self.origin_x + self.width
            && y < self.origin_y + self.height
    }

    /// Parse `g:{col}:{row}` with signed integers. `g:0:0` is valid.
    pub fn parse_cell_id(id: &str) -> Result<(i32, i32), HandsError> {
        let rest = id.strip_prefix("g:").ok_or_else(|| {
            HandsError::Target(format!("grid id must start with g: (got '{id}')"))
        })?;
        if rest.is_empty() {
            return Err(HandsError::Target("grid id is empty after g:".into()));
        }
        let mut parts = rest.split(':');
        let col_s = parts
            .next()
            .ok_or_else(|| HandsError::Target("grid id missing column".into()))?;
        let row_s = parts
            .next()
            .ok_or_else(|| HandsError::Target("grid id missing row".into()))?;
        if parts.next().is_some() {
            return Err(HandsError::Target(format!(
                "grid id has extra tokens (got '{id}')"
            )));
        }
        if col_s.is_empty() || row_s.is_empty() {
            return Err(HandsError::Target(format!(
                "grid id has empty token (got '{id}')"
            )));
        }
        let col = col_s.parse::<i32>().map_err(|_| {
            HandsError::Target(format!("grid column is not an integer (got '{id}')"))
        })?;
        let row = row_s
            .parse::<i32>()
            .map_err(|_| HandsError::Target(format!("grid row is not an integer (got '{id}')")))?;
        Ok((col, row))
    }

    pub fn clip_rect(self, rect: Rect) -> Rect {
        let left = rect.x.max(self.origin_x);
        let top = rect.y.max(self.origin_y);
        let right = (rect.x.saturating_add(rect.w)).min(self.origin_x.saturating_add(self.width));
        let bottom = (rect.y.saturating_add(rect.h)).min(self.origin_y.saturating_add(self.height));
        Rect {
            x: left,
            y: top,
            w: (right - left).max(0),
            h: (bottom - top).max(0),
        }
    }

    pub fn inflate_clip(self, rect: Rect, pad: i32) -> Rect {
        self.clip_rect(Rect {
            x: rect.x.saturating_sub(pad),
            y: rect.y.saturating_sub(pad),
            w: rect.w.saturating_add(pad.saturating_mul(2)),
            h: rect.h.saturating_add(pad.saturating_mul(2)),
        })
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
        assert_eq!(space.cell_rect(2, 1).center(), (225, 140));
    }

    #[test]
    fn parse_cell_id_signed_and_rejects() {
        assert_eq!(Space::parse_cell_id("g:0:0").unwrap(), (0, 0));
        assert_eq!(Space::parse_cell_id("g:-19:2").unwrap(), (-19, 2));
        assert!(Space::parse_cell_id("0:0").is_err());
        assert!(Space::parse_cell_id("g:").is_err());
        assert!(Space::parse_cell_id("g:1").is_err());
        assert!(Space::parse_cell_id("g:1:2:3").is_err());
        assert!(Space::parse_cell_id("g:1:").is_err());
        assert!(Space::parse_cell_id("").is_err());
    }

    #[test]
    fn point_on_edge_inside_one_px_outside() {
        let space = Space::new(-1920, 0, 3840, 1080).unwrap();
        assert!(space.contains_point(-1920, 0));
        assert!(space.contains_point(1919, 1079));
        assert!(!space.contains_point(1920, 1079));
        assert!(!space.contains_point(1919, 1080));
        assert!(!space.contains_point(-1921, 0));
        let last = space.cell_rect(38, 10);
        assert!(last.area() > 0);
        assert!(space.contains(last));
    }

    #[test]
    fn extreme_grid_cell_does_not_panic() {
        let space = Space::new(0, 0, 250, 180).unwrap();
        let huge = space.cell_rect(i32::MAX, i32::MIN);
        assert_eq!(huge.area(), 0);
    }
}
