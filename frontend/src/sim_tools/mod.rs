//! Views for the `infinity-sim-tools` plugin: the terrain radar database
//! (a Settings section and a configurator panel) and the developer Perf
//! page (with the FLIR panel).

mod charts;
mod flir;
mod perf;
mod terrain;

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::ipc;
use infinity_ui::prelude::*;

pub use perf::Perf;
pub use terrain::{TerrainSettings, configurator_panel as terrain_panel};

/// Adds the sim-tools views: the terrain database as a Settings section
/// and a configurator panel, and the Perf page (with FLIR) behind `perf`.
/// The desktop app must register the `infinity-sim-tools` plugin.
pub fn register(extensions: &mut crate::Extensions, perf: crate::Gate) {
    extensions.pages.push(crate::ExtensionPage {
        slug: "perf",
        label: "Perf",
        gate: perf,
        view: || view! { <Perf /> }.into_any(),
    });
    extensions.settings.push(crate::SettingsSection {
        label: "Terrain radar",
        gate: crate::Gate::Everyone,
        view: || view! { <TerrainSettings /> }.into_any(),
    });
    extensions.configurator.push(crate::ConfiguratorPanel {
        view: terrain_panel,
    });
}

/// Runs `tick` every `every_ms` while the calling component is mounted.
fn poll<F>(every_ms: u32, mut tick: impl FnMut() -> F + 'static)
where
    F: Future<Output = ()> + 'static,
{
    let alive = Arc::new(AtomicBool::new(true));
    let flag = alive.clone();
    on_cleanup(move || flag.store(false, Ordering::Relaxed));
    leptos::task::spawn_local(async move {
        while alive.load(Ordering::Relaxed) {
            tick().await;
            ipc::sleep(every_ms).await;
        }
    });
}

/// A titled panel, as the Perf and FLIR pages lay them out.
#[component]
fn Section(
    title: &'static str,
    #[prop(optional)] icon: Option<IconData>,
    #[prop(optional)] aside: Option<ViewFn>,
    children: Children,
) -> impl IntoView {
    view! {
        <section class="overflow-hidden rounded-sm border border-white/10 bg-white/[0.03]">
            <div class="flex items-center gap-3 border-b border-white/10 px-5 py-3">
                <h2 class="flex flex-1 items-center gap-2 text-sm font-medium tracking-wide text-white/85">
                    {icon.map(|i| view! { <Icon icon=i class="h-4 w-4 text-accent-400" /> })}
                    {title}
                </h2>
                {aside.map(|a| a.run())}
            </div>
            <div class="p-5">{children()}</div>
        </section>
    }
}
