//! In-memory VLM-bound raster preprocess. Not a public MCP tool.
//!
//! Order: env skip → 3×3 median (≤1080p) → JPEG 85 (w,h ≥ 8) → scale-restore.
//! On-disk format stays PNG. Dimensions stay the capture size.

use image::codecs::jpeg::JpegEncoder;
use image::imageops::{self, FilterType};
use image::{ExtendedColorType, RgbImage, Rgba, RgbaImage};

use crate::bezier::Rng;

pub const JPEG_QUALITY: u8 = 85;
pub const PREPROCESS_ENV: &str = "HANDS_PREPROCESS";
const MEDIAN_MAX_PIXELS: u64 = 1920 * 1080;
const JPEG_MIN_SIDE: u32 = 8;

pub struct PreprocessOpts {
    pub median: bool,
    pub jpeg: bool,
    pub scale: f64,
    pub rng: Option<Rng>,
}

pub fn for_vlm(img: RgbaImage) -> RgbaImage {
    if preprocess_disabled() {
        return img;
    }
    let mut rng = Rng::from_time();
    let k = rng.range_inclusive(0, 4);
    let scale = 0.98 + f64::from(k) * 0.01;
    for_vlm_with(
        img,
        PreprocessOpts {
            median: true,
            jpeg: true,
            scale,
            rng: Some(rng),
        },
    )
}

pub fn for_vlm_with(img: RgbaImage, opts: PreprocessOpts) -> RgbaImage {
    let _ = opts.rng;
    let mut out = img;
    if opts.median {
        out = median3x3(out);
    }
    if opts.jpeg {
        out = jpeg_roundtrip(out);
    }
    if (opts.scale - 1.0).abs() > f64::EPSILON {
        out = scale_restore(out, opts.scale);
    }
    out
}

fn preprocess_disabled() -> bool {
    match std::env::var(PREPROCESS_ENV) {
        Ok(raw) => matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        Err(_) => false,
    }
}

fn median3x3(img: RgbaImage) -> RgbaImage {
    let (w, h) = img.dimensions();
    if u64::from(w).saturating_mul(u64::from(h)) > MEDIAN_MAX_PIXELS {
        return img;
    }
    if w < 3 || h < 3 {
        return img;
    }
    let mut out = img.clone();
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let mut r = [0u8; 9];
            let mut g = [0u8; 9];
            let mut b = [0u8; 9];
            let mut i = 0usize;
            for dy in 0..3u32 {
                for dx in 0..3u32 {
                    let p = img.get_pixel(x + dx - 1, y + dy - 1);
                    r[i] = p[0];
                    g[i] = p[1];
                    b[i] = p[2];
                    i += 1;
                }
            }
            r.sort_unstable();
            g.sort_unstable();
            b.sort_unstable();
            let a = img.get_pixel(x, y)[3];
            out.put_pixel(x, y, Rgba([r[4], g[4], b[4], a]));
        }
    }
    out
}

fn jpeg_roundtrip(img: RgbaImage) -> RgbaImage {
    let (w, h) = img.dimensions();
    if w < JPEG_MIN_SIDE || h < JPEG_MIN_SIDE {
        return img;
    }
    let rgb = rgba_to_rgb(&img);
    let mut buf = Vec::new();
    let mut enc = JpegEncoder::new_with_quality(&mut buf, JPEG_QUALITY);
    if enc
        .encode(rgb.as_raw(), w, h, ExtendedColorType::Rgb8)
        .is_err()
    {
        return img;
    }
    let Ok(decoded) = image::load_from_memory(&buf) else {
        return img;
    };
    let mut rgba = decoded.to_rgba8();
    for px in rgba.pixels_mut() {
        px[3] = 255;
    }
    if rgba.dimensions() != (w, h) {
        rgba = imageops::resize(&rgba, w, h, FilterType::Triangle);
        for px in rgba.pixels_mut() {
            px[3] = 255;
        }
    }
    rgba
}

fn rgba_to_rgb(img: &RgbaImage) -> RgbImage {
    let (w, h) = img.dimensions();
    let mut raw = Vec::with_capacity(w as usize * h as usize * 3);
    for p in img.pixels() {
        raw.extend_from_slice(&[p[0], p[1], p[2]]);
    }
    RgbImage::from_raw(w, h, raw).unwrap_or_else(|| RgbImage::new(w, h))
}

fn scale_restore(img: RgbaImage, factor: f64) -> RgbaImage {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return img;
    }
    let nw = scaled_side(w, factor);
    let nh = scaled_side(h, factor);
    if nw == w && nh == h {
        return img;
    }
    let scaled = imageops::resize(&img, nw, nh, FilterType::Triangle);
    imageops::resize(&scaled, w, h, FilterType::Triangle)
}

