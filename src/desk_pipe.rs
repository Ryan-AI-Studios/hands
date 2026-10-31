//! Elevated desk pipe server and stdio bridge.
//!
//! This module implements the High-IL Hands server (`serve --pipe`) and the
//! medium-IL stdio bridge (`mcp --bridge`) that connects Hermes to the
//! elevated server over a named pipe.

use std::sync::Mutex;

use rmcp::ServiceExt;
use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer, ServerOptions};
use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE};
use windows::Win32::Security::{
    ACE_FLAGS, ACL, ACL_REVISION, AddAccessAllowedAce, AddMandatoryAce, AdjustTokenPrivileges,
    CreateWellKnownSid, GetLengthSid, GetTokenInformation, InitializeAcl,
    InitializeSecurityDescriptor, LookupPrivilegeValueW, PSECURITY_DESCRIPTOR,
    SE_PRIVILEGE_ENABLED, SECURITY_DESCRIPTOR, SetSecurityDescriptorDacl,
    SetSecurityDescriptorSacl, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY, TOKEN_USER,
    TokenUser, WinBuiltinAdministratorsSid, WinMediumLabelSid,
};
use windows::Win32::System::Pipes::{GetNamedPipeServerProcessId, NMPWAIT_NOWAIT, WaitNamedPipeW};
use windows::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcess, OpenMutexW, OpenProcessToken,
};

use crate::error::HandsError;
use crate::lease;

// Constants not exported by windows crate
const SECURITY_DESCRIPTOR_REVISION: u32 = 1;
const SYNCHRONIZE: u32 = 0x00100000;
const READ_CONTROL: u32 = 0x0002_0000;
const FILE_READ_DATA: u32 = 0x1;
const FILE_WRITE_DATA: u32 = 0x2;
/// `FILE_APPEND_DATA` is also `FILE_CREATE_PIPE_INSTANCE`: the right to add an instance to an
/// existing pipe name. Clients never need it.
pub const FILE_CREATE_PIPE_INSTANCE: u32 = 0x4;
const FILE_READ_ATTRIBUTES: u32 = 0x80;
/// What a bridge client may do on the High server's pipe: read and write the byte stream, nothing
/// else. In particular not `FILE_CREATE_PIPE_INSTANCE`, so a same-user medium process cannot add
/// an impostor instance to the elevated pipe (a medium token has Administrators deny-only).
pub const CLIENT_PIPE_ACCESS: u32 =
    FILE_READ_DATA | FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE;
use windows::Win32::System::SystemServices::SYSTEM_MANDATORY_LABEL_NO_WRITE_UP;

// Pipe and mutex names
pub const PIPE_NAME: &str = r"\\.\pipe\hands-elev";
pub const MUTEX_NAME: &str = r"Local\hands-desk-driver";

// Role for the process-global slot
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DeskRole {
    #[default]
    Cli,
    StdioDesk,
    PipeDesk,
}

/// Process-global slot for role/pipe info (not OnceLock so tests can reset)
static ROLE_SLOT: Mutex<Option<(DeskRole, Option<String>)>> = Mutex::new(None);

/// Set the role and pipe name. Production is set-once; second set fails loud.
pub fn set_role(role: DeskRole, pipe: Option<String>) {
    let mut slot = ROLE_SLOT.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_some() {
        // Second set in production is a logic error
        panic!("desk role already set: {:?}", slot);
    }
    *slot = Some((role, pipe));
}

/// Get the current role and pipe name
pub fn get_role() -> (DeskRole, Option<String>) {
    ROLE_SLOT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or((DeskRole::Cli, None))
}

/// Test helper: hold the lock and clear the slot on drop
#[cfg(test)]
pub struct RoleSlotGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for RoleSlotGuard {
    fn drop(&mut self) {
        *ROLE_SLOT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

#[cfg(test)]
pub fn take_role_slot_for_test() -> RoleSlotGuard {
    let lock = crate::elevation::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *ROLE_SLOT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    RoleSlotGuard { _lock: lock }
}

/// Security descriptor holder that keeps all backing buffers alive (like native_host::CurrentUserSa)
struct MediumSecurityDescriptor {
    _sd: Box<SECURITY_DESCRIPTOR>,
    _acl: Vec<u8>,
    _sacl: Vec<u8>,
    sa: windows::Win32::Security::SECURITY_ATTRIBUTES,
}

impl MediumSecurityDescriptor {
    /// Permissive: current user `GENERIC_ALL`. Used for the mutex and for any non-High creator.
    fn new() -> Result<Self, HandsError> {
        Self::with_instance_restriction(false)
    }

    /// `restrict_instance_creation`: Administrators `GENERIC_ALL` plus the current user limited to
    /// `CLIENT_PIPE_ACCESS`. Only the elevated server (Administrators enabled) can then add pipe
    /// instances. Only meaningful when the creator itself is a High-IL admin token.
    fn with_instance_restriction(restrict_instance_creation: bool) -> Result<Self, HandsError> {
        // Get current user SID
        let mut token = HANDLE::default();
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) }?;

        let _token_guard = OwnedHandle(token);
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
        }?;
        let token_user = unsafe { &*buf.as_ptr().cast::<TOKEN_USER>() };
        let user_sid = token_user.User.Sid;

