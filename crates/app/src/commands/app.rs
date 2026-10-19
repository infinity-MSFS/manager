use infinity_ipc::commands as cmd;
use infinity_ipc::dto;
use tauri::State;

use super::Out;
use crate::state::AppState;

#[tauri::command]
pub(crate) async fn app_info(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Out<cmd::AppInfo> {
    let info = app.package_info();
    Ok(dto::AppInfo {
        product_name: info.name.clone(),
        version: info.version.to_string(),
        edition: state.providers.edition,
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
    })
}

#[tauri::command]
pub(crate) async fn capabilities(state: State<'_, AppState>) -> Out<cmd::Capabilities> {
    Ok(state.providers.capabilities())
}
