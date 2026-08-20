//! Chrome native-messaging framing, origin check, and the MCP named-pipe server.

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use serde_json::Value;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_BAD_PIPE, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_OPERATION_ABORTED,
    ERROR_PIPE_CONNECTED, GENERIC_ALL, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Security::{
    ACL, ACL_REVISION, AddAccessAllowedAce, GetLengthSid, GetTokenInformation, InitializeAcl,
    InitializeSecurityDescriptor, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR,
    SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FlushFileBuffers, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT, PeekNamedPipe,
};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, OpenProcessToken, WaitForSingleObject,
};
use windows::core::PCWSTR;

use crate::error::HandsError;

pub const HOST_NAME: &str = "com.helpinghands.host";
pub const EXTENSION_ID: &str = "fdnpjnnnmfhlpgaabjflhjoepmejcnha";
pub const DEFAULT_PIPE_NAME: &str = r"\\.\pipe\hands-chrome";
pub const MAX_HOST_TO_CHROME: usize = 1_048_576;
pub const MAX_CHROME_TO_HOST: usize = 64 * 1024 * 1024;
pub const CLIENT_TIMEOUT_MS: u32 = 400;
pub const HOST_FORWARD_TIMEOUT: Duration = Duration::from_millis(2_000);

const PIPE_BUF: u32 = 65_536;
const SECURITY_DESCRIPTOR_REVISION: u32 = 1;

pub fn allowed_origin() -> String {
    format!("chrome-extension://{EXTENSION_ID}")
}

pub fn normalize_origin(origin: &str) -> String {
    origin.trim().trim_end_matches('/').to_string()
}

/// `HANDS_NATIVE_ORIGIN` (full origin, trailing slash optional) is also accepted.
pub fn origin_is_allowed(origin: &str) -> bool {
    let got = normalize_origin(origin);
    if got == normalize_origin(&allowed_origin()) {
        return true;
    }
    match std::env::var("HANDS_NATIVE_ORIGIN") {
        Ok(extra) if !extra.trim().is_empty() => got == normalize_origin(&extra),
        _ => false,
    }
}

pub fn pipe_name() -> String {
    std::env::var("HANDS_CHROME_PIPE")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_PIPE_NAME.to_string())
}

pub fn manifest_json(extension_id: &str, exe_path: &str) -> Value {
    let id = extension_id.trim().trim_end_matches('/');
    let id = id
        .strip_prefix("chrome-extension://")
        .unwrap_or(id)
        .trim_end_matches('/');
    serde_json::json!({
        "name": HOST_NAME,
        "description": "Helping Hands native messaging host",
        "path": exe_path,
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{id}/")],
    })
}

pub fn write_frame<W: Write>(writer: &mut W, value: &Value) -> Result<(), HandsError> {
    write_frame_limited(writer, value, MAX_HOST_TO_CHROME)
}

pub fn write_pipe_frame<W: Write>(writer: &mut W, value: &Value) -> Result<(), HandsError> {
    write_frame_limited(writer, value, MAX_CHROME_TO_HOST)
}

pub fn write_frame_limited<W: Write>(
    writer: &mut W,
    value: &Value,
    max: usize,
) -> Result<(), HandsError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|err| HandsError::Chrome(format!("native-host json encode: {err}")))?;
    if bytes.len() > max {
        return Err(HandsError::Chrome(format!(
            "host→Chrome payload is {} bytes (max {max}); not written",
            bytes.len()
        )));
    }
    let len = u32::try_from(bytes.len()).map_err(|_| {
        HandsError::Chrome(format!(
            "native-host frame length {} does not fit u32",
            bytes.len()
        ))
    })?;
    writer
        .write_all(&len.to_le_bytes())
        .and_then(|()| writer.write_all(&bytes))
        .and_then(|()| writer.flush())
        .map_err(|err| HandsError::Chrome(format!("native-host frame write: {err}")))
}

