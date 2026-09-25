use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use image::RgbaImage;

use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap,
    CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, HBITMAP, HDC,
    HGDIOBJ, ReleaseDC, SRCCOPY, SelectObject,
};

use crate::error::HandsError;
use crate::space::{Rect, Space, ensure_dpi};

pub struct RoiFrame {
    pub width: i32,
    pub height: i32,
    pub pixels: Vec<u8>,
}

pub struct CapturePaths {
    pub screenshot_path: PathBuf,
    pub observe_path: PathBuf,
    pub preview_path: PathBuf,
}

/// BitBlt vs `for_vlm` vs PNG encode+disk. `screenshot_ms` is their sum.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaptureTiming {
    pub blit_ms: u64,
    pub preprocess_ms: u64,
    pub encode_ms: u64,
}

impl CaptureTiming {
    pub fn screenshot_ms(self) -> u64 {
        self.blit_ms
            .saturating_add(self.preprocess_ms)
            .saturating_add(self.encode_ms)
    }
}

/// Capture the virtual-screen union via GDI BitBlt and write an unlabeled PNG.
pub fn capture_virtual_screen(space: Space) -> Result<CapturePaths, HandsError> {
    Ok(capture_virtual_screen_timed(space, false)?.0)
}

pub fn capture_virtual_screen_timed(
    space: Space,
    keep_raw: bool,
) -> Result<(CapturePaths, CaptureTiming, Vec<u8>, i32, i32), HandsError> {
    ensure_dpi()?;
    let (width, height) = dims(space)?;
    let blit_t = Instant::now();
    let pixels = blit_rect(space.origin_x, space.origin_y, width, height)?;
    let blit_ms = blit_t.elapsed().as_millis() as u64;
    let paths = observe_paths()?;
    let (raw, preprocess_ms, encode_ms) = if keep_raw {
        let timing = write_png(&paths.screenshot_path, width, height, pixels.clone())?;
        (pixels, timing.0, timing.1)
    } else {
        let timing = write_png(&paths.screenshot_path, width, height, pixels)?;
        (Vec::new(), timing.0, timing.1)
    };
    Ok((
        paths,
        CaptureTiming {
            blit_ms,
            preprocess_ms,
            encode_ms,
        },
        raw,
        width,
        height,
    ))
}

/// Crop `rect` (virtual-screen coords) from a raw RGBA blit of `buf_w`×`buf_h`
/// whose (0,0) pixel is `origin`. `None` if the rect is empty or out of bounds.
pub fn crop_rgba(
    pixels: &[u8],
    buf_w: i32,
    buf_h: i32,
    origin_x: i32,
    origin_y: i32,
    rect: Rect,
) -> Option<(i32, i32, Vec<u8>)> {
    if rect.area() == 0 || buf_w <= 0 || buf_h <= 0 {
        return None;
    }
    let x = rect.x.checked_sub(origin_x)?;
    let y = rect.y.checked_sub(origin_y)?;
    if x < 0 || y < 0 || x.saturating_add(rect.w) > buf_w || y.saturating_add(rect.h) > buf_h {
        return None;
    }
    let expected = (buf_w as usize)
        .checked_mul(buf_h as usize)?
        .checked_mul(4)?;
    if pixels.len() != expected {
        return None;
    }
    let mut out = Vec::with_capacity(
        (rect.w as usize)
            .saturating_mul(rect.h as usize)
            .saturating_mul(4),
    );
    let src_stride = buf_w as usize * 4;
    let dst_stride = rect.w as usize * 4;
    let x0 = x as usize;
    for row in 0..rect.h as usize {
        let start = (y as usize + row) * src_stride + x0 * 4;
        let end = start + dst_stride;
        out.extend_from_slice(pixels.get(start..end)?);
    }
    Some((rect.w, rect.h, out))
}

pub fn write_preview_png(
    path: &Path,
    width: i32,
    height: i32,
    pixels: Vec<u8>,
) -> Result<(), HandsError> {
    write_png(path, width, height, pixels).map(|_| ())
}

