//! SimBrief for in-sim gauges: a gauge asks over its bridge, the manager
//! fetches with the saved pilot ID and answers on the same bridge.

use infinity_bridge::{BridgeEvent, BridgeManager};
use infinity_persistence::PreferencesStore;
use infinity_simbrief::SimBriefClient;
use infinity_simbrief::wire::{self, Surface, WireError};
use tauri::Manager;
use tokio::sync::broadcast::error::RecvError;

pub(crate) fn serve(app: &tauri::AppHandle, bridges: &BridgeManager) {
    let mut events = bridges.subscribe();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match events.recv().await {
                Ok(e)
                    if e.name == wire::FETCH_REQUEST_EVENT
                        || e.name == wire::STATUS_REQUEST_EVENT =>
                {
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move { answer(&app, e).await });
                }
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return,
            }
        }
    });
}

async fn answer(app: &tauri::AppHandle, event: BridgeEvent) {
    let bridges = app.state::<BridgeManager>();
    let pilot_id = match app.state::<PreferencesStore>().load().await {
        Ok(p) => p.simbrief_pilot_id,
        Err(e) => {
            tracing::warn!(error = %e, "simbrief: preferences unreadable");
            None
        }
    };
    let (command, body) = if event.name == wire::STATUS_REQUEST_EVENT {
        (
            wire::STATUS_COMMAND,
            wire::bridge_status(pilot_id.as_deref()),
        )
    } else {
        let request_id = wire::request_id(&event.data);
        let client = app.state::<SimBriefClient>();
        let outcome = wire::fetch_plan(&client, pilot_id.as_deref())
            .await
            .map_err(|e| WireError::from_error(&e, Surface::Gauge));
        (
            wire::DELIVER_COMMAND,
            wire::bridge_deliver(request_id.as_deref(), outcome.as_ref()),
        )
    };
    if let Err(e) = bridges
        .command(&event.namespace, command, body, wire::DELIVER_TIMEOUT)
        .await
    {
        tracing::warn!(namespace = %event.namespace, command, error = %e, "simbrief: reply not delivered");
    }
}
