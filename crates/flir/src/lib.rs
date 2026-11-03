//! The MD-10 HUD's FLIR picture for the manager.
//!
//! ```text
//! MSFS <-inject- infinity-viewports (bridge + core DLLs): an extra NOSE camera
//!   |            on the airframe, streamed back as depth + colour
//!   v
//! engine thread: pose feed -> camera; frame -> flir-pipeline (scene + sensor)
//!   -> centre square (the HUD's field) -> gray `size x size` -> latest frame
//!   v
//! the sim-tools plugin: flir-stream chunks -> bridge -> Coherent relay -> HUD
//! ```
//!
//! The picture comes from a dedicated camera fixed to the airframe, looking
//! down the HUD's boresight with the HUD's field of view. The HUD is
//! collimated, so that is the picture behind the combiner wherever the crew
//! look: no camera tracking, no projection model.
//!
//! The engine is demand-driven ([`demand`]): it attaches, injects and
//! develops only while a HUD gauge asks, and takes the DLLs back out of the
//! sim a while after the last gauge says goodbye (the flight ending), when
//! the gauges go quiet, or when the FLIR has gone unused for a while. A
//! restarted flight is not unloaded: the core recreates the nose view the
//! reload took, and the HUD picks the stream back up.
//!
//! Injection is by the sim's actual state, never by whether a channel opens
//! (a named mapping outlives the DLL that made it): a module already loaded
//! is reused only if it is the exact build this manager would inject, and
//! is otherwise swapped. What is injected is a private copy ([`shadow`]), so
//! neither the installed DLLs nor a developer's build output is ever locked
//! by the sim; a rebuild on disk is picked up by swapping the copies.
//!
//! Unloading is ordered and confirmed: stream stopped, core asked to erase
//! its view and unload, its module seen gone, then the bridge, likewise.
//!
//! The engine thread ends when the last [`FlirEngine`] handle is dropped.

pub mod demand;
pub mod dlls;
pub mod env;
mod error;
pub mod settings;
pub mod shadow;

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use demand::{Demand, Release, nose_patience};
use flir_stream::{Frame, StateReport};
use infinity_sim_tools_ipc::flir::{FlirDiag, FlirPreview, FlirSettings, FlirState};
use tokio::sync::watch;
use tracing::info;

pub use error::{FlirError, Result};

pub use demand::REQUEST_TTL;
/// Smallest frame the engine makes.
pub const MIN_SIZE: u32 = 64;
/// The gauge's ink, for tinting the preview like the HUD draws it.
pub const INK: [u8; 3] = [70, 255, 110];

#[derive(Default)]
struct Control {
    demand: Demand,
    unload: bool,
    shutdown: bool,
    /// Completed unloads, for [`FlirEngine::unload_and_wait`].
    unloads: u64,
}

struct Shared {
    control: Mutex<Control>,
    wake: Condvar,
    /// Signalled after every unload.
    unloaded: Condvar,
    settings: Mutex<FlirSettings>,
    diag: Mutex<FlirDiag>,
    preview: Mutex<Option<Arc<Frame>>>,
    app_data_dir: PathBuf,
}

impl Shared {
    fn stopping(&self) -> bool {
        lock(&self.control).shutdown
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Stops the engine thread when the last handle goes.
struct Owner(Arc<Shared>);

impl Drop for Owner {
    fn drop(&mut self) {
        lock(&self.0.control).shutdown = true;
        self.0.wake.notify_all();
    }
}

/// Cheap to clone.
#[derive(Clone)]
pub struct FlirEngine {
    shared: Arc<Shared>,
    _owner: Arc<Owner>,
    frames: watch::Receiver<Option<Arc<Frame>>>,
}

impl std::fmt::Debug for FlirEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlirEngine")
            .field("app_data_dir", &self.shared.app_data_dir)
            .finish_non_exhaustive()
    }
}

