//! What the crew or a developer can tune, persisted as
//! `<appdata>/flir/settings.json` (the same file and field names as v3).

use std::path::{Path, PathBuf};

use flir_types::SensorParams;
use infinity_sim_tools_ipc::flir::{FlirSettings, NoseCamera, SensorLook};
use serde_json::Value;

use crate::error::{FlirError, Result};

pub fn path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("flir").join("settings.json")
}

/// The nose camera on the MD-10 HUD's boresight.
pub fn default_camera() -> NoseCamera {
    // TODO_DATA(nose-datum): the turret's station on the MD-10 is not
    // measured; 20 m ahead of the datum puts the camera near the nose. With
    // a collimated HUD only the parallax to nearby objects (taxi, flare)
    // depends on it. Pitch and FOV are the HUD field (panel.cfg
    // `hud = 2, 0.0, 3.9, 27.8, 27.8`).
    NoseCamera {
        forward_m: 20.0,
        up_m: -1.5,
        right_m: 0.0,
        pitch_deg: -3.9,
        yaw_deg: 0.0,
        fov_deg: 27.8,
        near_m: 0.0,
    }
}

/// The pipeline's HUD look.
pub fn default_look() -> SensorLook {
    let p = flir_pipeline::hud_params();
    SensorLook {
        gamma: p.gamma,
        brightness: p.brightness,
        detail_gain: p.detail_gain,
        agc_clip: p.agc_clip,
        agc_plateau: p.agc_plateau,
        agc_speed: p.agc_speed,
        noise_sigma: p.noise_sigma,
    }
}

pub fn defaults() -> FlirSettings {
    FlirSettings {
        bridge_dll: None,
        core_dll: None,
        max_fps: 15.0,
        quant: 2,
        camera: default_camera(),
        sensor: default_look(),
        clouds: true,
        cloud_alt_m: 3000.0,
        // MSFS reports pitch positive nose down and bank positive left; the
        // pose builder wants nose up and right wing down. Bank was flown in
        // the MD-10: uninverted, the picture banked with the aircraft
        // instead of holding the horizon.
        invert_pitch: true,
        invert_bank: true,
    }
}

/// Clamps everything to what the engine accepts.
pub fn sanitized(mut s: FlirSettings) -> FlirSettings {
    let f = |v: f32, lo: f32, hi: f32, d: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
    let d = defaults();
    s.max_fps = f(s.max_fps, 1.0, 30.0, d.max_fps);
    s.quant = s.quant.clamp(1, 16);
    let (c, dc) = (&mut s.camera, d.camera);
    c.forward_m = f(c.forward_m, -500.0, 500.0, dc.forward_m);
    c.up_m = f(c.up_m, -500.0, 500.0, dc.up_m);
    c.right_m = f(c.right_m, -500.0, 500.0, dc.right_m);
    c.pitch_deg = f(c.pitch_deg, -89.0, 89.0, dc.pitch_deg);
    c.yaw_deg = f(c.yaw_deg, -180.0, 180.0, dc.yaw_deg);
    c.fov_deg = f(c.fov_deg, 1.0, 140.0, dc.fov_deg);
    c.near_m = f(c.near_m, 0.0, 20.0, dc.near_m);
    s.cloud_alt_m = f(s.cloud_alt_m, 0.0, 15_000.0, d.cloud_alt_m);
    let blank = |p: &mut Option<String>| {
        if p.as_deref().is_some_and(|v| v.trim().is_empty()) {
            *p = None;
        }
    };
    blank(&mut s.bridge_dll);
    blank(&mut s.core_dll);
    s
}