pub fn read_frame<R: Read>(reader: &mut R) -> Result<Value, HandsError> {
    read_frame_limited(reader, MAX_CHROME_TO_HOST)
}

pub fn read_frame_limited<R: Read>(reader: &mut R, max: usize) -> Result<Value, HandsError> {
    let mut len_buf = [0u8; 4];
    reader
        .read_exact(&mut len_buf)
        .map_err(|err| HandsError::Chrome(format!("native-host frame length: {err}")))?;
    let len = usize::try_from(u32::from_le_bytes(len_buf)).unwrap_or(usize::MAX);
    if len > max {
        return Err(HandsError::Chrome(format!(
            "native-host frame is {len} bytes (max {max})"
        )));
    }
    let mut buf = vec![0u8; len];
    if len > 0 {
        reader
            .read_exact(&mut buf)
            .map_err(|err| HandsError::Chrome(format!("native-host frame body: {err}")))?;
    }
    serde_json::from_slice(&buf)
        .map_err(|err| HandsError::Chrome(format!("native-host json decode: {err}")))
}

pub fn run(origin: Option<&str>) -> Result<(), HandsError> {
    let origin = origin
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("HANDS_NATIVE_ORIGIN")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });
    let Some(origin) = origin else {
        return Err(HandsError::Chrome(
            "native-host requires chrome-extension:// origin (or HANDS_NATIVE_ORIGIN)".into(),
        ));
    };
    if !origin_is_allowed(&origin) {
        return Err(HandsError::Chrome(format!(
            "unknown native-messaging origin '{origin}'"
        )));
    }
    serve_pipe()
}

/// Client WaitNamedPipe+connect is 400 ms so observe stays snappy when the host
/// is absent. Host-forward to Chrome stdin/stdout may wait up to 2 s. v1 is
/// one in-flight request (accept-then-handle); there is no correlation id, so
/// a later multi-request change must not silently reorder replies.
fn serve_pipe() -> Result<(), HandsError> {
    let name = pipe_name();
    let wide = to_wide(&name);
    let sa = CurrentUserSa::new()?;
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(wide.as_ptr()),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            PIPE_BUF,
            PIPE_BUF,
            0,
            Some(&raw const sa.sa),
        )
    };
    if handle.is_invalid() {
        return Err(HandsError::Chrome(format!(
            "CreateNamedPipe({name}) failed (second host or access denied)"
        )));
    }
    let _keep_sa = sa;
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let stdin_handle = HANDLE(stdin.as_raw_handle());
    loop {
        connect_pipe(handle)?;
        let mut pipe = PipeRef(handle);
        let req = match read_frame_limited(&mut pipe, MAX_HOST_TO_CHROME) {
            Ok(v) => v,
            Err(err) => {
                eprintln!("native-host: pipe read: {err}");
                let _ = unsafe { DisconnectNamedPipe(handle) };
                continue;
            }
        };
        if let Err(err) = forward_one(&mut stdout, stdin_handle, &mut pipe, &req) {
            eprintln!("native-host: forward: {err}");
        }
        let _ = unsafe { DisconnectNamedPipe(handle) };
    }
}

fn forward_one<W: Write, P: Write>(
    chrome_out: &mut W,
    chrome_in_handle: HANDLE,
    pipe: &mut P,
    req: &Value,
) -> Result<(), HandsError> {
    write_frame(chrome_out, req)?;
    let reply = read_frame_timeout(chrome_in_handle, HOST_FORWARD_TIMEOUT)?;
    write_pipe_frame(pipe, &reply)
}

