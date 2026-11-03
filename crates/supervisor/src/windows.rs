//! The `InfinityLaunchSvc` Windows service.
//!
//! Status comes straight from the SCM. Control verbs run the service binary
//! elevated (`ShellExecuteExW` with `runas`, one UAC prompt each):
//!
//! 1. The binary is written to a per-user temp file.
//! 2. For `install`, it runs with `--self-relocate <ProgramData path>`, so
//!    the SCM points at `%ProgramData%\InfinityManager\bin`.
//! 3. `update` runs from the temp copy: it stops the service, copies itself
//!    over the installed binary and starts it again.
//!
//! Where the binary comes from, in order:
//! - `INFINITY_LAUNCH_SVC_EXE`: used in place, verbs run directly (v3's
//!   development override);
//! - the copy embedded at build time (`INFINITY_LAUNCH_SVC_PREBUILT`, set
//!   by `cargo xtask dist`);
//! - `infinity-launch-svc.exe` beside this executable, which is where a
//!   plain `cargo build --workspace` leaves it.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};

use infinity_ipc::dto::{SupervisorMechanism, SupervisorStatus};

use crate::{Error, Result, Verb};

const SERVICE_NAME: &str = "InfinityLaunchSvc";
const SVC_EXE: &str = "infinity-launch-svc.exe";
const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
const ERROR_CANCELLED: u32 = 1223;
/// The service is a console binary; without this a console flashes.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// How long an elevated verb may take before we stop waiting.
const VERB_TIMEOUT_MS: u32 = 30_000;

static EMBEDDED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/launch-svc.exe"));

fn program_data_bin() -> Result<PathBuf> {
    let pd = std::env::var_os("ProgramData")
        .ok_or(Error::Failed("%ProgramData% is not set".to_owned()))?;
    Ok(PathBuf::from(pd).join("InfinityManager").join("bin"))
}

/// Where the SCM runs the service from. Shared with v3.
fn install_path() -> Result<PathBuf> {
    Ok(program_data_bin()?.join(SVC_EXE))
}

fn dev_override() -> Option<PathBuf> {
    std::env::var_os("INFINITY_LAUNCH_SVC_EXE")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
}

fn sibling() -> Option<PathBuf> {
    let p = std::env::current_exe().ok()?.parent()?.join(SVC_EXE);
    p.is_file().then_some(p)
}

fn bundled() -> bool {
    !EMBEDDED.is_empty() || dev_override().is_some() || sibling().is_some()
}

/// A runnable copy of the service binary outside `%ProgramData%`.
fn staged() -> Result<PathBuf> {
    if EMBEDDED.is_empty() {
        return sibling().ok_or(Error::Unsupported(
            "this build does not include the launch supervisor; build it with `cargo xtask dist`",
        ));
    }
    use sha2::Digest;
    let digest = sha2::Sha256::digest(EMBEDDED);
    let short: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    let dir = std::env::temp_dir().join("InfinityManager");
    std::fs::create_dir_all(&dir).map_err(|source| Error::Io {
        what: format!("create {}", dir.display()),
        source,
    })?;
    // The hash in the name keeps different manager versions apart.
    let path = dir.join(format!("infinity-launch-svc-{short}.exe"));
    if !path.is_file() {
        let tmp = path.with_extension("exe.tmp");
        std::fs::write(&tmp, EMBEDDED)
            .and_then(|()| std::fs::rename(&tmp, &path))
            .map_err(|source| Error::Io {
                what: format!("stage {}", path.display()),
                source,
            })?;
    }
    Ok(path)
}

pub(crate) fn status(bundled_version: String) -> Result<SupervisorStatus> {
    let exe = install_path().ok();
    let (installed, running) = query_scm()?;
    let installed_version = installed
        .then(|| exe.as_deref().and_then(probe_version))
        .flatten();
    Ok(SupervisorStatus {
        supported: true,
        mechanism: SupervisorMechanism::WindowsService,
        installed,
        running,
        exe_path: exe.map(|p| p.to_string_lossy().into_owned()),
        installed_version,
        bundled_version,
        bundled: bundled(),
    })
}

fn query_scm() -> Result<(bool, bool)> {
    use windows_service::service::{ServiceAccess, ServiceState};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|e| Error::Failed(format!("service manager: {e}")))?;
    match manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS) {
        Ok(svc) => {
            let st = svc
                .query_status()
                .map_err(|e| Error::Failed(format!("service status: {e}")))?;
            Ok((true, st.current_state == ServiceState::Running))
        }
        Err(windows_service::Error::Winapi(e))
            if e.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST) =>
        {
            Ok((false, false))
        }
        Err(e) => Err(Error::Failed(format!("open service: {e}"))),
    }
}

/// `infinity-launch-svc.exe version`. No elevation needed. `None` on any
/// failure: the UI shows "unknown" rather than an error.
fn probe_version(exe: &Path) -> Option<String> {
    if !exe.is_file() {
        return None;
    }
    let out = std::process::Command::new(exe)
        .arg("version")
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !v.is_empty()).then_some(v)
}

