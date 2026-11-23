use std::sync::Arc;

use infinity_bridge::BridgeManager;
use infinity_ipc::commands as cmd;
use infinity_ipc::{IpcError, IpcErrorCode};
use infinity_persistence::EquipmentStore;
use infinity_persistence::equipment::{delete_preset, upsert_preset};
use tauri::State;

use super::{Args, Out};
use crate::equipment::{apply_live, broadcast};

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn unknown_preset(id: &str) -> IpcError {
    IpcError::new(
        IpcErrorCode::ValidationFailed,
        format!("no equipment preset {id:?}"),
    )
}

#[tauri::command]
pub(crate) async fn equipment_profile(
    store: State<'_, Arc<EquipmentStore>>,
    args: Args<cmd::EquipmentProfileGet>,
) -> Out<cmd::EquipmentProfileGet> {
    store.load(&args).await
}

#[tauri::command]
pub(crate) async fn equipment_preset_save(
    app: tauri::AppHandle,
    store: State<'_, Arc<EquipmentStore>>,
    args: Args<cmd::EquipmentPresetSaveCmd>,
) -> Out<cmd::EquipmentPresetSaveCmd> {
    let name = args.preset.name.trim().to_owned();
    if name.is_empty() {
        return Err(IpcError::new(
            IpcErrorCode::ValidationFailed,
            "a preset needs a name",
        ));
    }
    let mut preset = args.preset;
    preset.name = name;
    let (profile, _) = store
        .update(&args.namespace, |p| {
            upsert_preset(p, preset, now_ms());
            Ok(true)
        })
        .await?;
    broadcast(&app, &args.namespace, profile.clone());
    Ok(profile)
}

#[tauri::command]
pub(crate) async fn equipment_preset_delete(
    app: tauri::AppHandle,
    store: State<'_, Arc<EquipmentStore>>,
    args: Args<cmd::EquipmentPresetDelete>,
) -> Out<cmd::EquipmentPresetDelete> {
    let (profile, changed) = store
        .update(&args.namespace, |p| Ok(delete_preset(p, &args.id)))
        .await?;
    if changed {
        broadcast(&app, &args.namespace, profile.clone());
    }
    Ok(profile)
}

#[tauri::command]
pub(crate) async fn equipment_preset_apply(
    store: State<'_, Arc<EquipmentStore>>,
    bridges: State<'_, BridgeManager>,
    args: Args<cmd::EquipmentPresetApply>,
) -> Out<cmd::EquipmentPresetApply> {
    let profile = store.load(&args.namespace).await?;
    let preset = profile.preset(&args.id).ok_or_else(|| unknown_preset(&args.id))?;
    Ok(apply_live(&bridges, &args.namespace, &preset.items).await?)
}

#[tauri::command]
pub(crate) async fn equipment_startup_set(
    app: tauri::AppHandle,
    store: State<'_, Arc<EquipmentStore>>,
    args: Args<cmd::EquipmentStartupSet>,
) -> Out<cmd::EquipmentStartupSet> {
    let (profile, changed) = store
        .update(&args.namespace, |p| {
            if let Some(id) = args.preset_id.as_deref()
                && p.preset(id).is_none()
            {
                return Err(unknown_preset(id));
            }
            let v = p.variant_mut(&args.variant);
            let changed = v.startup != args.preset_id;
            v.startup = args.preset_id.clone();
            Ok(changed)
        })
        .await?;
    if changed {
        broadcast(&app, &args.namespace, profile.clone());
    }
    Ok(profile)
}