/// MCP/CLI side of the pipe: write one request and read one reply before `deadline`.
/// The 400 ms client budget must cover this exchange, not only WaitNamedPipe.
/// Uses overlapped I/O so a connected-but-silent host cannot block observe/click.
pub fn exchange_pipe_deadline(
    handle: HANDLE,
    req: &Value,
    deadline: Instant,
) -> Result<Value, HandsError> {
    let bytes = serde_json::to_vec(req)
        .map_err(|err| HandsError::Chrome(format!("native-host json encode: {err}")))?;
    if bytes.len() > MAX_CHROME_TO_HOST {
        return Err(HandsError::Chrome(format!(
            "host→Chrome payload is {} bytes (max {MAX_CHROME_TO_HOST}); not written",
            bytes.len()
        )));
    }
    let len = u32::try_from(bytes.len()).map_err(|_| {
        HandsError::Chrome(format!(
            "native-host frame length {} does not fit u32",
            bytes.len()
        ))
    })?;
    let mut framed = Vec::with_capacity(4 + bytes.len());
    framed.extend_from_slice(&len.to_le_bytes());
    framed.extend_from_slice(&bytes);
    overlapped_exact(handle, &mut framed, true, deadline)?;
    let mut len_buf = [0u8; 4];
    overlapped_exact(handle, &mut len_buf, false, deadline)?;
    let n = usize::try_from(u32::from_le_bytes(len_buf)).unwrap_or(usize::MAX);
    if n > MAX_CHROME_TO_HOST {
        return Err(HandsError::Chrome(format!(
            "native-host frame is {n} bytes (max {MAX_CHROME_TO_HOST})"
        )));
    }
    let mut body = vec![0u8; n];
    if n > 0 {
        overlapped_exact(handle, &mut body, false, deadline)?;
    }
    serde_json::from_slice(&body)
        .map_err(|err| HandsError::Chrome(format!("native-host json decode: {err}")))
}

fn overlapped_exact(
    handle: HANDLE,
    buf: &mut [u8],
    write: bool,
    deadline: Instant,
) -> Result<(), HandsError> {
    let mut filled = 0usize;
    while filled < buf.len() {
        if deadline.saturating_duration_since(Instant::now()).is_zero() {
            return Err(client_timeout());
        }
        let event = unsafe { CreateEventW(None, true, false, None) }
            .map_err(|err| HandsError::Chrome(format!("CreateEvent: {err}")))?;
        let mut ov = OVERLAPPED {
            hEvent: event,
            ..Default::default()
        };
        let pending = if write {
            let chunk = &buf[filled..];
            match unsafe { WriteFile(handle, Some(chunk), None, Some(&raw mut ov)) } {
                Ok(()) => false,
                Err(err) if err.code() == ERROR_IO_PENDING.to_hresult() => true,
                Err(err) => {
                    let _ = unsafe { CloseHandle(event) };
                    return Err(HandsError::Chrome(format!("pipe write: {err}")));
                }
            }
        } else {
            match unsafe { ReadFile(handle, Some(&mut buf[filled..]), None, Some(&raw mut ov)) } {
                Ok(()) => false,
                Err(err) if err.code() == ERROR_IO_PENDING.to_hresult() => true,
                Err(err) => {
                    let _ = unsafe { CloseHandle(event) };
                    return Err(HandsError::Chrome(format!("pipe read: {err}")));
                }
            }
        };
        if pending {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(cancel_overlapped(handle, &raw const ov, event, None));
            }
            let ms = u32::try_from(remaining.as_millis()).unwrap_or(u32::MAX);
            let wait = unsafe { WaitForSingleObject(event, ms) };
            if wait == WAIT_TIMEOUT {
                return Err(cancel_overlapped(handle, &raw const ov, event, None));
            }
            if wait != WAIT_OBJECT_0 {
                return Err(cancel_overlapped(
                    handle,
                    &raw const ov,
                    event,
                    Some(HandsError::Chrome(format!("pipe wait failed ({})", wait.0))),
                ));
            }
        }
        let mut transferred = 0u32;
        let got =
            unsafe { GetOverlappedResult(handle, &raw const ov, &raw mut transferred, false) };
        let _ = unsafe { CloseHandle(event) };
        got.map_err(|err| HandsError::Chrome(format!("GetOverlappedResult: {err}")))?;
        if transferred == 0 {
            return Err(HandsError::Chrome(
                "pipe closed during 400 ms client exchange".into(),
            ));
        }
        filled += transferred as usize;
    }
    Ok(())
}