/// In-memory RGBA ROI. No file. Clip to `virtual_screen`. Reject zero area.
pub fn capture_roi(space: Space, rect: Rect) -> Result<RoiFrame, HandsError> {
    ensure_dpi()?;
    let clipped = space.clip_rect(rect);
    if clipped.area() == 0 {
        return Err(HandsError::Capture("ROI has zero area after clip".into()));
    }
    let pixels = blit_rect(clipped.x, clipped.y, clipped.w, clipped.h)?;
    Ok(RoiFrame {
        width: clipped.w,
        height: clipped.h,
        pixels,
    })
}

fn dims(space: Space) -> Result<(i32, i32), HandsError> {
    if space.width <= 0 || space.height <= 0 {
        return Err(HandsError::Capture(format!(
            "zero-size virtual screen {}x{}",
            space.width, space.height
        )));
    }
    Ok((space.width, space.height))
}

fn blit_rect(origin_x: i32, origin_y: i32, width: i32, height: i32) -> Result<Vec<u8>, HandsError> {
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return Err(HandsError::Capture("GetDC(NULL) failed".to_string()));
        }
        let _screen = DcRelease { hdc: screen };

        let mem = CreateCompatibleDC(Some(screen));
        if mem.is_invalid() {
            return Err(HandsError::Capture("CreateCompatibleDC failed".to_string()));
        }
        let _mem = CompatibleDc(mem);

        let bitmap = CreateCompatibleBitmap(screen, width, height);
        if bitmap.is_invalid() {
            return Err(HandsError::Capture(
                "CreateCompatibleBitmap failed".to_string(),
            ));
        }
        let _bitmap = BitmapGuard(bitmap);

        let previous = SelectObject(mem, bitmap.into());
        if previous.is_invalid() {
            return Err(HandsError::Capture("SelectObject failed".to_string()));
        }
        let _restore = RestoreSelect { hdc: mem, previous };

        let rop = SRCCOPY | CAPTUREBLT;
        BitBlt(
            mem,
            0,
            0,
            width,
            height,
            Some(screen),
            origin_x,
            origin_y,
            rop,
        )
        .map_err(|err| HandsError::Capture(format!("BitBlt failed: {err}")))?;

        read_bgra(mem, bitmap, width, height)
    }
}

fn read_bgra(hdc: HDC, bitmap: HBITMAP, width: i32, height: i32) -> Result<Vec<u8>, HandsError> {
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let stride = width as usize * 4;
    let mut buf = vec![0u8; stride * height as usize];
    let lines = unsafe {
        GetDIBits(
            hdc,
            bitmap,
            0,
            height as u32,
            Some(buf.as_mut_ptr().cast()),
            &raw mut info,
            DIB_RGB_COLORS,
        )
    };
    if lines == 0 {
        return Err(HandsError::Capture("GetDIBits failed".to_string()));
    }
    for px in buf.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Ok(buf)
}

fn write_png(
    path: &Path,
    width: i32,
    height: i32,
    pixels: Vec<u8>,
) -> Result<(u64, u64), HandsError> {
    let prep_t = Instant::now();
    let img = RgbaImage::from_raw(width as u32, height as u32, pixels).ok_or_else(|| {
        HandsError::Capture("pixel buffer size does not match virtual-screen size".to_string())
    })?;
    let img = crate::preprocess::for_vlm(img);
    let preprocess_ms = prep_t.elapsed().as_millis() as u64;
    let enc_t = Instant::now();
    img.save(path)
        .map_err(|err| HandsError::Capture(format!("PNG encode failed: {err}")))?;
    let encode_ms = enc_t.elapsed().as_millis() as u64;
    Ok((preprocess_ms, encode_ms))
}

pub(crate) fn observe_dir() -> Result<PathBuf, HandsError> {
    let dir = std::env::temp_dir().join("hands").join("observe");
    std::fs::create_dir_all(&dir)
        .map_err(|err| HandsError::Capture(format!("create observe dir: {err}")))?;
    Ok(dir)
}

pub fn observe_paths() -> Result<CapturePaths, HandsError> {
    let dir = observe_dir()?;
    let stamp = utc_compact();
    let nonce = format!("{:08x}", uuid::Uuid::new_v4().as_fields().0);
    let stem = format!("observe-{stamp}-{nonce}");
    Ok(CapturePaths {
        screenshot_path: dir.join(format!("{stem}.png")),
        observe_path: dir.join(format!("{stem}.json")),
        preview_path: dir.join(format!("{stem}-preview.png")),
    })
}

