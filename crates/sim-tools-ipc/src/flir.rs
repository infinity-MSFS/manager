//! The MD-10 HUD's FLIR: diagnostics, tunables and the preview.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlirState {
    #[default]
    Idle,
    Attaching,
    Running,
    Error,
    /// The nose camera needs Windows.
    Unsupported,
}

/// What the engine is doing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlirDiag {
    pub state: FlirState,
    pub error: Option<String>,
    pub pid: Option<u32>,
    /// A gauge is asking for a picture right now.
    pub requested: bool,
    pub size: u32,
    pub bridge_dll: Option<String>,
    pub core_dll: Option<String>,
    /// Frames the nose stream published per second (sim side).
    pub stream_fps: f32,
    /// Frames developed per second (manager side).
    pub develop_fps: f32,
    /// Cost of the last develop, milliseconds.
    pub develop_ms: f32,
    pub frames_developed: u64,
    pub frames_sent: u64,
    pub frames_failed: u64,
    /// Base64 bytes per frame on the wire, last frame.
    pub wire_bytes: u64,
    /// The camera as the core DLL applied it: `follow`, `placed`,
    /// `parented` or `missing` (`-` before the first status).
    pub camera: String,
    pub camera_note: String,
    /// The SimConnect pose feed is live (needed to place the camera).
    pub pose: bool,
    pub pose_error: Option<String>,
    pub last_seq: u64,
}

/// Where the nose camera sits and points, relative to the aircraft's
/// reference datum and body axes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NoseCamera {
    /// Metres forward of the datum.
    pub forward_m: f32,
    pub up_m: f32,
    pub right_m: f32,
    /// Degrees, positive up.
    pub pitch_deg: f32,
    /// Degrees, positive right.
    pub yaw_deg: f32,
    /// Vertical field of view, degrees.
    pub fov_deg: f32,
}

/// The sensor look.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SensorLook {
    pub gamma: f32,
    pub brightness: f32,
    pub detail_gain: f32,
    pub agc_clip: f32,
    pub agc_plateau: f32,
    pub agc_speed: f32,
    pub noise_sigma: f32,
}

/// Everything the panel tunes. The engine owns the defaults
/// ([`crate::FlirSettingsDefault`]) and clamps what it is given.
///
/// Field names are v3's `flir/settings.json` (snake case), so a file saved
/// by either version reads in the other.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlirSettings {
    /// `infinity_viewports_bridge.dll`; `None` searches the usual places.
    pub bridge_dll: Option<String>,
    /// `infinity_viewports_core.dll`.
    pub core_dll: Option<String>,
    /// Frames developed and streamed per second, at most.
    pub max_fps: f32,
    /// Gray levels rounded to multiples of this before packing (1 lossless).
    pub quant: u8,
    pub camera: NoseCamera,
    pub sensor: SensorLook,
    /// The colour-buffer cloud composite.
    pub clouds: bool,
    pub cloud_alt_m: f32,
    /// MSFS reports pitch positive nose down; the pose builder wants up.
    pub invert_pitch: bool,
    pub invert_bank: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlirSettingsSetArgs {
    pub settings: FlirSettings,
    pub persist: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FlirPreviewArgs {
    /// Tint like the HUD draws it (green over black) rather than gray.
    pub tinted: bool,
    /// The `seq` the caller already shows: an unchanged frame answers `None`.
    pub after_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlirPreview {
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    /// `data:image/png;base64,...`, ready for an `<img src>`.
    pub data_url: String,
}
