//! Startup work ported from v3's `main`: support diagnostics, the
//! supervisor path hint, starting hidden after a supervisor launch, the
//! link server for in-sim panels, the one-time v1 import, and listing
//! downloads the last run left unfinished.

use std::sync::{Arc, OnceLock};

use tauri::Manager;
use tauri_plugin_opener::OpenerExt;

use crate::commands::migrate;

static PROBE_URL: OnceLock<String> = OnceLock::new();

pub(crate) fn set_probe_url(url: String) {
    let _ = PROBE_URL.set(url);
}

pub(crate) fn setup(app: &tauri::AppHandle) {
    // Support context first, off the setup thread: reading the root store
    // and the registry is cheap, but the probe waits on the network.
    tauri::async_runtime::spawn(async {
        if let Err(e) =
            tauri::async_runtime::spawn_blocking(infinity_netdiag::log_environment).await
        {
            tracing::debug!(error = %e, "netdiag environment");
        }
        if let Some(url) = PROBE_URL.get() {
            infinity_netdiag::probe(url).await;
        }
    });

    // Where the Windows service finds this manager.
    infinity_supervisor::write_manager_path_hint();

    if infinity_supervisor::is_remote_start(std::env::args()) {
        // Started by the supervisor for an aircraft: stay out of the way
        // until the user opens the manager themselves.
        tracing::info!("started by the launch supervisor; window hidden");
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.hide();
        }
    }

    serve_links(app);

    // Downloads paused or cut off when the app last quit.
    crate::commands::install::restore_interrupted(app);

    app.manage(migrate::V1Import::default());
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        match migrate::run_once(&handle).await {
            Ok(Some(report)) => tracing::info!(?report, "v1 settings import"),
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "v1 settings import failed"),
        }
    });
}

/// Lets in-sim panels open http(s) links in the browser.
fn serve_links(app: &tauri::AppHandle) {
    let handle = app.clone();
    let open: infinity_link_server::Opener = Arc::new(move |url: String| {
        if let Err(e) = handle.opener().open_url(&url, None::<&str>) {
            tracing::warn!(error = %e, %url, "could not open a link for an in-sim panel");
        }
    });
    tauri::async_runtime::spawn(async move {
        if let Err(e) = infinity_link_server::run(open).await {
            tracing::warn!(error = %e, port = infinity_link_server::PORT, "link server did not start");
        }
    });
}
