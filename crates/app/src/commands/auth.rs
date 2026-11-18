use infinity_ipc::commands as cmd;
use infinity_ipc::dto::{LoginStarted, SessionState};
use tauri::State;
use tauri_plugin_opener::OpenerExt;

use super::Out;
use crate::presets::session_changed;
use crate::state::AppState;

#[tauri::command]
pub(crate) async fn auth_session(state: State<'_, AppState>) -> Out<cmd::AuthSession> {
    Ok(state.providers.identity.session().await)
}

#[tauri::command]
pub(crate) async fn auth_begin_login(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Out<cmd::AuthBeginLogin> {
    let url = state.providers.identity.begin_login().await?;
    if let Err(e) = app.opener().open_url(url.as_str(), None::<&str>) {
        tracing::warn!(error = %e, "could not open the system browser");
    }
    session_changed(&app, &SessionState::Pending);
    Ok(LoginStarted {
        authorize_url: url.into(),
    })
}

#[tauri::command]
pub(crate) async fn auth_logout(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Out<cmd::AuthLogout> {
    state.providers.identity.logout().await?;
    session_changed(&app, &SessionState::SignedOut);
    Ok(())
}