        // Get SID length for ACL size calculation
        let sid_len = unsafe { GetLengthSid(user_sid) } as usize;

        let mut admin_sid = [0u8; 68];
        let mut admin_len = admin_sid.len() as u32;
        let admins = windows::Win32::Security::PSID(admin_sid.as_mut_ptr().cast());
        if restrict_instance_creation {
            unsafe {
                CreateWellKnownSid(
                    WinBuiltinAdministratorsSid,
                    None,
                    Some(admins),
                    &raw mut admin_len,
                )
            }?;
        }

        // Build DACL. Default: current user GENERIC_ALL (like native_host.rs). Restricted:
        // Administrators GENERIC_ALL + current user client rights only.
        let ace_overhead = std::mem::size_of::<windows::Win32::Security::ACCESS_ALLOWED_ACE>()
            - std::mem::size_of::<u32>();
        let mut acl_len = std::mem::size_of::<ACL>() + ace_overhead + sid_len;
        if restrict_instance_creation {
            acl_len += ace_overhead + admin_len as usize;
        }
        let mut acl = vec![0u8; acl_len];
        unsafe {
            InitializeAcl(acl.as_mut_ptr().cast(), acl_len as u32, ACL_REVISION)?;
            if restrict_instance_creation {
                AddAccessAllowedAce(acl.as_mut_ptr().cast(), ACL_REVISION, GENERIC_ALL.0, admins)?;
                AddAccessAllowedAce(
                    acl.as_mut_ptr().cast(),
                    ACL_REVISION,
                    CLIENT_PIPE_ACCESS,
                    user_sid,
                )?;
            } else {
                AddAccessAllowedAce(
                    acl.as_mut_ptr().cast(),
                    ACL_REVISION,
                    GENERIC_ALL.0,
                    user_sid,
                )?;
            }
        }

        let mut label_sid = [0u8; 68];
        let mut label_len = label_sid.len() as u32;
        let label = windows::Win32::Security::PSID(label_sid.as_mut_ptr().cast());
        unsafe { CreateWellKnownSid(WinMediumLabelSid, None, Some(label), &raw mut label_len) }?;
        let sacl_len = std::mem::size_of::<ACL>()
            + std::mem::size_of::<windows::Win32::Security::SYSTEM_MANDATORY_LABEL_ACE>()
            - std::mem::size_of::<u32>()
            + label_len as usize;
        let mut sacl = vec![0u8; sacl_len];
        unsafe {
            InitializeAcl(sacl.as_mut_ptr().cast(), sacl_len as u32, ACL_REVISION)?;
            AddMandatoryAce(
                sacl.as_mut_ptr().cast(),
                ACL_REVISION,
                ACE_FLAGS(0),
                SYSTEM_MANDATORY_LABEL_NO_WRITE_UP,
                label,
            )?;
        }

        // Initialize security descriptor (boxed to keep stable address)
        let mut sd = Box::new(SECURITY_DESCRIPTOR::default());
        unsafe {
            InitializeSecurityDescriptor(
                PSECURITY_DESCRIPTOR(std::ptr::from_mut(&mut *sd).cast()),
                SECURITY_DESCRIPTOR_REVISION,
            )?;

            // Set DACL
            SetSecurityDescriptorDacl(
                PSECURITY_DESCRIPTOR(std::ptr::from_mut(&mut *sd).cast()),
                true,
                Some(acl.as_ptr().cast()),
                false,
            )?;

            SetSecurityDescriptorSacl(
                PSECURITY_DESCRIPTOR(std::ptr::from_mut(&mut *sd).cast()),
                true,
                Some(sacl.as_ptr().cast()),
                false,
            )?;
        }

        // Build SECURITY_ATTRIBUTES - pointers are set now while acl/sacl/sd are in local variables
        let sa = windows::Win32::Security::SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<windows::Win32::Security::SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::from_mut(&mut *sd).cast(),
            bInheritHandle: false.into(),
        };

        // Return struct that keeps all buffers alive together
        Ok(Self {
            _sd: sd,
            _acl: acl,
            _sacl: sacl,
            sa,
        })
    }
}

/// Build a security descriptor with:
/// - Current-user DACL (GENERIC_ALL)
/// - Medium mandatory label (SYSTEM_MANDATORY_LABEL_NO_WRITE_UP)
///
/// This descriptor is used for both the mutex and the pipe.
pub fn build_medium_security_descriptor()
-> Result<windows::Win32::Security::SECURITY_ATTRIBUTES, HandsError> {
    let holder = MediumSecurityDescriptor::new()?;
    // Return the SA but leak the holder to keep buffers alive for process lifetime
    let sa = holder.sa;
    Box::leak(Box::new(holder)); // Keep all buffers alive for process lifetime
    Ok(sa)
}

