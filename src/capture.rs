use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
}

/// Capture the virtual-screen union via GDI BitBlt and write an unlabeled PNG.
pub fn capture_virtual_screen(space: Space) -> Result<CapturePaths, HandsError> {
    ensure_dpi()?;
    let (width, height) = dims(space)?;
    let pixels = blit_rect(space.origin_x, space.origin_y, width, height)?;
    let paths = observe_paths()?;
    write_png(&paths.screenshot_path, width, height, pixels)?;
    Ok(paths)
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

fn write_png(path: &Path, width: i32, height: i32, pixels: Vec<u8>) -> Result<(), HandsError> {
    let img = RgbaImage::from_raw(width as u32, height as u32, pixels).ok_or_else(|| {
        HandsError::Capture("pixel buffer size does not match virtual-screen size".to_string())
    })?;
    img.save(path)
        .map_err(|err| HandsError::Capture(format!("PNG encode failed: {err}")))
}

pub fn observe_paths() -> Result<CapturePaths, HandsError> {
    let dir = std::env::temp_dir().join("hands").join("observe");
    std::fs::create_dir_all(&dir)
        .map_err(|err| HandsError::Capture(format!("create observe dir: {err}")))?;
    let stamp = utc_compact();
    let nonce = format!("{:08x}", uuid::Uuid::new_v4().as_fields().0);
    let stem = format!("observe-{stamp}-{nonce}");
    Ok(CapturePaths {
        screenshot_path: dir.join(format!("{stem}.png")),
        observe_path: dir.join(format!("{stem}.json")),
    })
}

pub fn display_path(path: &Path) -> String {
    match std::path::absolute(path) {
        Ok(abs) => abs.to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

fn utc_compact() -> String {
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
