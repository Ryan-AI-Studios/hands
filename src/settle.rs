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

pub fn roi_unchanged(a: &RoiFrame, b: &RoiFrame) -> bool {
    a.width == b.width && a.height == b.height && changed_ratio(&a.pixels, &b.pixels) < RATIO_LIMIT
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

/// Foreground-window ROI for standalone `wait_settle`. Does not use last-target or cursor.
pub fn default_wait_roi(space: Space, fg: Option<Rect>) -> Result<Rect, HandsError> {
    let Some(rect) = fg else {
        return Err(HandsError::Settle(
            "wait_settle default ROI needs a foreground window".into(),
        ));
    };
    let clipped = space.clip_rect(rect);
    if clipped.area() == 0 {
        return Err(HandsError::Settle(
            "wait_settle default ROI has zero area after clipping the foreground window".into(),
        ));
    }
    Ok(clipped)
}

/// Cloudflare interstitial captions that must not report `settled: true`.
pub fn title_blocks_settled(title: &str) -> bool {
    crate::challenge::title_is_interstitial(title)
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

    fn frame(width: i32, height: i32, pixels: Vec<u8>) -> RoiFrame {
        RoiFrame {
            width,
            height,
            pixels,
        }
    }

    #[test]
    fn roi_unchanged_matches_same_expression() {
        let a_pix = rgba(1000, 0, 0, 0, 255);
        let mut below = a_pix.clone();
        for i in 0..4 {
            below[i * 4] = 20;
        }
        let a = frame(10, 100, a_pix.clone());
        let b = frame(10, 100, below);
        let same = a.width == b.width
            && a.height == b.height
            && changed_ratio(&a.pixels, &b.pixels) < RATIO_LIMIT;
        assert_eq!(roi_unchanged(&a, &b), same);
        assert!(same);

        let mut at_limit = a_pix.clone();
        for i in 0..5 {
            at_limit[i * 4] = 20;
        }
        let c = frame(10, 100, at_limit);
        let same_c = a.width == c.width
            && a.height == c.height
            && changed_ratio(&a.pixels, &c.pixels) < RATIO_LIMIT;
        assert_eq!(roi_unchanged(&a, &c), same_c);
        assert!(!same_c);

        let d = frame(20, 50, a_pix);
        let same_d = a.width == d.width
            && a.height == d.height
            && changed_ratio(&a.pixels, &d.pixels) < RATIO_LIMIT;
        assert_eq!(roi_unchanged(&a, &d), same_d);
        assert!(!same_d);
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

    fn desk() -> Space {
        Space::new(0, 0, 1920, 1080).expect("desk")
    }

    #[test]
    fn default_wait_roi_clips_foreground() {
        let space = desk();
        let fg = Rect {
            x: 100,
            y: 80,
            w: 800,
            h: 600,
        };
        let roi = default_wait_roi(space, Some(fg)).expect("clip");
        assert_eq!(roi, space.clip_rect(fg));
        let overhang = Rect {
            x: 1800,
            y: 80,
            w: 800,
            h: 600,
        };
        let clipped = default_wait_roi(space, Some(overhang)).expect("partial clip");
        assert_eq!(clipped, space.clip_rect(overhang));
        assert!(clipped.area() > 0);
        assert_eq!(clipped.w, 120);
    }

    #[test]
    fn default_wait_roi_none_or_zero_names_foreground() {
        let space = desk();
        let none_err = default_wait_roi(space, None).expect_err("none");
        assert!(none_err.to_string().contains("foreground"), "{none_err}");
        let zero = Rect {
            x: 3000,
            y: 0,
            w: 10,
            h: 10,
        };
        let zero_err = default_wait_roi(space, Some(zero)).expect_err("zero");
        assert!(zero_err.to_string().contains("foreground"), "{zero_err}");
        assert_eq!(space.clip_rect(zero).area(), 0);
    }

    #[test]
    fn default_roi_prefers_last_target_inflate_over_cursor() {
        let space = desk();
        let last = Rect {
            x: 100,
            y: 100,
            w: 20,
            h: 20,
        };
        let cursor = (900, 700);
        let roi = default_roi(space, Some(last), cursor);
        let inflated = space.inflate_clip(last, INFLATE_PAD);
        assert_eq!(roi, inflated);
        let half = CURSOR_ROI / 2;
        let around_cursor = space.clip_rect(Rect {
            x: cursor.0 - half,
            y: cursor.1 - half,
            w: CURSOR_ROI,
            h: CURSOR_ROI,
        });
        assert_ne!(roi, around_cursor);
    }

    #[test]
    fn title_blocks_settled_table() {
        let cases = [
            ("Just a moment...", true),
            ("Performing security verification", true),
            ("Checking if the site connection is secure", true),
            ("cars.com: Camry", false),
            ("Continue as Ryan", false),
            ("Accept cookies", false),
            ("", false),
        ];
        for (title, blocked) in cases {
            assert_eq!(title_blocks_settled(title), blocked, "title {title:?}");
        }
    }

    #[test]
    fn wait_settle_pixel_loop_does_not_title_gate() {
        let src = include_str!("settle.rs");
        let start = src.find("pub fn wait_settle(").expect("wait_settle");
        let rest = &src[start..];
        let end = rest.find("\n#[cfg(test)]").unwrap_or(rest.len());
        let body = &rest[..end];
        assert!(
            !body.contains("title_blocks_settled"),
            "title gate must stay out of settle::wait_settle:\n{body}"
        );
        assert!(
            !body.contains("challenge::"),
            "wait_settle must not call challenge:\n{body}"
        );
    }
}
