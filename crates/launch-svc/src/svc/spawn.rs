//! Starting the manager in the active console user's session.
//!
//! The service runs as LocalSystem, but the manager must run as the user at
//! the keyboard (their keyring, their app data). The recipe:
//!
//! 1. `WTSGetActiveConsoleSessionId`: the console session, `0xFFFFFFFF`
//!    when nobody is logged in.
//! 2. `WTSQueryUserToken`: that user's primary token.
//! 3. `CreateEnvironmentBlock`: their environment (`APPDATA` and friends).
//! 4. `CreateProcessAsUserW` with no console.
//!
//! Where the manager lives comes from, in order: `INFINITY_MANAGER_EXE`
//! (development), the path hint every manager launch writes to
//! `%ProgramData%\InfinityManager\bin\manager-path.txt`, then the service's
//! own folder.

#![allow(unsafe_code)]

use std::ffi::OsStr;
use std::iter::once;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;

use tracing::{info, warn};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows_sys::Win32::System::RemoteDesktop::{WTSGetActiveConsoleSessionId, WTSQueryUserToken};
use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, DETACHED_PROCESS, OpenProcess,
    OpenProcessToken, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, STARTUPINFOW,
};

const NO_SESSION: u32 = 0xFFFF_FFFF;

/// Starts the manager for the console user unless it is already running
/// for them.
pub(super) fn ensure_manager_running(exe_name: &str) -> crate::Result {
    // SAFETY: no arguments; returns a session id or NO_SESSION.
    let session = unsafe { WTSGetActiveConsoleSessionId() };
    if session == NO_SESSION {
        return Err("no active console session (locked or logged out)".into());
    }
    let token = user_token(session)?;

    // Scope the "already running" check to this user: a manager in another
    // session cannot serve this user's sim.
    let sid = match token_sid(token.0) {
        Ok(sid) => Some(sid),
        Err(e) => {
            warn!(error = %e, "could not read the console user's SID; checking by name only");
            None
        }
    };
    if manager_running(exe_name, sid.as_deref()) {
        info!("manager already running for the console user");
        return Ok(());
    }

    let exe = manager_exe(exe_name)?;
    info!(exe = %exe.display(), session, "starting the manager in the user session");
    spawn_as_user(token.0, &exe)
}

struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: we own the handle and close it once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(once(0)).collect()
}

fn user_token(session: u32) -> crate::Result<Owned> {
    let mut token: HANDLE = null_mut();
    // SAFETY: `token` is a valid out pointer.
    if unsafe { WTSQueryUserToken(session, &mut token) } == 0 {
        // SAFETY: reads the calling thread's last error.
        let err = unsafe { GetLastError() };
        return Err(format!("WTSQueryUserToken(session {session}) failed: {err}").into());
    }
    Ok(Owned(token))
}

fn manager_exe(name: &str) -> crate::Result<PathBuf> {
    if let Some(p) = std::env::var_os("INFINITY_MANAGER_EXE").map(PathBuf::from) {
        if p.is_file() {
            return Ok(p);
        }
        warn!(path = %p.display(), "INFINITY_MANAGER_EXE does not exist; ignoring it");
    }
    if let Some(hint) = path_hint() {
        if hint.is_file() {
            return Ok(hint);
        }
        warn!(path = %hint.display(), "manager-path hint points at a missing file");
    }
    let me = std::env::current_exe()?;
    let candidate = me
        .parent()
        .ok_or("the service executable has no folder")?
        .join(name);
    if candidate.is_file() {
        Ok(candidate)
    } else {
        Err(format!("manager not found at {}", candidate.display()).into())
    }
}

