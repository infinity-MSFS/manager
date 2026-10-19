use infinity_ipc::commands as cmd;
use infinity_ipc::{IpcError, IpcErrorCode};
use tauri::State;

use super::{Args, Out};
use crate::state::AppState;

#[tauri::command]
pub(crate) async fn catalog_fetch(
    state: State<'_, AppState>,
    args: Args<cmd::CatalogFetch>,
) -> Out<cmd::CatalogFetch> {
    let channel = args;
    if !state.providers.channels.contains(&channel) {
        return Err(IpcError::new(
            IpcErrorCode::AuthForbidden,
            format!(
                "channel {} is not available in this build",
                channel.as_str()
            ),
        ));
    }
    state.providers.content.catalog(channel).await
}
