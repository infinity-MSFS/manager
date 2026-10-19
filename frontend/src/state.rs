use infinity_ipc::commands as cmd;
use infinity_ipc::dto::{AppInfo, Capabilities, Preferences, SessionState, SimStatus};
use infinity_ipc::events::{PreferencesChanged, SessionChanged, SimStatusChanged};
use leptos::prelude::*;

use crate::ipc;

#[derive(Debug, Clone, Copy)]
pub struct AppState {
    pub info: RwSignal<Option<AppInfo>>,
    pub capabilities: RwSignal<Capabilities>,
    pub session: RwSignal<SessionState>,
    pub prefs: RwSignal<Preferences>,
    pub sim: RwSignal<SimStatus>,
}

impl AppState {
    fn new() -> Self {
        Self {
            info: RwSignal::new(None),
            capabilities: RwSignal::new(Capabilities::default()),
            session: RwSignal::new(SessionState::SignedOut),
            prefs: RwSignal::new(Preferences::default()),
            sim: RwSignal::new(SimStatus::default()),
        }
    }

    pub fn reduce_motion(self) -> Signal<bool> {
        Signal::derive(move || {
            let sim_running = self.sim.get().running.is_some();
            self.prefs.with(|p| p.motion.reduce(sim_running))
        })
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
    });
    ipc::listen::<SessionChanged>(move |s| state.session.set(s));
    ipc::listen::<PreferencesChanged>(move |p| state.prefs.set(p));
    ipc::listen::<SimStatusChanged>(move |s| state.sim.set(s));
}

pub fn use_app() -> AppState {
    use_context::<AppState>().unwrap_or_else(|| {
        ipc::warn("AppState missing from context");
        AppState::new()
    })
}