/// The manager path the most recent manager launch recorded.
fn path_hint() -> Option<PathBuf> {
    let pd = std::env::var_os("ProgramData")?;
    let raw = std::fs::read_to_string(
        PathBuf::from(pd)
            .join("InfinityManager")
            .join("bin")
            .join("manager-path.txt"),
    )
    .ok()?;
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

fn spawn_as_user(token: HANDLE, exe: &Path) -> crate::Result {
    let mut env: *mut std::ffi::c_void = null_mut();
    // SAFETY: `env` is a valid out pointer and `token` a live user token.
    if unsafe { CreateEnvironmentBlock(&mut env, token, 0) } == 0 {
        // SAFETY: as above.
        let err = unsafe { GetLastError() };
        return Err(format!("CreateEnvironmentBlock failed: {err}").into());
    }

    // CreateProcessAsUserW may write to the command line, so it is a
    // mutable buffer; every buffer outlives the call.
    let mut cmd = wide(OsStr::new(&format!("\"{}\" --remote-start", exe.display())));
    let exe_w = wide(exe.as_os_str());
    let mut desktop = wide(OsStr::new(r"winsta0\default"));

    // SAFETY: plain-old-data structs, zero is their documented default.
    let mut si: STARTUPINFOW = unsafe { zeroed() };
    si.cb = u32::try_from(size_of::<STARTUPINFOW>()).unwrap_or(u32::MAX);
    si.lpDesktop = desktop.as_mut_ptr();
    // SAFETY: as above.
    let mut pi: PROCESS_INFORMATION = unsafe { zeroed() };

    // SAFETY: every pointer is either null or points at a live,
    // NUL-terminated buffer declared above; `env` came from
    // CreateEnvironmentBlock and is destroyed right after.
    let (ok, err) = unsafe {
        let ok = CreateProcessAsUserW(
            token,
            exe_w.as_ptr(),
            cmd.as_mut_ptr(),
            null_mut(),
            null_mut(),
            0,
            CREATE_UNICODE_ENVIRONMENT | DETACHED_PROCESS,
            env,
            null_mut(),
            &si,
            &mut pi,
        );
        let err = GetLastError();
        DestroyEnvironmentBlock(env);
        (ok, err)
    };
    if ok == 0 {
        return Err(format!("CreateProcessAsUserW failed: {err}").into());
    }
    info!(pid = pi.dwProcessId, "manager started");
    // SAFETY: both handles were just returned to us.
    unsafe {
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
    }
    Ok(())
}

/// A token's user SID in string form (`S-1-5-21-…`).
fn token_sid(token: HANDLE) -> crate::Result<String> {
    let mut needed: u32 = 0;
    // SAFETY: a size probe with a null buffer, as documented.
    unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut needed) };
    if needed == 0 {
        return Err("GetTokenInformation size probe returned 0".into());
    }
    // u64 storage keeps TOKEN_USER suitably aligned.
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    // SAFETY: `buf` holds at least `needed` bytes.
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buf.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        // SAFETY: reads the last error.
        let err = unsafe { GetLastError() };
        return Err(format!("GetTokenInformation failed: {err}").into());
    }
    // SAFETY: the call above filled `buf` with a TOKEN_USER.
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let mut text: *mut u16 = null_mut();
    // SAFETY: `sid` points into `buf`, which is alive; `text` is an out
    // pointer the system allocates and we free with LocalFree.
    unsafe {
        if ConvertSidToStringSidW(sid, &mut text) == 0 {
            return Err(format!("ConvertSidToStringSidW failed: {}", GetLastError()).into());
        }
        let mut len = 0usize;
        while *text.add(len) != 0 {
            len += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
        LocalFree(text as HLOCAL);
        Ok(s)
    }
}

/// Whether `exe_name` runs as `owner` (any user when `None`). Any API
/// failure answers `false`: a duplicate manager is harmless, as its
/// single-instance lock forwards to the running one and exits.
fn manager_running(exe_name: &str, owner: Option<&str>) -> bool {
    // SAFETY: snapshot of all processes; closed below.
    let snap = Owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) });
    if snap.0 == INVALID_HANDLE_VALUE {
        return false;
    }
    // SAFETY: POD struct; dwSize set before use as required.
    let mut entry: PROCESSENTRY32W = unsafe { zeroed() };
    entry.dwSize = u32::try_from(size_of::<PROCESSENTRY32W>()).unwrap_or(u32::MAX);
    // SAFETY: `snap` is a live snapshot and `entry` is initialised.
    if unsafe { Process32FirstW(snap.0, &mut entry) } == 0 {
        return false;
    }
    loop {
        let len = entry
            .szExeFile
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(entry.szExeFile.len());
        let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
        if name.eq_ignore_ascii_case(exe_name)
            && owner.is_none_or(|sid| process_sid(entry.th32ProcessID).as_deref() == Some(sid))
        {
            return true;
        }
        // SAFETY: as above.
        if unsafe { Process32NextW(snap.0, &mut entry) } == 0 {
            return false;
        }
    }
}

fn process_sid(pid: u32) -> Option<String> {
    // SAFETY: opening with query rights only; the handle is owned below.
    let process = Owned(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) });
    if process.0.is_null() {
        return None;
    }
    let mut token: HANDLE = null_mut();
    // SAFETY: `process` is live and `token` is an out pointer.
    if unsafe { OpenProcessToken(process.0, TOKEN_QUERY, &mut token) } == 0 || token.is_null() {
        return None;
    }
    let token = Owned(token);
    token_sid(token.0).ok()
}
