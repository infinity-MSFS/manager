//! What the EFB server's chart lease knows about the simulator: is MSFS
//! running, and is an aircraft's WASM module connected over its bridge.
//!
//! Charts are licensed for use inside the aircraft (see
//! `infinity_efb_server::charts`). The third signal, the in-sim panel's own
//! check-in, the server tracks itself.
//!
//! Bridges normally start when the configurator opens. The lease cannot wait
//! for that, so while the sim runs this starts the bridges of every
//! installed product; a gauge that dials its bridge finds it listening.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use infinity_bridge::BridgeManager;
use infinity_efb_server::charts::SimPresence;
use infinity_persistence::InstalledStore;
use infinity_sim_watch::SimWatch;
use tauri::Manager;

use crate::state::AppState;

/// How often the bridges are re-read. A lease answer is at most this stale;
/// the server's own 15 s panel window dominates anyway.
const POLL: Duration = Duration::from_secs(2);
/// A product whose bridges could not be started (catalog offline) is tried
/// again after this, not every tick.
const RETRY_START: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub(crate) struct ChartsPresence {
    sim: SimWatch,
    aircraft: AtomicBool,
}

impl SimPresence for ChartsPresence {
    fn sim_running(&self) -> bool {
        self.sim.status().running.is_some()
    }

    fn aircraft_connected(&self) -> bool {
        // Both: a bridge left "ready" by a sim that just quit must not count.
        self.sim_running() && self.aircraft.load(Ordering::Acquire)
    }
}

/// Builds the presence and starts its poll. Needs `SimWatch`,
/// `BridgeManager`, `InstalledStore` and `AppState` managed first.
pub(crate) fn start(app: &tauri::AppHandle) -> Arc<ChartsPresence> {
    let presence = Arc::new(ChartsPresence {
        sim: app.state::<SimWatch>().inner().clone(),
        aircraft: AtomicBool::new(false),
    });
    let app = app.clone();
    let poll = presence.clone();
    tauri::async_runtime::spawn(async move {
        let mut started: HashSet<String> = HashSet::new();
        let mut last_try: Option<Instant> = None;
        let mut tick = tokio::time::interval(POLL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if !poll.sim_running() {
                poll.aircraft.store(false, Ordering::Release);
                continue;
            }
            let bridges = app.state::<BridgeManager>().inner().clone();
            if last_try.is_none_or(|t| t.elapsed() >= RETRY_START) {
                last_try = Some(Instant::now());
                start_product_bridges(&app, &bridges, &mut started).await;
            }
            let ready = bridges.statuses().await.iter().any(|b| b.ready);
            let was = poll.aircraft.swap(ready, Ordering::AcqRel);
            if was != ready {
                tracing::info!(ready, "charts presence: aircraft bridge");
            }
        }
    });
    presence
}

/// Starts every installed product's bridges. A product already done is
/// skipped; one that failed is retried on the next pass.
async fn start_product_bridges(
    app: &tauri::AppHandle,
    bridges: &BridgeManager,
    started: &mut HashSet<String>,
) {
    let installed = match app.state::<Arc<InstalledStore>>().list().await {
        Ok(list) => list,
        Err(e) => {
            tracing::debug!(error = %e, "charts presence: installed list");
            return;
        }
    };
    let state = app.state::<AppState>();
    for package in installed {
        let id = package.product_id.to_string();
        if started.contains(&id) {
            continue;
        }
        let mut done = false;
        for channel in state.providers.channels.clone() {
            let Ok(catalog) = state.catalog(channel).await else {
                continue;
            };
            let Some(product) = catalog.products.iter().find(|p| p.id == package.product_id) else {
                continue;
            };
            done = true;
            for spec in &product.bridges {
                if let Err(e) = bridges.ensure_started(spec.clone()).await {
                    tracing::debug!(namespace = spec.namespace, error = %e, "charts presence: bridge");
                    done = false;
                }
            }
            break;
        }
        if done {
            started.insert(id);
        }
    }
}
