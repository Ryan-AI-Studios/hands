//! Serial CDC host for research-identity hardware mouse/keyboard.
//! Daily identity never calls this module. Not a flag wipe. Not a new MCP tool.

use std::mem::size_of;

use windows::Win32::Devices::Communication::{
    COMMTIMEOUTS, DCB, GetCommState, NOPARITY, ONESTOPBIT, SetCommState, SetCommTimeouts,
};
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_NONE, OPEN_EXISTING, WriteFile,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_KEYBOARD, INPUT_MOUSE, KEYEVENTF_KEYUP, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_WHEEL,
};
use windows::Win32::UI::WindowsAndMessaging::WHEEL_DELTA;
use windows::core::PCWSTR;

use crate::error::HandsError;
use crate::lease;

pub const PORT_ENV: &str = "HANDS_HID_PORT";
pub const BAUD_ENV: &str = "HANDS_HID_BAUD";
pub const HID_PORT_ERR: &str = "hid port";

const MAGIC: u8 = 0x48;
const VERSION: u8 = 1;
const KIND_ABS: u8 = 1;
const KIND_BTN: u8 = 2;
const KIND_WHEEL: u8 = 3;
const KIND_KEY: u8 = 4;
const WRITE_TIMEOUT_MS: u32 = 500;
const BAUD_DEFAULT: u32 = 115200;
const BAUD_ALLOWED: [u32; 5] = [9600, 19200, 38400, 57600, 115200];

pub trait HidLink {
    fn write(&self, frame: &[u8]) -> Result<(), HandsError>;
}

pub fn configured() -> bool {
    std::env::var(PORT_ENV)
        .ok()
        .is_some_and(|s| !s.trim().is_empty())
}

pub fn parse_com_port(raw: &str) -> Result<String, HandsError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(hid_port_err("empty"));
    }
    let rest = trimmed.strip_prefix(r"\\.\").unwrap_or(trimmed);
    let upper = rest.to_ascii_uppercase();
    let Some(digits) = upper.strip_prefix("COM") else {
        return Err(hid_port_err("not COM"));
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return Err(hid_port_err("not COM"));
    }
    Ok(format!(r"\\.\{upper}"))
}

fn hid_port_err(detail: impl std::fmt::Display) -> HandsError {
    HandsError::Input(format!("{HID_PORT_ERR}: {detail}"))
}

fn baud_rate() -> u32 {
    std::env::var(BAUD_ENV)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .filter(|b| BAUD_ALLOWED.contains(b))
        .unwrap_or(BAUD_DEFAULT)
}

/// fBinary=1; clear CTS/DSR/Xon/Xoff flow bits from a GetCommState bitfield.
fn apply_no_flow(bits: u32) -> u32 {
    let mut b = bits | 1;
    b &= !(1 << 2);
    b &= !(1 << 3);
    b &= !(1 << 8);
    b &= !(1 << 9);
    b
}

fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(3 + payload.len());
    out.push(MAGIC);
    out.push(VERSION);
    out.push(kind);
    out.extend_from_slice(payload);
    out
}

fn wheel_notches(mouse_data: u32) -> i8 {
    let notches = (mouse_data as i32).saturating_div(WHEEL_DELTA as i32);
    notches.clamp(i32::from(i8::MIN), i32::from(i8::MAX)) as i8
}

fn encode_input(input: &INPUT) -> Result<Vec<u8>, HandsError> {
    if input.r#type == INPUT_MOUSE {
        let mi = unsafe { input.Anonymous.mi };
        let flags = mi.dwFlags;
        if flags.contains(MOUSEEVENTF_MOVE) && flags.contains(MOUSEEVENTF_ABSOLUTE) {
            let x = mi.dx as u16;
            let y = mi.dy as u16;
            let mut payload = [0u8; 4];
            payload[..2].copy_from_slice(&x.to_le_bytes());
            payload[2..].copy_from_slice(&y.to_le_bytes());
            return Ok(frame(KIND_ABS, &payload));
        }
        if flags.contains(MOUSEEVENTF_LEFTDOWN) {
            return Ok(frame(KIND_BTN, &[1]));
        }
        if flags.contains(MOUSEEVENTF_LEFTUP) {
            return Ok(frame(KIND_BTN, &[0]));
        }
        if flags.contains(MOUSEEVENTF_WHEEL) {
            let dy = wheel_notches(mi.mouseData) as u8;
            return Ok(frame(KIND_WHEEL, &[dy, 0]));
        }
        if flags.contains(MOUSEEVENTF_HWHEEL) {
            let dx = wheel_notches(mi.mouseData) as u8;
            return Ok(frame(KIND_WHEEL, &[0, dx]));
        }
        return Err(hid_port_err("unsupported mouse event"));
    }
    if input.r#type == INPUT_KEYBOARD {
        let ki = unsafe { input.Anonymous.ki };
        if ki.wVk.0 == 0 {
            return Err(hid_port_err("unicode"));
        }
        let vk = ki.wVk.0 as u8;
        let down = if ki.dwFlags.contains(KEYEVENTF_KEYUP) {
            0u8
        } else {
            1u8
        };
        return Ok(frame(KIND_KEY, &[vk, down]));
    }
    Err(hid_port_err("unsupported input type"))
}