impl FlirEngine {
    /// Starts the engine thread. Settings come from `<app_data_dir>/flir/`
    /// (read here, so call it off the async runtime or accept a small read).
    pub fn spawn(app_data_dir: PathBuf) -> FlirEngine {
        let settings = settings::load(&app_data_dir);
        let shared = Arc::new(Shared {
            control: Mutex::new(Control::default()),
            wake: Condvar::new(),
            unloaded: Condvar::new(),
            settings: Mutex::new(settings),
            diag: Mutex::new(FlirDiag {
                camera: "-".into(),
                ..FlirDiag::default()
            }),
            preview: Mutex::new(None),
            app_data_dir,
        });
        let (tx, rx) = watch::channel(None);
        let s = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("flir-engine".into())
            .spawn(move || worker::run(&s, &tx));
        if let Err(e) = spawned {
            let mut d = lock(&shared.diag);
            d.state = FlirState::Error;
            d.error = Some(format!("engine thread failed to start: {e}"));
        }
        let owner = Arc::new(Owner(Arc::clone(&shared)));
        FlirEngine {
            shared,
            _owner: owner,
            frames: rx,
        }
    }

    /// A gauge's state report (about 15 Hz while it is alive), wanting a
    /// picture or not.
    pub fn report(&self, report: StateReport) {
        lock(&self.shared.control)
            .demand
            .report(report, Instant::now());
        self.shared.wake.notify_all();
    }

    /// A gauge is being killed. The last one leaving unloads the DLLs.
    pub fn goodbye(&self, source: &str) {
        lock(&self.shared.control)
            .demand
            .goodbye(source, Instant::now());
        self.shared.wake.notify_all();
    }

    /// Stops, then takes the nose view and the bridge out of the sim.
    pub fn unload(&self) {
        lock(&self.shared.control).unload = true;
        self.shared.wake.notify_all();
    }

    /// [`unload`](Self::unload), waiting up to `timeout` for it to finish.
    /// For the manager exiting: true if the DLLs are out (or never were).
    pub fn unload_and_wait(&self, timeout: Duration) -> bool {
        let mut c = lock(&self.shared.control);
        let before = c.unloads;
        c.unload = true;
        self.shared.wake.notify_all();
        let deadline = Instant::now() + timeout;
        while c.unloads == before {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || c.shutdown {
                return false;
            }
            c = self
                .shared
                .unloaded
                .wait_timeout(c, left)
                .map_or_else(|e| e.into_inner().0, |(g, _)| g);
        }
        true
    }

    /// The latest developed frame (latest wins).
    pub fn frames(&self) -> watch::Receiver<Option<Arc<Frame>>> {
        self.frames.clone()
    }

    pub fn preview(&self) -> Option<Arc<Frame>> {
        lock(&self.shared.preview).clone()
    }

    /// The latest frame as a PNG data URL, unless it is `after_seq`.
    /// Blocking (PNG encoding).
    pub fn preview_png(&self, tinted: bool, after_seq: Option<u64>) -> Result<Option<FlirPreview>> {
        let Some(frame) = self.preview().filter(|f| Some(f.seq) != after_seq) else {
            return Ok(None);
        };
        let png = encode_png(&frame, tinted)?;
        Ok(Some(FlirPreview {
            seq: frame.seq,
            width: frame.width,
            height: frame.height,
            data_url: format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(png)
            ),
        }))
    }

    pub fn diag(&self) -> FlirDiag {
        lock(&self.shared.diag).clone()
    }

    pub fn settings(&self) -> FlirSettings {
        lock(&self.shared.settings).clone()
    }

    /// Applies new settings from the next frame; `persist` also saves them
    /// (blocking).
    pub fn set_settings(&self, settings: FlirSettings, persist: bool) -> Result<()> {
        let settings = settings::sanitized(settings);
        if persist {
            settings::save(&settings, &self.shared.app_data_dir)?;
        }
        *lock(&self.shared.settings) = settings;
        Ok(())
    }

    /// The sender's delivery report for one frame.
    pub fn note_sent(&self, ok: bool, wire_bytes: u64) {
        let mut d = lock(&self.shared.diag);
        if ok {
            d.frames_sent += 1;
        } else {
            d.frames_failed += 1;
        }
        d.wire_bytes = wire_bytes;
    }
}