fn cancel_overlapped(
    handle: HANDLE,
    ov: *const OVERLAPPED,
    event: HANDLE,
    err: Option<HandsError>,
) -> HandsError {
    let _ = unsafe { CancelIoEx(handle, Some(ov)) };
    let mut transferred = 0u32;
    match unsafe { GetOverlappedResult(handle, ov, &raw mut transferred, true) } {
        Ok(()) => {}
        Err(e) if e.code() == ERROR_OPERATION_ABORTED.to_hresult() => {}
        Err(_) => {}
    }
    let _ = unsafe { CloseHandle(event) };
    err.unwrap_or_else(client_timeout)
}

pub fn client_timeout() -> HandsError {
    HandsError::Chrome(format!(
        "Chrome host client timed out after {CLIENT_TIMEOUT_MS} ms; chr: unavailable"
    ))
}

fn connect_pipe(handle: HANDLE) -> Result<(), HandsError> {
    match unsafe { ConnectNamedPipe(handle, None) } {
        Ok(()) => Ok(()),
        Err(err) if err.code() == ERROR_PIPE_CONNECTED.to_hresult() => Ok(()),
        Err(err) => Err(HandsError::Chrome(format!("ConnectNamedPipe: {err}"))),
    }
}

fn read_frame_timeout(handle: HANDLE, timeout: Duration) -> Result<Value, HandsError> {
    let deadline = Instant::now() + timeout;
    let mut len_buf = [0u8; 4];
    read_exact_deadline(handle, &mut len_buf, deadline)?;
    let len = usize::try_from(u32::from_le_bytes(len_buf)).unwrap_or(usize::MAX);
    if len > MAX_CHROME_TO_HOST {
        return Err(HandsError::Chrome(format!(
            "Chrome→host frame is {len} bytes (max {MAX_CHROME_TO_HOST})"
        )));
    }
    let mut buf = vec![0u8; len];
    if len > 0 {
        read_exact_deadline(handle, &mut buf, deadline)?;
    }
    serde_json::from_slice(&buf)
        .map_err(|err| HandsError::Chrome(format!("Chrome→host json decode: {err}")))
}

fn stdin_closed() -> HandsError {
    HandsError::Chrome("Chrome stdin closed during host-forward".into())
}

fn host_forward_timeout() -> HandsError {
    HandsError::Chrome("Chrome host-forward timed out after 2 s".into())
}

fn read_exact_deadline(
    handle: HANDLE,
    buf: &mut [u8],
    deadline: Instant,
) -> Result<(), HandsError> {
    let mut filled = 0usize;
    while filled < buf.len() {
        if deadline.saturating_duration_since(Instant::now()).is_zero() {
            return Err(host_forward_timeout());
        }
        let mut avail = 0u32;
        match unsafe { PeekNamedPipe(handle, None, 0, None, Some(&raw mut avail), None) } {
            Ok(()) => {}
            Err(err)
                if err.code() == ERROR_BROKEN_PIPE.to_hresult()
                    || err.code() == ERROR_BAD_PIPE.to_hresult() =>
            {
                return Err(stdin_closed());
            }
            Err(err) => {
                return Err(HandsError::Chrome(format!("Chrome stdin peek: {err}")));
            }
        }
        let need = buf.len() - filled;
        if avail == 0 {
            std::thread::sleep(Duration::from_millis(8));
            continue;
        }
        let take = need.min(avail as usize);
        let mut n = 0u32;
        match unsafe {
            ReadFile(
                handle,
                Some(&mut buf[filled..filled + take]),
                Some(&raw mut n),
                None,
            )
        } {
            Ok(()) if n == 0 => return Err(stdin_closed()),
            Ok(()) => filled += n as usize,
            Err(err)
                if err.code() == ERROR_BROKEN_PIPE.to_hresult()
                    || err.code() == ERROR_BAD_PIPE.to_hresult() =>
            {
                return Err(stdin_closed());
            }
            Err(err) => {
                return Err(HandsError::Chrome(format!("Chrome stdin read: {err}")));
            }
        }
    }
    Ok(())
}

