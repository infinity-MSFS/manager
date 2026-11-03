use infinity_bridge::{BridgeManager, DEFAULT_CMD_TIMEOUT};
use infinity_ipc::commands as cmd;
use tauri::State;

use super::{Args, Out};
use crate::state::AppState;

#[tauri::command]
pub(crate) async fn bridge_ensure(
    state: State<'_, AppState>,
    bridges: State<'_, BridgeManager>,
    args: Args<cmd::BridgeEnsure>,
) -> Out<cmd::BridgeEnsure> {
    let product = state.product(&args).await?;
    for spec in &product.bridges {
        bridges.ensure_started(spec.clone()).await?;
    }
    let mine: Vec<&str> = product
        .bridges
        .iter()
        .map(|b| b.namespace.as_str())
        .collect();
    Ok(bridges
        .statuses()
        .await
        .into_iter()
        .filter(|s| mine.contains(&s.namespace.as_str()))
        .collect())
}

#[tauri::command]
pub(crate) async fn bridge_status(bridges: State<'_, BridgeManager>) -> Out<cmd::BridgeStatusList> {
    Ok(bridges.statuses().await)
}

#[tauri::command]
pub(crate) async fn bridge_command(
    bridges: State<'_, BridgeManager>,
    args: Args<cmd::BridgeCommandSend>,
) -> Out<cmd::BridgeCommandSend> {
    Ok(bridges
        .command(
            &args.namespace,
            &args.command,
            args.payload,
            DEFAULT_CMD_TIMEOUT,
        )
        .await?)
}
