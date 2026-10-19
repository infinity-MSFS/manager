use infinity_ipc::dto::SessionState;
use infinity_ipc::events::SessionChanged;
use tauri::Manager;
use tauri_plugin_deep_link::DeepLinkExt;

use crate::events::emit;
use crate::state::AppState;

pub(crate) fn install(app: &tauri::AppHandle) -> tauri::Result<()> {
    #[cfg(all(debug_assertions, any(windows, target_os = "linux")))]
    if let Err(e) = app.deep_link().register_all() {
        tracing::warn!(error = %e, "could not register deep-link scheme");
    }

    let handle = app.clone();
    app.deep_link().on_open_url(move |event| {
        for url in event.urls() {
            dispatch(&handle, url);
        }
    });
    Ok(())
}

fn dispatch(app: &tauri::AppHandle, url: url::Url) {
    let identity = app.state::<AppState>().providers.identity.clone();
    if !identity.is_callback(&url) {
        tracing::debug!(scheme = url.scheme(), "ignoring unrecognised deep link");
        return;
    }
    focus_main(app);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = match identity.complete_login(&url).await {
            Ok(state) => state,
            Err(e) => {
                tracing::warn!(code = %e.code, "sign-in failed");
                SessionState::SignedOut
            }
        };
        emit::<SessionChanged>(&app, &state);
    });
}

pub(crate) fn focus_main(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
