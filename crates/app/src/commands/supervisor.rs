//! The launch supervisor; see `infinity_supervisor`.

use infinity_ipc::commands as cmd;
use infinity_supervisor::Verb;

use super::Out;
use crate::paths::blocking;

async fn run(verb: Verb) -> infinity_ipc::IpcResult<()> {
    // Waits on the elevation prompt or systemctl.
    Ok(blocking(move || infinity_supervisor::run(verb)).await??)
}

#[tauri::command]
pub(crate) async fn svc_status() -> Out<cmd::SvcStatus> {
    Ok(blocking(infinity_supervisor::status).await??)
}

#[tauri::command]
pub(crate) async fn svc_install() -> Out<cmd::SvcInstall> {
    run(Verb::Install).await
}

#[tauri::command]
pub(crate) async fn svc_install_and_start() -> Out<cmd::SvcInstallAndStart> {
    run(Verb::InstallAndStart).await
}

#[tauri::command]
pub(crate) async fn svc_start() -> Out<cmd::SvcStart> {
    run(Verb::Start).await
}

#[tauri::command]
pub(crate) async fn svc_stop() -> Out<cmd::SvcStop> {
    run(Verb::Stop).await
}

#[tauri::command]
pub(crate) async fn svc_uninstall() -> Out<cmd::SvcUninstall> {
    run(Verb::Uninstall).await
}

#[tauri::command]
pub(crate) async fn svc_update() -> Out<cmd::SvcUpdate> {
    run(Verb::Update).await
}
