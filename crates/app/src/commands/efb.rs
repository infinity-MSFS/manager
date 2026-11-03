use infinity_efb_server::DEFAULT_TOKEN_TTL_SECS;
use infinity_ipc::IpcError;
use infinity_ipc::commands as cmd;
use tauri::State;

use super::{Args, Out};
use crate::efb::{self, Efb};

#[tauri::command]
pub(crate) async fn efb_status(efb: State<'_, Efb>) -> Out<cmd::EfbStatusGet> {
    Ok(efb.server.status().await)
}

#[tauri::command]
pub(crate) async fn efb_lan_urls(efb: State<'_, Efb>) -> Out<cmd::EfbLanUrls> {
    let port = efb
        .server
        .bind_addr()
        .await
        .map_or(infinity_efb_server::DEFAULT_PORT, |a| a.port());
    crate::paths::blocking(move || infinity_efb_server::lan::lan_urls(port)).await
}

#[tauri::command]
pub(crate) async fn efb_pairing_code(
    efb: State<'_, Efb>,
    args: Args<cmd::EfbPairingCode>,
) -> Out<cmd::EfbPairingCode> {
    let device = args.trim();
    if device.is_empty() {
        return Err(IpcError::new(
            infinity_ipc::IpcErrorCode::ValidationFailed,
            "name the tablet first",
        ));
    }
    Ok(efb
        .server
        .mint_pairing_code(device, DEFAULT_TOKEN_TTL_SECS)
        .await?)
}

#[tauri::command]
pub(crate) async fn efb_disconnect(
    efb: State<'_, Efb>,
    args: Args<cmd::EfbDisconnect>,
) -> Out<cmd::EfbDisconnect> {
    Ok(efb.server.disconnect_client(args).await)
}

#[tauri::command]
pub(crate) async fn efb_open_docs_folder(app: tauri::AppHandle) -> Out<cmd::EfbOpenDocsFolder> {
    use tauri::Manager;
    use tauri_plugin_opener::OpenerExt;
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|e| IpcError::internal(format!("app data dir: {e}")))?;
    let dir = efb::user_docs_dir(&app_data);
    tokio::fs::create_dir_all(&dir).await.map_err(|e| {
        IpcError::new(
            infinity_ipc::IpcErrorCode::IoError,
            format!("{}: {e}", dir.display()),
        )
    })?;
    app.opener()
        .open_path(dir.to_string_lossy(), None::<&str>)
        .map_err(|e| IpcError::internal(format!("open folder: {e}")))
}

#[tauri::command]
pub(crate) async fn efb_forget_devices(app: tauri::AppHandle) -> Out<cmd::EfbForgetDevices> {
    efb::forget_devices(&app).await
}