/// Descriptor for the desk pipe. `restrict_instance_creation` is for the High-IL server only.
pub fn build_pipe_security_descriptor(
    restrict_instance_creation: bool,
) -> Result<windows::Win32::Security::SECURITY_ATTRIBUTES, HandsError> {
    let holder = MediumSecurityDescriptor::with_instance_restriction(restrict_instance_creation)?;
    let sa = holder.sa;
    Box::leak(Box::new(holder));
    Ok(sa)
}

/// Enable SE_SECURITY_NAME privilege for the current token (required to set SACL on new object)
/// Returns a guard that disables the privilege on drop.
pub fn enable_security_privilege() -> Result<Option<SecurityPrivilegeGuard>, HandsError> {
    let mut token = HANDLE::default();
    unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &raw mut token,
        )
    }?;

    let mut guard = SecurityPrivilegeGuard {
        token,
        previous: TOKEN_PRIVILEGES::default(),
    };
    let mut luid = Default::default();
    unsafe {
        LookupPrivilegeValueW(
            None,
            windows::core::w!("SeSecurityPrivilege"),
            &raw mut luid,
        )
    }?;
    let tp = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [windows::Win32::Security::LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    let mut returned = 0;
    unsafe {
        AdjustTokenPrivileges(
            token,
            false,
            Some(&tp),
            std::mem::size_of::<TOKEN_PRIVILEGES>() as u32,
            Some(&raw mut guard.previous),
            Some(&raw mut returned),
        )
    }?;
    let error = unsafe { windows::Win32::Foundation::GetLastError() };
    if error.0 != 0 {
        return Err(HandsError::Input(format!(
            "cannot label the desk mutex and pipe: privilege enable failed ({})",
            error.0
        )));
    }
    Ok(Some(guard))
}

/// Restore the previous privilege state and close the token even on failure.
pub struct SecurityPrivilegeGuard {
    token: HANDLE,
    previous: TOKEN_PRIVILEGES,
}

impl Drop for SecurityPrivilegeGuard {
    fn drop(&mut self) {
        if self.previous.PrivilegeCount != 0 {
            let _ = unsafe {
                AdjustTokenPrivileges(self.token, false, Some(&self.previous), 0, None, None)
            };
        }
        let _ = unsafe { CloseHandle(self.token) };
    }
}

/// Create the desk mutex with the medium security descriptor.
/// Returns Ok(()) if we own the mutex (first creator).
/// Returns Err with "already running" if mutex exists (ERROR_ALREADY_EXISTS = 183).
/// Returns Err with "not openable at this integrity" if access denied.
pub fn acquire_desk_mutex(
    sa: &windows::Win32::Security::SECURITY_ATTRIBUTES,
) -> Result<HANDLE, HandsError> {
    acquire_named_desk_mutex(sa, MUTEX_NAME)
}

fn acquire_named_desk_mutex(
    sa: &windows::Win32::Security::SECURITY_ATTRIBUTES,
    mutex_name: &str,
) -> Result<HANDLE, HandsError> {
    let name: Vec<u16> = mutex_name.encode_utf16().chain(Some(0)).collect();
    let handle = unsafe {
        CreateMutexW(
            Some(sa as *const _),
            true,
            windows::core::PCWSTR(name.as_ptr()),
        )
    }
    .map_err(|e| {
        if e.code() == windows::core::HRESULT::from_win32(5) {
            HandsError::Input(format!(
                "desk mutex is not openable at this integrity ({mutex_name})"
            ))
        } else {
            HandsError::from(e)
        }
    })?;
    let err = unsafe { windows::Win32::Foundation::GetLastError() };
    if err.0 == 183 {
        // ERROR_ALREADY_EXISTS
        let _ = unsafe { CloseHandle(handle) };
        return Err(HandsError::Input(format!(
            "desk driver already running ({mutex_name})"
        )));
    }
    if err.0 == 5 {
        // ERROR_ACCESS_DENIED
        let _ = unsafe { CloseHandle(handle) };
        return Err(HandsError::Input(format!(
            "desk mutex is not openable at this integrity ({mutex_name})"
        )));
    }
    Ok(handle)
}

/// Probe the mutex without taking ownership (bridge uses this)
/// Uses SYNCHRONIZE only (0x100000)
pub fn probe_desk_mutex() -> Result<MutexProbeResult, HandsError> {
    probe_named_desk_mutex(MUTEX_NAME)
}

