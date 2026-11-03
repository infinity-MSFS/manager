//! The `infinity-sim-tools` Tauri plugin: the custom terrain radar, the
//! MD-10 HUD's FLIR (nose camera) and the developer Perf page.
//!
//! Each talks to its gauges over a loopback bridge on the app's
//! [`BridgeManager`], which the community app creates in its own setup.
//! Plugins are set up before that, so the bridges are wired on
//! [`tauri::RunEvent::Ready`].

mod flir;
mod perf;
mod terrain;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use infinity_bridge::{BridgeKind, BridgeManager, BridgeSpec};
use infinity_flir_engine::FlirEngine;
use infinity_ipc::{Command, IpcError, IpcResult};
use infinity_sim_tools_ipc as contract;
use infinity_terrain::{LoadInfo, TerrainEngine};
use tauri::async_runtime::JoinHandle;
use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Manager, RunEvent, State, Wry};
use tokio::sync::broadcast::error::RecvError;

type Out<C> = IpcResult<<C as Command>::Output>;
type Args<C> = <C as Command>::Args;

const DEFAULT_WORKER_BASE_URL: &str = "https://manager.infinity-simulations.com";

#[derive(Debug, Clone)]
pub struct Config {
    /// Base of the Worker the terrain database is served from.
    pub worker_base_url: String,
}

impl Default for Config {
    /// The production Worker; `INFINITY_WORKER_BASE_URL` overrides it, as
    /// as the catalog URLs are overridden.
    fn default() -> Self {
        Self {
            worker_base_url: std::env::var("INFINITY_WORKER_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_WORKER_BASE_URL.to_owned()),
        }
    }
}

pub fn init(config: Config) -> TauriPlugin<Wry> {
    tauri::plugin::Builder::new(contract::PLUGIN)
        .invoke_handler(invoke_handler())
        .setup(move |app, _api| {
            let app_data = app.path().app_data_dir()?;
            let http = reqwest::Client::builder()
                .user_agent(concat!("infinity-manager/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(15))
                .read_timeout(Duration::from_secs(60))
                .build()?;
            app.manage(SimTools {
                terrain: TerrainEngine::spawn()?,
                flir: FlirEngine::spawn(app_data.clone()),
                app_data,
                worker_base: config.worker_base_url,
                http,
                terrain_info: Default::default(),
                terrain_error: Default::default(),
                download: Default::default(),
                prompting: AtomicBool::new(false),
                perf: Arc::default(),
                started: AtomicBool::new(false),
            });
            Ok(())
        })
        .on_event(|app, event| {
            if let RunEvent::Ready = event {
                start(app);
            }
        })
        .build()
}

pub(crate) struct SimTools {
    app_data: PathBuf,
    worker_base: String,
    http: reqwest::Client,
    terrain: TerrainEngine,
    terrain_info: tokio::sync::Mutex<Option<LoadInfo>>,
    terrain_error: tokio::sync::Mutex<Option<String>>,
    download: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    /// A download prompt is on screen.
    prompting: AtomicBool,
    flir: FlirEngine,
    perf: Arc<Mutex<perf::Perf>>,
    started: AtomicBool,
}

impl SimTools {
    fn perf(&self) -> MutexGuard<'_, perf::Perf> {
        self.perf
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Wires the bridges once the app (and its bridge manager) is up.
fn start(app: &AppHandle) {
    let state = app.state::<SimTools>();
    if state.started.swap(true, Ordering::AcqRel) {
        return;
    }
    let Some(bridges) = app.try_state::<BridgeManager>() else {
        tracing::warn!("sim-tools: the app manages no bridge manager; gauges cannot connect");
        return;
    };
    let bridges = bridges.inner().clone();
    terrain::serve(app, &bridges);
    flir::serve(&state.flir, &bridges);
    serve_perf(&state.perf, &bridges);
    let handle = app.clone();
    tauri::async_runtime::spawn(async move { terrain::load_existing(&handle).await });
    terrain::watch_for_t38n(app);
}

/// Binds the perf bridge and keeps every sample.
fn serve_perf(store: &Arc<Mutex<perf::Perf>>, bridges: &BridgeManager) {
    let spec = BridgeSpec {
        id: "perf".into(),
        kind: BridgeKind::TauriWsBridge,
        port: perf::PORT,
        namespace: perf::NAMESPACE.into(),
    };
    let mgr = bridges.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = mgr.ensure_started(spec).await {
            tracing::warn!(error = %e, port = perf::PORT, "perf bridge did not start");
        }
    });
    let store = Arc::clone(store);
    let mut rx = bridges.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(evt) if evt.namespace == perf::NAMESPACE && evt.name == perf::SAMPLE_EVENT => {
                    store
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .ingest(evt.data, Instant::now());
                }
                Ok(_) => {}
                Err(RecvError::Lagged(n)) => tracing::warn!(dropped = n, "perf: samples lagged"),
                Err(RecvError::Closed) => break,
            }
        }
    });
}

