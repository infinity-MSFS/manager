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
//! The engine is demand-driven: it attaches, injects and develops only while
//! a gauge keeps asking ([`FlirEngine::request`], refreshed by every state
//! report); a request older than [`REQUEST_TTL`] lapses and the stream
//! stops. The injected DLLs stay in the sim until [`FlirEngine::unload`] (or
//! the sim exits): re-injecting is not a restart (the core initialises once
//! per process), so stopping only stops the stream.
//!
//! The engine thread ends when the last [`FlirEngine`] handle is dropped.

pub mod dlls;
pub mod env;
mod error;
pub mod settings;

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use flir_stream::{Frame, StateReport};
use infinity_sim_tools_ipc::flir::{FlirDiag, FlirPreview, FlirSettings, FlirState};
use tokio::sync::watch;
use tracing::info;

pub use error::{FlirError, Result};

/// A request lapses this long after the last state report.
pub const REQUEST_TTL: Duration = Duration::from_secs(2);
/// Smallest frame the engine makes.
pub const MIN_SIZE: u32 = 64;
/// The gauge's ink, for tinting the preview like the HUD draws it.
pub const INK: [u8; 3] = [70, 255, 110];

struct Request {
    report: StateReport,
    at: Instant,
}

#[derive(Default)]
struct Control {
    request: Option<Request>,
    unload: bool,
    shutdown: bool,
}

struct Shared {
    control: Mutex<Control>,
    wake: Condvar,
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

    /// A gauge wants a picture (refresh on every state report).
    pub fn request(&self, report: StateReport) {
        lock(&self.shared.control).request = Some(Request {
            report,
            at: Instant::now(),
        });
        self.shared.wake.notify_all();
    }

    /// The gauge stopped asking (`active: false`).
    pub fn withdraw(&self) {
        lock(&self.shared.control).request = None;
        self.shared.wake.notify_all();
    }

    /// Stops, then tears the nose view and the bridge out of the sim.
    pub fn unload(&self) {
        let mut c = lock(&self.shared.control);
        c.request = None;
        c.unload = true;
        drop(c);
        self.shared.wake.notify_all();
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
    use infinity_viewports_host::{Host, HostConfig, Sim, SimTarget};
    use infinity_viewports_protocol::{
        APPLIED_FOLLOW, APPLIED_MISSING, APPLIED_PARENTED, APPLIED_PLACED, AircraftPose,
        CameraControl, MOUNT_AIRCRAFT,
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

    struct Session {
        sim: Sim,
        camera: Option<CameraLink>,
        reader: Option<VideoReader>,
        streaming: bool,
        heartbeat: u64,
        heartbeat_at: Instant,
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
        };
        while !shared.stopping() {
            w.step();
        }
        w.stop_stream();
        info!("flir: engine stopped");
    }

    impl Worker<'_> {
        fn set_state(&self, state: FlirState, error: Option<String>) {
            let mut d = lock(&self.shared.diag);
            d.state = state;
            d.error = error;
        }

        /// The live request, if any (lapsing a stale one), and whether an
        /// unload was asked for.
        fn current(&self) -> (Option<StateReport>, bool) {
            let mut c = lock(&self.shared.control);
            let unload = std::mem::take(&mut c.unload);
            if c.request
                .as_ref()
                .is_some_and(|r| r.at.elapsed() > REQUEST_TTL)
            {
                c.request = None;
            }
            (c.request.as_ref().map(|r| r.report.clone()), unload)
        }

        fn step(&mut self) {
            let (request, unload) = self.current();
            if unload {
                self.unload();
            }
            lock(&self.shared.diag).requested = request.is_some();
            let Some(report) = request else {
                self.stop_stream();
                // Nothing to do: sleep until a request (or poll the TTL).
                let c = lock(&self.shared.control);
                let _ = self.shared.wake.wait_timeout_while(c, IDLE_POLL, |c| {
                    c.request.is_none() && !c.unload && !c.shutdown
                });
                return;
            };
            if self.session.is_none() && !self.attach() {
                std::thread::sleep(Duration::from_millis(200));
                return;
            }
            self.tick(&report);
            std::thread::sleep(TICK);
        }

        /// Attaches (or reconnects) to the sim, injects the nose view and
        /// starts the stream. False while it cannot (retried every few
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
            let (bridge, core) = match paths {
                Ok(p) => p,
                Err(e) => {
                    self.set_state(FlirState::Error, Some(e));
                    return false;
                }
            };
            {
                let mut d = lock(&self.shared.diag);
                d.bridge_dll = Some(bridge.display().to_string());
                d.core_dll = Some(core.display().to_string());
            }
            let host = match Host::new(HostConfig {
                bridge_path: bridge,
                bridge_sha256: None,
                sim_path: None,
            }) {
                Ok(h) => h,
                Err(e) => {
                    self.set_state(FlirState::Error, Some(e.to_string()));
                    return false;
                }
            };
            // A bridge already in the sim (an earlier session) is re-used;
            // otherwise inject it.
            let sim = match host
                .connect(SimTarget::Auto)
                .or_else(|_| host.attach(SimTarget::Auto))
            {
                Ok(s) => s,
                Err(e) => {
                    self.set_state(FlirState::Error, Some(format!("attach: {e}")));
                    return false;
                }
            };
            // The core DLL creates the nose view. LoadLibrary of a module
            // already loaded is a refcount bump, so this is safe to repeat.
            if let Err(e) = infinity_viewports_host::inject::inject(sim.pid(), &core) {
                self.set_state(FlirState::Error, Some(format!("inject core: {e}")));
                return false;
            }
            let _ = sim.enable(true);
            if let Err(e) = sim.set_stream_source(COLOR_FORMAT, 0) {
                warn!(error = %e, "flir: set_stream_source failed");
            }
            let pid = sim.pid();
            info!(pid, "flir: attached to the sim, nose view injected");
            lock(&self.shared.diag).pid = Some(pid);
            self.pipeline.reset();
            self.session = Some(Session {
                sim,
                camera: None,
                reader: None,
                streaming: false,
                heartbeat: 0,
                heartbeat_at: Instant::now(),
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

        fn unload(&mut self) {
            if let Some(s) = self.session.take() {
                let _ = s.sim.stop_stream();
                let _ = s.sim.teardown_nose();
                let _ = s.sim.teardown_bridge();
                info!("flir: nose view and bridge torn down");
            }
            let mut d = lock(&self.shared.diag);
            d.state = FlirState::Idle;
            d.pid = None;
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
                warn!("flir: bridge heartbeat lost; re-attaching");
                self.session = None;
                self.set_state(FlirState::Attaching, Some("bridge heartbeat lost".into()));
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
            if let Some(cam) = &s.camera {
                let _ = cam.write_control(Self::control(&settings));
                if pose.valid != 0 {
                    let _ = cam.write_pose(pose);
                }
                if let Ok(st) = cam.read_status() {
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
