//! The few registry values the environment line reports.

#![allow(unsafe_code)]

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};

const INTERNET_SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
const AUTH_ROOT_POLICY: &str = r"Software\Policies\Microsoft\SystemCertificates\AuthRoot";

/// `(ProxyServer when ProxyEnable, AutoConfigURL)` for the current user.
pub(crate) fn proxy() -> (Option<String>, Option<String>) {
    let enabled = dword(HKEY_CURRENT_USER, INTERNET_SETTINGS, "ProxyEnable").unwrap_or(0) != 0;
    let server = enabled
        .then(|| string(HKEY_CURRENT_USER, INTERNET_SETTINGS, "ProxyServer"))
        .flatten();
    (
        server,
        string(HKEY_CURRENT_USER, INTERNET_SETTINGS, "AutoConfigURL"),
    )
}

/// Machine policy first, then the user's.
pub(crate) fn root_auto_update_disabled() -> Option<bool> {
    dword(
        HKEY_LOCAL_MACHINE,
        AUTH_ROOT_POLICY,
        "DisableRootAutoUpdate",
    )
    .or_else(|| dword(HKEY_CURRENT_USER, AUTH_ROOT_POLICY, "DisableRootAutoUpdate"))
    .map(|v| v != 0)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn string(hkey: HKEY, subkey: &str, value: &str) -> Option<String> {
    let sub = wide(subkey);
    let name = wide(value);
    // Proxy lists and PAC URLs are short; 2 KiB avoids a sizing call.
    let mut buf = [0u16; 1024];
    let mut bytes = u32::try_from(std::mem::size_of_val(&buf)).ok()?;
    // SAFETY: `sub` and `name` are NUL-terminated, `buf` is a live buffer
    // of exactly `bytes` bytes, and RegGetValueW writes at most that much.
    let rc = unsafe {
        RegGetValueW(
            hkey,
            sub.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if rc != ERROR_SUCCESS {
        return None;
    }
    // `bytes` includes the terminating NUL.
    let len = (bytes as usize / 2).saturating_sub(1);
    let s = String::from_utf16_lossy(buf.get(..len)?);
    (!s.is_empty()).then_some(s)
}

fn dword(hkey: HKEY, subkey: &str, value: &str) -> Option<u32> {
    let sub = wide(subkey);
    let name = wide(value);
    let mut out: u32 = 0;
    let mut bytes = u32::try_from(std::mem::size_of::<u32>()).ok()?;
    // SAFETY: as above; `out` is a live u32 of `bytes` bytes.
    let rc = unsafe {
        RegGetValueW(
            hkey,
            sub.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            std::ptr::addr_of_mut!(out).cast(),
            &mut bytes,
        )
    };
    (rc == ERROR_SUCCESS).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_values_and_keys_are_none() {
        assert_eq!(
            string(
                HKEY_CURRENT_USER,
                INTERNET_SETTINGS,
                "InfinityManagerNoSuchValue"
            ),
            None
        );
        assert_eq!(
            dword(
                HKEY_CURRENT_USER,
                INTERNET_SETTINGS,
                "InfinityManagerNoSuchValue"
            ),
            None
        );
        assert_eq!(
            dword(HKEY_CURRENT_USER, r"Software\InfinityManagerNoSuchKey", "x"),
            None
        );
        // Present-value paths, values vary per machine.
        let _ = proxy();
        let _ = root_auto_update_disabled();
    }
}
