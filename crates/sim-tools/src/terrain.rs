//! The custom terrain radar: the gauge bridge, the database download and
//! the prompt to fetch it.
//!
//! Wire protocol:
//! - Gauge -> manager: bridge event `terrain.request` with the live aircraft
//!   state `{ lat, lon, heading, altitude_ft, range_nm, span_nm?, size?,
//!   seq?, format? }`. The gauge only emits it when its configured radar
//!   provider is `custom`.
//! - Manager -> gauge: a run of `terrain.chunk` commands carrying the frame
//!   as base64 PNG split into bridge-frame-sized pieces:
//!   `{ seq, idx, total, data }`. The gauge reassembles `total` chunks,
//!   decodes the PNG and uploads it to its NanoVG terrain image.
//!
//! Tiles are rendered north-up; the gauge rotates its canvas heading-up and
//! positions the cached tile, so heading changes never need a re-render.
//! A request carrying `format: "elevation"` (the DC-10 family's EGPWS
//! displays) is answered with the database's heights instead of a picture,
//! a 16-bit greyscale PNG, over the same chunks.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use infinity_bridge::{BridgeEvent, BridgeKind, BridgeManager, BridgeSpec};
use infinity_installer::Installer;
use infinity_ipc::Event as _;
use infinity_ipc::dto::{Job, JobPhase, Progress};
use infinity_ipc::events::JobChanged;
use infinity_ipc::{IpcError, IpcResult};
use infinity_persistence::InstalledStore;
use infinity_sim_tools_ipc::terrain::{TERRAIN_JOB_ID, TerrainDownloadArgs, TerrainStatus};
use infinity_terrain::download::{self, Source, Step};
use infinity_terrain::{HeightsRequest, RenderRequest, TerrainEngine};
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager, Wry};
use tokio::sync::broadcast::error::RecvError;
use tracing::{info, warn};

use crate::SimTools;

/// Catalog id of the T-38N expansion, whose ND has the custom terrain radar.
pub(crate) const T38N_PRODUCT_ID: &str = "aerodynamics-t38n";
const JOB_NAME: &str = "T-38N Terrain Radar Data";

/// Worker route (public) and R2 key of the database.
const TERRAIN_PATH: &str = "/v1/terrain/terrain/aerodynamics-t38n/world.itr";

/// Loopback bridge the T-38 terrain relay connects to. Must match the relay
/// in `T-38C-WASM/js/bridge/Main.res` and the catalog `BridgeSpec`.
const PORT: u16 = 9874;
const NAMESPACE: &str = "t38_terrain";
const REQUEST_EVENT: &str = "terrain.request";
const CHUNK_COMMAND: &str = "terrain.chunk";
const FORMAT_ELEVATION: &str = "elevation";

/// Base64 characters per chunk: comfortably inside a CommBus call, so each
/// chunk survives the JS relay to WASM hop.
const CHUNK_CHARS: usize = 6 * 1024;
/// Tile edge when the gauge does not say. Kept small: the ND decodes the
/// PNG on its single frame thread, and a large image trips MSFS's per-frame
/// watchdog. The gauge caches and scales tiles, so 512 is plenty.
const DEFAULT_SIZE: u32 = 512;
/// Per-chunk ack budget. MSFS frames can stall under load.
const CHUNK_TIMEOUT: Duration = Duration::from_secs(5);

/// "Not now" on the prompt, remembered until the T-38N is installed again.
const DISMISSED_MARKER: &str = "prompt-dismissed";

pub(crate) fn default_url(worker_base: &str) -> String {
    format!("{}{TERRAIN_PATH}", worker_base.trim_end_matches('/'))
}