fn send_with(link: &dyn HidLink, inputs: &[INPUT]) -> Result<(), HandsError> {
    for input in inputs {
        let bytes = encode_input(input)?;
        lease::note_hid_own();
        let wrote = link.write(&bytes).map_err(|err| {
            let msg = err.to_string();
            if msg.contains(HID_PORT_ERR) {
                err
            } else {
                hid_port_err(msg)
            }
        });
        lease::note_hid_own();
        wrote?;
    }
    Ok(())
}

pub fn send(inputs: &[INPUT]) -> Result<(), HandsError> {
    if inputs.is_empty() {
        return Ok(());
    }
    #[cfg(test)]
    if let Some(link) = test_link() {
        return send_with(&*link, inputs);
    }
    let port = OpenPort::open()?;
    send_with(&port, inputs)
}

struct OpenPort {
    handle: HANDLE,
}

impl OpenPort {
    fn open() -> Result<Self, HandsError> {
        let raw = std::env::var(PORT_ENV).map_err(|_| hid_port_err("missing"))?;
        let path = parse_com_port(&raw)?;
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }
        .map_err(hid_port_err)?;
        let mut dcb = DCB {
            DCBlength: size_of::<DCB>() as u32,
            ..Default::default()
        };
        if let Err(err) = unsafe { GetCommState(handle, &mut dcb) } {
            let _ = unsafe { CloseHandle(handle) };
            return Err(hid_port_err(err));
        }
        dcb.BaudRate = baud_rate();
        dcb.ByteSize = 8;
        dcb.Parity = NOPARITY;
        dcb.StopBits = ONESTOPBIT;
        dcb._bitfield = apply_no_flow(dcb._bitfield);
        if let Err(err) = unsafe { SetCommState(handle, &dcb) } {
            let _ = unsafe { CloseHandle(handle) };
            return Err(hid_port_err(err));
        }
        let timeouts = COMMTIMEOUTS {
            WriteTotalTimeoutConstant: WRITE_TIMEOUT_MS,
            ..Default::default()
        };
        if let Err(err) = unsafe { SetCommTimeouts(handle, &timeouts) } {
            let _ = unsafe { CloseHandle(handle) };
            return Err(hid_port_err(err));
        }
        Ok(Self { handle })
    }
}

