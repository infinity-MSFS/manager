//! Equipment persistence: follows every `equip_state` an aircraft pushes on
//! any bridge, records the user's changes and restores the saved fit when a
//! new aircraft loads. The rule itself is
//! [`infinity_persistence::equipment::observe`]; this module only runs it.
//!
//! It lives in the backend, not on the equipment page, so a flight is
//! restored and remembered whether or not the manager's window is open on
//! that page: the user may only ever touch the EFB. Bridges are up while the
//! sim runs (`efb_presence` starts every installed product's).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use infinity_bridge::{BridgeManager, DEFAULT_CMD_TIMEOUT};
use infinity_ipc::dto::EquipmentProfileChanged;
use infinity_ipc::events::EquipmentProfileUpdated;
use infinity_persistence::EquipmentStore;
use infinity_persistence::equipment::{Observed, observe};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::sync::broadcast::error::RecvError;

use crate::events::emit;

pub(crate) const STATE_EVENT: &str = "equip_state";
pub(crate) const APPLY: &str = "equip_apply";

/// A restore that has not landed is not re-sent sooner than this. The
/// aircraft re-pushes every 2 s until it lands, and a slow module must not
/// be handed a second restore while it is still answering the first.
const RESTORE_RETRY: Duration = Duration::from_secs(6);

/// Starts following `bridges`. Needs `EquipmentStore` managed.
pub(crate) fn start(app: &tauri::AppHandle, bridges: &BridgeManager, store: Arc<EquipmentStore>) {
    let mut events = bridges.subscribe();
    let bridges = bridges.clone();
    let app = app.clone();
    let attempts: Arc<Mutex<HashMap<String, Instant>>> = Arc::default();
    tauri::async_runtime::spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(e) => e,
                Err(RecvError::Lagged(n)) => {
                    tracing::debug!(skipped = n, "equipment: events lagged");
                    continue;
                }
                Err(RecvError::Closed) => return,
            };
            if event.name != STATE_EVENT {
                continue;
            }
            let Some(seen) = Observed::parse(&event.data) else {
                continue;
            };
            let ns = event.namespace;
            let restore = record(&app, &store, &ns, &seen).await;
            let Some(items) = restore else {
                attempts.lock().await.remove(&ns);
                continue;
            };
            {
                let mut a = attempts.lock().await;
                if a.get(&ns).is_some_and(|t| t.elapsed() < RESTORE_RETRY) {
                    continue;
                }
                a.insert(ns.clone(), Instant::now());
            }
            let (app, store, bridges) = (app.clone(), store.clone(), bridges.clone());
            tauri::async_runtime::spawn(async move {
                tracing::info!(namespace = ns, variant = seen.variant, ?items, "equipment: restoring saved fit");
                match bridges
                    .command(&ns, APPLY, json!({ "items": items, "restore": true }), DEFAULT_CMD_TIMEOUT)
                    .await
                {
                    Ok(snapshot) => {
                        if let Some(seen) = Observed::parse(&snapshot) {
                            record(&app, &store, &ns, &seen).await;
                        }
                    }
                    // Retried on a later push once RESTORE_RETRY has passed.
                    Err(e) => tracing::warn!(namespace = ns, error = %e, "equipment: restore failed"),
                }
            });
        }
    });
}

/// Runs the rule on one snapshot, saves and broadcasts a change, and
/// returns the restore to send, if any.
async fn record(
    app: &tauri::AppHandle,
    store: &EquipmentStore,
    namespace: &str,
    seen: &Observed,
) -> Option<Vec<u32>> {
    let mut restore = None;
    match store
        .update(namespace, |profile| {
            let o = observe(profile, seen);
            restore = o.restore;
            Ok(o.changed)
        })
        .await
    {
        Ok((profile, true)) => broadcast(app, namespace, profile),
        Ok((_, false)) => {}
        Err(e) => tracing::warn!(namespace, error = %e, "equipment: could not save"),
    }
    restore
}

pub(crate) fn broadcast(
    app: &tauri::AppHandle,
    namespace: &str,
    profile: infinity_ipc::dto::EquipmentProfile,
) {
    emit::<EquipmentProfileUpdated>(
        app,
        &EquipmentProfileChanged {
            namespace: namespace.to_owned(),
            profile,
        },
    );
}

/// Sends `items` to the running aircraft as a user's choice (a preset
/// load), which the aircraft reports as `origin: select` and the rule
/// above then records as the last fit.
pub(crate) async fn apply_live(
    bridges: &BridgeManager,
    namespace: &str,
    items: &[u32],
) -> Result<Value, infinity_bridge::BridgeError> {
    bridges
        .command(
            namespace,
            APPLY,
            json!({ "items": items, "restore": false }),
            DEFAULT_CMD_TIMEOUT,
        )
        .await
}