fn probe_named_desk_mutex(mutex_name: &str) -> Result<MutexProbeResult, HandsError> {
    let name: Vec<u16> = mutex_name.encode_utf16().chain(Some(0)).collect();
    let handle = unsafe {
        OpenMutexW(
            windows::Win32::System::Threading::SYNCHRONIZATION_ACCESS_RIGHTS(SYNCHRONIZE),
            false,
            windows::core::PCWSTR(name.as_ptr()),
        )
    };
    let handle = match handle {
        Ok(h) => h,
        Err(e) => {
            // The windows crate already captured the last error into `e`.
            let code = e.code();
            if code == windows::core::HRESULT::from_win32(2) {
                // ERROR_FILE_NOT_FOUND
                return Ok(MutexProbeResult::NotFound);
            }
            if code == windows::core::HRESULT::from_win32(5) {
                // ERROR_ACCESS_DENIED
                return Ok(MutexProbeResult::AccessDenied);
            }
            return Ok(MutexProbeResult::Other(code.0 as u32));
        }
    };
    let _ = unsafe { CloseHandle(handle) };
    Ok(MutexProbeResult::Found)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutexProbeResult {
    NotFound,
    Found,
    AccessDenied,
    Other(u32),
}

/// Check if the named pipe exists without connecting
pub fn pipe_exists(pipe_name: &str) -> bool {
    let name: Vec<u16> = pipe_name.encode_utf16().chain(Some(0)).collect();
    let result = unsafe { WaitNamedPipeW(windows::core::PCWSTR(name.as_ptr()), NMPWAIT_NOWAIT) };
    result.as_bool()
}

/// Run the elevated pipe server loop.
/// This is the `serve --pipe` entry point.
/// It takes the mutex, creates the pipe, and accepts connections in a loop.
pub async fn serve_pipe(pipe_name: &str) -> Result<(), HandsError> {
    serve_pipe_named(pipe_name, MUTEX_NAME).await
}

/// `serve_pipe` with an explicit desk-mutex name so qualification harnesses and tests
/// never grab `Local\hands-desk-driver`. The `serve` CLI always passes `MUTEX_NAME`.
pub async fn serve_pipe_named(pipe_name: &str, mutex_name: &str) -> Result<(), HandsError> {
    if !pipe_name.starts_with(r"\\.\pipe\") {
        return Err(HandsError::Input(
            "pipe name must start with \\\\.\\pipe\\".into(),
        ));
    }

    // Mutex: current-user DACL + medium label. Pipe: same label, and when this creator is High the
    // DACL also withholds FILE_CREATE_PIPE_INSTANCE from the (medium) user so nobody but the
    // elevated server can add an instance to the pipe name.
    let is_high = crate::elevation::process_integrity_rid()? >= crate::elevation::HIGH_IL_RID;
    let sa = build_medium_security_descriptor()?;
    let pipe_sa = build_pipe_security_descriptor(is_high)?;

    // High creators must never silently create High-labelled desk objects.
    let privilege = if is_high {
        enable_security_privilege()
            .map_err(|e| HandsError::Input(format!("cannot label the desk mutex and pipe: {e}")))?
    } else {
        None
    };

    // Acquire the desk mutex (fails if another driver holds it)
    let _mutex = OwnedHandle(acquire_named_desk_mutex(&sa, mutex_name)?);
    drop(privilege);

    // Set role before installing lease
    set_role(DeskRole::PipeDesk, Some(pipe_name.to_string()));

    // Install the desk lease (WH_KEYBOARD_LL / WH_MOUSE_LL)
    let _lease = lease::install()?;

    serve_loop(pipe_name, &pipe_sa).await
}

/// Keep the desk-driver mutex handle alive for the stdio server lifetime.
pub fn hold_desk_mutex(
    sa: &windows::Win32::Security::SECURITY_ATTRIBUTES,
) -> Result<DeskMutexGuard, HandsError> {
    Ok(DeskMutexGuard(OwnedHandle(acquire_desk_mutex(sa)?)))
}

pub struct DeskMutexGuard(#[allow(dead_code)] OwnedHandle);

struct OwnedHandle(HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn create_listener(
    pipe_name: &str,
    sa: &windows::Win32::Security::SECURITY_ATTRIBUTES,
    first: bool,
) -> Result<NamedPipeServer, HandsError> {
    let _privilege = if crate::elevation::process_integrity_rid()? >= crate::elevation::HIGH_IL_RID
    {
        enable_security_privilege()
            .map_err(|e| HandsError::Input(format!("cannot label the desk mutex and pipe: {e}")))?
    } else {
        None
    };
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                pipe_name,
                std::ptr::from_ref(sa).cast_mut().cast(),
            )
    }
    .map_err(|e| HandsError::Input(format!("pipe create failed: {e}")))
}

/// Accept/serve loop without desk lease installation, also used by same-IL tests.
async fn serve_loop(
    pipe_name: &str,
    sa: &windows::Win32::Security::SECURITY_ATTRIBUTES,
) -> Result<(), HandsError> {
    let mut server = create_listener(pipe_name, sa, true)?;
    loop {
        server
            .connect()
            .await
            .map_err(|e| HandsError::Input(format!("pipe connect failed: {e}")))?;
        let next_server = create_listener(pipe_name, sa, false)?;
        match crate::mcp::HandsServer.serve(server).await {
            Ok(service) => {
                let _ = service.waiting().await;
            }
            Err(e) => eprintln!("serve error: {e}"),
        }
        server = next_server;
    }
}

/// Run the stdio bridge (`mcp --bridge`).
/// Probes mutex first, then pipe presence, then connects and copies bytes.
pub async fn run_bridge(pipe_name: &str) -> Result<(), HandsError> {
    run_bridge_named(pipe_name, MUTEX_NAME, crate::elevation::HIGH_IL_RID).await
}

/// Open the desk pipe with exactly `CLIENT_PIPE_ACCESS` (no `FILE_CREATE_PIPE_INSTANCE`), as the
/// High server's DACL requires. `ClientOptions::open` asks for GENERIC_WRITE, which includes it.
fn open_bridge_client(pipe_name: &str) -> std::io::Result<NamedPipeClient> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::IntoRawHandle;
    const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
    let file = std::fs::OpenOptions::new()
        .access_mode(CLIENT_PIPE_ACCESS)
        .custom_flags(FILE_FLAG_OVERLAPPED)
        .open(pipe_name)?;
    // SAFETY: the handle is a freshly opened, overlapped pipe client handle we exclusively own.
    unsafe { NamedPipeClient::from_raw_handle(file.into_raw_handle()) }
}

