use std::path::PathBuf;

use infinity_bridge::{BridgeManager, DEFAULT_CMD_TIMEOUT};
use infinity_configurator::{BridgeSink, ConfigStore, EmitContext};
use infinity_ipc::catalog::{BridgeSpec, OutputSpec, Product};
use infinity_ipc::commands as cmd;
use infinity_ipc::{IpcError, IpcResult};
use infinity_persistence::PreferencesStore;
use tauri::{Manager, State};
use tauri_plugin_dialog::DialogExt;

use super::{Args, Out};
use crate::paths;
use crate::state::AppState;

/// The schema version the catalog's migration ladder ends at; a product
/// without migrations is at version 1.
fn target_version(product: &Product) -> u32 {
    product
        .configurator
        .as_ref()
        .into_iter()
        .flat_map(|c| &c.migrations)
        .filter_map(|m| m.get("to")?.as_u64())
        .filter_map(|v| u32::try_from(v).ok())
        .max()
        .unwrap_or(1)
}

#[tauri::command]
pub(crate) async fn configurator_load(
    state: State<'_, AppState>,
    store: State<'_, ConfigStore>,
    args: Args<cmd::ConfiguratorLoad>,
) -> Out<cmd::ConfiguratorLoad> {
    let product = state.product(&args).await?;
    Ok(infinity_configurator::load_config(&store, &product, target_version(&product)).await?)
}

#[tauri::command]
pub(crate) async fn configurator_save(
    state: State<'_, AppState>,
    store: State<'_, ConfigStore>,
    args: Args<cmd::ConfiguratorSave>,
) -> Out<cmd::ConfiguratorSave> {
    let product = state.product(&args.product).await?;
    Ok(infinity_configurator::save_config(&store, &product, &args.envelope).await?)
}

#[tauri::command]
pub(crate) async fn configurator_emit(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    store: State<'_, ConfigStore>,
    prefs: State<'_, PreferencesStore>,
    bridges: State<'_, BridgeManager>,
    args: Args<cmd::ConfiguratorEmit>,
) -> Out<cmd::ConfiguratorEmit> {
    let product = state.product(&args.product).await?;
    // Resolved up front: finding the work folder is async, the crate's hook
    // is not. Only file outputs need it.
    let work_dir = if writes_file(&product, &args.output_id) {
        Some(resolve_work_dir(&app, &prefs, &product).await)
    } else {
        None
    };
    let sink = Bridges(bridges.inner().clone());
    Ok(infinity_configurator::emit_output(
        &store,
        &product,
        &args.output_id,
        &EmitContext::now(),
        move || work_dir.unwrap_or_else(|| Err(IpcError::internal("no work folder resolved"))),
        &sink,
    )
    .await?)
}

#[tauri::command]
pub(crate) async fn configurator_export(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    store: State<'_, ConfigStore>,
    prefs: State<'_, PreferencesStore>,
    args: Args<cmd::ConfiguratorExport>,
) -> Out<cmd::ConfiguratorExport> {
    let product = state.product(&args).await?;
    let work_dir = resolve_work_dir(&app, &prefs, &product).await?;
    let path = infinity_configurator::export_to_sim(&store, &product, &work_dir).await?;
    Ok(path.to_string_lossy().into_owned())
}

#[tauri::command]
pub(crate) async fn configurator_share_save(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    store: State<'_, ConfigStore>,
    args: Args<cmd::ConfiguratorShareSave>,
) -> Out<cmd::ConfiguratorShareSave> {
    let product = state.product(&args).await?;
    let name = format!("{}.infinity.json", product.id);
    let dest = paths::blocking(move || {
        app.dialog()
            .file()
            .set_title("Save configuration")
            .set_file_name(name)
            .add_filter("Configuration", &["json", "infinity"])
            .blocking_save_file()
    })
    .await?;
    let Some(dest) = dest.and_then(|p| p.into_path().ok()) else {
        return Ok(None);
    };
    infinity_configurator::share_save(&store, &product, &dest).await?;
    Ok(Some(dest.to_string_lossy().into_owned()))
}

#[tauri::command]
pub(crate) async fn configurator_share_load(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    store: State<'_, ConfigStore>,
    args: Args<cmd::ConfiguratorShareLoad>,
) -> Out<cmd::ConfiguratorShareLoad> {
    let product = state.product(&args).await?;
    let src = paths::blocking(move || {
        app.dialog()
            .file()
            .set_title("Load configuration")
            .add_filter("Configuration", &["json", "infinity"])
            .blocking_pick_file()
    })
    .await?;
    let Some(src) = src.and_then(|p| p.into_path().ok()) else {
        return Ok(None);
    };
    Ok(Some(
        infinity_configurator::share_load(&store, &product, &src).await?,
    ))
}

#[tauri::command]
pub(crate) async fn configurator_reset(
    state: State<'_, AppState>,
    store: State<'_, ConfigStore>,
    args: Args<cmd::ConfiguratorReset>,
) -> Out<cmd::ConfiguratorReset> {
    let product = state.product(&args).await?;
    Ok(infinity_configurator::reset_config(&store, &product).await?)
}

fn writes_file(product: &Product, output_id: &str) -> bool {
    product
        .configurator
        .as_ref()
        .and_then(|c| c.outputs.iter().find(|o| o.id() == output_id))
        .is_some_and(|o| matches!(o, OutputSpec::File { .. }))
}

async fn resolve_work_dir(
    app: &tauri::AppHandle,
    prefs: &PreferencesStore,
    product: &Product,
) -> IpcResult<PathBuf> {
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|e| IpcError::internal(format!("app data dir: {e}")))?;
    paths::work_dir(&prefs.load().await?, &app_data, product).await
}

/// Bridge outputs go to the product's bridge, started on demand.
struct Bridges(BridgeManager);

impl BridgeSink for Bridges {
    async fn command(
        &self,
        bridge: &BridgeSpec,
        command: &str,
        payload: serde_json::Value,
    ) -> IpcResult<serde_json::Value> {
        self.0.ensure_started(bridge.clone()).await?;
        Ok(self
            .0
            .command(&bridge.namespace, command, payload, DEFAULT_CMD_TIMEOUT)
            .await?)
    }
}