/// Generates the handler and the list of names it registers from one list.
macro_rules! handlers {
    ($($name:ident),* $(,)?) => {
        #[cfg(test)]
        const REGISTERED: &[&str] = &[$(stringify!($name)),*];

        fn invoke_handler() -> impl Fn(tauri::ipc::Invoke<Wry>) -> bool + Send + Sync + 'static {
            tauri::generate_handler![$($name),*]
        }
    };
}

handlers![
    terrain_status,
    terrain_download,
    terrain_cancel,
    flir_diag,
    flir_settings_get,
    flir_settings_default,
    flir_settings_set,
    flir_unload,
    flir_preview,
    perf_overview,
    perf_view,
    perf_snapshot,
    perf_set_paused,
    perf_reset,
];

fn join_error(e: impl std::fmt::Display) -> IpcError {
    IpcError::internal(format!("background task: {e}"))
}

#[tauri::command]
async fn terrain_status(state: State<'_, SimTools>) -> Out<contract::TerrainStatusGet> {
    Ok(terrain::status(&state).await)
}

#[tauri::command]
async fn terrain_download(
    app: AppHandle,
    args: Args<contract::TerrainDownload>,
) -> Out<contract::TerrainDownload> {
    terrain::start_download(&app, args).await
}

#[tauri::command]
async fn terrain_cancel(app: AppHandle) -> Out<contract::TerrainCancel> {
    terrain::cancel_download(&app).await;
    Ok(())
}

#[tauri::command]
async fn flir_diag(state: State<'_, SimTools>) -> Out<contract::FlirDiagGet> {
    Ok(state.flir.diag())
}

#[tauri::command]
async fn flir_settings_get(state: State<'_, SimTools>) -> Out<contract::FlirSettingsGet> {
    Ok(state.flir.settings())
}

#[tauri::command]
async fn flir_settings_default() -> Out<contract::FlirSettingsDefault> {
    Ok(infinity_flir_engine::settings::defaults())
}

#[tauri::command]
async fn flir_settings_set(
    state: State<'_, SimTools>,
    args: Args<contract::FlirSettingsSet>,
) -> Out<contract::FlirSettingsSet> {
    let engine = state.flir.clone();
    tauri::async_runtime::spawn_blocking(move || engine.set_settings(args.settings, args.persist))
        .await
        .map_err(join_error)?
        .map_err(IpcError::from)
}

#[tauri::command]
async fn flir_unload(state: State<'_, SimTools>) -> Out<contract::FlirUnload> {
    state.flir.unload();
    Ok(())
}

#[tauri::command]
async fn flir_preview(
    state: State<'_, SimTools>,
    args: Args<contract::FlirPreviewGet>,
) -> Out<contract::FlirPreviewGet> {
    let engine = state.flir.clone();
    tauri::async_runtime::spawn_blocking(move || engine.preview_png(args.tinted, args.after_seq))
        .await
        .map_err(join_error)?
        .map_err(IpcError::from)
}

#[tauri::command]
async fn perf_overview(state: State<'_, SimTools>) -> Out<contract::PerfOverviewGet> {
    Ok(state.perf().overview(Instant::now()))
}

#[tauri::command]
async fn perf_view(
    state: State<'_, SimTools>,
    args: Args<contract::PerfViewGet>,
) -> Out<contract::PerfViewGet> {
    Ok(state.perf().view(&args, Instant::now()))
}

#[tauri::command]
async fn perf_snapshot(
    state: State<'_, SimTools>,
    args: Args<contract::PerfSnapshot>,
) -> Out<contract::PerfSnapshot> {
    Ok(state.perf().snapshot(&args))
}

#[tauri::command]
async fn perf_set_paused(
    state: State<'_, SimTools>,
    args: Args<contract::PerfSetPaused>,
) -> Out<contract::PerfSetPaused> {
    state.perf().set_paused(args);
    Ok(())
}

#[tauri::command]
async fn perf_reset(state: State<'_, SimTools>) -> Out<contract::PerfReset> {
    state.perf().reset();
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn handlers_match_the_contract() {
        let mut registered = super::REGISTERED.to_vec();
        let mut contract = super::contract::ALL.to_vec();
        registered.sort_unstable();
        contract.sort_unstable();
        assert_eq!(registered, contract);
    }
}
