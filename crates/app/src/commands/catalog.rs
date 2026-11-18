use std::sync::Arc;

use infinity_ipc::commands as cmd;
use tauri::State;

use super::{Args, Out};
use crate::state::AppState;

#[tauri::command]
pub(crate) async fn catalog_fetch(
    state: State<'_, AppState>,
    args: Args<cmd::CatalogFetch>,
) -> Out<cmd::CatalogFetch> {
    state.fetch_catalog(args).await.map(Arc::unwrap_or_clone)
}

#[tauri::command]
pub(crate) async fn catalog_changelog(
    state: State<'_, AppState>,
    args: Args<cmd::CatalogChangelog>,
) -> Out<cmd::CatalogChangelog> {
    state.providers.content.changelog(&args).await
}

#[tauri::command]
pub(crate) async fn catalog_liveries(
    state: State<'_, AppState>,
    args: Args<cmd::CatalogLiveries>,
) -> Out<cmd::CatalogLiveries> {
    state.providers.content.liveries(&args).await
}

#[tauri::command]
pub(crate) async fn account_entitlements(
    state: State<'_, AppState>,
) -> Out<cmd::AccountEntitlements> {
    state.providers.content.entitlements().await
}

/// Refresh purchases. Inside the throttle a repeat press answers from the
/// current token rather than spending another renewal.
#[tauri::command]
pub(crate) async fn account_refresh(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Out<cmd::AccountRefresh> {
    match crate::recheck::if_due(&app, crate::recheck::ON_DEMAND).await {
        Some(result) => result,
        None => state.providers.content.entitlements().await,
    }
}