struct PipeRef(HANDLE);

impl Read for PipeRef {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut n = 0u32;
        unsafe { ReadFile(self.0, Some(buf), Some(&raw mut n), None) }
            .map_err(|_| io::Error::last_os_error())?;
        Ok(n as usize)
    }
}

impl Write for PipeRef {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut n = 0u32;
        unsafe { WriteFile(self.0, Some(buf), Some(&raw mut n), None) }
            .map_err(|_| io::Error::last_os_error())?;
        Ok(n as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        unsafe { FlushFileBuffers(self.0) }.map_err(|_| io::Error::last_os_error())
    }
}

struct CurrentUserSa {
    _sd: Box<SECURITY_DESCRIPTOR>,
    _acl: Vec<u8>,
    sa: SECURITY_ATTRIBUTES,
}

impl CurrentUserSa {
    fn new() -> Result<Self, HandsError> {
        let mut token = HANDLE::default();
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) }
            .map_err(|err| HandsError::Chrome(format!("OpenProcessToken: {err}")))?;
        let mut needed = 0u32;
        let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &raw mut needed) };
        let mut buf = vec![0u8; needed as usize];
        unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                Some(buf.as_mut_ptr().cast()),
                needed,
                &raw mut needed,
            )
        }
        .map_err(|err| HandsError::Chrome(format!("GetTokenInformation: {err}")))?;
        let _ = unsafe { CloseHandle(token) };
        let user = unsafe { &*buf.as_ptr().cast::<TOKEN_USER>() };
        let sid = user.User.Sid;
        let sid_len = unsafe { GetLengthSid(sid) } as usize;
        let acl_len = std::mem::size_of::<ACL>()
            + std::mem::size_of::<windows::Win32::Security::ACCESS_ALLOWED_ACE>()
            + sid_len
            - std::mem::size_of::<u32>();
        let mut acl = vec![0u8; acl_len];
        unsafe {
            InitializeAcl(acl.as_mut_ptr().cast(), acl_len as u32, ACL_REVISION)
                .map_err(|err| HandsError::Chrome(format!("InitializeAcl: {err}")))?;
            AddAccessAllowedAce(acl.as_mut_ptr().cast(), ACL_REVISION, GENERIC_ALL.0, sid)
                .map_err(|err| HandsError::Chrome(format!("AddAccessAllowedAce: {err}")))?;
        }
        let mut sd = Box::new(SECURITY_DESCRIPTOR::default());
        unsafe {
            InitializeSecurityDescriptor(
                PSECURITY_DESCRIPTOR(std::ptr::from_mut(&mut *sd).cast()),
                SECURITY_DESCRIPTOR_REVISION,
            )
            .map_err(|err| HandsError::Chrome(format!("InitializeSecurityDescriptor: {err}")))?;
            SetSecurityDescriptorDacl(
                PSECURITY_DESCRIPTOR(std::ptr::from_mut(&mut *sd).cast()),
                true,
                Some(acl.as_ptr().cast()),
                false,
            )
            .map_err(|err| HandsError::Chrome(format!("SetSecurityDescriptorDacl: {err}")))?;
        }
        let sa = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(0),
            lpSecurityDescriptor: std::ptr::from_mut(&mut *sd).cast(),
            bInheritHandle: false.into(),
        };
        Ok(Self {
            _sd: sd,
            _acl: acl,
            sa,
        })
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

