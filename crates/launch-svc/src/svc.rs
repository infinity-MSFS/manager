//! Service hosting: the SCM handshake, the sentinel listener and the spawn.

mod spawn;

use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tracing::{info, warn};
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_dispatcher;

/// The SCM name. Shared with v3.
pub(crate) const SERVICE_NAME: &str = "InfinityLaunchSvc";
/// Gauge relays dial this when the manager is not up. Shared with v3, the
/// manager's systemd socket unit and the relays themselves.
pub(crate) const SENTINEL_BIND_ADDR: &str = "127.0.0.1:9899";
const MANAGER_EXE_NAME: &str = "infinity-manager.exe";

#[allow(unsafe_code)]
mod ffi {
    windows_service::define_windows_service!(ffi_service_main, service_main);

    fn service_main(_args: Vec<std::ffi::OsString>) {
        super::service_main();
    }

    pub(super) fn entry() -> extern "system" fn(u32, *mut *mut u16) {
        ffi_service_main
    }
}

pub(crate) fn run() -> crate::Result {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("INFINITY_SVC_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    service_dispatcher::start(SERVICE_NAME, ffi::entry())?;
    Ok(())
}

fn service_main() {
    if let Err(e) = run_service() {
        warn!(error = ?e, "service exited with an error");
    }
}

fn status(state: ServiceState, exit_code: ServiceExitCode) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: if state == ServiceState::Running {
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
        } else {
            ServiceControlAccept::empty()
        },
        exit_code,
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    }
}

fn run_service() -> windows_service::Result<()> {
    let shutdown = Arc::new(AtomicBool::new(false));
    let on_control = shutdown.clone();
    let handler = move |event| match event {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            on_control.store(true, Ordering::SeqCst);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let handle = service_control_handler::register(SERVICE_NAME, handler)?;
    handle.set_service_status(status(ServiceState::Running, ServiceExitCode::Win32(0)))?;

    info!(addr = SENTINEL_BIND_ADDR, "sentinel listener starting");
    let listener = match TcpListener::bind(SENTINEL_BIND_ADDR) {
        Ok(l) => l,
        Err(e) => {
            warn!(error = %e, "could not bind the sentinel port; stopping");
            let _ = handle.set_service_status(status(
                ServiceState::Stopped,
                ServiceExitCode::ServiceSpecific(1),
            ));
            return Ok(());
        }
    };
    // Non-blocking so the loop can watch the shutdown flag.
    let _ = listener.set_nonblocking(true);

    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, peer)) => {
                // The connection is the whole message.
                drop(stream);
                info!(?peer, "sentinel probe, making sure the manager is up");
                if let Err(e) = spawn::ensure_manager_running(MANAGER_EXE_NAME) {
                    warn!(error = %e, "could not start the manager");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(e) => {
                warn!(error = %e, "sentinel accept failed");
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }

    handle.set_service_status(status(ServiceState::Stopped, ServiceExitCode::Win32(0)))?;
    Ok(())
}