pub(crate) async fn status(state: &SimTools) -> TerrainStatus {
    let path = infinity_terrain::database_path(&state.app_data);
    let size_bytes = tokio::fs::metadata(&path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    let info = *state.terrain_info.lock().await;
    TerrainStatus {
        installed: size_bytes > 0,
        loaded: state.terrain.is_loaded(),
        path: path.display().to_string(),
        size_bytes,
        tiles: info.as_ref().map(|i| i.tiles as u64),
        resolution: info.as_ref().map(|i| i.resolution),
        downloading: state
            .download
            .lock()
            .await
            .as_ref()
            .is_some_and(|h| !h.inner().is_finished()),
        load_error: state.terrain_error.lock().await.clone(),
    }
}

/// Opens the database on disk, if there is one.
pub(crate) async fn load_existing(app: &AppHandle) {
    let state = app.state::<SimTools>();
    let path = infinity_terrain::database_path(&state.app_data);
    if tokio::fs::metadata(&path).await.is_err() {
        return;
    }
    let result = state.terrain.load(path).await;
    record_load(&state, result).await;
}

async fn record_load(
    state: &SimTools,
    result: infinity_terrain::Result<infinity_terrain::LoadInfo>,
) {
    match result {
        Ok(info) => {
            *state.terrain_info.lock().await = Some(info);
            *state.terrain_error.lock().await = None;
        }
        Err(e) => {
            warn!(error = %e, "terrain: database did not load");
            *state.terrain_info.lock().await = None;
            *state.terrain_error.lock().await = Some(e.to_string());
        }
    }
}

// ---------------------------------------------------------------------------
// The download job
// ---------------------------------------------------------------------------

fn job(phase: JobPhase, progress: Option<Progress>) -> Job {
    Job {
        product_id: TERRAIN_JOB_ID.into(),
        name: JOB_NAME.into(),
        version: String::new(),
        channel: Default::default(),
        phase,
        progress,
    }
}

fn emit_job(app: &AppHandle, job: &Job) {
    if let Err(e) = app.emit(JobChanged::NAME, job) {
        warn!(error = %e, "terrain: job event emit failed");
    }
}

/// Starts the download unless one is running.
pub(crate) async fn start_download(app: &AppHandle, args: TerrainDownloadArgs) -> IpcResult<()> {
    let state = app.state::<SimTools>();
    let mut slot = state.download.lock().await;
    if slot.as_ref().is_some_and(|h| !h.inner().is_finished()) {
        return Ok(());
    }
    let url = args
        .url
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| default_url(&state.worker_base));
    let source = Source::parse(&url, args.sha256).map_err(IpcError::from)?;
    emit_job(app, &job(JobPhase::Queued, None));
    let handle = app.clone();
    *slot = Some(tauri::async_runtime::spawn(async move {
        let phase = match run_download(&handle, &source).await {
            Ok(()) => JobPhase::Completed,
            Err(error) => JobPhase::Failed { error },
        };
        emit_job(&handle, &job(phase, None));
    }));
    Ok(())
}

async fn run_download(app: &AppHandle, source: &Source) -> IpcResult<()> {
    let state = app.state::<SimTools>();
    let dest = infinity_terrain::database_path(&state.app_data);
    info!(url = %source.url, "terrain: downloading the database");
    let staged = download::fetch(&state.http, source, &dest, &mut |step| {
        let (phase, progress) = match step {
            Step::Downloading { done, total } => {
                (JobPhase::Downloading, Some(Progress { done, total }))
            }
            Step::Verifying => (JobPhase::Verifying, None),
        };
        emit_job(app, &job(phase, progress));
    })
    .await?;
    emit_job(app, &job(JobPhase::Installing, None));
    let result = state.terrain.replace_database(&staged, &dest).await;
    let failed = result.as_ref().err().map(ToString::to_string);
    record_load(&state, result).await;
    if let Some(e) = failed {
        return Err(IpcError::new(infinity_ipc::IpcErrorCode::IoError, e));
    }
    let _ = tokio::fs::remove_file(dismissed_marker(&state.app_data)).await;
    info!("terrain: database downloaded and loaded");
    Ok(())
}

/// Stops the download and deletes the partial file.
pub(crate) async fn cancel_download(app: &AppHandle) {
    let state = app.state::<SimTools>();
    let running = state.download.lock().await.take();
    if let Some(h) = running {
        h.abort();
        let _ = h.await;
        emit_job(app, &job(JobPhase::Cancelled, None));
    }
    download::discard(&infinity_terrain::database_path(&state.app_data)).await;
}

// ---------------------------------------------------------------------------
// The prompt
// ---------------------------------------------------------------------------

fn dismissed_marker(app_data: &Path) -> std::path::PathBuf {
    app_data.join("terrain").join(DISMISSED_MARKER)
}

async fn database_present(app_data: &Path) -> bool {
    tokio::fs::metadata(infinity_terrain::database_path(app_data))
        .await
        .is_ok_and(|m| m.len() > 0)
}

