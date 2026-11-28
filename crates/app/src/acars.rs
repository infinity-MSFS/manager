//! ACARS account persistence: follows every `acars.account` an aircraft
//! pushes on any bridge, saves the callsign and logon code the user typed
//! into the aircraft's EFB, and hands them back when the aircraft next
//! loads. The rule itself is [`infinity_persistence::acars::observe`]; this
//! module only runs it. `equipment`'s shape, for its reasons: it lives in
//! the backend, so the account is restored and remembered whether or not the
//! manager's window is open.
//!
//! The callsign is saved in `<app_data>/acars/<ns>.json`. The logon code is
//! a credential (the user's account on the Hoppie network) and is kept in
//! the OS keyring under the app's identifier, never on disk and never in a
//! log line. If the keyring cannot be reached the code is simply not
//! remembered: it is not written anywhere else instead.
//!
//! The aircraft's event name is dotted so that `forward_bridges` does not
//! pass it to the UI (`BridgeEvent::channel`): the snapshot carries the logon
//! code while a choice waits to be saved.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use infinity_bridge::{BridgeManager, DEFAULT_CMD_TIMEOUT};
use infinity_persistence::AcarsStore;
use infinity_persistence::acars::{AcarsProfile, Account, Observed, observe};
use serde_json::json;
use tokio::sync::Mutex;
use tokio::sync::broadcast::error::RecvError;

pub(crate) const ACCOUNT_EVENT: &str = "acars.account";
pub(crate) const APPLY: &str = "acars_account_apply";

/// An apply that has not landed is not re-sent sooner than this. The
/// aircraft re-pushes every 2 s until it lands, and a slow module must not
/// be handed a second one while it is still answering the first.
const APPLY_RETRY: Duration = Duration::from_secs(6);

/// Starts following `bridges`.
pub(crate) fn start(app: &tauri::AppHandle, bridges: &BridgeManager, store: Arc<AcarsStore>) {
    let mut events = bridges.subscribe();
    let bridges = bridges.clone();
    let identifier = app.config().identifier.clone();
    let attempts: Arc<Mutex<HashMap<String, Instant>>> = Arc::default();
    tauri::async_runtime::spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(e) => e,
                Err(RecvError::Lagged(n)) => {
                    tracing::debug!(skipped = n, "acars: events lagged");
                    continue;
                }
                Err(RecvError::Closed) => return,
            };
            if event.name != ACCOUNT_EVENT {
                continue;
            }
            let Some(seen) = Observed::parse(&event.data) else {
                continue;
            };
            let ns = event.namespace;
            let saved = load(&store, &identifier, &ns).await;
            let observation = observe(&saved, &seen);
            if let Some(account) = &observation.save {
                save(&store, &identifier, &ns, account).await;
            }
            let Some(account) = observation.send else {
                attempts.lock().await.remove(&ns);
                continue;
            };
            {
                let mut a = attempts.lock().await;
                if a.get(&ns).is_some_and(|t| t.elapsed() < APPLY_RETRY) {
                    continue;
                }
                a.insert(ns.clone(), Instant::now());
            }
            let bridges = bridges.clone();
            tauri::async_runtime::spawn(async move {
                tracing::info!(namespace = ns, ?account, "acars: sending the saved account");
                let payload = json!({
                    "callsign": account.callsign,
                    "logon": account.logon,
                    "restore": true,
                });
                // Retried on a later push once APPLY_RETRY has passed.
                if let Err(e) = bridges
                    .command(&ns, APPLY, payload, DEFAULT_CMD_TIMEOUT)
                    .await
                {
                    tracing::warn!(namespace = ns, error = %e, "acars: account not delivered");
                }
            });
        }
    });
}

/// The saved account: the callsign from the store, the logon code from the
/// keyring. A part that cannot be read is empty.
async fn load(store: &AcarsStore, identifier: &str, namespace: &str) -> Account {
    let callsign = match store.load(namespace).await {
        Ok(profile) => profile.callsign,
        Err(e) => {
            tracing::warn!(namespace, error = %e, "acars: could not read the saved callsign");
            String::new()
        }
    };
    let logon = match logon_entry(identifier, namespace).map(|e| e.get_password()) {
        Ok(Ok(code)) => code,
        Ok(Err(keyring::Error::NoEntry)) => String::new(),
        Ok(Err(e)) | Err(e) => {
            tracing::warn!(namespace, error = %e, "acars: could not read the keyring");
            String::new()
        }
    };
    Account { callsign, logon }
}

async fn save(store: &AcarsStore, identifier: &str, namespace: &str, account: &Account) {
    let profile = AcarsProfile {
        callsign: account.callsign.clone(),
    };
    match store.save(namespace, &profile).await {
        Ok(true) => tracing::info!(
            namespace,
            callsign = profile.callsign,
            "acars: callsign saved"
        ),
        Ok(false) => {}
        Err(e) => tracing::warn!(namespace, error = %e, "acars: could not save the callsign"),
    }
    let written = logon_entry(identifier, namespace).and_then(|entry| {
        if account.logon.is_empty() {
            // The user removed the code.
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(e),
            }
        } else {
            entry.set_password(&account.logon)
        }
    });
    if let Err(e) = written {
        tracing::warn!(namespace, error = %e, "acars: could not save the logon code to the keyring");
    }
}

fn logon_entry(identifier: &str, namespace: &str) -> Result<keyring::Entry, keyring::Error> {
    keyring::Entry::new(identifier, &format!("acars-logon-{namespace}"))
}