pub(crate) fn run(verb: Verb) -> Result<()> {
    let dest = install_path()?;
    let (exe, args): (PathBuf, Vec<String>) = if let Some(dev) = dev_override() {
        (dev, vec![verb.as_str().to_owned()])
    } else {
        let staged = staged()?;
        match verb {
            // Only installing needs the binary at the stable path. The other
            // verbs address the service by name, and relocating while it runs
            // would fail on the locked file.
            Verb::Install | Verb::InstallAndStart => (
                staged,
                vec![
                    "--self-relocate".to_owned(),
                    dest.to_string_lossy().into_owned(),
                    verb.as_str().to_owned(),
                ],
            ),
            _ => (staged, vec![verb.as_str().to_owned()]),
        }
    };
    elevated::run(&exe, &args)?;

    if verb == Verb::Uninstall {
        // The SCM entry is gone; tidy the binary. Best effort.
        let _ = std::fs::remove_file(&dest);
        let _ = std::fs::remove_file(dest.with_extension("sha256"));
        if let Some(dir) = dest.parent() {
            // Fails, as intended, while the path hint is still in there.
            let _ = std::fs::remove_dir(dir);
        }
    }
    Ok(())
}

pub(crate) fn write_manager_path_hint() {
    let (Ok(dir), Ok(exe)) = (program_data_bin(), std::env::current_exe()) else {
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::debug!(error = %e, dir = %dir.display(), "manager-path hint: no folder");
        return;
    }
    let path = dir.join("manager-path.txt");
    let text = exe.to_string_lossy();
    if std::fs::read_to_string(&path).is_ok_and(|t| t.trim() == text) {
        return;
    }
    if let Err(e) = std::fs::write(&path, text.as_bytes()) {
        tracing::warn!(error = %e, path = %path.display(), "manager-path hint: write failed");
    }
}

#[allow(unsafe_code)]
mod elevated {
    use std::ffi::OsStr;
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows_sys::Win32::UI::Shell::{
        SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    use super::{ERROR_CANCELLED, VERB_TIMEOUT_MS};
    use crate::{Error, Result};

    fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(once(0)).collect()
    }

    /// Quotes arguments with spaces (the ProgramData path can have them).
    fn join(args: &[String]) -> String {
        args.iter()
            .map(|a| {
                if a.contains(' ') {
                    format!("\"{a}\"")
                } else {
                    a.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Runs `exe args` elevated and waits for it.
    pub(super) fn run(exe: &Path, args: &[String]) -> Result<()> {
        let exe_w = wide(exe.as_os_str());
        let verb_w = wide(OsStr::new("runas"));
        let args_w = wide(OsStr::new(&join(args)));

        // SAFETY: POD struct; zero is its documented empty value.
        let mut sei: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        sei.cbSize = u32::try_from(std::mem::size_of::<SHELLEXECUTEINFOW>()).unwrap_or(u32::MAX);
        sei.fMask = SEE_MASK_NOCLOSEPROCESS;
        sei.lpVerb = verb_w.as_ptr();
        sei.lpFile = exe_w.as_ptr();
        sei.lpParameters = args_w.as_ptr();
        sei.nShow = SW_HIDE;

        // SAFETY: the string pointers reference buffers that outlive the call.
        if unsafe { ShellExecuteExW(&mut sei) } == 0 {
            // SAFETY: reads the last error.
            let err = unsafe { GetLastError() };
            return Err(if err == ERROR_CANCELLED {
                Error::Declined
            } else {
                Error::Failed(format!("could not start the elevated helper (error {err})"))
            });
        }
        if sei.hProcess.is_null() {
            return Ok(());
        }
        let mut code: u32 = 0;
        // SAFETY: hProcess is ours (SEE_MASK_NOCLOSEPROCESS) and closed here.
        let waited = unsafe {
            let waited = WaitForSingleObject(sei.hProcess, VERB_TIMEOUT_MS);
            GetExitCodeProcess(sei.hProcess, &mut code);
            CloseHandle(sei.hProcess);
            waited
        };
        if waited != WAIT_OBJECT_0 {
            return Err(Error::Failed(
                "the launch supervisor did not finish in time".to_owned(),
            ));
        }
        if code == 0 {
            Ok(())
        } else {
            Err(Error::Failed(format!(
                "the launch supervisor exited with code {code}"
            )))
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn arguments_with_spaces_are_quoted() {
            let args = [
                "--self-relocate".to_owned(),
                r"C:\Program Data\x.exe".to_owned(),
                "install".to_owned(),
            ];
            assert_eq!(
                super::join(&args),
                r#"--self-relocate "C:\Program Data\x.exe" install"#
            );
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn install_path_matches_v3() {
        let p = super::install_path().unwrap();
        assert!(p.ends_with(r"InfinityManager\bin\infinity-launch-svc.exe"));
    }
}
