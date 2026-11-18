//! Rechecks what the account owns without signing out and in.
//!
//! Ownership and channel grants are claims on the access token (`permissions`
//! plus the post-login Action's namespaced claims), fixed when the token is
//! issued. So a purchase, or a role such as `dc10-beta-tester`, reaches the
//! manager only with a new token. A recheck renews the token (one
//! refresh-token grant: the identity provider re-runs RBAC and the Action)
//! and asks the content provider what the account owns now.
//!
//! It runs when it is likely to matter, not on a timer:
//! - the window gains focus (bought in the browser, came back): at most
//!   every [`ON_FOCUS`];
//! - a download is refused as not owned: once, then the request is retried;
//! - Refresh purchases in account settings.
//!
//! Each trigger is throttled by the time since the last renewal, whatever
//! triggered it, so a session costs a handful of token calls an hour at
//! most.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use infinity_ipc::IpcResult;
use infinity_ipc::catalog::EntitlementsResponse;
use infinity_ipc::dto::SessionState;
use infinity_ipc::events::EntitlementsChanged;
use tauri::Manager;

use crate::events::emit;
use crate::state::AppState;

/// Focus: the common way back from a purchase in the browser.
pub(crate) const ON_FOCUS: Duration = Duration::from_secs(5 * 60);
/// A refused download or the button: the user is waiting on the answer.
pub(crate) const ON_DEMAND: Duration = Duration::from_secs(15);

#[derive(Debug)]
pub(crate) struct Recheck {
    last_renewal: Mutex<Instant>,
    /// What the last recheck found, to tell whether it changed.
    known: Mutex<Option<EntitlementsResponse>>,
}

impl Recheck {
    /// Starts as just renewed: the session was restored (or the user signs
    /// in) at launch, so its token is new already.
    pub(crate) fn new() -> Self {
        Self {
            last_renewal: Mutex::new(Instant::now()),
            known: Mutex::new(None),
        }
    }

    /// Takes the next renewal if none happened within `min_age`.
    fn claim(&self, min_age: Duration) -> bool {
        let Ok(mut last) = self.last_renewal.lock() else {
            return false;
        };
        if last.elapsed() < min_age {
            return false;
        }
        *last = Instant::now();
        true
    }

    /// Records `now`, returning whether it differs from what was known.
    fn record(&self, now: &Option<EntitlementsResponse>) -> bool {
        let Ok(mut known) = self.known.lock() else {
            return true;
        };
        let changed = known.as_ref() != now.as_ref();
        known.clone_from(now);
        changed
    }
}

/// Renews and rechecks if the last renewal is older than `min_age`.
/// `None` when throttled: nothing was asked.
pub(crate) async fn if_due(
    app: &tauri::AppHandle,
    min_age: Duration,
) -> Option<IpcResult<Option<EntitlementsResponse>>> {
    let recheck = app.try_state::<Recheck>()?;
    if !recheck.claim(min_age) {
        return None;
    }
    Some(run(app).await)
}

/// Renews the token, asks what the account owns, and tells the UI. Signed
/// out, there is nothing to renew: `Ok(None)`.
pub(crate) async fn run(app: &tauri::AppHandle) -> IpcResult<Option<EntitlementsResponse>> {
    let state = app.state::<AppState>();
    let providers = &state.providers;
    if !matches!(providers.identity.session().await, SessionState::SignedIn { .. }) {
        return Ok(None);
    }
    providers.identity.renew().await?;
    let owned = providers.content.entitlements().await?;
    emit::<EntitlementsChanged>(app, &owned);
    let changed = app
        .try_state::<Recheck>()
        .is_some_and(|r| r.record(&owned));
    if changed {
        tracing::info!("entitlements changed on recheck");
        crate::presets::entitlements_changed(app);
    }
    Ok(owned)
}

/// Rechecks when the main window gains focus, at most every [`ON_FOCUS`].
pub(crate) fn on_focus(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let app = app.clone();
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::Focused(true) = event {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Some(Err(e)) = if_due(&app, ON_FOCUS).await {
                    tracing::debug!(code = %e.code, "entitlement recheck on focus failed");
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owning(products: &[&str]) -> Option<EntitlementsResponse> {
        Some(EntitlementsResponse {
            products: products.iter().map(|p| (*p).into()).collect(),
            channels: Vec::new(),
        })
    }

    #[test]
    fn claims_are_throttled_from_the_last_renewal() {
        let r = Recheck::new();
        assert!(!r.claim(ON_DEMAND), "launch counts as a renewal");
        assert!(r.claim(Duration::ZERO));
        assert!(!r.claim(ON_DEMAND));
    }

    #[test]
    fn record_reports_only_changes() {
        let r = Recheck::new();
        assert!(r.record(&owning(&["aerodynamics-t38c"])));
        assert!(!r.record(&owning(&["aerodynamics-t38c"])));
        assert!(r.record(&owning(&["aerodynamics-t38c", "aerodynamics-dc10"])));
        assert!(r.record(&None));
    }
}
