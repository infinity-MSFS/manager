//! Service control manager operations, run elevated from the manager.
//! Output goes to stdout/stderr; the manager reads the exit code.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use windows_service::service::{
    Service, ServiceAccess, ServiceAction, ServiceActionType, ServiceErrorControl,
    ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState,
    ServiceType,
};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

use crate::svc::SERVICE_NAME;

const DISPLAY_NAME: &str = "Infinity Launch Supervisor";
const DESCRIPTION: &str =
    "Starts the Infinity Manager in the active user session when an in-sim aircraft requests it.";

const ERROR_SERVICE_EXISTS: i32 = 1073;
const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;

fn win_error(e: &windows_service::Error) -> Option<i32> {
    match e {
        windows_service::Error::Winapi(io) => io.raw_os_error(),
        _ => None,
    }
}

pub(crate) fn install() -> crate::Result {
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )?;
    let info = ServiceInfo {
        name: SERVICE_NAME.into(),
        display_name: DISPLAY_NAME.into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: std::env::current_exe()?,
        launch_arguments: vec![],
        // LocalSystem: WTSQueryUserToken needs SeTcbPrivilege.
        account_name: None,
        account_password: None,
        dependencies: vec![],
    };
    let service = match manager.create_service(&info, ServiceAccess::CHANGE_CONFIG) {
        Ok(s) => s,
        Err(e) if win_error(&e) == Some(ERROR_SERVICE_EXISTS) => {
            println!("service already installed");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    service.set_description(DESCRIPTION)?;

    // Restart after a crash: 1 s, 5 s, then every 30 s, forgiven after a
    // day. Without this a crash leaves the service stopped until reboot.
    let restart = |secs| ServiceAction {
        action_type: ServiceActionType::Restart,
        delay: Duration::from_secs(secs),
    };
    let actions = ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(60 * 60 * 24)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![restart(1), restart(5), restart(30)]),
    };
    if let Err(e) = service.update_failure_actions(actions) {
        eprintln!("warn: could not set failure actions: {e}");
    }
    if let Err(e) = service.set_failure_actions_on_non_crash_failures(true) {
        eprintln!("warn: could not enable non-crash failure actions: {e}");
    }
    println!("installed service '{SERVICE_NAME}'");
    Ok(())
}

/// Install and start under one elevation prompt. The service is set to
/// start at boot, so a failed start here is only a warning.
pub(crate) fn install_and_start() -> crate::Result {
    install()?;
    if let Err(e) = start() {
        eprintln!("warn: installed but not started: {e}");
    }
    Ok(())
}

pub(crate) fn uninstall() -> crate::Result {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let service = match manager.open_service(
        SERVICE_NAME,
        ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
    ) {
        Ok(s) => s,
        Err(e) if win_error(&e) == Some(ERROR_SERVICE_DOES_NOT_EXIST) => {
            println!("service not installed");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    if service.query_status()?.current_state != ServiceState::Stopped {
        let _ = service.stop();
        wait_for(&service, ServiceState::Stopped);
    }
    service.delete()?;
    println!("uninstalled service '{SERVICE_NAME}'");
    Ok(())
}

pub(crate) fn start() -> crate::Result {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let service = manager.open_service(
        SERVICE_NAME,
        ServiceAccess::START | ServiceAccess::QUERY_STATUS,
    )?;
    if service.query_status()?.current_state == ServiceState::Running {
        println!("already running");
        return Ok(());
    }
    service.start(&[] as &[&str])?;
    wait_for(&service, ServiceState::Running);
    println!("started");
    Ok(())
}

pub(crate) fn stop() -> crate::Result {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let service = manager.open_service(
        SERVICE_NAME,
        ServiceAccess::STOP | ServiceAccess::QUERY_STATUS,
    )?;
    if service.query_status()?.current_state == ServiceState::Stopped {
        println!("already stopped");
        return Ok(());
    }
    service.stop()?;
    wait_for(&service, ServiceState::Stopped);
    println!("stopped");
    Ok(())
}

pub(crate) fn print_status_json() -> crate::Result {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let (installed, running) = match manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS)
    {
        Ok(s) => (
            true,
            s.query_status()?.current_state == ServiceState::Running,
        ),
        Err(e) if win_error(&e) == Some(ERROR_SERVICE_DOES_NOT_EXIST) => (false, false),
        Err(e) => return Err(e.into()),
    };
    println!(r#"{{"installed":{installed},"running":{running}}}"#);
    Ok(())
}

/// Replaces the installed binary with this one: stop (frees the file
/// lock), copy, start. Run from a staged copy, never through
/// `--self-relocate`, because the stop has to come before the copy.
pub(crate) fn update() -> crate::Result {
    let dest = install_dest()?;
    if let Err(e) = stop() {
        // The copy fails loudly if the file really is still locked.
        eprintln!("warn: stop before update failed, continuing: {e}");
    }
    let current = std::env::current_exe()?;
    let same =
        std::fs::canonicalize(&current).ok() == std::fs::canonicalize(&dest).ok() && dest.exists();
    if same {
        println!("already running from the install path; nothing to copy");
    } else {
        replace(&current, &dest)?;
        println!("updated {}", dest.display());
    }
    start()
}

/// Where the manager stages the service, and the path the SCM runs.
/// Shared with v3 and the manager.
fn install_dest() -> crate::Result<PathBuf> {
    let pd = std::env::var_os("ProgramData").ok_or("%ProgramData% is not set")?;
    Ok(PathBuf::from(pd)
        .join("InfinityManager")
        .join("bin")
        .join("infinity-launch-svc.exe"))
}

/// Copies through a `.tmp` so a half-written binary is never in place.
fn replace(from: &Path, to: &Path) -> crate::Result {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = to.with_extension("exe.tmp");
    std::fs::copy(from, &tmp)?;
    if to.exists() {
        std::fs::remove_file(to)?;
    }
    std::fs::rename(&tmp, to)?;
    Ok(())
}

/// Copies this exe to `dest` and runs `sub` from there, so the SCM points
/// at the stable path rather than a temp file.
pub(crate) fn self_relocate_and_exec(dest: &str, sub: &str) -> crate::Result {
    use std::os::windows::process::CommandExt;
    // The service is a console binary; without this a console flashes.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let dest = PathBuf::from(dest);
    let current = std::env::current_exe()?;
    if std::fs::canonicalize(&current).ok() != std::fs::canonicalize(&dest).ok() {
        replace(&current, &dest)?;
    }
    let status = std::process::Command::new(&dest)
        .arg(sub)
        .creation_flags(CREATE_NO_WINDOW)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{sub} failed: {status}").into())
    }
}

fn wait_for(service: &Service, target: ServiceState) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        match service.query_status() {
            Ok(s) if s.current_state == target => return,
            _ => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}