/// The bridge's whole point is reaching the privileged server. A same-user medium process can
/// squat the whole pipe name while no High server runs. `GetNamedPipeServerProcessId` reports the
/// pipe's FIRST creator (measured: it does not follow the connected instance), so this catches a
/// squatted name; adding an instance to a live High pipe is stopped by the pipe DACL instead
/// (`CLIENT_PIPE_ACCESS` has no `FILE_CREATE_PIPE_INSTANCE`). A medium process cannot forge a High
/// token. Fails closed when the token cannot be read.
fn verify_pipe_server(client: &NamedPipeClient, min_rid: u32) -> Result<(), HandsError> {
    use std::os::windows::io::AsRawHandle;
    let mut pid = 0u32;
    unsafe { GetNamedPipeServerProcessId(HANDLE(client.as_raw_handle()), &raw mut pid) }
        .map_err(|e| HandsError::Input(format!("cannot identify the pipe server process: {e}")))?;
    let rid = crate::elevation::pid_integrity_rid(pid).map_err(|e| {
        HandsError::Input(format!("cannot verify the pipe server (pid {pid}): {e}"))
    })?;
    if rid < min_rid {
        return Err(HandsError::Input(format!(
            "pipe server (pid {pid}) is not elevated (integrity 0x{rid:x}, need >= 0x{min_rid:x}); refusing to bridge to a possible impostor; not starting a local desk server"
        )));
    }
    Ok(())
}

/// Bridge preflight: mutex first (`SYNCHRONIZE` only), then pipe presence
/// (`NMPWAIT_NOWAIT`, does not connect). Never opens the pipe, so a failed
/// mutex probe cannot burn the listening instance.
fn bridge_preflight(pipe_name: &str, mutex_name: &str) -> Result<(), HandsError> {
    if !pipe_name.starts_with(r"\\.\pipe\") {
        return Err(HandsError::Input(
            "bridge pipe name must start with \\\\.\\pipe\\".into(),
        ));
    }
    match probe_named_desk_mutex(mutex_name)? {
        MutexProbeResult::NotFound => {
            return Err(HandsError::Input(
                "privileged pipe not available: no desk driver; desk mutex not found; not starting a local desk server"
                    .into(),
            ));
        }
        MutexProbeResult::AccessDenied => {
            return Err(HandsError::Input(
                "privileged pipe not available: desk mutex is not openable at this integrity; not starting a local desk server"
                    .into(),
            ));
        }
        MutexProbeResult::Other(code) => {
            return Err(HandsError::Input(format!(
                "privileged pipe not available: desk mutex probe returned {code}; not starting a local desk server"
            )));
        }
        MutexProbeResult::Found => {}
    }
    if !pipe_exists(pipe_name) {
        return Err(HandsError::Input(
            "privileged pipe not available: desk driver already running; not starting a local desk server"
                .into(),
        ));
    }
    Ok(())
}

