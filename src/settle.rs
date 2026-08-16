//! `wait_settle`: in-memory ROI pixel-diff until motion stops.

use std::time::{Duration, Instant};

use crate::capture::{RoiFrame, capture_roi};
use crate::error::HandsError;
use crate::lease;
use crate::space::{Rect, Space};

pub const SETTLE_TIMEOUT: Duration = Duration::from_secs(2);
pub const FRAME_GAP: Duration = Duration::from_millis(50);
pub const CHANNEL_DELTA: u8 = 8;
pub const RATIO_LIMIT: f64 = 0.005;
pub const STREAK_NEEDED: u32 = 2;
pub const INFLATE_PAD: i32 = 8;
pub const CURSOR_ROI: i32 = 64;

/// Max-channel delta > 8 counts as changed. Ratio is changed / pixels.
pub fn changed_ratio(a: &[u8], b: &[u8]) -> f64 {
    let n = a.len().min(b.len()) / 4;
    if n == 0 {
        return 1.0;
    }
    let mut changed = 0usize;
    for i in 0..n {
        let o = i * 4;
        let d0 = a[o].abs_diff(b[o]);
        let d1 = a[o + 1].abs_diff(b[o + 1]);
        let d2 = a[o + 2].abs_diff(b[o + 2]);
        let d3 = a[o + 3].abs_diff(b[o + 3]);
        if d0.max(d1).max(d2).max(d3) > CHANNEL_DELTA {
            changed += 1;
        }
    }
    changed as f64 / n as f64
}

pub fn default_roi(space: Space, last_target: Option<Rect>, cursor: (i32, i32)) -> Rect {
    if let Some(rect) = last_target {
        let inflated = space.inflate_clip(rect, INFLATE_PAD);
        if inflated.area() > 0 {
            return inflated;
        }
    }
    let half = CURSOR_ROI / 2;
    space.clip_rect(Rect {
        x: cursor.0 - half,
        y: cursor.1 - half,
        w: CURSOR_ROI,
        h: CURSOR_ROI,
    })
}

pub fn wait_settle(space: Space, roi: Rect) -> Result<(bool, RoiFrame), HandsError> {
    lease::poll()?;
    if space.clip_rect(roi).area() == 0 {
        return Err(HandsError::Settle("settle ROI has zero area".into()));
    }
    let mut prev = capture_roi(space, roi)?;
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    let mut streak = 0u32;
    loop {
        std::thread::sleep(FRAME_GAP);
        lease::poll()?;
        let curr = capture_roi(space, roi)?;
        let ratio = if prev.width == curr.width && prev.height == curr.height {
            changed_ratio(&prev.pixels, &curr.pixels)
        } else {
            1.0
        };
        if ratio < RATIO_LIMIT {
            streak += 1;
            if streak >= STREAK_NEEDED {
                return Ok((true, curr));
            }
        } else {
            streak = 0;
        }
        prev = curr;
        if Instant::now() >= deadline {
            return Ok((false, prev));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(n: usize, r: u8, g: u8, b: u8, a: u8) -> Vec<u8> {
        let mut v = Vec::with_capacity(n * 4);
        for _ in 0..n {
            v.extend_from_slice(&[r, g, b, a]);
        }
        v
    }

    #[test]
    fn identical_buffers_ratio_zero() {
        let a = rgba(100, 10, 20, 30, 255);
        assert_eq!(changed_ratio(&a, &a), 0.0);
    }

    #[test]
    fn small_delta_does_not_count() {
        let a = rgba(100, 10, 20, 30, 255);
        let mut b = a.clone();
        b[0] = 18; // delta 8, not greater than 8
        assert_eq!(changed_ratio(&a, &b), 0.0);
        b[0] = 19; // delta 9
        assert!((changed_ratio(&a, &b) - 0.01).abs() < 1e-9);
    }

    #[test]
    fn half_percent_threshold() {
        // 1000 pixels; 5 changed = 0.5% — must be strictly less than 0.5%.
        let a = rgba(1000, 0, 0, 0, 255);
        let mut b = a.clone();
        for i in 0..5 {
            b[i * 4] = 20;
        }
        assert!((changed_ratio(&a, &b) - 0.005).abs() < 1e-12);
        assert!(changed_ratio(&a, &b) >= RATIO_LIMIT);
        b[5 * 4] = 0; // still 5
        let mut c = a.clone();
        for i in 0..4 {
            c[i * 4] = 20;
        }
        assert!(changed_ratio(&a, &c) < RATIO_LIMIT);
    }
}
