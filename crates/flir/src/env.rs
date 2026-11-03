//! The gauge's environment report (sim units, every field optional) to the
//! thermal model's `EnvironmentWire`.

use flir_stream::EnvReport;
use flir_types::EnvironmentWire;

const FT_TO_M: f64 = 0.3048;
const KT_TO_MS: f64 = 0.514_444;

/// `AMBIENT PRECIP STATE` bits (SDK): 2 none, 4 rain, 8 snow.
fn precip_kind(state: Option<f64>) -> u8 {
    match state.map(|s| s as u32) {
        Some(s) if s & 8 != 0 => 2,
        Some(s) if s & 4 != 0 => 1,
        _ => 0,
    }
}

/// Fills an `EnvironmentWire` from what the aircraft reported, falling back
/// to the pipeline's defaults for anything unknown. `pose` is the SimConnect
/// pose feed's position (lat, lon, alt m), used when the report has none.
/// Unknown humidity is left NaN: `Environment::from_wire` then estimates it
/// from the visibility (the sim has no humidity var).
pub fn to_wire(r: &EnvReport, pose: Option<(f64, f64, f64)>, now_unix_s: f64) -> EnvironmentWire {
    let mut w = flir_pipeline::default_env();
    let fin = |v: Option<f64>| v.filter(|x| x.is_finite());
    if let Some(t) = fin(r.temp_c) {
        w.ambient_temp_c = t as f32;
    }
    if let Some(p) = fin(r.pressure_hpa).filter(|p| *p > 100.0) {
        w.pressure_hpa = p as f32;
    }
    if let Some(v) = fin(r.visibility_m).filter(|v| *v > 0.0) {
        w.visibility_m = v as f32;
    }
    w.precip_kind = precip_kind(fin(r.precip_state));
    w.precip_rate = fin(r.precip_rate).unwrap_or(0.0).max(0.0) as f32;
    w.in_cloud = u8::from(fin(r.in_cloud).unwrap_or(0.0) > 0.5);
    w.wind_dir_deg = fin(r.wind_dir_deg).unwrap_or(0.0) as f32;
    w.wind_speed_ms = (fin(r.wind_kt).unwrap_or(0.0) * KT_TO_MS) as f32;

    if let Some(lat) = fin(r.lat_deg).or(pose.map(|p| p.0)) {
        w.lat_deg = lat;
    }
    if let Some(lon) = fin(r.lon_deg).or(pose.map(|p| p.1)) {
        w.lon_deg = lon;
    }
    if let Some(alt) = fin(r.alt_ft).map(|ft| ft * FT_TO_M).or(pose.map(|p| p.2)) {
        w.alt_m = alt as f32;
    }
    if let Some(g) = fin(r.ground_alt_m) {
        w.ground_alt_m = g as f32;
    }
    w.humidity_pct = f32::NAN;
    w.dewpoint_c = f32::NAN;
    w.utc_unix_s = r.utc_unix_s().unwrap_or(now_unix_s);
    w.sun_dir = [f32::NAN; 3];
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_fields_convert_and_fall_back() {
        let r = EnvReport {
            temp_c: Some(-5.0),
            visibility_m: Some(1500.0),
            precip_state: Some(8.0),
            wind_kt: Some(10.0),
            alt_ft: Some(10_000.0),
            zulu_year: Some(2024.0),
            zulu_day_of_year: Some(173.0),
            zulu_s: Some(3600.0),
            ..EnvReport::default()
        };
        let w = to_wire(&r, Some((40.0, -90.0, 1.0)), 0.0);
        assert_eq!(w.ambient_temp_c, -5.0);
        assert_eq!(w.visibility_m, 1500.0);
        assert_eq!(w.precip_kind, 2);
        assert!((w.wind_speed_ms - 5.144).abs() < 0.01);
        assert!((w.alt_m - 3048.0).abs() < 0.1);
        assert_eq!(
            (w.lat_deg, w.lon_deg),
            (40.0, -90.0),
            "position from the pose feed"
        );
        assert_eq!(w.utc_unix_s, 1_718_928_000.0 + 3600.0);
        assert!(w.humidity_pct.is_nan());
    }
}
