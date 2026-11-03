mod boot;
mod commands;
mod deep_link;
mod efb;
mod efb_browser;
mod efb_docs;
mod events;
mod paths;
mod simbrief_gauge;
mod state;
mod telemetry;
mod tray;
mod window_geometry;

use std::process::ExitCode;

use std::sync::Arc;

use infinity_installer::Installer;
use infinity_ipc::dto::JobPhase;
use infinity_ipc::events::{InstalledChanged, JobChanged, SimStatusChanged};
use infinity_persistence::{InstalledStore, PreferencesStore};
use infinity_providers::{Providers, SimContext};
use infinity_sim_watch::SimWatch;
use tauri::Manager;

use crate::state::AppState;

pub use infinity_providers;
pub use tauri;

pub fn init_logging() {
    telemetry::init();
}

type Extension = Box<dyn FnOnce(tauri::Builder<tauri::Wry>) -> tauri::Builder<tauri::Wry>>;

pub struct Builder {
    providers: Providers,
    extensions: Vec<Extension>,
}

impl std::fmt::Debug for Builder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Builder")
            .field("providers", &self.providers)
            .field("extensions", &self.extensions.len())
            .finish()
    }
}

impl Builder {
    pub fn new(providers: Providers) -> Self {
        Self {
            providers,
            extensions: Vec::new(),
        }
    }

    #[must_use]
    pub fn plugin<P>(mut self, plugin: P) -> Self
    where
        P: tauri::plugin::Plugin<tauri::Wry> + 'static,
    {
        self.extensions.push(Box::new(move |b| b.plugin(plugin)));
        self
    }

    /// Where the boot-time network diagnostics probe DNS, TCP and TLS.
    #[must_use]
    pub fn network_probe(self, url: impl Into<String>) -> Self {
        boot::set_probe_url(url.into());
        self
    }

    pub fn run(self, context: tauri::Context<tauri::Wry>) -> ExitCode {
        telemetry::init();
        match self.build(context) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                tracing::error!(error = %e, "app exited with an error");
                ExitCode::FAILURE
            }
        }
    }

    fn build(self, context: tauri::Context<tauri::Wry>) -> tauri::Result<()> {
        let Self {
            providers,
            extensions,
        } = self;
        tracing::info!(edition = ?providers.edition, "starting");

        let mut builder = tauri::Builder::default()
            .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
                // A supervisor relaunch must not pop the window up.
                if !infinity_supervisor::is_remote_start(&argv) {
                    deep_link::focus_main(app);
                }
            }))
            .plugin(tauri_plugin_deep_link::init())
            .plugin(tauri_plugin_opener::init())
            .plugin(tauri_plugin_dialog::init())
            .plugin(tauri_plugin_window_state::Builder::default().build());

        for extend in extensions {
            builder = extend(builder);
        }

        builder
            .manage(AppState::new(providers))
            .invoke_handler(commands::invoke_handler())
            .setup(|app| {
                let app_data = app.path().app_data_dir()?;
                app.manage(PreferencesStore::new(&app_data));
                let registry = Arc::new(InstalledStore::new(&app_data));
                let installer = Installer::new(registry.clone())
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
                forward_jobs(app.handle(), &installer, registry.clone());
                app.manage(registry);
                app.manage(installer);
                app.manage(infinity_configurator::ConfigStore::new(&app_data));
                let bridges = infinity_bridge::BridgeManager::default();
                forward_bridges(app.handle(), &bridges);
                simbrief_gauge::serve(app.handle(), &bridges);
                app.manage(infinity_simbrief::SimBriefClient::simbrief().map_err(|e| {
                    Box::new(infinity_ipc::IpcError::from(e)) as Box<dyn std::error::Error>
                })?);
                app.manage(bridges);
                // Bundled; absent in builds that ship without it.
                let mtr = app
                    .path()
                    .resolve(
                        infinity_nav_db::MTR_RESOURCE,
                        tauri::path::BaseDirectory::Resource,
                    )
                    .ok()
                    .filter(|p| p.is_file());
                app.manage(commands::nav::NavDbs::new(mtr));
                let navigraph = commands::navigraph::NavigraphState::new(
                    app.state::<AppState>().providers.navigraph.as_ref(),
                    &app_data,
                )
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
                app.manage(navigraph);
                app.manage(efb::Efb::new());
                efb::forward_changes(app.handle());
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let prefs = handle.state::<PreferencesStore>().load().await;
                    efb::apply(&handle, &prefs.unwrap_or_default()).await;
                });
                app.manage(watch_sim(app.handle()));
                boot::setup(app.handle());
                deep_link::install(app.handle())?;
                window_geometry::repair(app.handle());
                tray::install(app.handle())?;
                start_sim_services(app.handle());
                Ok(())
            })
            .build(context)?
            .run(|app, event| {
                if let tauri::RunEvent::Exit = event {
                    stop_sim_services(app);
                    let bridges = app
                        .state::<infinity_bridge::BridgeManager>()
                        .inner()
                        .clone();
                    tauri::async_runtime::block_on(bridges.stop_all());
                    let efb = app.state::<efb::Efb>();
                    if let Err(e) = tauri::async_runtime::block_on(efb.server.stop()) {
                        tracing::debug!(error = %e, "EFB server stop");
                    }
                }
            });
        Ok(())
    }
}