pub fn display_path(path: &Path) -> String {
    match std::path::absolute(path) {
        Ok(abs) => abs.to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

pub(crate) fn utc_compact() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_unix_utc(secs)
}

fn format_unix_utc(secs: u64) -> String {
    let days = secs / 86_400;
    let tod = secs % 86_400;
    let (year, month, day) = civil_from_unix_days(days as i64);
    let hh = tod / 3_600;
    let mm = (tod % 3_600) / 60;
    let ss = tod % 60;
    format!("{year:04}{month:02}{day:02}T{hh:02}{mm:02}{ss:02}Z")
}

/// Howard Hinnant civil_from_days; unix day 0 is 1970-01-01.
fn civil_from_unix_days(unix_days: i64) -> (i32, u32, u32) {
    let z = unix_days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

struct DcRelease {
    hdc: HDC,
}

impl Drop for DcRelease {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseDC(None, self.hdc);
        }
    }
}

struct CompatibleDc(HDC);

impl Drop for CompatibleDc {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

struct BitmapGuard(HBITMAP);

impl Drop for BitmapGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.0.into());
        }
    }
}

struct RestoreSelect {
    hdc: HDC,
    previous: HGDIOBJ,
}

impl Drop for RestoreSelect {
    fn drop(&mut self) {
        unsafe {
            let _ = SelectObject(self.hdc, self.previous);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_timing_screenshot_ms_is_sum() {
        let t = CaptureTiming {
            blit_ms: 10,
            preprocess_ms: 20,
            encode_ms: 5,
        };
        assert_eq!(t.screenshot_ms(), 35);
    }

    #[test]
    fn crop_rgba_copies_interior_and_rejects_empty() {
        let mut pixels = vec![0u8; 20 * 10 * 4];
        for y in 0..10 {
            for x in 0..20 {
                let i = (y * 20 + x) * 4;
                pixels[i] = x as u8;
                pixels[i + 1] = y as u8;
                pixels[i + 2] = 7;
                pixels[i + 3] = 255;
            }
        }
        let (w, h, out) = crop_rgba(
            &pixels,
            20,
            10,
            0,
            0,
            Rect {
                x: 2,
                y: 3,
                w: 4,
                h: 2,
            },
        )
        .expect("crop");
        assert_eq!((w, h), (4, 2));
        assert_eq!(out.len(), 4 * 2 * 4);
        assert_eq!(&out[0..4], &[2, 3, 7, 255]);
        assert_eq!(&out[4..8], &[3, 3, 7, 255]);
        assert!(
            crop_rgba(
                &pixels,
                20,
                10,
                0,
                0,
                Rect {
                    x: 0,
                    y: 0,
                    w: 0,
                    h: 1,
                },
            )
            .is_none()
        );
        assert!(
            crop_rgba(
                &pixels,
                20,
                10,
                0,
                0,
                Rect {
                    x: 19,
                    y: 0,
                    w: 4,
                    h: 1,
                },
            )
            .is_none()
        );
    }

    #[test]
    fn utc_epoch() {
        assert_eq!(format_unix_utc(0), "19700101T000000Z");
        assert_eq!(format_unix_utc(86_400), "19700102T000000Z");
        assert_eq!(format_unix_utc(1_704_067_200), "20240101T000000Z");
    }

    #[test]
    fn capture_smoke_png_matches_virtual_screen() {
        crate::space::ensure_dpi().expect("dpi");
        let space = crate::space::virtual_screen().expect("virtual screen");
        let paths = capture_virtual_screen(space).expect("capture");
        assert!(
            paths.screenshot_path.is_file(),
            "missing {}",
            paths.screenshot_path.display()
        );
        let img = image::open(&paths.screenshot_path).expect("decode png");
        assert_eq!(img.width(), space.width as u32);
        assert_eq!(img.height(), space.height as u32);
        let _ = std::fs::remove_file(&paths.screenshot_path);
    }

    #[test]
    fn zero_size_is_error_not_panic() {
        let err = Space::new(0, 0, 0, 0).unwrap_err();
        assert!(err.to_string().contains("non-positive"));
    }
}