fn scaled_side(side: u32, factor: f64) -> u32 {
    let n = (f64::from(side) * factor).round();
    n.clamp(1.0, f64::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn checker(w: u32, h: u32) -> RgbaImage {
        let mut img = RgbaImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = if (x + y) % 2 == 0 { 0 } else { 255 };
                img.put_pixel(x, y, Rgba([v, v, v, 255]));
            }
        }
        img
    }

    fn neighbor_energy(img: &RgbaImage) -> u64 {
        let (w, h) = img.dimensions();
        let mut e = 0u64;
        for y in 0..h {
            for x in 0..w {
                let p = img.get_pixel(x, y);
                if x + 1 < w {
                    let n = img.get_pixel(x + 1, y);
                    e += abs_diff(p, n);
                }
                if y + 1 < h {
                    let n = img.get_pixel(x, y + 1);
                    e += abs_diff(p, n);
                }
            }
        }
        e
    }

    fn abs_diff(a: &Rgba<u8>, b: &Rgba<u8>) -> u64 {
        (a[0].abs_diff(b[0]) as u64) + (a[1].abs_diff(b[1]) as u64) + (a[2].abs_diff(b[2]) as u64)
    }

    fn with_env<T>(val: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os(PREPROCESS_ENV);
        match val {
            Some(v) => unsafe { std::env::set_var(PREPROCESS_ENV, v) },
            None => unsafe { std::env::remove_var(PREPROCESS_ENV) },
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        match prev {
            Some(v) => unsafe { std::env::set_var(PREPROCESS_ENV, v) },
            None => unsafe { std::env::remove_var(PREPROCESS_ENV) },
        }
        match result {
            Ok(v) => v,
            Err(p) => std::panic::resume_unwind(p),
        }
    }

    fn opts(median: bool, jpeg: bool, scale: f64) -> PreprocessOpts {
        PreprocessOpts {
            median,
            jpeg,
            scale,
            rng: None,
        }
    }

    #[test]
    fn dimensions_preserved_1080x720_8x8_7x7() {
        for (w, h) in [(1080u32, 720u32), (8, 8), (7, 7)] {
            let img = checker(w, h);
            let out = for_vlm_with(img, opts(true, true, 1.00));
            assert_eq!(out.dimensions(), (w, h), "{w}x{h}");
        }
    }

    #[test]
    fn hands_preprocess_off_is_identity() {
        let img = checker(32, 32);
        let raw = img.as_raw().clone();
        with_env(Some("0"), || {
            let out = for_vlm(img.clone());
            assert_eq!(out.as_raw(), &raw);
        });
        with_env(Some(" Off "), || {
            let out = for_vlm(img.clone());
            assert_eq!(out.as_raw(), &raw);
        });
        with_env(Some("FALSE"), || {
            let out = for_vlm(img.clone());
            assert_eq!(out.as_raw(), &raw);
        });
        with_env(Some("no"), || {
            let out = for_vlm(img.clone());
            assert_eq!(out.as_raw(), &raw);
        });
    }

    #[test]
    fn jpeg_drops_checkerboard_energy() {
        let img = checker(64, 64);
        let before = neighbor_energy(&img);
        let out = for_vlm_with(img, opts(false, true, 1.00));
        assert_eq!(out.dimensions(), (64, 64));
        let after = neighbor_energy(&out);
        assert!(
            after < before,
            "JPEG 85 should drop neighbor energy: {before} -> {after}"
        );
    }

    #[test]
    fn scale_1_02_restore_same_wxh_not_identical() {
        let img = checker(32, 32);
        let raw = img.as_raw().clone();
        let out = for_vlm_with(img, opts(false, false, 1.02));
        assert_eq!(out.dimensions(), (32, 32));
        assert_ne!(out.as_raw(), &raw);
    }

    #[test]
    fn width_7_skips_jpeg() {
        let img = checker(7, 7);
        let raw = img.as_raw().clone();
        let out = for_vlm_with(img, opts(false, true, 1.00));
        assert_eq!(out.dimensions(), (7, 7));
        assert_eq!(out.as_raw(), &raw);
    }

    #[test]
    fn settle_and_capture_roi_do_not_call_preprocess() {
        let settle = include_str!("settle.rs");
        assert!(
            !settle.contains("preprocess::"),
            "wait_settle must stay on raw capture_roi"
        );
        let capture = include_str!("capture.rs");
        let start = capture
            .find("pub fn capture_roi")
            .expect("capture_roi present");
        let rest = &capture[start..];
        let end = rest.find("\nfn ").unwrap_or(rest.len());
        let body = &rest[..end];
        assert!(
            !body.contains("for_vlm") && !body.contains("preprocess::"),
            "capture_roi must not preprocess:\n{body}"
        );
    }

    #[test]
    fn cargo_jpeg_feature_no_imageproc_or_rand() {
        let cargo = include_str!("../Cargo.toml");
        assert!(
            cargo.contains("features = [\"png\", \"jpeg\"]"),
            "image jpeg feature"
        );
        assert!(!cargo.contains("imageproc"), "do not add imageproc");
        assert!(!cargo.contains("rand ="), "do not add rand crate");
    }

    #[test]
    #[ignore = "execute-local 1080p timing; not a CI gate"]
    fn time_1080p_for_vlm() {
        let img = checker(1920, 1080);
        let start = std::time::Instant::now();
        let out = for_vlm_with(img, opts(true, true, 1.00));
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(out.dimensions(), (1920, 1080));
        eprintln!("0012 for_vlm 1920x1080 elapsed: {ms:.1} ms");
    }

    #[test]
    fn mcp_and_cli_mention_untrusted() {
        let mcp = include_str!("mcp.rs");
        let main = include_str!("main.rs");
        let mcp_l = mcp.to_ascii_lowercase();
        let main_l = main.to_ascii_lowercase();
        assert!(
            mcp_l.contains("untrusted"),
            "mcp observe/pick/ground must mention untrusted"
        );
        assert!(
            main_l.contains("untrusted"),
            "cli observe/pick/ground about must mention untrusted"
        );
    }
}
