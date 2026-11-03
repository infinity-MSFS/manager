//! The custom terrain radar for the manager.
//!
//! [`TerrainEngine`] wraps the headless `infinity-terrain-radar` renderer.
//! The renderer owns a wgpu device, so it lives on one dedicated thread;
//! async callers submit jobs over a channel and await the PNG, which also
//! keeps the blocking GPU work off the async runtime.
//!
//! Beside the picture, the same thread answers HEIGHTS: the database's own
//! elevations as a 16-bit PNG, for a client that colours the ground itself
//! (the DC-10 family's EGPWS displays). No GPU: the same database, opened a
//! second time for the sampler.
//!
//! [`download`] fetches the database (`world.itr`) itself.

pub mod download;
mod error;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use infinity_terrain_radar::{
    Dataset, FrameOptions, HeightSampler, HeightsOptions, LatLon, Renderer,
};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

pub use error::{Result, TerrainError};

/// `<app data>/terrain/world.itr`, shared with v3 (same app identifier).
pub fn database_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("terrain").join("world.itr")
}

/// One terrain frame: the live aircraft state the gauge sent up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderRequest {
    pub lat: f32,
    pub lon: f32,
    pub heading: f32,
    pub altitude_ft: f32,
    pub range_nm: f32,
    /// Square output edge in pixels.
    pub size: u32,
}

/// One heights request: a north-up square about `lat`/`lon`, `span_nm` from
/// its centre to each side, `size` pixels a side.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeightsRequest {
    pub lat: f32,
    pub lon: f32,
    pub span_nm: f32,
    pub size: u32,
}

/// Stats about a loaded database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadInfo {
    pub tiles: usize,
    pub resolution: u32,
}

enum Job {
    Load(PathBuf, oneshot::Sender<Result<LoadInfo>>),
    Unload(oneshot::Sender<()>),
    Render(RenderRequest, oneshot::Sender<Result<Vec<u8>>>),
    Heights(HeightsRequest, oneshot::Sender<Result<Vec<u8>>>),
}

/// Cheap-to-clone handle to the engine thread. The thread ends when the
/// last handle is dropped.
#[derive(Debug, Clone)]
pub struct TerrainEngine {
    tx: mpsc::UnboundedSender<Job>,
    loaded: Arc<AtomicBool>,
}

impl TerrainEngine {
    /// Starts the engine thread with no database loaded.
    pub fn spawn() -> Result<Self> {
        let (tx, rx) = mpsc::unbounded_channel::<Job>();
        let loaded = Arc::new(AtomicBool::new(false));
        let flag = loaded.clone();
        std::thread::Builder::new()
            .name("terrain-engine".into())
            .spawn(move || worker(rx, &flag))
            .map_err(|e| TerrainError::Engine(format!("could not start the engine thread: {e}")))?;
        Ok(Self { tx, loaded })
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded.load(Ordering::Relaxed)
    }

    /// Loads (or swaps) the active `.itr` database; the renderer is
    /// recreated.
    pub async fn load(&self, path: PathBuf) -> Result<LoadInfo> {
        self.ask(|reply| Job::Load(path, reply)).await?
    }

    /// Closes the database, e.g. before replacing the file (Windows will not
    /// replace a mapped file).
    pub async fn unload(&self) -> Result<()> {
        self.ask(Job::Unload).await
    }

    /// Swaps `staged` (a finished download) in as the database at `dest`
    /// and loads it. The old database is closed first: Windows will not
    /// replace a file that is mapped.
    pub async fn replace_database(&self, staged: &Path, dest: &Path) -> Result<LoadInfo> {
        use crate::error::IoContext;
        self.unload().await?;
        tokio::fs::rename(staged, dest).await.at(dest)?;
        self.load(dest.to_path_buf()).await
    }

    /// Renders one frame to PNG bytes (flat TAWS, no hillshade).
    pub async fn render(&self, req: RenderRequest) -> Result<Vec<u8>> {
        if !self.is_loaded() {
            return Err(TerrainError::NotLoaded);
        }
        self.ask(|reply| Job::Render(req, reply)).await?
    }

    /// Samples one heights frame to a 16-bit greyscale PNG
    /// (`infinity_terrain_radar::heights` has the code).
    pub async fn heights(&self, req: HeightsRequest) -> Result<Vec<u8>> {
        if !self.is_loaded() {
            return Err(TerrainError::NotLoaded);
        }
        self.ask(|reply| Job::Heights(req, reply)).await?
    }

