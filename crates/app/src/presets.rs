//! Keeps installed packages' preset visibility in step with the account:
//! at startup, whenever the session changes, and after each install.
//!
//! The file work lives in `infinity_installer::presets`; this module only
//! decides when it runs and for whom.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use infinity_installer::presets::{self, Outcome};
use infinity_ipc::catalog::Product;
use infinity_ipc::dto::{Channel, PresetsChanged, SessionState};
use infinity_ipc::events::{SessionChanged, SimRestartNeeded};
use infinity_persistence::{InstalledStore, PreferencesStore};
use infinity_sim_watch::SimWatch;
use tauri::Manager;

use crate::events::emit;
use crate::state::AppState;

#[derive(Debug, Default)]
pub(crate) struct PresetGate {
    /// One pass at a time, so two triggers never write the same file.
    run: tokio::sync::Mutex<()>,
    /// A retry is waiting for the sim to stop.
    retry_armed: AtomicBool,
}

/// Tells the UI the session changed and re-applies preset visibility for
/// whoever is now signed in (or for nobody).
///
/// Every session change goes through here. A build that restores a saved
/// session at startup (`PkceIdentity::restore`) must call this afterwards,
/// or the restored account's presets apply only at the next trigger.
pub fn session_changed(app: &tauri::AppHandle, state: &SessionState) {
    emit::<SessionChanged>(app, state);
    let app = app.clone();
    tauri::async_runtime::spawn(async move { apply_installed(&app).await });
}

/// Once at startup.
pub(crate) fn on_startup(app: &tauri::AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move { apply_installed(&app).await });
}

/// After a job for `product_id` completes: the swap may have brought in
/// cfgs that show every preset.
pub(crate) fn after_job(app: &tauri::AppHandle, product_id: String, channel: Channel) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };
        let product = match state.catalog(channel).await {
            Ok(catalog) => catalog
                .products
                .iter()
                .find(|p| p.id == product_id && p.presets.is_some())
                .cloned(),
            Err(e) => {
                tracing::warn!(product = product_id, code = %e.code, "no catalog to gate presets with");
                None
            }
        };
        if let Some(product) = product {
            apply_with_retry(&app, vec![product]).await;
        }
    });
}

/// Every product with preset lists, from each channel's catalog.
async fn apply_installed(app: &tauri::AppHandle) {
    let products = gated_products(app).await;
    apply_with_retry(app, products).await;
}

async fn apply_with_retry(app: &tauri::AppHandle, products: Vec<Product>) {
    if apply(app, &products).await {
        retry_when_sim_stops(app);
    }
}

/// Products listing presets. The library's channel comes first: the
/// install record does not say which channel a package came from, and
/// that is the one the user installs from.
async fn gated_products(app: &tauri::AppHandle) -> Vec<Product> {
    let Some(state) = app.try_state::<AppState>() else {
        return Vec::new();
    };
    let preferred = match app.try_state::<PreferencesStore>() {
        Some(prefs) => prefs.load().await.ok().map(|p| p.channel),
        None => None,
    };
    let mut channels: Vec<Channel> = preferred
        .into_iter()
        .filter(|c| state.providers.channels.contains(c))
        .collect();
    for channel in &state.providers.channels {
        if !channels.contains(channel) {
            channels.push(*channel);
        }
    }
    let mut products: Vec<Product> = Vec::new();
    for channel in channels {
        match state.catalog(channel).await {
            Ok(catalog) => {
                for product in &catalog.products {
                    if product.presets.is_some() && !products.iter().any(|p| p.id == product.id) {
                        products.push(product.clone());
                    }
                }
            }
            // Offline: nothing to apply. Never guess.
            Err(e) => {
                tracing::debug!(channel = channel.as_str(), code = %e.code, "no catalog for preset gating")
            }
        }
    }
    products
}

/// One pass over `products`. Returns whether a file was locked under a
/// running sim, so a retry should follow once it stops.
async fn apply(app: &tauri::AppHandle, products: &[Product]) -> bool {
    if products.is_empty() {
        return false;
    }
    let (Some(state), Some(gate), Some(registry)) = (
        app.try_state::<AppState>(),
        app.try_state::<PresetGate>(),
        app.try_state::<Arc<InstalledStore>>(),
    ) else {
        return false;
    };
    let _one_at_a_time = gate.run.lock().await;
    // Read the session now, never an event's word for it: a failed
    // sign-in is reported as signed out.
    let profile = match state.providers.identity.session().await {
        SessionState::SignedIn { profile } => Some(profile),
        SessionState::SignedOut => None,
        SessionState::Pending => return false,
    };
    let running = app.try_state::<SimWatch>().and_then(|w| w.status().running);
    let mut retry = false;
    for product in products {
        let package = match registry.get(&product.id).await {
            Ok(Some(package)) => package,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(product = product.id, code = %e.code, "could not read the install record");
                continue;
            }
        };
        let path = PathBuf::from(&package.path);
        let gating = product.presets.clone();
        let profile = profile.clone();
        let outcome = tauri::async_runtime::spawn_blocking(move || {
            presets::gate(&path, gating.as_ref(), profile.as_ref())
        })
        .await;
        let report = match outcome {
            Ok(Some(Outcome::Applied(report))) => report,
            Ok(Some(Outcome::Unmanaged)) => {
                tracing::info!(
                    product = product.id,
                    path = package.path,
                    "package not installed by the manager; presets left alone"
                );
                continue;
            }
            Ok(Some(Outcome::NoPresets)) => {
                tracing::warn!(product = product.id, "package has no preset folder");
                continue;
            }
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(product = product.id, error = %e, "preset gating task failed");
                continue;
            }
        };
        for failure in &report.failed {
            tracing::warn!(
                product = product.id,
                path = %failure.path.display(),
                error = %failure.error,
                "could not update a preset"
            );
        }
        if !report.changed.is_empty() {
            tracing::info!(
                product = product.id,
                files = report.changed.len(),
                "preset visibility updated"
            );
        }
        let locked = report.locked();
        if let Some(sim) = running
            && (locked || !report.changed.is_empty())
        {
            emit::<SimRestartNeeded>(
                app,
                &PresetsChanged {
                    product_id: product.id.clone(),
                    name: product.name.clone(),
                    sim,
                },
            );
            retry |= locked;
        }
    }
    retry
}

/// Runs the gating again once the sim exits, once however many passes
/// asked for it.
fn retry_when_sim_stops(app: &tauri::AppHandle) {
    let (Some(gate), Some(watch)) = (app.try_state::<PresetGate>(), app.try_state::<SimWatch>())
    else {
        return;
    };
    if gate.retry_armed.swap(true, Ordering::SeqCst) {
        return;
    }
    let mut status = watch.subscribe();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        while status.borrow_and_update().running.is_some() {
            if status.changed().await.is_err() {
                return;
            }
        }
        if let Some(gate) = app.try_state::<PresetGate>() {
            gate.retry_armed.store(false, Ordering::SeqCst);
        }
        tracing::info!("sim stopped; retrying preset visibility");
        let products = gated_products(&app).await;
        // The sim is gone, so this pass never asks for another.
        apply(&app, &products).await;
    });
}
