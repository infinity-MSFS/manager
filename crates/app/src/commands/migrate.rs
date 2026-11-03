//! The one-time import of v1's settings.
//!
//! v1 managed one aircraft and kept its configuration in a single global
//! `config.json`. It becomes that product's saved configuration, at the
//! schema version v1's file corresponds to; loading it migrates it on to
//! the catalog's current version. Same target as v3.

use infinity_configurator::ConfigStore;
use infinity_ipc::commands as cmd;
use infinity_ipc::dto::V1Migration;
use infinity_ipc::{IpcError, IpcResult};
use tauri::Manager;

use super::Out;

/// The product v1's settings belong to.
const V1_PRODUCT_ID: &str = "aerodynamics-t38c";
/// The configurator schema version v1's file is.
const V1_SCHEMA_VERSION: u32 = 1;

/// Runs the import at most once per process. Startup and the first-run
/// wizard both ask; whichever is first does the work.
#[derive(Debug, Default)]
pub(crate) struct V1Import(tokio::sync::OnceCell<Option<V1Migration>>);

pub(crate) async fn run_once(app: &tauri::AppHandle) -> IpcResult<Option<V1Migration>> {
    let state = app.state::<V1Import>();
    state.0.get_or_try_init(|| import(app)).await.cloned()
}

async fn import(app: &tauri::AppHandle) -> IpcResult<Option<V1Migration>> {
    // v1 kept its data under the roaming app-data root, by identifier.
    let root = app
        .path()
        .data_dir()
        .map_err(|e| IpcError::internal(format!("app data folder: {e}")))?;
    let Some(v1) = infinity_configurator::find_v1_app_data(&root).await else {
        return Ok(None);
    };
    let store = app.state::<ConfigStore>();
    // Idempotent: a marker in app data makes later runs return this report.
    let report = store
        .migrate_v1_config(&v1, V1_PRODUCT_ID, V1_SCHEMA_VERSION)
        .await?;
    Ok(Some(V1Migration {
        source: v1.to_string_lossy().into_owned(),
        ran_at: report.ran_at,
        migrated_configs: report.migrated_configs,
        skipped: report.skipped,
        errors: report.errors,
    }))
}

#[tauri::command]
pub(crate) async fn config_migrate_v1(app: tauri::AppHandle) -> Out<cmd::ConfigMigrateV1> {
    run_once(&app).await
}