/// 8-bit gray, or tinted RGB, PNG of a frame.
fn encode_png(frame: &Frame, tinted: bool) -> Result<Vec<u8>> {
    let bad = |e: &dyn std::fmt::Display| FlirError::Encode(e.to_string());
    let pixels = (frame.width as usize) * (frame.height as usize);
    if frame.data.len() < pixels {
        return Err(FlirError::Encode(format!(
            "frame {} is {} bytes, short of {}x{}",
            frame.seq,
            frame.data.len(),
            frame.width,
            frame.height
        )));
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, frame.width, frame.height);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let data: Vec<u8> = if tinted {
            encoder.set_color(png::ColorType::Rgb);
            frame.data[..pixels]
                .iter()
                .flat_map(|&v| INK.map(|ink| ((u16::from(v) * u16::from(ink)) / 255) as u8))
                .collect()
        } else {
            encoder.set_color(png::ColorType::Grayscale);
            frame.data[..pixels].to_vec()
        };
        let mut writer = encoder.write_header().map_err(|e| bad(&e))?;
        writer.write_image_data(&data).map_err(|e| bad(&e))?;
        writer.finish().map_err(|e| bad(&e))?;
    }
    Ok(out)
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Frame sequence numbers count from the wall clock (ms), so a restarted
/// manager never sends a `seq` below what the gauge last completed.
fn first_seq() -> u64 {
    (unix_now() * 1000.0) as u64
}

#[cfg(not(windows))]
mod worker {
    use super::*;

    pub(super) fn run(shared: &Shared, _tx: &watch::Sender<Option<Arc<Frame>>>) {
        lock(&shared.diag).state = FlirState::Unsupported;
        info!("flir: the nose camera needs Windows; engine idle");
    }
}

#[cfg(windows)]
mod worker {
    use std::path::Path;

    use super::*;

    use flir_pipeline::decode::{self, ROLE_COLOR, ROLE_DEPTH};
    use flir_pipeline::{CloudParams, Pipeline};
    use infinity_viewports_host::camera::CameraLink;
    use infinity_viewports_host::geo::aircraft_pose;
    use infinity_viewports_host::simvars::AircraftFeed;
    use infinity_viewports_host::video::VideoReader;
    use infinity_viewports_host::{Host, HostConfig, Sim, SimTarget, locate};
    use infinity_viewports_protocol::{
        APPLIED_FOLLOW, APPLIED_MISSING, APPLIED_PARENTED, APPLIED_PLACED, AircraftPose,
        CameraControl, MOUNT_AIRCRAFT, NOSE_CREATING, NOSE_LIVE, NOSE_REACQUIRING, NOSE_STOPPED,
    };
    use tracing::warn;

    /// The nose colour target's format and occurrence (R11G11B10, first).
    const COLOR_FORMAT: u32 = decode::DXGI_R11G11B10_FLOAT;
    const ATTACH_RETRY: Duration = Duration::from_secs(3);
    /// A bridge heartbeat that stops this long means the sim went away.
    const HEARTBEAT_LOST: Duration = Duration::from_secs(4);
    const TICK: Duration = Duration::from_millis(8);
    const IDLE_POLL: Duration = Duration::from_millis(500);
    const FEET_TO_M: f64 = 0.3048;

    /// How long to wait for a torn-down DLL to leave the sim.
    const UNLOAD_WAIT: Duration = Duration::from_secs(4);
    /// How often a running session checks the DLLs on disk for a rebuild.
    const REBUILD_CHECK: Duration = Duration::from_secs(2);

    struct Session {
        sim: Sim,
        camera: Option<CameraLink>,
        reader: Option<VideoReader>,
        streaming: bool,
        heartbeat: u64,
        heartbeat_at: Instant,
        /// When the DLLs went in (the demand clock starts here).
        since: Instant,
        /// The DLLs the copies were made from, as they were then.
        sources: Vec<(PathBuf, Option<shadow::Stamp>)>,
        next_rebuild_check: Instant,
        /// Since when the core has reported its nose view not live.
        nose_down_since: Option<Instant>,
    }