/// The sensor parameters for a frame: the look over the pipeline's HUD
/// preset, clamped.
pub fn sensor_params(look: &SensorLook, polarity: u8) -> SensorParams {
    let finite = |v: f32, d: f32| if v.is_finite() { v } else { d };
    let base = flir_pipeline::hud_params();
    SensorParams {
        polarity_u8: polarity.min(1),
        gamma: finite(look.gamma, base.gamma).clamp(0.3, 8.0),
        brightness: finite(look.brightness, base.brightness).clamp(0.05, 1.5),
        detail_gain: finite(look.detail_gain, base.detail_gain).clamp(0.0, 4.0),
        agc_clip: finite(look.agc_clip, base.agc_clip).clamp(0.0, 0.2),
        agc_plateau: finite(look.agc_plateau, base.agc_plateau).clamp(0.0, 1.0),
        agc_speed: finite(look.agc_speed, base.agc_speed).clamp(0.02, 1.0),
        noise_sigma: finite(look.noise_sigma, base.noise_sigma).clamp(0.0, 0.03),
        ..base
    }
}

/// Settings from disk over the defaults: a field the file lacks keeps its
/// default, and a missing or unreadable file is the defaults (the FLIR must
/// still come up). Blocking.
pub fn load(app_data_dir: &Path) -> FlirSettings {
    let file = path(app_data_dir);
    let parsed = std::fs::read(&file)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let Some(on_disk) = parsed else {
        return defaults();
    };
    match overlay(&defaults(), on_disk) {
        Ok(s) => sanitized(s),
        Err(e) => {
            tracing::warn!(path = %file.display(), error = %e, "flir: settings unreadable; using defaults");
            defaults()
        }
    }
}

fn overlay(base: &FlirSettings, on_top: Value) -> serde_json::Result<FlirSettings> {
    let mut merged = serde_json::to_value(base)?;
    merge(&mut merged, on_top);
    serde_json::from_value(merged)
}

fn merge(into: &mut Value, from: Value) {
    match (into, from) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                match a.get_mut(&k) {
                    Some(slot) => merge(slot, v),
                    None => {
                        a.insert(k, v);
                    }
                }
            }
        }
        (slot, v) => *slot = v,
    }
}

/// Writes the settings. Blocking.
pub fn save(settings: &FlirSettings, app_data_dir: &Path) -> Result<()> {
    let file = path(app_data_dir);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| FlirError::io(dir, e))?;
    }
    let json = serde_json::to_vec_pretty(settings)?;
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| FlirError::io(&tmp, e))?;
    std::fs::rename(&tmp, &file).map_err(|e| FlirError::io(&file, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partial_v3_file_keeps_the_other_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let file = path(dir.path());
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            &file,
            br#"{ "max_fps": 99, "camera": { "pitch_deg": -2.5 }, "bridge_dll": "C:\\x.dll" }"#,
        )
        .unwrap();
        let s = load(dir.path());
        assert_eq!(s.max_fps, 30.0, "clamped");
        assert_eq!(s.camera.pitch_deg, -2.5);
        assert_eq!(s.camera.fov_deg, default_camera().fov_deg);
        assert_eq!(s.bridge_dll.as_deref(), Some("C:\\x.dll"));
        assert_eq!(s.sensor, default_look());
    }

    #[test]
    fn saves_and_loads() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = defaults();
        s.clouds = false;
        s.sensor.gamma = 2.0;
        save(&s, dir.path()).unwrap();
        assert_eq!(load(dir.path()), s);
    }

    #[test]
    fn garbage_is_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()), defaults());
        let file = path(dir.path());
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"{ not json").unwrap();
        assert_eq!(load(dir.path()), defaults());
        std::fs::write(&file, br#"{ "quant": "many" }"#).unwrap();
        assert_eq!(load(dir.path()), defaults());
    }

    #[test]
    fn sanitizing_clamps_and_repairs() {
        let mut s = defaults();
        s.max_fps = f32::NAN;
        s.quant = 0;
        s.camera.fov_deg = 500.0;
        s.core_dll = Some("  ".into());
        let s = sanitized(s);
        assert_eq!(s.max_fps, 15.0);
        assert_eq!(s.quant, 1);
        assert_eq!(s.camera.fov_deg, 140.0);
        assert_eq!(s.core_dll, None);
    }
}