fn watch_sim(app: &tauri::AppHandle) -> SimWatch {
    let (watch, poll) = SimWatch::new(infinity_sim_watch::DEFAULT_INTERVAL);
    tauri::async_runtime::spawn(poll);

    let mut changes = watch.subscribe();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        while changes.changed().await.is_ok() {
            let status = *changes.borrow_and_update();
            events::emit::<SimStatusChanged>(&app, &status);
        }
    });
    watch
}

/// Relays job changes to the UI, and the new installed set once a job
/// completes.
fn forward_jobs(app: &tauri::AppHandle, installer: &Installer, registry: Arc<InstalledStore>) {
    let mut changes = installer.subscribe();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let job = match changes.recv().await {
                Ok(job) => job,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::debug!(skipped, "job updates lagged");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            };
            events::emit::<JobChanged>(&app, &job);
            if job.phase == JobPhase::Completed {
                match registry.list().await {
                    Ok(installed) => events::emit::<InstalledChanged>(&app, &installed),
                    Err(e) => tracing::warn!(error = %e, "could not read installed products"),
                }
            }
        }
    });
}

/// Relays gauge events and bridge connection changes to the UI.
fn forward_bridges(app: &tauri::AppHandle, bridges: &infinity_bridge::BridgeManager) {
    use infinity_ipc::dto::BridgeMessage;
    use infinity_ipc::events::{BridgeConnectionChanged, BridgeEventReceived};
    use tokio::sync::broadcast::error::RecvError;

    let mut gauge_events = bridges.subscribe();
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match gauge_events.recv().await {
                // Dotted names are high-rate internal streams the UI never reads.
                Ok(e) if e.channel().is_some() => events::emit::<BridgeEventReceived>(
                    &handle,
                    &BridgeMessage {
                        namespace: e.namespace,
                        name: e.name,
                        data: e.data,
                    },
                ),
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return,
            }
        }
    });
    let mut connections = bridges.subscribe_status();
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match connections.recv().await {
                Ok(change) => events::emit::<BridgeConnectionChanged>(&handle, &change),
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return,
            }
        }
    });
}

fn start_sim_services(app: &tauri::AppHandle) {
    let providers = app.state::<AppState>().providers.clone();
    for service in providers.sim_services {
        let ctx = SimContext {
            identity: providers.identity.clone(),
        };
        tauri::async_runtime::spawn(async move {
            match service.start(ctx).await {
                Ok(()) => tracing::info!(service = service.name(), "sim service started"),
                Err(e) => {
                    tracing::error!(service = service.name(), error = %e, "sim service failed")
                }
            }
        });
    }
}

fn stop_sim_services(app: &tauri::AppHandle) {
    let services = app.state::<AppState>().providers.sim_services.clone();
    tauri::async_runtime::block_on(async move {
        for service in services {
            if let Err(e) = service.stop().await {
                tracing::warn!(service = service.name(), error = %e, "sim service did not stop cleanly");
            }
        }
    });
}