/// `run_bridge` with an explicit mutex name (qualification harness and tests).
pub async fn run_bridge_named(
    pipe_name: &str,
    mutex_name: &str,
    min_server_rid: u32,
) -> Result<(), HandsError> {
    bridge_preflight(pipe_name, mutex_name)?;
    let mut connected = open_bridge_client(pipe_name)
        .map_err(|e| HandsError::Input(format!("bridge pipe connect failed: {e}")))?;
    verify_pipe_server(&connected, min_server_rid)?;
    let mut stdio = tokio::io::join(tokio::io::stdin(), tokio::io::stdout());
    tokio::io::copy_bidirectional(&mut stdio, &mut connected)
        .await
        .map_err(|e| HandsError::Input(format!("bridge copy failed: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::windows::named_pipe::ClientOptions;

    #[test]
    fn descriptor_sacl_has_medium_no_write_up_ace() {
        use windows::Win32::Security::{
            GetAce, GetSecurityDescriptorSacl, IsWellKnownSid, PSID, SYSTEM_MANDATORY_LABEL_ACE,
        };
        let holder = MediumSecurityDescriptor::new().unwrap();
        let mut present = windows::core::BOOL::default();
        let mut defaulted = windows::core::BOOL::default();
        let mut sacl = std::ptr::null_mut();
        unsafe {
            GetSecurityDescriptorSacl(
                PSECURITY_DESCRIPTOR(holder.sa.lpSecurityDescriptor),
                &raw mut present,
                &raw mut sacl,
                &raw mut defaulted,
            )
            .unwrap();
            assert!(present.as_bool());
            assert_eq!((*sacl).AceCount, 1);
            let mut ace = std::ptr::null_mut();
            GetAce(sacl, 0, &raw mut ace).unwrap();
            let label = &*ace.cast::<SYSTEM_MANDATORY_LABEL_ACE>();
            assert_eq!(label.Header.AceType, 0x11);
            assert_eq!(label.Mask, SYSTEM_MANDATORY_LABEL_NO_WRITE_UP);
            assert!(
                IsWellKnownSid(
                    PSID(std::ptr::from_ref(&label.SidStart).cast_mut().cast()),
                    WinMediumLabelSid
                )
                .as_bool()
            );
        }
    }

    #[test]
    fn labelled_mutex_second_create_is_ok_with_already_exists() {
        let holder = MediumSecurityDescriptor::new().unwrap();
        let name = format!("Local\\hands-test-{}", uuid::Uuid::new_v4());
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let create = |sa: &windows::Win32::Security::SECURITY_ATTRIBUTES| unsafe {
            CreateMutexW(
                Some(std::ptr::from_ref(sa)),
                false,
                windows::core::PCWSTR(name.as_ptr()),
            )
        };
        let first = match create(&holder.sa) {
            Ok(h) => h,
            Err(e) if e.code() == windows::core::HRESULT::from_win32(1314) => {
                // Test #8 fallback only; test #9 still verifies the explicit label.
                unsafe {
                    SetSecurityDescriptorSacl(
                        PSECURITY_DESCRIPTOR(holder.sa.lpSecurityDescriptor),
                        false,
                        None,
                        false,
                    )
                }
                .unwrap();
                create(&holder.sa).unwrap()
            }
            Err(e) => panic!("labelled mutex create: {e}"),
        };
        let _first = OwnedHandle(first);
        let second = create(&holder.sa).unwrap();
        let error = unsafe { windows::Win32::Foundation::GetLastError() };
        let _second = OwnedHandle(second);
        assert_eq!(error.0, 183);
        let error = acquire_named_desk_mutex(
            &holder.sa,
            &String::from_utf16_lossy(&name[..name.len() - 1]),
        )
        .unwrap_err();
        assert!(error.to_string().contains("desk driver already running"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pipe_serves_initialize_then_accepts_second_connection() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let holder = MediumSecurityDescriptor::new().unwrap();
        let name = format!(r"\\.\pipe\hands-test-{}", uuid::Uuid::new_v4());
        let loop_task = serve_loop(&name, &holder.sa);
        let client_task = async {
            for _ in 0..2 {
                let mut client = loop {
                    match ClientOptions::new().open(&name) {
                        Ok(c) => break c,
                        Err(e) if matches!(e.raw_os_error(), Some(2 | 231)) => {
                            tokio::task::yield_now().await
                        }
                        Err(e) => panic!("connect: {e}"),
                    }
                };
                client.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1\"}}}\n").await.unwrap();
                let mut reader = BufReader::new(client);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let response: serde_json::Value = serde_json::from_str(&line).unwrap();
                assert_eq!(response["id"], 1);
                assert!(response.get("result").is_some(), "{response}");
                reader
                    .get_mut()
                    .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
                    .await
                    .unwrap();
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::select! { result = loop_task => panic!("server stopped: {result:?}"), _ = client_task => {} }
        }).await.expect("two initialize responses before timeout");
    }

    #[tokio::test]
    async fn serve_rejects_non_pipe_path() {
        let error = serve_pipe("C:\\not-a-pipe").await.unwrap_err();
        assert!(error.to_string().contains(r"\\.\pipe\"));
    }

    #[test]
    fn security_descriptor_builds() {
        let sa = build_medium_security_descriptor().expect("build descriptor");
        assert!(!sa.lpSecurityDescriptor.is_null());
    }

    #[test]
    fn role_slot_defaults_to_cli() {
        let _g = take_role_slot_for_test();
        let (role, pipe) = get_role();
        assert_eq!(role, DeskRole::Cli);
        assert_eq!(pipe, None);
    }

    #[test]
    fn role_slot_set_once() {
        let _g = take_role_slot_for_test();
        set_role(DeskRole::PipeDesk, Some("test".into()));
        let (role, pipe) = get_role();
        assert_eq!(role, DeskRole::PipeDesk);
        assert_eq!(pipe, Some("test".into()));
    }

    #[test]
    #[should_panic(expected = "desk role already set")]
    fn role_slot_second_set_panics() {
        let _g = take_role_slot_for_test();
        set_role(DeskRole::PipeDesk, Some("test".into()));
        set_role(DeskRole::StdioDesk, Some("other".into()));
    }

    fn unique(prefix: &str) -> String {
        format!("{prefix}-{}", uuid::Uuid::new_v4())
    }

    #[test]
    fn bridge_absent_pipe_mutex_free_does_not_start_local_desk() {
        let err = bridge_preflight(
            &format!(r"\\.\pipe\{}", unique("hands-test-absent")),
            &format!(r"Local\{}", unique("hands-test-free")),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("privileged pipe not available"), "{err}");
        assert!(err.contains("not starting a local desk server"), "{err}");
    }

    #[test]
    fn bridge_absent_pipe_mutex_held_names_running_driver() {
        let holder = MediumSecurityDescriptor::new().unwrap();
        let mutex = format!(r"Local\{}", unique("hands-test-held"));
        let _held = OwnedHandle(acquire_named_desk_mutex(&holder.sa, &mutex).unwrap());
        let err = bridge_preflight(
            &format!(r"\\.\pipe\{}", unique("hands-test-absent")),
            &mutex,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("privileged pipe not available"), "{err}");
        assert!(err.contains("not starting a local desk server"), "{err}");
        assert!(err.contains("desk driver already running"), "{err}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bridge_pipe_up_mutex_free_says_no_desk_driver() {
        let holder = MediumSecurityDescriptor::new().unwrap();
        let pipe = format!(r"\\.\pipe\{}", unique("hands-test-up"));
        let _listener = create_listener(&pipe, &holder.sa, true).unwrap();
        let err = bridge_preflight(&pipe, &format!(r"Local\{}", unique("hands-test-free")))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no desk driver"), "{err}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bridge_pipe_up_mutex_denied_does_not_connect() {
        use windows::Win32::Security::{ACL, ACL_REVISION, InitializeAcl};
        // Empty DACL: nobody (including this process) gets SYNCHRONIZE on open.
        let mut acl = vec![0u8; std::mem::size_of::<ACL>() + 8];
        let mut sd = Box::new(SECURITY_DESCRIPTOR::default());
        unsafe {
            InitializeAcl(acl.as_mut_ptr().cast(), acl.len() as u32, ACL_REVISION).unwrap();
            InitializeSecurityDescriptor(
                PSECURITY_DESCRIPTOR(std::ptr::from_mut(&mut *sd).cast()),
                SECURITY_DESCRIPTOR_REVISION,
            )
            .unwrap();
            SetSecurityDescriptorDacl(
                PSECURITY_DESCRIPTOR(std::ptr::from_mut(&mut *sd).cast()),
                true,
                Some(acl.as_ptr().cast()),
                false,
            )
            .unwrap();
        }
        let deny_sa = windows::Win32::Security::SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<windows::Win32::Security::SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::from_mut(&mut *sd).cast(),
            bInheritHandle: false.into(),
        };
        let mutex = format!(r"Local\{}", unique("hands-test-denied"));
        let wide: Vec<u16> = mutex.encode_utf16().chain(Some(0)).collect();
        let _held = OwnedHandle(
            unsafe {
                CreateMutexW(
                    Some(std::ptr::from_ref(&deny_sa)),
                    false,
                    windows::core::PCWSTR(wide.as_ptr()),
                )
            }
            .unwrap(),
        );
        let holder = MediumSecurityDescriptor::new().unwrap();
        let pipe = format!(r"\\.\pipe\{}", unique("hands-test-up"));
        let listener = create_listener(&pipe, &holder.sa, true).unwrap();
        let err = bridge_preflight(&pipe, &mutex).unwrap_err().to_string();
        assert!(
            err.contains("desk mutex is not openable at this integrity"),
            "{err}"
        );
        // Preflight must not have consumed the listening instance.
        tokio::time::timeout(std::time::Duration::from_millis(200), listener.connect())
            .await
            .expect_err("no client may have connected during preflight");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn verify_pipe_server_requires_server_integrity_at_or_above_min() {
        let holder = MediumSecurityDescriptor::new().unwrap();
        let pipe = format!(r"\\.\pipe\{}", unique("hands-test-verify"));
        let _listener = create_listener(&pipe, &holder.sa, true).unwrap();
        let client = ClientOptions::new().open(&pipe).unwrap();
        let own = crate::elevation::process_integrity_rid().unwrap();
        // The server end is this very process: exactly our own integrity passes...
        verify_pipe_server(&client, own).unwrap();
        // ...and one level above does not (an impostor at medium cannot satisfy a High minimum).
        let err = verify_pipe_server(&client, own + 0x1000)
            .unwrap_err()
            .to_string();
        assert!(err.contains("is not elevated"), "{err}");
        assert!(err.contains("not starting a local desk server"), "{err}");
    }

    #[test]
    fn restricted_pipe_dacl_gives_admins_everything_and_the_user_no_instance_right() {
        use windows::Win32::Security::{
            ACCESS_ALLOWED_ACE, GetAce, GetSecurityDescriptorDacl, IsWellKnownSid, PSID,
        };
        let ace_masks = |holder: &MediumSecurityDescriptor| {
            let mut present = windows::core::BOOL::default();
            let mut defaulted = windows::core::BOOL::default();
            let mut dacl = std::ptr::null_mut();
            let mut out = Vec::new();
            unsafe {
                GetSecurityDescriptorDacl(
                    PSECURITY_DESCRIPTOR(holder.sa.lpSecurityDescriptor),
                    &raw mut present,
                    &raw mut dacl,
                    &raw mut defaulted,
                )
                .unwrap();
                assert!(present.as_bool());
                for i in 0..(*dacl).AceCount {
                    let mut ace = std::ptr::null_mut();
                    GetAce(dacl, u32::from(i), &raw mut ace).unwrap();
                    let ace = &*ace.cast::<ACCESS_ALLOWED_ACE>();
                    let sid = PSID(std::ptr::from_ref(&ace.SidStart).cast_mut().cast());
                    out.push((
                        ace.Mask,
                        IsWellKnownSid(sid, WinBuiltinAdministratorsSid).as_bool(),
                    ));
                }
            }
            out
        };
        let permissive = ace_masks(&MediumSecurityDescriptor::new().unwrap());
        assert_eq!(permissive, vec![(GENERIC_ALL.0, false)]);
        let restricted =
            ace_masks(&MediumSecurityDescriptor::with_instance_restriction(true).unwrap());
        assert_eq!(
            restricted,
            vec![(GENERIC_ALL.0, true), (CLIENT_PIPE_ACCESS, false)]
        );
        assert_eq!(CLIENT_PIPE_ACCESS & FILE_CREATE_PIPE_INSTANCE, 0);
        assert_ne!(CLIENT_PIPE_ACCESS & FILE_READ_DATA, 0);
        assert_ne!(CLIENT_PIPE_ACCESS & FILE_WRITE_DATA, 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bridge_client_opens_a_restricted_pipe_and_a_non_admin_cannot_add_instances() {
        let holder = MediumSecurityDescriptor::with_instance_restriction(true).unwrap();
        let pipe = format!(r"\\.\pipe\{}", unique("hands-test-restricted"));
        let _first = create_listener(&pipe, &holder.sa, true).unwrap();
        // The reduced access mask is enough to talk to the server.
        let _client = open_bridge_client(&pipe).expect("bridge client opens the restricted pipe");
        let own = crate::elevation::process_integrity_rid().unwrap();
        if own < crate::elevation::HIGH_IL_RID {
            // A medium token has Administrators deny-only: no FILE_CREATE_PIPE_INSTANCE, so it
            // cannot add an impostor instance, and a GENERIC_WRITE open is refused too.
            let squat = create_listener(&pipe, &holder.sa, false);
            assert!(squat.is_err(), "medium creator must not add an instance");
            let wide = tokio::net::windows::named_pipe::ClientOptions::new().open(&pipe);
            assert!(wide.is_err(), "GENERIC_WRITE open must be refused");
        }
    }

    #[test]
    fn bridge_never_becomes_a_desk() {
        let src = include_str!("desk_pipe.rs");
        let production = src.split("#[cfg(test)]\nmod tests {").next().unwrap();
        let body = production
            .split("async fn run_bridge_named(")
            .nth(1)
            .unwrap()
            .split("\n}\n")
            .next()
            .unwrap();
        assert!(!body.contains("lease::install"));
        assert!(!body.contains("HandsServer"));
        assert!(!body.contains("set_role"));
        assert!(
            body.contains("verify_pipe_server("),
            "bridge must verify the pipe server before copying bytes"
        );
        let verify_at = body.find("verify_pipe_server(").unwrap();
        assert!(body.find("copy_bidirectional").unwrap() > verify_at);
        let pre = production
            .split("fn bridge_preflight(")
            .nth(1)
            .unwrap()
            .split("\n}\n")
            .next()
            .unwrap();
        assert!(
            !pre.contains("ClientOptions"),
            "preflight must not open the pipe"
        );
        assert!(!production.contains("uiAccess"));
        assert!(!production.contains("stop-request"));
    }

    #[test]
    fn pipe_name_constant() {
        assert_eq!(PIPE_NAME, r"\\.\pipe\hands-elev");
    }

    #[test]
    fn mutex_name_constant() {
        assert_eq!(MUTEX_NAME, r"Local\hands-desk-driver");
    }
}