/// Asks to download the database when the T-38N is installed without it:
/// on start (unless the user said "not now") and whenever a T-38N install
/// completes.
pub(crate) fn watch_for_t38n(app: &AppHandle) {
    let (Some(registry), Some(installer)) = (
        app.try_state::<Arc<InstalledStore>>(),
        app.try_state::<Installer>(),
    ) else {
        warn!("terrain: no installer in this build; not prompting for the database");
        return;
    };
    let registry = registry.inner().clone();
    let mut jobs = installer.subscribe();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let app_data = app.state::<SimTools>().app_data.clone();
        let installed = matches!(registry.get(T38N_PRODUCT_ID).await, Ok(Some(_)));
        let dismissed = tokio::fs::metadata(dismissed_marker(&app_data))
            .await
            .is_ok();
        if installed && !dismissed && !database_present(&app_data).await {
            prompt(&app);
        }
        loop {
            match jobs.recv().await {
                Ok(job)
                    if job.product_id == T38N_PRODUCT_ID && job.phase == JobPhase::Completed =>
                {
                    if !database_present(&app_data).await {
                        prompt(&app);
                    }
                }
                Ok(_) => {}
                Err(RecvError::Lagged(n)) => warn!(dropped = n, "terrain: job watch lagged"),
                Err(RecvError::Closed) => break,
            }
        }
    });
}

fn prompt(app: &AppHandle) {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    if app
        .state::<SimTools>()
        .prompting
        .swap(true, std::sync::atomic::Ordering::AcqRel)
    {
        return;
    }
    info!("terrain: T-38N installed without the terrain database; asking");
    let handle = app.clone();
    app.dialog()
        .message(
            "The T-38N's custom ground-map radar needs the terrain database. \
             Download it now? Progress shows under Settings, Terrain radar.",
        )
        .title("Terrain radar data")
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Download".into(),
            "Not now".into(),
        ))
        .show(move |download| {
            handle
                .state::<SimTools>()
                .prompting
                .store(false, std::sync::atomic::Ordering::Release);
            let app = handle.clone();
            tauri::async_runtime::spawn(async move {
                if download {
                    if let Err(e) = start_download(&app, TerrainDownloadArgs::default()).await {
                        warn!(error = %e, "terrain: could not start the download");
                    }
                } else {
                    let marker = dismissed_marker(&app.state::<SimTools>().app_data);
                    if let Some(dir) = marker.parent() {
                        let _ = tokio::fs::create_dir_all(dir).await;
                    }
                    let _ = tokio::fs::write(marker, b"").await;
                }
            });
        });
}

// ---------------------------------------------------------------------------
// The gauge bridge
// ---------------------------------------------------------------------------

pub(crate) fn serve(app: &AppHandle<Wry>, bridges: &BridgeManager) {
    let engine = app.state::<SimTools>().terrain.clone();
    let spec = BridgeSpec {
        id: "terrain".into(),
        kind: BridgeKind::TauriWsBridge,
        port: PORT,
        namespace: NAMESPACE.into(),
    };
    // Bound here as well as by the catalog, so the relay connects even
    // against a catalog that does not declare it. Idempotent.
    let mgr = bridges.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = mgr.ensure_started(spec).await {
            warn!(error = %e, port = PORT, "terrain bridge did not start");
        }
    });

    let mut rx = bridges.subscribe();
    let mgr = bridges.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(evt) if evt.name == REQUEST_EVENT => handle_request(&engine, &mgr, evt).await,
                Ok(_) => {}
                Err(RecvError::Lagged(n)) => warn!(dropped = n, "terrain handler lagged"),
                Err(RecvError::Closed) => break,
            }
        }
    });
}

/// What a `terrain.request` asks for.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Ask {
    Picture(RenderRequest),
    Heights(HeightsRequest),
}

fn parse_request(d: &Value) -> (u64, Ask) {
    let f = |k: &str| d.get(k).and_then(Value::as_f64).unwrap_or(0.0) as f32;
    let seq = d.get("seq").and_then(Value::as_u64).unwrap_or(0);
    let size = d
        .get("size")
        .and_then(Value::as_u64)
        .map_or(DEFAULT_SIZE, |s| s.clamp(128, 2048) as u32);
    // `span_nm` is the tile's half-extent (falls back to `range_nm`).
    let span = match f("span_nm") {
        s if s > 0.0 => s,
        _ => f("range_nm"),
    };
    let ask = if d.get("format").and_then(Value::as_str) == Some(FORMAT_ELEVATION) {
        Ask::Heights(HeightsRequest {
            lat: f("lat"),
            lon: f("lon"),
            span_nm: span,
            size,
        })
    } else {
        Ask::Picture(RenderRequest {
            lat: f("lat"),
            lon: f("lon"),
            heading: 0.0,
            altitude_ft: f("altitude_ft"),
            range_nm: span,
            size,
        })
    };
    (seq, ask)
}