    struct Worker<'a> {
        shared: &'a Shared,
        tx: &'a watch::Sender<Option<Arc<Frame>>>,
        session: Option<Session>,
        feed: AircraftFeed,
        pipeline: Pipeline,
        next_attach: Instant,
        next_frame: Instant,
        seq: u64,
        fps_window: (Instant, u32),
        /// Re-injections for a lost nose view that did not bring it back.
        nose_resets: u32,
    }

    pub(super) fn run(shared: &Shared, tx: &watch::Sender<Option<Arc<Frame>>>) {
        let mut w = Worker {
            shared,
            tx,
            session: None,
            feed: AircraftFeed::new(),
            pipeline: Pipeline::default(),
            next_attach: Instant::now(),
            next_frame: Instant::now(),
            seq: first_seq(),
            fps_window: (Instant::now(), 0),
            nose_resets: 0,
        };
        while !shared.stopping() {
            w.step();
        }
        // Dropped without an unload (the manager going down without its
        // exit hook): at least stop the stream.
        w.stop_stream();
        info!("flir: engine stopped");
    }

    /// Waits up to [`UNLOAD_WAIT`] for `name` to leave `pid`. True once it
    /// has (or the sim is gone).
    fn wait_gone(pid: u32, name: &str) -> bool {
        let deadline = Instant::now() + UNLOAD_WAIT;
        loop {
            match locate::loaded_module(pid, name) {
                Ok(None) | Err(_) => return true,
                Ok(Some(_)) if Instant::now() >= deadline => return false,
                Ok(Some(_)) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }

    /// Whether two paths name the same file (the loader reports its own
    /// spelling of a path).
    fn same_file(a: &Path, b: &Path) -> bool {
        match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }

    impl Worker<'_> {
        fn set_state(&self, state: FlirState, error: Option<String>) {
            let mut d = lock(&self.shared.diag);
            d.state = state;
            d.error = error;
        }

        fn step(&mut self) {
            let now = Instant::now();
            let (request, release, sources) = {
                let mut c = lock(&self.shared.control);
                let asked = std::mem::take(&mut c.unload);
                let since = self.session.as_ref().map_or(now, |s| s.since);
                // Evaluated even with nothing injected, so a farewell is
                // consumed rather than left to fire after the next inject.
                let due = c.demand.release_due(since, now);
                let release = if asked {
                    Some(Release::Asked)
                } else {
                    due.filter(|_| self.session.is_some())
                };
                (c.demand.request(now), release, c.demand.sources(now))
            };
            {
                let mut d = lock(&self.shared.diag);
                d.requested = request.is_some();
                d.sources = sources;
            }
            if let Some(reason) = release {
                self.release(reason);
                lock(&self.shared.control).unloads += 1;
                self.shared.unloaded.notify_all();
                return;
            }
            let Some(report) = request else {
                self.stop_stream();
                // Nothing to develop: sleep until a report, an unload or the
                // next look at the release clock.
                let c = lock(&self.shared.control);
                let _ = self
                    .shared
                    .wake
                    .wait_timeout_while(c, IDLE_POLL, |c| !c.unload && !c.shutdown);
                return;
            };
            if self.session.is_none() && !self.attach() {
                std::thread::sleep(Duration::from_millis(200));
                return;
            }
            if self.rebuilt() {
                self.release(Release::Rebuilt);
                return;
            }
            self.tick(&report);
            std::thread::sleep(TICK);
        }

        /// The DLLs the running copies came from changed on disk.
        fn rebuilt(&mut self) -> bool {
            let Some(s) = self.session.as_mut() else {
                return false;
            };
            if Instant::now() < s.next_rebuild_check {
                return false;
            }
            s.next_rebuild_check = Instant::now() + REBUILD_CHECK;
            s.sources
                .iter()
                .any(|(path, was)| shadow::stamp(path) != *was)
        }

        /// The sim's pid, if it is running.
        fn sim_pid(&self) -> Option<u32> {
            self.session
                .as_ref()
                .map(|s| s.sim.pid())
                .or_else(|| locate::select(None).ok().map(|p| p.pid))
        }

        /// Takes the nose view and the bridge out of the sim, in order, and
        /// confirms each is gone. Also clears out DLLs an earlier manager
        /// left behind.
        fn release(&mut self, reason: Release) {
            let pid = self.sim_pid();
            let session = self.session.take();
            {
                let mut d = lock(&self.shared.diag);
                d.state = FlirState::Releasing;
                d.requested = false;
            }
            let Some(pid) = pid else {
                self.released(reason, "the sim is not running");
                return;
            };
            let core_in = matches!(locate::loaded_module(pid, dlls::CORE_DLL), Ok(Some(_)));
            let bridge_in = matches!(locate::loaded_module(pid, dlls::BRIDGE_DLL), Ok(Some(_)));
            if !core_in && !bridge_in {
                self.released(reason, "nothing was loaded");
                return;
            }
            // The session's link, or a fresh one to DLLs it does not own.
            let sim = match session {
                Some(s) => {
                    let Session {
                        sim,
                        camera,
                        reader,
                        streaming,
                        ..
                    } = s;
                    drop((camera, reader));
                    if streaming {
                        let _ = sim.stop_stream();
                    }
                    Some(sim)
                }
                None if bridge_in => Host::new(HostConfig {
                    bridge_path: PathBuf::new(),
                    bridge_sha256: None,
                    sim_path: None,
                })
                .and_then(|h| h.connect(SimTarget::Pid(pid)))
                .ok(),
                None => None,
            };

            // The core first: it erases its view on the sim's update, then
            // unloads. The bridge goes after, so nothing is left capturing a
            // view that is still being drawn.
            let core_gone = !core_in || {
                let _ = infinity_viewports_host::signal_nose_teardown(pid);
                wait_gone(pid, dlls::CORE_DLL)
            };
            let bridge_gone = !bridge_in || {
                match &sim {
                    Some(sim) => {
                        let _ = sim.teardown_bridge();
                    }
                    None => warn!(
                        "flir: the bridge is loaded but not answering; it stays until the sim exits"
                    ),
                }
                // Our handles to its mappings go before the wait.
                drop(sim);
                wait_gone(pid, dlls::BRIDGE_DLL)
            };
            let outcome = match (core_gone, bridge_gone) {
                (true, true) => "unloaded",
                (false, true) => "the core did not unload",
                (true, false) => "the bridge did not unload",
                (false, false) => "neither DLL unloaded",
            };
            if !(core_gone && bridge_gone) {
                warn!(pid, outcome, "flir: unload incomplete");
            }
            self.released(reason, outcome);
        }

        fn released(&mut self, reason: Release, outcome: &str) {
            info!(reason = reason.describe(), outcome, "flir: released");
            self.pipeline.reset();
            self.next_attach = Instant::now();
            *lock(&self.shared.preview) = None;
            let mut d = lock(&self.shared.diag);
            d.state = FlirState::Idle;
            d.pid = None;
            d.injected = false;
            d.camera = "-".into();
            d.stream_fps = 0.0;
            d.nose.clear();
            d.nose_losses = 0;
            d.last_release = Some(format!("{}: {outcome}", reason.describe()));
        }

        /// Attaches to the sim and injects whatever of the two DLLs it lacks,
        /// then starts the stream. False while it cannot (retried every few
        /// seconds).
        fn attach(&mut self) -> bool {
            if Instant::now() < self.next_attach {
                return false;
            }
            self.next_attach = Instant::now() + ATTACH_RETRY;
            self.set_state(FlirState::Attaching, None);
            let settings = lock(&self.shared.settings).clone();
            let dir = &self.shared.app_data_dir;
            fn configured(p: &Option<String>) -> Option<&Path> {
                p.as_deref().map(Path::new)
            }
            let paths = dlls::resolve(configured(&settings.bridge_dll), dlls::BRIDGE_DLL, dir)
                .and_then(|b| {
                    dlls::resolve(configured(&settings.core_dll), dlls::CORE_DLL, dir)
                        .map(|c| (b, c))
                });
            let (bridge_src, core_src) = match paths {
                Ok(p) => p,
                Err(e) => {
                    self.set_state(FlirState::Error, Some(e));
                    return false;
                }
            };
            {
                let mut d = lock(&self.shared.diag);
                d.bridge_dll = Some(bridge_src.display().to_string());
                d.core_dll = Some(core_src.display().to_string());
            }
            let Ok(proc) = locate::select(None) else {
                self.set_state(FlirState::Attaching, Some("waiting for the sim".into()));
                return false;
            };
            let pid = proc.pid;

            let root = shadow::root(dir);
            let staged = shadow::stage(&bridge_src, &root)
                .and_then(|b| shadow::stage(&core_src, &root).map(|c| (b, c)));
            let (bridge, core) = match staged {
                Ok(p) => p,
                Err(e) => {
                    self.set_state(FlirState::Error, Some(format!("copy the DLLs: {e}")));
                    return false;
                }
            };

            // Whatever is in the sim must be this build, or come out first.
            let loaded = |name: &str| locate::loaded_module(pid, name).ok().flatten();
            let (bridge_in, core_in) = (loaded(dlls::BRIDGE_DLL), loaded(dlls::CORE_DLL));
            let foreign = bridge_in.as_deref().is_some_and(|p| !same_file(p, &bridge))
                || core_in.as_deref().is_some_and(|p| !same_file(p, &core));
            if foreign {
                info!(pid, "flir: a different build is in the sim; swapping it");
                self.release(Release::Rebuilt);
                // Straight back here on the next step.
                self.next_attach = Instant::now();
                return false;
            }

            let host = match Host::new(HostConfig {
                bridge_path: bridge.clone(),
                bridge_sha256: None,
                sim_path: None,
            }) {
                Ok(h) => h,
                Err(e) => {
                    self.set_state(FlirState::Error, Some(e.to_string()));
                    return false;
                }
            };
            let sim = if bridge_in.is_some() {
                host.connect(SimTarget::Pid(pid))
            } else {
                host.attach(SimTarget::Pid(pid))
            };
            let sim = match sim {
                Ok(s) => s,
                Err(e) => {
                    self.set_state(FlirState::Error, Some(format!("attach: {e}")));
                    return false;
                }
            };
            // Only when absent: a second LoadLibrary of a loaded module is a
            // refcount bump, and the teardown's one FreeLibrary would then
            // leave it loaded and unhooked.
            if core_in.is_none()
                && let Err(e) = infinity_viewports_host::inject::inject(pid, &core)
            {
                self.set_state(FlirState::Error, Some(format!("inject core: {e}")));
                return false;
            }
            let _ = sim.enable(true);
            if let Err(e) = sim.set_stream_source(COLOR_FORMAT, 0) {
                warn!(error = %e, "flir: set_stream_source failed");
            }
            info!(
                pid,
                reused = bridge_in.is_some(),
                bridge = %bridge.display(),
                core = %core.display(),
                "flir: attached to the sim, nose view injected"
            );
            shadow::sweep(&root, &[&bridge, &core]);
            {
                let mut d = lock(&self.shared.diag);
                d.pid = Some(pid);
                d.injected = true;
            }
            self.pipeline.reset();
            let now = Instant::now();
            self.session = Some(Session {
                sim,
                camera: None,
                reader: None,
                streaming: false,
                heartbeat: 0,
                heartbeat_at: now,
                since: now,
                sources: [bridge_src, core_src]
                    .into_iter()
                    .map(|p| {
                        let st = shadow::stamp(&p);
                        (p, st)
                    })
                    .collect(),
                next_rebuild_check: now + REBUILD_CHECK,
                nose_down_since: None,
            });
            self.set_state(FlirState::Running, None);
            true
        }

        pub(super) fn stop_stream(&mut self) {
            if let Some(s) = self.session.as_mut()
                && s.streaming
            {
                let _ = s.sim.stop_stream();
                s.streaming = false;
                info!("flir: stream stopped (no gauge asking)");
            }
            let mut d = lock(&self.shared.diag);
            if d.state == FlirState::Running {
                d.state = FlirState::Idle;
            }
        }

        /// The SimConnect pose with the configured sign corrections.
        fn pose(&self, s: &FlirSettings) -> AircraftPose {
            match self.feed.sample() {
                Some(x) => aircraft_pose(
                    x.lat_deg,
                    x.lon_deg,
                    x.alt_ft * FEET_TO_M,
                    x.heading_deg,
                    if s.invert_pitch {
                        -x.pitch_deg
                    } else {
                        x.pitch_deg
                    },
                    if s.invert_bank {
                        -x.bank_deg
                    } else {
                        x.bank_deg
                    },
                ),
                None => AircraftPose::absent(),
            }
        }

        fn control(s: &FlirSettings) -> CameraControl {
            let c = &s.camera;
            CameraControl {
                mount: MOUNT_AIRCRAFT,
                right_m: c.right_m,
                up_m: c.up_m,
                forward_m: c.forward_m,
                yaw_deg: c.yaw_deg,
                pitch_deg: c.pitch_deg,
                roll_deg: 0.0,
                fov_deg: c.fov_deg,
                near_m: c.near_m,
                ..CameraControl::stock()
            }
            .sanitized()
        }

        fn tick(&mut self, report: &StateReport) {
            let settings = lock(&self.shared.settings).clone();
            self.feed.poll();
            let pose = self.pose(&settings);
            let sample = self.feed.sample();
            let feed_error = self.feed.error().map(str::to_string);
            let Some(s) = self.session.as_mut() else {
                return;
            };

            // A dead bridge (sim closed or crashed) drops the session.
            if let Ok(st) = s.sim.status() {
                if st.heartbeat != s.heartbeat {
                    s.heartbeat = st.heartbeat;
                    s.heartbeat_at = Instant::now();
                }
                lock(&self.shared.diag).stream_fps = st.fps;
            }
            if s.heartbeat_at.elapsed() > HEARTBEAT_LOST {
                // The sim exited (nothing to unload), or the bridge hung:
                // take it out so the next attach injects a working one.
                warn!("flir: bridge heartbeat lost; releasing");
                self.release(Release::Stalled);
                return;
            }

            if !s.streaming {
                match s.sim.start_stream() {
                    Ok(()) => {
                        s.streaming = true;
                        info!("flir: stream started");
                    }
                    Err(e) => {
                        lock(&self.shared.diag).error = Some(format!("start_stream: {e}"));
                        return;
                    }
                }
            }

            // Drive the camera every tick: core does not follow the aircraft
            // by itself, and a pose it stops receiving freezes it in place.
            if s.camera.is_none() {
                s.camera = s.sim.camera().ok();
            }
            let mut camera_label = None;
            let mut nose = None;
            if let Some(cam) = &s.camera {
                let _ = cam.write_control(Self::control(&settings));
                if pose.valid != 0 {
                    let _ = cam.write_pose(pose);
                }
                if let Ok(st) = cam.read_status() {
                    nose = Some((st.nose, st.nose_losses));
                    let applied = match st.applied {
                        APPLIED_FOLLOW => "follow",
                        APPLIED_PLACED => "placed",
                        APPLIED_PARENTED => "parented",
                        APPLIED_MISSING => "missing",
                        _ => "?",
                    };
                    let end = st
                        .note
                        .iter()
                        .position(|&b| b == 0)
                        .unwrap_or(st.note.len());
                    camera_label = Some((
                        applied.to_string(),
                        String::from_utf8_lossy(st.note.get(..end).unwrap_or_default())
                            .into_owned(),
                    ));
                }
            }
            {
                let mut d = lock(&self.shared.diag);
                d.pose = pose.valid != 0;
                d.pose_error = if pose.valid != 0 { None } else { feed_error };
                if let Some((a, n)) = camera_label {
                    d.camera = a;
                    d.camera_note = n;
                }
                if let Some((state, losses)) = nose {
                    d.nose = match state {
                        NOSE_CREATING => "creating",
                        NOSE_LIVE => "live",
                        NOSE_REACQUIRING => "reacquiring",
                        NOSE_STOPPED => "stopped",
                        _ => "",
                    }
                    .into();
                    d.nose_losses = u32::from(losses);
                }
            }

            // The core gets a lost view back by itself; if it cannot, a
            // fresh injection is the last resort. A core that stopped (torn
            // down by someone else) will not make one at all.
            if let Some((state, _)) = nose {
                let now = Instant::now();
                match state {
                    NOSE_LIVE => {
                        s.nose_down_since = None;
                        self.nose_resets = 0;
                    }
                    NOSE_CREATING | NOSE_REACQUIRING | NOSE_STOPPED => {
                        let down = *s.nose_down_since.get_or_insert(now);
                        let patience = if state == NOSE_STOPPED {
                            Duration::ZERO
                        } else {
                            nose_patience(self.nose_resets)
                        };
                        if now.duration_since(down) > patience {
                            warn!(
                                state,
                                down_s = now.duration_since(down).as_secs(),
                                "flir: the nose view is not coming back; re-injecting"
                            );
                            self.nose_resets = self.nose_resets.saturating_add(1);
                            self.release(Release::NoseLost);
                            return;
                        }
                    }
                    // A core from before the nose report.
                    _ => {}
                }
            }

            if s.reader.is_none() {
                s.reader = s.sim.open_video().ok();
            }
            let Some(reader) = s.reader.as_mut() else {
                return;
            };
            let Some(frame) = reader.read_new() else {
                return;
            };
            if Instant::now() < self.next_frame {
                return;
            }
            let interval = Duration::from_secs_f32(1.0 / settings.max_fps.max(1.0));
            self.next_frame = Instant::now() + interval;

            let Some(depth_layer) = frame.layers.iter().find(|l| l.role == ROLE_DEPTH) else {
                return;
            };
            let (w, h) = (depth_layer.width as usize, depth_layer.height as usize);
            let Some(depth) = decode::depth_f32(w, h, &depth_layer.bytes) else {
                return;
            };
            let rgb = frame
                .layers
                .iter()
                .find(|l| l.role == ROLE_COLOR)
                .filter(|c| c.width as usize == w && c.height as usize == h)
                .and_then(|c| decode::linear_rgb(c.format, w, h, &c.bytes));

            let started = Instant::now();
            let pose_pos = sample.map(|x| (x.lat_deg, x.lon_deg, x.alt_ft * FEET_TO_M));
            let env = env::to_wire(&report.env, pose_pos, unix_now());
            self.pipeline.params = settings::sensor_params(&settings.sensor, report.polarity);
            self.pipeline.clouds = CloudParams {
                enabled: settings.clouds,
                alt_m: settings.cloud_alt_m,
                ..CloudParams::default()
            };
            let size = report.size.clamp(MIN_SIZE, flir_stream::MAX_SIZE) as usize;
            let gray = self
                .pipeline
                .develop_square(&depth, w, h, rgb.as_deref(), &env, size);
            let develop_ms = started.elapsed().as_secs_f32() * 1000.0;

            self.seq = self.seq.max(first_seq()).wrapping_add(1);
            let out = Arc::new(Frame {
                seq: self.seq,
                width: gray.w as u32,
                height: gray.h as u32,
                data: gray.data,
            });
            *lock(&self.shared.preview) = Some(Arc::clone(&out));
            let _ = self.tx.send(Some(out));

            let (since, n) = &mut self.fps_window;
            *n += 1;
            let mut d = lock(&self.shared.diag);
            if since.elapsed() >= Duration::from_secs(1) {
                d.develop_fps = *n as f32 / since.elapsed().as_secs_f32();
                *since = Instant::now();
                *n = 0;
            }
            d.develop_ms = develop_ms;
            d.frames_developed += 1;
            d.size = size as u32;
            d.last_seq = self.seq;
            d.state = FlirState::Running;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_encode_as_png() {
        let frame = Frame {
            seq: 7,
            width: 4,
            height: 2,
            data: vec![0, 64, 128, 255, 10, 20, 30, 40],
        };
        for tinted in [false, true] {
            let png = encode_png(&frame, tinted).unwrap();
            assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        }
        let short = Frame {
            data: vec![0; 3],
            ..frame
        };
        assert!(encode_png(&short, false).is_err());
    }

    #[test]
    fn engine_starts_idle_and_stops_with_its_last_handle() {
        let dir = tempfile::tempdir().unwrap();
        let engine = FlirEngine::spawn(dir.path().to_path_buf());
        assert!(engine.preview_png(true, None).unwrap().is_none());
        let mut s = engine.settings();
        s.max_fps = 100.0;
        engine.set_settings(s, true).unwrap();
        assert_eq!(engine.settings().max_fps, 30.0);
        assert_eq!(settings::load(dir.path()).max_fps, 30.0);
        let weak = Arc::downgrade(&engine.shared);
        drop(engine);
        // The worker holds its own reference until it notices the shutdown.
        for _ in 0..200 {
            if weak.strong_count() == 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the engine thread did not stop");
    }
}
