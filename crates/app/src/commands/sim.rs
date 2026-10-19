use infinity_ipc::commands as cmd;
use infinity_sim_watch::SimWatch;
use tauri::State;

use super::Out;

#[tauri::command]
pub(crate) async fn sim_status(watch: State<'_, SimWatch>) -> Out<cmd::SimStatus> {
    Ok(watch.status())
}