trait AsRawHandleExt {
    fn as_raw_handle(&self) -> *mut std::ffi::c_void;
}

impl AsRawHandleExt for io::Stdin {
    fn as_raw_handle(&self) -> *mut std::ffi::c_void {
        std::os::windows::io::AsRawHandle::as_raw_handle(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    static ORIGIN_ENV: Mutex<()> = Mutex::new(());

    #[test]
    fn frame_round_trip() {
        let value = json!({"op":"snapshot","detail":"dom"});
        let mut buf = Vec::new();
        write_frame(&mut buf, &value).unwrap();
        assert_eq!(
            &buf[..4],
            &(u32::try_from(buf.len() - 4).unwrap()).to_le_bytes()
        );
        let decoded = read_frame(&mut buf.as_slice()).unwrap();
        assert_eq!(decoded, value);
    }

    #[test]
    fn oversize_host_to_chrome_is_rejected() {
        let value = json!({"pad": "a".repeat(MAX_HOST_TO_CHROME)});
        let err = write_frame(&mut Vec::new(), &value).unwrap_err();
        assert!(err.to_string().contains("max"), "{err}");
        assert!(
            err.to_string().contains(&MAX_HOST_TO_CHROME.to_string()),
            "{err}"
        );
    }

    #[test]
    fn origin_accepts_committed_id() {
        assert!(origin_is_allowed(&allowed_origin()));
        assert!(origin_is_allowed(&format!("{}/", allowed_origin())));
        assert!(!origin_is_allowed(
            "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
        assert!(!origin_is_allowed("https://evil.example"));
        assert!(!origin_is_allowed(""));
    }

    #[test]
    fn origin_override_env() {
        let _g = ORIGIN_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("HANDS_NATIVE_ORIGIN");
        unsafe {
            std::env::set_var(
                "HANDS_NATIVE_ORIGIN",
                "chrome-extension://bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            );
        }
        let ok = origin_is_allowed("chrome-extension://bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/");
        let still_shipped = origin_is_allowed(&allowed_origin());
        match prev {
            Some(v) => unsafe { std::env::set_var("HANDS_NATIVE_ORIGIN", v) },
            None => unsafe { std::env::remove_var("HANDS_NATIVE_ORIGIN") },
        }
        assert!(ok);
        assert!(still_shipped);
    }

    #[test]
    fn manifest_lists_concrete_origin() {
        let v = manifest_json(EXTENSION_ID, r"C:\dev\hands.exe");
        assert_eq!(v["name"], HOST_NAME);
        assert_eq!(v["type"], "stdio");
        assert_eq!(
            v["allowed_origins"][0],
            format!("chrome-extension://{EXTENSION_ID}/")
        );
        assert!(!v["allowed_origins"][0].as_str().unwrap().contains('*'));
    }

    fn make_anon_pipe() -> (HANDLE, HANDLE) {
        use windows::Win32::System::Pipes::CreatePipe;
        let mut read = HANDLE::default();
        let mut write = HANDLE::default();
        unsafe { CreatePipe(&raw mut read, &raw mut write, None, 0) }.expect("CreatePipe");
        (read, write)
    }

    #[test]
    fn silent_host_forward_times_out_within_budget() {
        let (read, write) = make_anon_pipe();
        let started = Instant::now();
        let err = read_exact_deadline(
            read,
            &mut [0u8; 4],
            Instant::now() + Duration::from_millis(80),
        )
        .unwrap_err();
        let elapsed = started.elapsed();
        let _ = unsafe { CloseHandle(read) };
        let _ = unsafe { CloseHandle(write) };
        let msg = err.to_string();
        assert!(
            elapsed < Duration::from_millis(500),
            "silent stdin hung {elapsed:?}: {msg}"
        );
        assert!(
            elapsed >= Duration::from_millis(40),
            "timeout too fast ({elapsed:?}): {msg}"
        );
        assert!(msg.contains("timed out after 2 s"), "{msg}");
    }

    #[test]
    fn closed_pipe_peek_is_stdin_closed_fast() {
        let (read, write) = make_anon_pipe();
        let _ = unsafe { CloseHandle(write) };
        let started = Instant::now();
        let err = read_exact_deadline(read, &mut [0u8; 4], Instant::now() + Duration::from_secs(2))
            .unwrap_err();
        let elapsed = started.elapsed();
        let _ = unsafe { CloseHandle(read) };
        let msg = err.to_string();
        assert!(
            elapsed < Duration::from_millis(400),
            "closed pipe burned budget {elapsed:?}: {msg}"
        );
        assert!(msg.contains("stdin closed"), "{msg}");
        assert!(!msg.contains("timed out"), "{msg}");
    }

    #[test]
    fn large_json_frame_drains_past_anon_pipe_buffer() {
        use windows::Win32::System::Pipes::{CreatePipe, GetNamedPipeInfo};

        let mut read = HANDLE::default();
        let mut write = HANDLE::default();
        unsafe { CreatePipe(&raw mut read, &raw mut write, None, 4096) }.expect("CreatePipe");

        let mut out_buf = 0u32;
        if unsafe { GetNamedPipeInfo(write, None, Some(&raw mut out_buf), None, None) }.is_err()
            || out_buf == 0
        {
            out_buf = 4096;
        }
        let min_frame = (out_buf as usize).max(64 * 1024) + 1;
        let value = json!({"pad": "a".repeat(min_frame)});
        let mut frame = Vec::new();
        write_pipe_frame(&mut frame, &value).expect("encode framed JSON");
        assert!(
            frame.len() > out_buf as usize,
            "fixture must exceed pipe buffer (frame {} vs nOutBufferSize {}); pad more",
            frame.len(),
            out_buf
        );
        assert!(
            frame.len() > 64 * 1024,
            "fixture must exceed 64 KiB floor (frame {})",
            frame.len()
        );
        let frame_len = frame.len();
        // HANDLE is !Send; the integer is the process-wide write-end kernel object
        // used only on the writer thread.
        let write_bits = write.0 as usize;

        let writer = std::thread::spawn(move || {
            let write = HANDLE(write_bits as *mut std::ffi::c_void);
            let mut n = 0u32;
            let result =
                unsafe { WriteFile(write, Some(frame.as_slice()), Some(&raw mut n), None) };
            let _ = unsafe { CloseHandle(write) };
            result.map(|_| n)
        });

        let started = Instant::now();
        let decoded = read_frame_timeout(read, Duration::from_secs(2)).expect("drain large frame");
        let elapsed = started.elapsed();
        let _ = unsafe { CloseHandle(read) };
        let written = writer.join().expect("writer thread").expect("WriteFile");
        assert_eq!(
            written as usize, frame_len,
            "writer must complete the frame"
        );
        assert!(
            elapsed < Duration::from_millis(500),
            "large-frame drain hung {elapsed:?} (deadline 2 s)"
        );
        assert_eq!(decoded, value);
    }

    #[test]
    fn read_exact_deadline_drains_partial_avail() {
        let src = include_str!("native_host.rs");
        let skip = concat!("(avail as usize) < ", "need");
        assert!(
            !src.contains(skip),
            "must not skip ReadFile until the whole remainder is buffered"
        );
        assert!(
            src.contains("need.min(avail") || src.contains("min(need, avail"),
            "ReadFile must take min(need, avail)"
        );
        assert!(
            src.contains("filled..filled + take") || src.contains("filled..filled+take"),
            "ReadFile must target buf[filled..filled+take]"
        );
        assert!(
            src.contains("avail == 0"),
            "sleep only when Peek reports no bytes"
        );
        let wait_until = concat!("WaitForSingleObject", " until avail");
        assert!(
            !src.contains(wait_until),
            "must not WaitForSingleObject until the whole remainder is buffered"
        );
    }
}
