mod commands;
mod deep_link;
mod events;
mod state;
mod telemetry;

use std::process::ExitCode;

use infinity_ipc::events::SimStatusChanged;
use infinity_persistence::PreferencesStore;
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
            .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
                deep_link::focus_main(app);
            }))
            .plugin(tauri_plugin_deep_link::init())
            .plugin(tauri_plugin_opener::init())
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
                app.manage(watch_sim(app.handle()));
                deep_link::install(app.handle())?;
                start_sim_services(app.handle());
                Ok(())
            })
            .build(context)?
            .run(|app, event| {
                if let tauri::RunEvent::Exit = event {
                    stop_sim_services(app);
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
