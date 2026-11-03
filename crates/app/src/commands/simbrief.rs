use infinity_ipc::commands as cmd;
use infinity_ipc::events::PreferencesChanged;
use infinity_persistence::PreferencesStore;
use infinity_simbrief::SimBriefClient;
use tauri::State;

use super::{Args, Out};
use crate::events::emit;

#[tauri::command]
pub(crate) async fn simbrief_set_pilot_id(
    app: tauri::AppHandle,
    prefs: State<'_, PreferencesStore>,
    args: Args<cmd::SimBriefSetPilotId>,
) -> Out<cmd::SimBriefSetPilotId> {
    let pilot_id = infinity_simbrief::wire::normalize_pilot_id(args)?;
    let mut current = prefs.load().await?;
    current.simbrief_pilot_id = pilot_id;
    prefs.save(&current).await?;
    emit::<PreferencesChanged>(&app, &current);
    Ok(())
}

#[tauri::command]
pub(crate) async fn simbrief_fetch_plan(
    client: State<'_, SimBriefClient>,
    prefs: State<'_, PreferencesStore>,
) -> Out<cmd::SimBriefFetchPlan> {
    let pilot_id = prefs.load().await?.simbrief_pilot_id;
    Ok(infinity_simbrief::wire::fetch_plan(&client, pilot_id.as_deref()).await?)
}