    async fn ask<T>(&self, job: impl FnOnce(oneshot::Sender<T>) -> Job) -> Result<T> {
        let (reply, answer) = oneshot::channel();
        self.tx
            .send(job(reply))
            .map_err(|_| TerrainError::Engine("the engine thread is gone".into()))?;
        answer
            .await
            .map_err(|_| TerrainError::Engine("the engine dropped the reply".into()))
    }
}

fn worker(mut rx: mpsc::UnboundedReceiver<Job>, loaded: &AtomicBool) {
    let mut renderer: Option<Renderer> = None;
    // The heights sampler's own view of the same database.
    let mut heights: Option<(Dataset, HeightSampler)> = None;

    while let Some(job) = rx.blocking_recv() {
        match job {
            Job::Load(path, reply) => {
                loaded.store(false, Ordering::Relaxed);
                renderer = None;
                heights = None;
                let result = open(&path);
                let answer = match result {
                    Ok((r, h, info)) => {
                        info!(
                            tiles = info.tiles,
                            res = info.resolution,
                            "terrain: database loaded"
                        );
                        renderer = Some(r);
                        heights = Some((h, HeightSampler::new()));
                        loaded.store(true, Ordering::Relaxed);
                        Ok(info)
                    }
                    Err(e) => {
                        warn!(error = %e, "terrain: load failed");
                        Err(e)
                    }
                };
                let _ = reply.send(answer);
            }
            Job::Unload(reply) => {
                loaded.store(false, Ordering::Relaxed);
                renderer = None;
                heights = None;
                let _ = reply.send(());
            }
            Job::Render(req, reply) => {
                let result = match renderer.as_mut() {
                    Some(r) => {
                        let opt = FrameOptions {
                            width: req.size,
                            height: req.size,
                            center: LatLon {
                                lat: req.lat,
                                lon: req.lon,
                            },
                            range_nm: req.range_nm.max(1.0),
                            heading: req.heading,
                            altitude_ft: req.altitude_ft,
                            hillshade: false,
                        };
                        r.render_png(&opt)
                            .map_err(|e| TerrainError::Render(e.to_string()))
                    }
                    None => Err(TerrainError::NotLoaded),
                };
                let _ = reply.send(result);
            }
            Job::Heights(req, reply) => {
                let result = match heights.as_mut() {
                    Some((ds, sampler)) => {
                        let opt = HeightsOptions {
                            size: req.size,
                            center: LatLon {
                                lat: req.lat,
                                lon: req.lon,
                            },
                            range_nm: req.span_nm.max(1.0),
                        };
                        sampler
                            .sample_png(ds, &opt)
                            .map_err(|e| TerrainError::Render(e.to_string()))
                    }
                    None => Err(TerrainError::NotLoaded),
                };
                let _ = reply.send(result);
            }
        }
    }
}

fn open(path: &Path) -> Result<(Renderer, Dataset, LoadInfo)> {
    let bad = |e: &dyn std::fmt::Display| TerrainError::Database {
        path: path.to_path_buf(),
        message: e.to_string(),
    };
    let ds = Dataset::open(path).map_err(|e| bad(&e))?;
    let info = LoadInfo {
        tiles: ds.tile_count(),
        resolution: ds.resolution(),
    };
    let renderer = Renderer::new(ds).map_err(|e| TerrainError::Render(e.to_string()))?;
    let sampler_view = Dataset::open(path).map_err(|e| bad(&e))?;
    Ok((renderer, sampler_view, info))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn requests_before_a_load_are_refused() {
        let engine = TerrainEngine::spawn().expect("engine");
        assert!(!engine.is_loaded());
        let req = RenderRequest {
            lat: 0.0,
            lon: 0.0,
            heading: 0.0,
            altitude_ft: 0.0,
            range_nm: 10.0,
            size: 128,
        };
        assert!(matches!(
            engine.render(req).await,
            Err(TerrainError::NotLoaded)
        ));
    }

    #[tokio::test]
    async fn a_missing_database_fails_to_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = TerrainEngine::spawn().expect("engine");
        let result = engine.load(dir.path().join("absent.itr")).await;
        assert!(matches!(result, Err(TerrainError::Database { .. })));
        assert!(!engine.is_loaded());
        engine.unload().await.expect("unload");
    }
}