impl Drop for OpenPort {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

impl HidLink for OpenPort {
    fn write(&self, frame: &[u8]) -> Result<(), HandsError> {
        let mut written = 0u32;
        unsafe {
            WriteFile(
                self.handle,
                Some(frame),
                Some(&mut written as *mut u32),
                None,
            )
        }
        .map_err(hid_port_err)?;
        if written as usize != frame.len() {
            return Err(hid_port_err(format!("short write {written}")));
        }
        Ok(())
    }
}

#[cfg(test)]
use std::sync::{Arc, Mutex};

#[cfg(test)]
static TEST_LINK: Mutex<Option<Arc<dyn HidLink + Send + Sync>>> = Mutex::new(None);

#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
fn test_link() -> Option<Arc<dyn HidLink + Send + Sync>> {
    TEST_LINK.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

#[cfg(test)]
pub(crate) fn set_test_link(link: Option<Arc<dyn HidLink + Send + Sync>>) {
    *TEST_LINK.lock().unwrap_or_else(|e| e.into_inner()) = link;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attach::{self, Identity};
    use crate::input::{self, set_send_inputs_hook};
    use crate::lease;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT_0, KEYBDINPUT, KEYEVENTF_UNICODE, MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT, VIRTUAL_KEY,
        VK_CONTROL, VK_TAB, VK_V,
    };

    struct Rec {
        writes: AtomicUsize,
        frames: Mutex<Vec<Vec<u8>>>,
    }

    impl Rec {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                writes: AtomicUsize::new(0),
                frames: Mutex::new(Vec::new()),
            })
        }

        fn count(&self) -> usize {
            self.writes.load(Ordering::SeqCst)
        }

        fn frames(&self) -> Vec<Vec<u8>> {
            self.frames
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }
    }

    impl HidLink for Rec {
        fn write(&self, frame: &[u8]) -> Result<(), HandsError> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            self.frames
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(frame.to_vec());
            Ok(())
        }
    }

    struct FailLink;

    impl HidLink for FailLink {
        fn write(&self, _frame: &[u8]) -> Result<(), HandsError> {
            Err(HandsError::Input("gadget write failed".into()))
        }
    }

    struct EnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev_port: Option<std::ffi::OsString>,
        prev_baud: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn lock() -> Self {
            let lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let prev_port = std::env::var_os(PORT_ENV);
            let prev_baud = std::env::var_os(BAUD_ENV);
            attach::reset_identity_for_test();
            set_test_link(None);
            set_send_inputs_hook(None);
            Self {
                _lock: lock,
                prev_port,
                prev_baud,
            }
        }

        fn set_port(&self, value: Option<&str>) {
            match value {
                Some(v) => unsafe { std::env::set_var(PORT_ENV, v) },
                None => unsafe { std::env::remove_var(PORT_ENV) },
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev_port {
                Some(v) => unsafe { std::env::set_var(PORT_ENV, v) },
                None => unsafe { std::env::remove_var(PORT_ENV) },
            }
            match &self.prev_baud {
                Some(v) => unsafe { std::env::set_var(BAUD_ENV, v) },
                None => unsafe { std::env::remove_var(BAUD_ENV) },
            }
            attach::reset_identity_for_test();
            set_test_link(None);
            set_send_inputs_hook(None);
            input::set_skip_live_clipboard(false);
        }
    }

    fn abs_mouse(x: u16, y: u16) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: i32::from(x),
                    dy: i32::from(y),
                    mouseData: 0,
                    dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn left_button(down: bool) -> INPUT {
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

    fn wheel(dy: i32) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: dy.saturating_mul(WHEEL_DELTA as i32) as u32,
                    dwFlags: MOUSEEVENTF_WHEEL,
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

    fn unicode_unit(unit: u16) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: unit,
                    dwFlags: KEYEVENTF_UNICODE,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn os_count(_inputs: &[INPUT]) -> Result<(), HandsError> {
        OS_CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    static OS_CALLS: AtomicUsize = AtomicUsize::new(0);

    fn arm_os() {
        OS_CALLS.store(0, Ordering::SeqCst);
        set_send_inputs_hook(Some(os_count));
    }

    fn fn_slice<'a>(src: &'a str, sig: &str) -> &'a str {
        let start = src.find(sig).unwrap_or_else(|| panic!("{sig}"));
        let rest = &src[start..];
        let after = &rest[sig.len()..];
        let next_pub = after.find("\npub fn ");
        let next_fn = after.find("\nfn ");
        let rel = match (next_pub, next_fn) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => after.len(),
        };
        &rest[..sig.len() + rel]
    }

    #[test]
    fn parse_com_port_table() {
        assert_eq!(parse_com_port("COM5").unwrap(), r"\\.\COM5");
        assert_eq!(parse_com_port("COM17").unwrap(), r"\\.\COM17");
        assert_eq!(parse_com_port(r"\\.\COM5").unwrap(), r"\\.\COM5");
        assert_eq!(parse_com_port(" com5 ").unwrap(), r"\\.\COM5");
        for bad in ["", "COM", r"C:\foo", "COM5:", r"COM5\x", "COM5/"] {
            let err = parse_com_port(bad).expect_err(bad);
            assert!(err.to_string().contains(HID_PORT_ERR), "{bad}: {err}");
        }
    }

    #[test]
    fn encode_abs_button_wheel_key() {
        let abs = encode_input(&abs_mouse(0x1234, 0xABCD)).unwrap();
        assert_eq!(abs, vec![0x48, 1, 1, 0x34, 0x12, 0xCD, 0xAB]);
        let down = encode_input(&left_button(true)).unwrap();
        assert_eq!(down, vec![0x48, 1, 2, 1]);
        let up = encode_input(&left_button(false)).unwrap();
        assert_eq!(up, vec![0x48, 1, 2, 0]);
        let w = encode_input(&wheel(-6)).unwrap();
        assert_eq!(w[0], 0x48);
        assert_eq!(w[1], 1);
        assert_eq!(w[2], 3);
        assert_eq!(w[3] as i8, -6);
        assert_eq!(w[4], 0);
        let tab = encode_input(&key_vk(VK_TAB, false)).unwrap();
        assert_eq!(tab, vec![0x48, 1, 4, VK_TAB.0 as u8, 1]);
        let tab_up = encode_input(&key_vk(VK_TAB, true)).unwrap();
        assert_eq!(tab_up, vec![0x48, 1, 4, VK_TAB.0 as u8, 0]);
    }

    #[test]
    fn unicode_encode_is_hid_port() {
        let err = encode_input(&unicode_unit(b'h' as u16)).unwrap_err();
        assert!(err.to_string().contains(HID_PORT_ERR), "{err}");
    }

    #[test]
    fn daily_with_port_uses_os_not_hid() {
        let _g = EnvGuard::lock();
        let _lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        lease::reset_for_test();
        attach::set_identity_for_test(Identity::Daily);
        _g.set_port(Some("COM5"));
        let rec = Rec::new();
        set_test_link(Some(rec.clone()));
        arm_os();
        crate::input::send_inputs(&[abs_mouse(1, 2)]).unwrap();
        assert!(OS_CALLS.load(Ordering::SeqCst) >= 1);
        assert_eq!(rec.count(), 0);
    }

    #[test]
    fn research_unset_uses_os() {
        let _g = EnvGuard::lock();
        let _lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        lease::reset_for_test();
        attach::set_identity_for_test(Identity::Research);
        _g.set_port(None);
        let rec = Rec::new();
        set_test_link(Some(rec.clone()));
        arm_os();
        crate::input::send_inputs(&[abs_mouse(3, 4)]).unwrap();
        assert!(OS_CALLS.load(Ordering::SeqCst) >= 1);
        assert_eq!(rec.count(), 0);
    }

    #[test]
    fn research_fake_link_frames_os_zero() {
        let _g = EnvGuard::lock();
        let _lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        lease::reset_for_test();
        attach::set_identity_for_test(Identity::Research);
        _g.set_port(Some("COM5"));
        let rec = Rec::new();
        set_test_link(Some(rec.clone()));
        arm_os();
        crate::input::send_inputs(&[abs_mouse(0x1111, 0x2222)]).unwrap();
        crate::input::send_inputs(&[left_button(true)]).unwrap();
        crate::input::send_inputs(&[key_vk(VK_TAB, false)]).unwrap();
        assert_eq!(OS_CALLS.load(Ordering::SeqCst), 0);
        let frames = rec.frames();
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0], vec![0x48, 1, 1, 0x11, 0x11, 0x22, 0x22]);
        assert_eq!(frames[1], vec![0x48, 1, 2, 1]);
        assert_eq!(frames[2], vec![0x48, 1, 4, VK_TAB.0 as u8, 1]);
    }

    #[test]
    fn research_write_fail_is_hid_port_no_os() {
        let _g = EnvGuard::lock();
        let _lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        lease::reset_for_test();
        attach::set_identity_for_test(Identity::Research);
        _g.set_port(Some("COM5"));
        set_test_link(Some(Arc::new(FailLink)));
        arm_os();
        let err = crate::input::send_inputs(&[abs_mouse(1, 1)]).unwrap_err();
        assert!(err.to_string().contains(HID_PORT_ERR), "{err}");
        assert_eq!(OS_CALLS.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn research_unicode_send_is_hid_port_no_os() {
        let _g = EnvGuard::lock();
        let _lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        lease::reset_for_test();
        attach::set_identity_for_test(Identity::Research);
        _g.set_port(Some("COM5"));
        let rec = Rec::new();
        set_test_link(Some(rec.clone()));
        arm_os();
        let err = crate::input::send_inputs(&[unicode_unit(b'h' as u16)]).unwrap_err();
        assert!(err.to_string().contains(HID_PORT_ERR), "{err}");
        assert_eq!(OS_CALLS.load(Ordering::SeqCst), 0);
        assert_eq!(rec.count(), 0);
    }

    #[test]
    fn research_type_text_hi_is_clipboard_no_unicode() {
        let _g = EnvGuard::lock();
        let _lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        lease::reset_for_test();
        attach::set_identity_for_test(Identity::Research);
        _g.set_port(Some("COM5"));
        let rec = Rec::new();
        set_test_link(Some(rec.clone()));
        arm_os();
        input::set_skip_live_clipboard(true);
        let path = crate::input::type_text("hi").expect("type_text hi");
        assert_eq!(path, crate::input::TypePath::Clipboard);
        assert_eq!(OS_CALLS.load(Ordering::SeqCst), 0);
        let frames = rec.frames();
        assert!(!frames.is_empty());
        for f in &frames {
            assert_eq!(f[0], 0x48);
            assert_eq!(f[1], 1);
            assert_eq!(f[2], KIND_KEY, "no unicode frames: {f:?}");
            assert_ne!(f[3], 0, "vk must be non-zero: {f:?}");
        }
        let vks: Vec<u8> = frames.iter().map(|f| f[3]).collect();
        assert!(vks.contains(&(VK_CONTROL.0 as u8)), "{vks:?}");
        assert!(vks.contains(&(VK_V.0 as u8)), "{vks:?}");
    }

    #[test]
    fn named_tab_is_vk_kind_four() {
        let _g = EnvGuard::lock();
        let _lease = lease::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        lease::reset_for_test();
        attach::set_identity_for_test(Identity::Research);
        _g.set_port(Some("COM5"));
        let rec = Rec::new();
        set_test_link(Some(rec.clone()));
        arm_os();
        crate::input::named_key("tab").unwrap();
        assert_eq!(OS_CALLS.load(Ordering::SeqCst), 0);
        let frames = rec.frames();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0], vec![0x48, 1, 4, VK_TAB.0 as u8, 1]);
        assert_eq!(frames[1], vec![0x48, 1, 4, VK_TAB.0 as u8, 0]);
    }

    #[test]
    fn source_locks() {
        let src = include_str!("hid.rs");
        assert!(src.contains("CreateFileW"));
        assert!(src.contains("WriteFile"));
        assert!(src.contains("GetCommState"));
        assert!(src.contains("SetCommState"));
        assert!(src.contains("SetCommTimeouts"));
        let send = ["Send", "Input"].concat();
        assert!(!src.contains(&send), "hid.rs must not mention OS inject");
        let cargo =
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).to_ascii_lowercase();
        for needle in [
            ["serial", "port"].concat(),
            ["hid", "api"].concat(),
            ["on", "nx"].concat(),
            ["whis", "per"].concat(),
            ["2cap", "tcha"].concat(),
        ] {
            assert!(
                !cargo.contains(&needle),
                "Cargo.toml must not mention {needle}"
            );
        }
        let input = include_str!("input.rs");
        let os = fn_slice(input, "fn send_os_inputs");
        assert!(os.contains(&send), "OS helper must still inject");
        assert!(
            !os.contains("note_hid_own"),
            "Daily OS helper must not mark own-HID:\n{os}"
        );
        let attach = include_str!("attach.rs");
        let launch = fn_slice(attach, "fn launch_argv(");
        assert!(
            !launch.contains("--"),
            "Daily launch_argv must stay zero dash:\n{launch}"
        );
        let agents = include_str!("../AGENTS.md");
        let readme = include_str!("../README.md");
        assert!(agents.contains("HANDS_HID_PORT"));
        assert!(readme.contains("HANDS_HID_PORT"));
        assert!(agents.contains("LLMHF_INJECTED"));
        assert!(readme.contains("LLMHF_INJECTED"));
        let mcp = include_str!("mcp.rs");
        let main = include_str!("main.rs");
        assert!(mcp.contains("HANDS_HID_PORT") || main.contains("HANDS_HID_PORT"));
    }

    #[test]
    #[ignore = "live COM gadget; not a CI gate"]
    fn live_com_open_not_required_in_ci() {
        let raw = std::env::var(PORT_ENV).unwrap_or_default();
        if raw.trim().is_empty() {
            return;
        }
        let _ = parse_com_port(&raw).expect("configured port must parse");
    }
}
