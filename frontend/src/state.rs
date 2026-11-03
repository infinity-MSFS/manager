use std::collections::BTreeMap;

use infinity_ipc::catalog::{EntitlementsResponse, OwnershipPolicy, Product, ProductId};
use infinity_ipc::commands as cmd;
use infinity_ipc::dto::{
    AppInfo, Capabilities, Channel, InstalledPackage, Job, JobPhase, Preferences, SessionState,
    SimStatus,
};
use infinity_ipc::events::{
    InstalledChanged, JobChanged, PreferencesChanged, SessionChanged, SimStatusChanged,
};
use leptos::prelude::*;

use crate::ipc;

#[derive(Debug, Clone, Copy)]
pub struct AppState {
    pub info: RwSignal<Option<AppInfo>>,
    pub capabilities: RwSignal<Capabilities>,
    pub session: RwSignal<SessionState>,
    pub prefs: RwSignal<Preferences>,
    pub sim: RwSignal<SimStatus>,
    /// Running and stopped installs, by product. Finished ones drop out.
    pub jobs: RwSignal<BTreeMap<ProductId, Job>>,
    pub installed: RwSignal<Vec<InstalledPackage>>,
    /// What the account owns. `None` without a store or before sign-in.
    pub entitlements: RwSignal<Option<EntitlementsResponse>>,
}

impl AppState {
    fn new() -> Self {
        Self {
            info: RwSignal::new(None),
            capabilities: RwSignal::new(Capabilities::default()),
            session: RwSignal::new(SessionState::SignedOut),
            prefs: RwSignal::new(Preferences::default()),
            sim: RwSignal::new(SimStatus::default()),
            jobs: RwSignal::new(BTreeMap::new()),
            installed: RwSignal::new(Vec::new()),
            entitlements: RwSignal::new(None),
        }
    }

    /// The preferred channel if this build offers it, else stable.
    pub fn channel(self) -> Signal<Channel> {
        Signal::derive(move || {
            let wanted = self.prefs.with(|p| p.channel);
            let offered = self.capabilities.with(|c| c.channels.contains(&wanted));
            if offered { wanted } else { Channel::Stable }
        })
    }

    /// The account may see pre-release rings.
    pub fn is_tester(self) -> Signal<bool> {
        Signal::derive(move || {
            self.entitlements.with(|e| {
                e.as_ref().is_some_and(|e| {
                    e.channels
                        .iter()
                        .any(|c| matches!(c, Channel::Beta | Channel::Nightly))
                })
            })
        })
    }

    /// Whether the library lists a product, as v3: free products always,
    /// payware once owned, coming-soon teasers only for testers.
    pub fn shows(self, product: &Product) -> bool {
        if product.coming_soon {
            return self.is_tester().get();
        }
        matches!(product.ownership, OwnershipPolicy::Free)
            || self
                .entitlements
                .with(|e| e.as_ref().is_some_and(|e| e.products.contains(&product.id)))
    }

    pub fn reduce_motion(self) -> Signal<bool> {
        Signal::derive(move || {
            let sim_running = self.sim.get().running.is_some();
            self.prefs.with(|p| p.motion.reduce(sim_running))
        })
    }

    pub fn job(self, product_id: String) -> Signal<Option<Job>> {
        Signal::derive(move || self.jobs.with(|j| j.get(&product_id).cloned()))
    }

    pub fn installed_package(self, product_id: String) -> Signal<Option<InstalledPackage>> {
        Signal::derive(move || {
            self.installed
                .with(|all| all.iter().find(|p| p.product_id == product_id).cloned())
        })
    }

    fn apply_job(self, job: Job) {
        self.jobs.update(|jobs| match job.phase {
            JobPhase::Completed | JobPhase::Cancelled => {
                jobs.remove(&job.product_id);
            }
            _ => {
                jobs.insert(job.product_id.clone(), job);
            }
        });
    }
}

pub(crate) fn provide_app_state() {
    let state = AppState::new();
    provide_context(state);

    leptos::task::spawn_local(async move {
        if let Ok(info) = ipc::invoke::<cmd::AppInfo>(&()).await {
            state.info.set(Some(info));
        }
        if let Ok(caps) = ipc::invoke::<cmd::Capabilities>(&()).await {
            state.capabilities.set(caps);
        }
        if let Ok(prefs) = ipc::invoke::<cmd::PrefsLoad>(&()).await {
            state.prefs.set(prefs);
        }
        if let Ok(sim) = ipc::invoke::<cmd::SimStatus>(&()).await {
            state.sim.set(sim);
        }
        if let Ok(session) = ipc::invoke::<cmd::AuthSession>(&()).await {
            state.session.set(session);
        }
        if let Ok(installed) = ipc::invoke::<cmd::InstalledList>(&()).await {
            state.installed.set(installed);
        }
        if let Ok(jobs) = ipc::invoke::<cmd::JobsList>(&()).await {
            for job in jobs {
                state.apply_job(job);
            }
        }
    });
    // Ownership follows the account: refetch whenever the session changes.
    Effect::new(move |_| {
        let signed_in = matches!(state.session.get(), SessionState::SignedIn { .. });
        leptos::task::spawn_local(async move {
            let owned = if signed_in {
                ipc::invoke::<cmd::AccountEntitlements>(&())
                    .await
                    .ok()
                    .flatten()
            } else {
                None
            };
            state.entitlements.set(owned);
        });
    });
    ipc::listen::<SessionChanged>(move |s| state.session.set(s));
    ipc::listen::<PreferencesChanged>(move |p| state.prefs.set(p));
    ipc::listen::<SimStatusChanged>(move |s| state.sim.set(s));
    ipc::listen::<JobChanged>(move |job| state.apply_job(job));
    ipc::listen::<InstalledChanged>(move |all| state.installed.set(all));
}

pub fn use_app() -> AppState {
    use_context::<AppState>().unwrap_or_else(|| {
        ipc::warn("AppState missing from context");
        AppState::new()
    })
}
