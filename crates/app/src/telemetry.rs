//! Tracing init: stdout (dev) plus a daily log file the manager prunes itself.
//!
//! A release build is a windowed app with no console, so without the file
//! every record is lost — including the FLIR engine's and the viewports
//! bridge's (`flir::viewports`), which are what a tester sends back after a
//! GPU crash. The file lands where Tauri's `app_log_dir` would put it,
//! `%LOCALAPPDATA%\<identifier>\logs`, computed by hand because logging is up
//! before the Tauri builder runs.
//!
//! Writes are synchronous (no `non_blocking` worker): the volume is small, and
//! a record written is on disk even if the process dies right after it.

use std::path::PathBuf;
use std::sync::Once;

use tracing_appender::rolling::{Builder as RollingBuilder, Rotation};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// `identifier` in `apps/desktop/tauri.conf.json`.
const APP_IDENTIFIER: &str = "com.infinity.manager.community";
const LOG_PREFIX: &str = "infinity-manager";
/// Daily files kept; older ones are deleted by the appender at rotation.
const MAX_LOG_FILES: usize = 7;
const DEFAULT_FILTER: &str = "info,infinity_app=debug,infinity_auth=debug,flir::viewports=debug";

static INIT: Once = Once::new();

fn filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER))
}

pub(crate) fn log_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base =
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"));
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let base = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share"));
    base.map(|b| b.join(APP_IDENTIFIER).join("logs"))
}

/// Idempotent: `init_logging` and `Builder::run` both call it.
pub(crate) fn init() {
    INIT.call_once(|| {
        let appender = log_dir()
            .ok_or_else(|| "no local app data dir".to_string())
            .and_then(|dir| {
                std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                RollingBuilder::new()
                    .rotation(Rotation::DAILY)
                    .filename_prefix(LOG_PREFIX)
                    .filename_suffix("log")
                    .max_log_files(MAX_LOG_FILES)
                    .build(&dir)
                    .map_err(|e| format!("{}: {e}", dir.display()))
            });
        match appender {
            Ok(appender) => {
                let _ = tracing_subscriber::registry()
                    .with(filter())
                    .with(fmt::layer())
                    .with(fmt::layer().with_writer(appender).with_ansi(false))
                    .try_init();
                tracing::info!(version = env!("CARGO_PKG_VERSION"), "manager log started");
            }
            Err(e) => {
                let _ = tracing_subscriber::fmt().with_env_filter(filter()).try_init();
                tracing::warn!(error = %e, "log file unavailable; logging to stdout only");
            }
        }
    });
}
