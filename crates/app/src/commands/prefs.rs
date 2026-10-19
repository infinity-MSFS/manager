use infinity_ipc::commands as cmd;
use infinity_ipc::events::PreferencesChanged;
use infinity_persistence::PreferencesStore;
use tauri::State;

use super::{Args, Out};
use crate::events::emit;

#[tauri::command]
pub(crate) async fn prefs_load(store: State<'_, PreferencesStore>) -> Out<cmd::PrefsLoad> {
    store.load().await
}

#[tauri::command]
pub(crate) async fn prefs_save(
    app: tauri::AppHandle,
    store: State<'_, PreferencesStore>,
    args: Args<cmd::PrefsSave>,
) -> Out<cmd::PrefsSave> {
    store.save(&args).await?;
    emit::<PreferencesChanged>(&app, &args);
    Ok(())
}
