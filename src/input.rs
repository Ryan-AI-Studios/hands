//! `SendInput` mouse/keyboard. Never `SetCursorPos` / `mouse_event` / `keybd_event`.

use std::time::Duration;

use windows::Win32::Foundation::{HANDLE, HGLOBAL, POINT};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput,
    VIRTUAL_KEY, VK_A, VK_BACK, VK_C, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME,
    VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB, VK_UP, VK_V, VK_X,
};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, WHEEL_DELTA};

use crate::bezier::{self, Rng};
use crate::error::HandsError;
use crate::lease;
use crate::space::Space;

pub const TYPE_UNICODE_MAX: usize = 32;
const CF_UNICODETEXT: u32 = 13;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypePath {
    Unicode,
    Clipboard,
}

pub fn type_path_for(text: &str) -> TypePath {
    if text.chars().count() <= TYPE_UNICODE_MAX {
        TypePath::Unicode
    } else {
        TypePath::Clipboard
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardOp {
    Set(Vec<u16>),
    Empty,
}

/// Snapshot/restore boundary (no live clipboard). `None` means empty the board.
pub fn restore_op(previous: Option<&[u16]>) -> ClipboardOp {
    match previous {
        Some(text) => ClipboardOp::Set(text.to_vec()),
        None => ClipboardOp::Empty,
    }
}

pub fn send_inputs(inputs: &[INPUT]) -> Result<(), HandsError> {
    if inputs.is_empty() {
        return Ok(());
    }
    let cb = std::mem::size_of::<INPUT>() as i32;
    let sent = unsafe { SendInput(inputs, cb) };
    if sent as usize != inputs.len() {
        return Err(HandsError::Input(format!(
            "SendInput returned {sent} of {} (UIPI or blocked)",
            inputs.len()
        )));
    }
    Ok(())
}

pub fn cursor_pos() -> Result<(i32, i32), HandsError> {
    let mut pt = POINT::default();
    unsafe { GetCursorPos(&mut pt) }
        .map_err(|err| HandsError::Input(format!("GetCursorPos: {err}")))?;
    Ok((pt.x, pt.y))
}

fn mouse_move_abs(dx: i32, dy: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn mouse_button(down: bool) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: if down {
                    MOUSEEVENTF_LEFTDOWN
                } else {
                    MOUSEEVENTF_LEFTUP
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn mouse_wheel(
    flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS,
    notches: i32,
) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: notches.saturating_mul(WHEEL_DELTA as i32) as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn key_vk(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    Default::default()
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn key_unicode(unit: u16, up: bool) -> INPUT {
    let mut flags = KEYEVENTF_UNICODE;
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: unit,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

pub fn move_to(space: Space, x: i32, y: i32, rng: &mut Rng) -> Result<(), HandsError> {
    lease::poll()?;
    let start = cursor_pos()?;
    let samples = bezier::path(start, (x, y), rng);
    for (i, &(px, py)) in samples.iter().enumerate() {
        lease::poll()?;
        let (dx, dy) = bezier::to_absolute(space, px, py);
        send_inputs(&[mouse_move_abs(dx, dy)])?;
        if i + 1 < samples.len() {
            std::thread::sleep(Duration::from_millis(bezier::sleep_ms_between_samples(rng)));
        }
    }
    Ok(())
}

pub fn left_click(rng: &mut Rng) -> Result<(), HandsError> {
    lease::poll()?;
    send_inputs(&[mouse_button(true)])?;
    std::thread::sleep(Duration::from_millis(bezier::click_hold_ms(rng)));
    let frozen = lease::poll();
    // Always pair UP even if the lease froze mid-hold so the button does not stick.
    let up = send_inputs(&[mouse_button(false)]);
    frozen?;
    up
}

pub fn scroll_wheel(dy: i32, dx: Option<i32>) -> Result<(), HandsError> {
    lease::poll()?;
    if dy != 0 {
        send_inputs(&[mouse_wheel(MOUSEEVENTF_WHEEL, dy)])?;
        lease::poll()?;
    }
    if let Some(hx) = dx
        && hx != 0
    {
        lease::poll()?;
        send_inputs(&[mouse_wheel(MOUSEEVENTF_HWHEEL, hx)])?;
        lease::poll()?;
    }
    Ok(())
}

pub fn type_text(text: &str) -> Result<TypePath, HandsError> {
    match type_path_for(text) {
        TypePath::Unicode => {
            type_unicode(text)?;
            Ok(TypePath::Unicode)
        }
        TypePath::Clipboard => {
            type_clipboard(text)?;
            Ok(TypePath::Clipboard)
        }
    }
}

fn type_unicode(text: &str) -> Result<(), HandsError> {
    for ch in text.chars() {
        lease::poll()?;
        match ch {
            '\n' => tap_vk(VK_RETURN)?,
            '\t' => tap_vk(VK_TAB)?,
            other => {
                let mut buf = [0u16; 2];
                for unit in other.encode_utf16(&mut buf).iter().copied() {
                    send_inputs(&[key_unicode(unit, false)])?;
                    let frozen = lease::poll();
                    let up = send_inputs(&[key_unicode(unit, true)]);
                    frozen?;
                    up?;
                }
            }
        }
    }
    lease::poll()
}

fn tap_vk(vk: VIRTUAL_KEY) -> Result<(), HandsError> {
    lease::poll()?;
    send_inputs(&[key_vk(vk, false)])?;
    let frozen = lease::poll();
    let up = send_inputs(&[key_vk(vk, true)]);
    frozen?;
    up?;
    lease::poll()
}

fn type_clipboard(text: &str) -> Result<(), HandsError> {
    lease::poll()?;
    let wide: Vec<u16> = text.encode_utf16().collect();
    let previous = with_clipboard(|opened| {
        if !opened {
            return Err(HandsError::Input("OpenClipboard failed".into()));
        }
        let prev = read_unicode_clipboard();
        empty_and_set(&wide)?;
        Ok(prev)
    })?;
    let paste = (|| -> Result<(), HandsError> {
        // Clipboard must be closed before injected Ctrl+V.
        chord(&[VK_CONTROL, VK_V])?;
        // Give the target time to consume CF_UNICODETEXT before we restore.
        std::thread::sleep(Duration::from_millis(80));
        lease::poll()
    })();
    let restore = restore_clipboard(previous.as_deref());
    let after = lease::poll();
    paste?;
    restore?;
    after
}

fn restore_clipboard(previous: Option<&[u16]>) -> Result<(), HandsError> {
    let op = restore_op(previous);
    with_clipboard(|opened| {
        if !opened {
            return Err(HandsError::Input(
                "OpenClipboard failed while restoring previous text".into(),
            ));
        }
        match op {
            ClipboardOp::Set(units) => empty_and_set(&units),
            ClipboardOp::Empty => unsafe {
                EmptyClipboard().map_err(|err| HandsError::Input(format!("EmptyClipboard: {err}")))
            },
        }
    })
}

fn chord(keys: &[VIRTUAL_KEY]) -> Result<(), HandsError> {
    lease::poll()?;
    let mut first_err: Option<HandsError> = None;
    let mut pressed = Vec::new();
    for vk in keys {
        if first_err.is_none()
            && let Err(err) = lease::poll()
        {
            first_err = Some(err);
        }
        if first_err.is_some() {
            break;
        }
        if let Err(err) = send_inputs(&[key_vk(*vk, false)]) {
            first_err = Some(err);
            break;
        }
        pressed.push(*vk);
    }
    for vk in pressed.iter().rev() {
        if let Err(err) = send_inputs(&[key_vk(*vk, true)])
            && first_err.is_none()
        {
            first_err = Some(err);
        }
    }
    if first_err.is_none()
        && let Err(err) = lease::poll()
    {
        first_err = Some(err);
    }
    match first_err {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

pub fn is_enter_key(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "enter" | "return"
    )
}

pub fn named_key(name: &str) -> Result<(), HandsError> {
    lease::poll()?;
    let key = name.trim().to_ascii_lowercase();
    match key.as_str() {
        "enter" | "return" => tap_vk(VK_RETURN),
        "tab" => tap_vk(VK_TAB),
        "escape" | "esc" => tap_vk(VK_ESCAPE),
        "backspace" => tap_vk(VK_BACK),
        "delete" => tap_vk(VK_DELETE),
        "space" => tap_vk(VK_SPACE),
        "up" => tap_vk(VK_UP),
        "down" => tap_vk(VK_DOWN),
        "left" => tap_vk(VK_LEFT),
        "right" => tap_vk(VK_RIGHT),
        "home" => tap_vk(VK_HOME),
        "end" => tap_vk(VK_END),
        "pageup" => tap_vk(VK_PRIOR),
        "pagedown" => tap_vk(VK_NEXT),
        "ctrl+a" => chord(&[VK_CONTROL, VK_A]),
        "ctrl+c" => chord(&[VK_CONTROL, VK_C]),
        "ctrl+v" => chord(&[VK_CONTROL, VK_V]),
        "ctrl+x" => chord(&[VK_CONTROL, VK_X]),
        other => Err(HandsError::Input(format!("unknown key '{other}'"))),
    }
}

fn with_clipboard<T>(f: impl FnOnce(bool) -> Result<T, HandsError>) -> Result<T, HandsError> {
    let mut opened = false;
    for _ in 0..8 {
        if unsafe { OpenClipboard(None) }.is_ok() {
            opened = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let result = f(opened);
    if opened {
        let _ = unsafe { CloseClipboard() };
    }
    result
}

fn read_unicode_clipboard() -> Option<Vec<u16>> {
    if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) }.is_err() {
        return None;
    }
    let handle = unsafe { GetClipboardData(CF_UNICODETEXT) }.ok()?;
    let hglobal = HGLOBAL(handle.0);
    let size = unsafe { GlobalSize(hglobal) };
    if size < 2 {
        return Some(Vec::new());
    }
    let ptr = unsafe { GlobalLock(hglobal) };
    if ptr.is_null() {
        return None;
    }
    let units = size / 2;
    let slice = unsafe { std::slice::from_raw_parts(ptr as *const u16, units) };
    let end = slice.iter().position(|&u| u == 0).unwrap_or(slice.len());
    let out = slice[..end].to_vec();
    let _ = unsafe { GlobalUnlock(hglobal) };
    Some(out)
}

fn empty_and_set(units: &[u16]) -> Result<(), HandsError> {
    unsafe {
        EmptyClipboard().map_err(|err| HandsError::Input(format!("EmptyClipboard: {err}")))?;
    }
    let bytes = (units.len() + 1) * 2;
    let hglobal = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) }
        .map_err(|err| HandsError::Input(format!("GlobalAlloc: {err}")))?;
    let ptr = unsafe { GlobalLock(hglobal) };
    if ptr.is_null() {
        unsafe {
            let _ = windows::Win32::Foundation::GlobalFree(Some(hglobal));
        }
        return Err(HandsError::Input("GlobalLock failed".into()));
    }
    unsafe {
        std::ptr::copy_nonoverlapping(units.as_ptr(), ptr as *mut u16, units.len());
        *ptr.cast::<u16>().add(units.len()) = 0;
        let _ = GlobalUnlock(hglobal);
    }
    let handle = HANDLE(hglobal.0);
    match unsafe { SetClipboardData(CF_UNICODETEXT, Some(handle)) } {
        Ok(_) => Ok(()),
        Err(err) => {
            unsafe {
                let _ = windows::Win32::Foundation::GlobalFree(Some(hglobal));
            }
            Err(HandsError::Input(format!("SetClipboardData: {err}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_threshold_32_scalars() {
        assert_eq!(type_path_for("hi"), TypePath::Unicode);
        assert_eq!(type_path_for(&"x".repeat(32)), TypePath::Unicode);
        assert_eq!(type_path_for(&"x".repeat(33)), TypePath::Clipboard);
        assert_eq!(type_path_for(&"é".repeat(32)), TypePath::Unicode);
        assert_eq!(type_path_for(&"é".repeat(33)), TypePath::Clipboard);
    }

    #[test]
    fn return_is_enter_alias() {
        assert!(is_enter_key("return"));
        assert!(is_enter_key("ENTER"));
        assert!(is_enter_key(" Return "));
        assert!(is_enter_key("enter"));
        assert!(!is_enter_key("tab"));
        assert!(!is_enter_key("space"));
    }

    #[test]
    fn negative_six_notches_is_two_s_complement_dword() {
        assert_eq!(
            (-6i32).saturating_mul(WHEEL_DELTA as i32) as u32,
            (-720i32) as u32
        );
    }

    #[test]
    fn restore_helper_uses_saved_or_empty() {
        let saved: Vec<u16> = "hello".encode_utf16().collect();
        assert_eq!(restore_op(Some(&saved)), ClipboardOp::Set(saved.clone()));
        assert_eq!(restore_op(None), ClipboardOp::Empty);
        let empty: Vec<u16> = Vec::new();
        assert_eq!(restore_op(Some(&empty)), ClipboardOp::Set(Vec::new()));
    }

    #[test]
    #[ignore = "live desktop; not a CI gate. Run: cargo test -- --ignored"]
    fn live_move_within_2px() {
        crate::space::ensure_dpi().expect("dpi");
        let space = crate::space::virtual_screen().expect("space");
        let x = space.origin_x + space.width / 2;
        let y = space.origin_y + space.height / 2;
        let mut rng = Rng::new(1);
        move_to(space, x, y, &mut rng).expect("move");
        let (cx, cy) = cursor_pos().expect("cursor");
        assert!(
            (cx - x).abs() <= 2 && (cy - y).abs() <= 2,
            "cursor ({cx},{cy}) target ({x},{y})"
        );
    }
}
