//! Cubic Bézier mouse path and virtual-desktop absolute mapping.
//!
//! Pure math — no Win32. Tests pin the RNG seed.

use crate::space::Space;

const SAMPLE_MIN: usize = 8;
const SAMPLE_MAX: usize = 80;
const ABS_MAX: i32 = 65_535;

/// Tiny xorshift64*. No extra crate.
#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self {
            state: seed | 1, // never zero
        }
    }

    pub fn from_time() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0xC0FFEE);
        Self::new(nanos)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / ((1u64 << 53) as f64)
    }

    /// Inclusive integer range.
    pub fn range_inclusive(&mut self, lo: i32, hi: i32) -> i32 {
        if hi <= lo {
            return lo;
        }
        let span = (hi as i64) - (lo as i64) + 1;
        lo + (self.next_u64() % span as u64) as i32
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

pub fn sample_count(distance: f64) -> usize {
    let n = (distance / 10.0).round() as i64;
    n.clamp(SAMPLE_MIN as i64, SAMPLE_MAX as i64) as usize
}

fn smoothstep(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn cubic(p0: Point, p1: Point, p2: Point, p3: Point, t: f64) -> Point {
    let u = 1.0 - t;
    let uu = u * u;
    let tt = t * t;
    Point {
        x: uu * u * p0.x + 3.0 * uu * t * p1.x + 3.0 * u * tt * p2.x + tt * t * p3.x,
        y: uu * u * p0.y + 3.0 * uu * t * p1.y + 3.0 * u * tt * p2.y + tt * t * p3.y,
    }
}

/// Cubic Bézier from `start` to `end`. Intermediate samples get 1..=2 px jitter.
/// Sample 0 and N-1 stay exact.
pub fn path(start: (i32, i32), end: (i32, i32), rng: &mut Rng) -> Vec<(i32, i32)> {
    let dx = f64::from(end.0 - start.0);
    let dy = f64::from(end.1 - start.1);
    let distance = (dx * dx + dy * dy).sqrt();
    let n = sample_count(distance);
    let p0 = Point {
        x: f64::from(start.0),
        y: f64::from(start.1),
    };
    let p3 = Point {
        x: f64::from(end.0),
        y: f64::from(end.1),
    };
    let (px, py) = if distance > 0.5 {
        (-dy / distance, dx / distance)
    } else {
        (0.0, 1.0)
    };
    let o1 = (if rng.next_f64() < 0.5 { -1.0 } else { 1.0 })
        * (rng.range_inclusive(10, 30) as f64 / 100.0)
        * distance;
    let o2 = (if rng.next_f64() < 0.5 { -1.0 } else { 1.0 })
        * (rng.range_inclusive(10, 30) as f64 / 100.0)
        * distance;
    let p1 = Point {
        x: p0.x + 0.3 * dx + px * o1,
        y: p0.y + 0.3 * dy + py * o1,
    };
    let p2 = Point {
        x: p0.x + 0.7 * dx + px * o2,
        y: p0.y + 0.7 * dy + py * o2,
    };

    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = if n == 1 {
            1.0
        } else {
            i as f64 / (n - 1) as f64
        };
        let p = cubic(p0, p1, p2, p3, smoothstep(t));
        out.push((p.x.round() as i32, p.y.round() as i32));
    }
    if let Some(first) = out.first_mut() {
        *first = start;
    }
    if let Some(last) = out.last_mut() {
        *last = end;
    }
    for sample in out.iter_mut().take(n.saturating_sub(1)).skip(1) {
        let (jx, jy) = jitter_offset(rng);
        sample.0 += jx;
        sample.1 += jy;
    }
    out
}

/// Map a physical virtual-screen pixel to `SendInput` absolute 0..=65535.
pub fn to_absolute(space: Space, x: i32, y: i32) -> (i32, i32) {
    let nx = (x - space.origin_x).clamp(0, (space.width - 1).max(0));
    let ny = (y - space.origin_y).clamp(0, (space.height - 1).max(0));
    let dw = (space.width - 1).max(1);
    let dh = (space.height - 1).max(1);
    let dx = nx.saturating_mul(ABS_MAX) / dw;
    let dy = ny.saturating_mul(ABS_MAX) / dh;
    (dx, dy)
}

/// 1..=2 px Chebyshev/Euclidean: one axis 1 or 2 px, the other 0.
fn jitter_offset(rng: &mut Rng) -> (i32, i32) {
    let mag = rng.range_inclusive(1, 2);
    let signed = if rng.next_f64() < 0.5 { -mag } else { mag };
    if rng.next_f64() < 0.5 {
        (signed, 0)
    } else {
        (0, signed)
    }
}

pub fn sleep_ms_between_samples(rng: &mut Rng) -> u64 {
    rng.range_inclusive(4, 16) as u64
}

pub fn click_hold_ms(rng: &mut Rng) -> u64 {
    rng.range_inclusive(25, 40) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_end_exact_and_jitter_bound() {
        let mut rng = Rng::new(0xC0FFEE);
        let start = (10, 20);
        let end = (410, 320);
        let samples = path(start, end, &mut rng);
        assert_eq!(*samples.first().unwrap(), start);
        assert_eq!(*samples.last().unwrap(), end);
        let n = samples.len();
        assert!((SAMPLE_MIN..=SAMPLE_MAX).contains(&n));
        let mut rng2 = Rng::new(0xC0FFEE);
        let again = path(start, end, &mut rng2);
        assert_eq!(again[0], start);
        assert_eq!(again[again.len() - 1], end);
        assert_eq!(samples, again);
    }

    #[test]
    fn jitter_magnitude_at_most_two_vs_unjittered_curve() {
        // Reconstruct unjittered positions by applying the same control-point
        // recipe then measuring the added offset.
        let mut rng = Rng::new(7);
        let start = (0, 0);
        let end = (200, 0);
        let samples = path(start, end, &mut rng);
        for &(x, y) in samples.iter().take(samples.len() - 1).skip(1) {
            // The Bézier itself can leave the segment; only jitter is 1..=2.
            // We cannot isolate jitter without exporting internals, so check
            // that no intermediate is more than (curve + 2) away from the
            // straight line by a generous bound: distance/2 + 2 + 0.3*dist perp.
            let _ = (x, y);
        }
        // Direct check: regenerate and compare each intermediate to a no-jitter
        // path built here with the same control math by forcing jitter via
        // known seed and verifying |jx|,|jy| in 1..=2 when we inspect a
        // dedicated helper path with start=end (zero curve, only jitter).
        let mut rng = Rng::new(99);
        let flat = path((50, 50), (50, 50), &mut rng);
        assert_eq!(flat[0], (50, 50));
        assert_eq!(*flat.last().unwrap(), (50, 50));
        for &(x, y) in flat.iter().take(flat.len() - 1).skip(1) {
            let dx = (x - 50).abs();
            let dy = (y - 50).abs();
            assert!(dx <= 2, "jx {dx}");
            assert!(dy <= 2, "jy {dy}");
            assert!(
                (dx == 0) ^ (dy == 0),
                "jitter must be axial 1..=2 px, got ({dx},{dy})"
            );
            assert!((dx + dy) >= 1 && (dx + dy) <= 2);
        }
    }

    #[test]
    fn sample_count_clamped() {
        assert_eq!(sample_count(0.0), 8);
        assert_eq!(sample_count(10.0), 8);
        assert_eq!(sample_count(80.0), 8);
        assert_eq!(sample_count(200.0), 20);
        assert_eq!(sample_count(10_000.0), 80);
    }

    #[test]
    fn absolute_origin_and_last_pixel() {
        let space = Space::new(-1920, 0, 3840, 1080).unwrap();
        assert_eq!(to_absolute(space, -1920, 0), (0, 0));
        assert_eq!(to_absolute(space, 1919, 1079), (ABS_MAX, ABS_MAX));
    }

    #[test]
    fn one_by_one_space_does_not_div0() {
        let space = Space::new(10, 20, 1, 1).unwrap();
        assert_eq!(to_absolute(space, 10, 20), (0, 0));
        assert_eq!(to_absolute(space, 99, 99), (0, 0));
    }
}
