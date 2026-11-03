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
