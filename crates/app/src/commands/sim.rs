use infinity_ipc::commands as cmd;
use infinity_persistence::PreferencesStore;
use infinity_sim_watch::SimWatch;
use tauri::State;

use std::path::PathBuf;

use tauri_plugin_dialog::DialogExt;

use super::{Args, Out};
use crate::paths;

#[tauri::command]
pub(crate) async fn sim_status(watch: State<'_, SimWatch>) -> Out<cmd::SimStatus> {
    Ok(watch.status())
}

#[tauri::command]
pub(crate) async fn sim_paths(prefs: State<'_, PreferencesStore>) -> Out<cmd::SimPaths> {
    let prefs = prefs.load().await?;
    let locator = paths::locate(&prefs).await?;
    Ok(paths::sim_paths(&prefs, &locator))
}

#[tauri::command]
pub(crate) async fn sim_probe_work_root(
    args: Args<cmd::SimProbeWorkRoot>,
) -> Out<cmd::SimProbeWorkRoot> {
    let probe = paths::blocking(move || infinity_sim_paths::probe_work_root(&PathBuf::from(args)));
    Ok(paths::work_root_probe(probe.await?))
}

#[tauri::command]
pub(crate) async fn sim_probe_wine_prefix(
    args: Args<cmd::SimProbeWinePrefix>,
) -> Out<cmd::SimProbeWinePrefix> {
    let probe = paths::blocking(move || infinity_sim_paths::probe_prefix(&PathBuf::from(args)));
    Ok(paths::prefix_probe(probe.await?))
}

#[tauri::command]
pub(crate) async fn sim_detected_prefixes() -> Out<cmd::SimDetectedPrefixes> {
    let found = paths::blocking(infinity_sim_paths::detected_prefixes).await?;
    Ok(found.into_iter().map(paths::prefix_probe).collect())
}

#[tauri::command]
pub(crate) async fn dialog_pick_folder(
    app: tauri::AppHandle,
    args: Args<cmd::DialogPickFolder>,
) -> Out<cmd::DialogPickFolder> {
    let picked = paths::blocking(move || {
        let mut dialog = app.dialog().file().set_title(args.title);
        if let Some(start) = args.start.filter(|s| PathBuf::from(s).is_dir()) {
            dialog = dialog.set_directory(start);
        }
        dialog.blocking_pick_folder()
    })
    .await?;
    Ok(picked
        .and_then(|p| p.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned()))
}
