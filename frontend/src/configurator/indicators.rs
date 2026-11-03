//! Sidebar status lights (v3 `indicators.tsx`), one per `IndicatorSpec`.
//!
//! The bridge light follows `bridge:connection` events after one status
//! read, rather than v3's one-second poll.

use infinity_ipc::IpcErrorCode;
use infinity_ipc::catalog::IndicatorSpec;
use infinity_ipc::commands as cmd;
use infinity_ipc::events::BridgeConnectionChanged;
use infinity_ui::prelude::*;

use super::state::Configurator;
use crate::ipc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Light {
    On,
    Warning,
    Off,
}

impl Light {
    fn tone(self) -> Tone {
        match self {
            Self::On => Tone::Accent,
            Self::Warning => Tone::Warning,
            Self::Off => Tone::Danger,
        }
    }
}

#[component]
fn Row(#[prop(into)] light: Signal<Light>, #[prop(into)] label: Signal<String>) -> impl IntoView {
    view! {
        <div class="flex items-center gap-3 py-1.5">
            {move || view! { <StatusDot tone=light.get().tone() /> }}
            <span class="text-sm font-light text-white/70">{move || label.get()}</span>
        </div>
    }
}

/// Whether a bridge namespace has gauges connected, kept current.
pub(crate) fn bridge_connected(namespace: Option<String>) -> RwSignal<Option<bool>> {
    let state = RwSignal::new(None::<bool>);
    let Some(ns) = namespace else {
        state.set(Some(false));
        return state;
    };
    let initial = ns.clone();
    leptos::task::spawn_local(async move {
        let connected = match ipc::invoke::<cmd::BridgeStatusList>(&()).await {
            Ok(all) => all.iter().any(|b| b.namespace == initial && b.connected),
            Err(_) => false,
        };
        // A connection event may have landed first; it is newer.
        let _ = state.try_update(|s| {
            if s.is_none() {
                *s = Some(connected);
            }
        });
    });
    ipc::listen::<BridgeConnectionChanged>(move |change| {
        if change.namespace == ns {
            state.set(Some(change.connected));
        }
    });
    state
}

#[component]
fn BridgeIndicator(cfg: Configurator, label: String, bridge_ref: String) -> impl IntoView {
    let namespace = cfg.product.with_value(|p| {
        p.bridges
            .iter()
            .find(|b| b.id == bridge_ref)
            .map(|b| b.namespace.clone())
    });
    let state = bridge_connected(namespace);
    let light = Signal::derive(move || match state.get() {
        Some(true) => Light::On,
        Some(false) => Light::Off,
        None => Light::Warning,
    });
    view! { <Row light=light label=label /> }
}

#[component]
fn NavCycleIndicator(cfg: Configurator, label: String) -> impl IntoView {
    let cycle = LocalResource::new(move || async move {
        let result = ipc::invoke::<cmd::NavCycle>(&cfg.product_ref()).await;
        if let Err(e) = &result
            && e.code != IpcErrorCode::PackageDirNotFound
        {
            ipc::warn(&format!("nav cycle: {e}"));
        }
        result.ok()
    });
    let light = Signal::derive(move || match cycle.get() {
        Some(Some(_)) => Light::On,
        _ => Light::Off,
    });
    let text = Signal::derive(move || match cycle.get() {
        Some(Some(c)) => format!("{label}: {c}"),
        Some(None) => format!("{label}: Not found"),
        None => format!("{label}: …"),
    });
    view! { <Row light=light label=text /> }
}

pub(crate) fn indicator(cfg: Configurator, spec: IndicatorSpec) -> AnyView {
    match spec {
        IndicatorSpec::BridgeConnected {
            label, bridge_ref, ..
        } => view! { <BridgeIndicator cfg=cfg label=label bridge_ref=bridge_ref /> }.into_any(),
        IndicatorSpec::NavDbCycle { label, .. } => {
            view! { <NavCycleIndicator cfg=cfg label=label /> }.into_any()
        }
    }
}
