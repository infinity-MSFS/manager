//! The launch supervisor: whatever starts the manager when an in-sim
//! aircraft needs it and it is not running.
//!
//! Gauge relays that cannot reach the manager connect to the sentinel port
//! ([`SENTINEL_ADDR`]). The connection is the whole signal; the supervisor
//! answers it by launching the manager with [`REMOTE_START_FLAG`], which
//! starts it without showing the window.
//!
//! - **Windows**: the `infinity-launch-svc` service (its own crate). This
//!   crate embeds or finds that binary, runs it elevated for each control
//!   verb, and reads status from the service control manager.
//! - **Linux**: a pair of systemd *user* units, a socket on the sentinel
//!   port that activates the manager. No second binary, no elevation.
//! - Elsewhere: [`status`] reports `supported: false`.
//!
//! The service name, port, install path, path-hint file and unit names are
//! shared with v3: either manager can control what the other installed.
//!
//! Every function here blocks (subprocesses, the SCM); call them off the
//! async runtime.

use infinity_ipc::dto::SupervisorStatus;
use infinity_ipc::{IpcError, IpcErrorCode};

#[cfg(target_os = "linux")]
mod systemd;
#[cfg(windows)]
mod windows;

/// Gauge relays dial this to wake the manager.
pub const SENTINEL_ADDR: &str = "127.0.0.1:9899";

/// Command-line flag the supervisor launches the manager with: start
/// hidden, and do not reveal an already running instance.
pub const REMOTE_START_FLAG: &str = "--remote-start";

/// Version of the supervisor this build installs, from
/// `crates/launch-svc/Cargo.toml`.
pub const BUNDLED_VERSION: &str = include_str!(concat!(env!("OUT_DIR"), "/launch-svc.version"));

/// Whether this process was started by the supervisor.
pub fn is_remote_start<S: AsRef<str>>(args: impl IntoIterator<Item = S>) -> bool {
    args.into_iter().any(|a| a.as_ref() == REMOTE_START_FLAG)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Install,
    /// Install and start under one elevation prompt.
    InstallAndStart,
    Start,
    Stop,
    Uninstall,
    /// Replace the installed supervisor with this build's.
    Update,
}

impl Verb {
    /// The `infinity-launch-svc` subcommand. Shared with v3.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::InstallAndStart => "install_and_start",
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Uninstall => "uninstall",
            Self::Update => "update",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Unsupported(&'static str),
    #[error("the administrator prompt was declined")]
    Declined,
    #[error("{0}")]
    Failed(String),
    #[error("{what}: {source}")]
    Io {
        what: String,
        source: std::io::Error,
    },
}

impl From<Error> for IpcError {
    fn from(e: Error) -> Self {
        let code = match e {
            Error::Unsupported(_) => IpcErrorCode::Unsupported,
            Error::Declined => IpcErrorCode::AuthForbidden,
            Error::Failed(_) => IpcErrorCode::Internal,
            Error::Io { .. } => IpcErrorCode::IoError,
        };
        Self::new(code, e.to_string())
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Installed and running state, probed fresh.
pub fn status() -> Result<SupervisorStatus> {
    let bundled_version = BUNDLED_VERSION.trim().to_owned();
    #[cfg(windows)]
    return windows::status(bundled_version);
    #[cfg(target_os = "linux")]
    return systemd::status(bundled_version);
    #[cfg(not(any(windows, target_os = "linux")))]
    Ok(SupervisorStatus {
        supported: false,
        mechanism: infinity_ipc::dto::SupervisorMechanism::None,
        installed: false,
        running: false,
        exe_path: None,
        installed_version: None,
        bundled_version,
        bundled: false,
    })
}

/// Runs a control verb. On Windows this shows the elevation prompt and
/// waits for the elevated helper to finish.
pub fn run(verb: Verb) -> Result<()> {
    tracing::info!(verb = verb.as_str(), "launch supervisor");
    #[cfg(windows)]
    return windows::run(verb);
    #[cfg(target_os = "linux")]
    return systemd::run(verb);
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = verb;
        Err(Error::Unsupported(
            "this platform has no launch supervisor; start the manager before you fly",
        ))
    }
}

/// Records this executable's path where the Windows service looks for the
/// manager (`%ProgramData%\InfinityManager\bin\manager-path.txt`). The
/// manager installs per user, so the service cannot guess where. Best
/// effort; a no-op elsewhere.
pub fn write_manager_path_hint() {
    #[cfg(windows)]
    windows::write_manager_path_hint();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_version_comes_from_the_service_manifest() {
        let v = BUNDLED_VERSION.trim();
        assert!(!v.is_empty() && v != "0.0.0", "{v:?}");
        assert_eq!(v.split('.').count(), 3, "{v:?}");
    }

    #[test]
    fn remote_start_flag_is_recognised() {
        assert!(is_remote_start(["infinity-manager", "--remote-start"]));
        assert!(!is_remote_start(["infinity-manager"]));
    }

    #[test]
    fn verbs_match_the_service_cli() {
        let all = [
            Verb::Install,
            Verb::InstallAndStart,
            Verb::Start,
            Verb::Stop,
            Verb::Uninstall,
            Verb::Update,
        ];
        let names: Vec<_> = all.iter().map(|v| v.as_str()).collect();
        assert_eq!(
            names,
            [
                "install",
                "install_and_start",
                "start",
                "stop",
                "uninstall",
                "update"
            ]
        );
    }

    #[test]
    fn declining_elevation_is_forbidden_not_internal() {
        assert_eq!(
            IpcError::from(Error::Declined).code,
            IpcErrorCode::AuthForbidden
        );
    }

    #[test]
    fn status_is_callable() {
        // Values depend on the machine; only that probing does not fail.
        let s = status().unwrap();
        assert_eq!(s.bundled_version, BUNDLED_VERSION.trim());
    }
}