/// Base64 `png` split into `terrain.chunk` payloads.
fn chunk_payloads(seq: u64, png: &[u8]) -> Vec<Value> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    let total = b64.len().div_ceil(CHUNK_CHARS);
    b64.as_bytes()
        .chunks(CHUNK_CHARS)
        .enumerate()
        .map(|(idx, chunk)| {
            json!({ "seq": seq, "idx": idx, "total": total, "data": String::from_utf8_lossy(chunk) })
        })
        .collect()
}

async fn handle_request(engine: &TerrainEngine, mgr: &BridgeManager, evt: BridgeEvent) {
    if !engine.is_loaded() {
        warn!(namespace = %evt.namespace, "terrain request but no database loaded");
        return;
    }
    let (seq, ask) = parse_request(&evt.data);
    let answer = match ask {
        Ask::Heights(req) => engine.heights(req).await,
        Ask::Picture(req) => engine.render(req).await,
    };
    let png = match answer {
        Ok(p) => p,
        Err(e) => {
            warn!(namespace = %evt.namespace, error = %e, "terrain render failed");
            return;
        }
    };
    let payloads = chunk_payloads(seq, &png);
    tracing::debug!(
        namespace = %evt.namespace,
        seq,
        png_bytes = png.len(),
        chunks = payloads.len(),
        "terrain: streaming tile"
    );
    // Each chunk is an independent acked command: send them together, so
    // delivery costs one round trip rather than the sum of all of them.
    let sends = payloads.into_iter().map(|payload| {
        let mgr = mgr.clone();
        let ns = evt.namespace.clone();
        async move {
            mgr.command(&ns, CHUNK_COMMAND, payload, CHUNK_TIMEOUT)
                .await
        }
    });
    let failed = futures_util::future::join_all(sends)
        .await
        .iter()
        .filter(|r| r.is_err())
        .count();
    if failed > 0 {
        warn!(namespace = %evt.namespace, seq, failed, "terrain: some chunks failed to deliver");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_request_is_north_up_with_defaults() {
        let (seq, ask) = parse_request(&json!({
            "lat": 40.5, "lon": -90.0, "heading": 270, "altitude_ft": 5000,
            "range_nm": 20, "seq": 9
        }));
        assert_eq!(seq, 9);
        assert_eq!(
            ask,
            Ask::Picture(RenderRequest {
                lat: 40.5,
                lon: -90.0,
                heading: 0.0,
                altitude_ft: 5000.0,
                range_nm: 20.0,
                size: DEFAULT_SIZE,
            })
        );
    }

    #[test]
    fn an_elevation_request_asks_for_heights() {
        let (_, ask) = parse_request(&json!({
            "lat": 1, "lon": 2, "span_nm": 7.5, "range_nm": 40, "size": 9999,
            "format": "elevation"
        }));
        assert_eq!(
            ask,
            Ask::Heights(HeightsRequest {
                lat: 1.0,
                lon: 2.0,
                span_nm: 7.5,
                size: 2048,
            })
        );
    }

    #[test]
    fn chunks_reassemble() {
        let png: Vec<u8> = (0..20_000u32).map(|i| (i % 256) as u8).collect();
        let chunks = chunk_payloads(3, &png);
        assert!(chunks.len() > 1);
        let total = chunks[0]["total"].as_u64().unwrap() as usize;
        assert_eq!(total, chunks.len());
        let joined: String = chunks
            .iter()
            .map(|c| c["data"].as_str().unwrap().to_owned())
            .collect();
        let back = base64::engine::general_purpose::STANDARD
            .decode(joined)
            .unwrap();
        assert_eq!(back, png);
        assert!(chunks.iter().all(|c| c["seq"] == 3));
    }

    #[test]
    fn default_url_joins_the_worker() {
        assert_eq!(
            default_url("https://manager.example.com/"),
            "https://manager.example.com/v1/terrain/terrain/aerodynamics-t38n/world.itr"
        );
    }
}
