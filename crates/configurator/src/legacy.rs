//! Import of v1 share files.
//!
//! v1 exported two shapes:
//! - **Full export** (`.infinity`): the whole v1 `ConfigFile` (top-level
//!   `version` plus `META`, `ZONES`, `COM`, …). That is the shape v3/v4 store,
//!   so importing is v1's own load-time coercions plus an envelope.
//! - **Curated share** (`.json`, `aircraft_defaults` / `training_areas` / …):
//!   not importable; detected only so the user gets a useful error.
//!
//! The coercions are idempotent and leave modern values byte-identical
//! (including number representation), so a v3 bare-value dump that happens
//! to detect as [`LegacyShare::V1Full`] passes through unchanged.

use serde_json::Value;

use crate::error::{ConfiguratorError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyShare {
    V1Full,
    V1Curated,
}

const V1_SECTION_KEYS: [&str; 11] = [
    "META",
    "ZONES",
    "IFF",
    "MAIN",
    "DECLUTTER",
    "MFD_DECLUTTER",
    "WPN",
    "APP",
    "FLIGHT_PLANS",
    "DST",
    "COM",
];

const V1_CURATED_KEYS: [&str; 11] = [
    "approaches",
    "vhf_frequencies",
    "uhf_frequencies",
    "aircraft_defaults",
    "training_areas",
    "flight_plans",
    "no_fly_zones",
    "destinations",
    "mfd_declutter",
    "hud_declutter",
    "weapon_programs",
];

/// Shape-based: two or more known keys classify a file. v3 envelopes and
/// anything unrecognised return `None` and import as-is.
pub fn detect_legacy_share(value: &Value) -> Option<LegacyShare> {
    let obj = value.as_object()?;
    if obj.contains_key("schemaVersion") && obj.contains_key("value") {
        return None;
    }
    let hits = |keys: &[&str]| keys.iter().filter(|k| obj.contains_key(**k)).count();
    if hits(&V1_SECTION_KEYS) >= 2 {
        Some(LegacyShare::V1Full)
    } else if hits(&V1_CURATED_KEYS) >= 2 {
        Some(LegacyShare::V1Curated)
    } else {
        None
    }
}

/// v1's load-time coercions, in place:
/// - `version < 2` is rejected (v1's own import gate);
/// - `COM.VHF`/`COM.UHF` kHz (`118300`) and MHz×100 (`11830`) ints become MHz;
/// - flat `ZONES.training_zones[*].boundary` point lists become one ring.
pub fn coerce_legacy_v1_full(value: &mut Value) -> Result<()> {
    if let Some(version) = value.get("version").and_then(Value::as_i64)
        && version < 2
    {
        return Err(ConfiguratorError::Migration(format!(
            "config file is v1 format version {version}; only version 2 or newer can be \
             imported — re-export it from the latest v1 manager first"
        )));
    }
    normalize_com_frequencies(value);
    nest_zone_boundaries(value);
    Ok(())
}

/// Values already in MHz range are left untouched (v1 re-rounded them;
/// skipping that keeps modern exports byte-identical).
fn normalize_com_frequencies(value: &mut Value) {
    let Some(com) = value.get_mut("COM") else {
        return;
    };
    for band in ["VHF", "UHF"] {
        let Some(arr) = com.get_mut(band).and_then(Value::as_array_mut) else {
            continue;
        };
        for item in arr {
            let Some(n) = item.as_f64() else { continue };
            if !n.is_finite() || n.abs() < 1_000.0 {
                continue;
            }
            let mhz = if n.abs() >= 100_000.0 {
                n / 1_000.0
            } else {
                n / 100.0
            };
            let rounded = (mhz * 1_000.0).round() / 1_000.0;
            if let Some(num) = serde_json::Number::from_f64(rounded) {
                *item = Value::Number(num);
            }
        }
    }
}

fn nest_zone_boundaries(value: &mut Value) {
    let Some(zones) = value
        .get_mut("ZONES")
        .and_then(|z| z.get_mut("training_zones"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for zone in zones {
        let Some(boundary) = zone.get_mut("boundary") else {
            continue;
        };
        let is_flat = boundary
            .as_array()
            .and_then(|b| b.first())
            .is_some_and(Value::is_object);
        if is_flat {
            let flat = boundary.take();
            *boundary = Value::Array(vec![flat]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_shapes() {
        let envelope = json!({ "schemaVersion": 1, "productId": "x", "value": {} });
        assert_eq!(detect_legacy_share(&envelope), None);
        let full = json!({ "version": 2, "MAIN": {}, "COM": {}, "DECLUTTER": {} });
        assert_eq!(detect_legacy_share(&full), Some(LegacyShare::V1Full));
        let curated = json!({ "aircraft_defaults": {}, "training_areas": [], "approaches": {} });
        assert_eq!(detect_legacy_share(&curated), Some(LegacyShare::V1Curated));
        assert_eq!(detect_legacy_share(&json!({ "foo": 1, "MAIN": 2 })), None);
        assert_eq!(detect_legacy_share(&json!([1, 2])), None);
        assert_eq!(detect_legacy_share(&json!("hi")), None);
    }

    #[test]
    fn rejects_pre_v2() {
        let mut v = json!({ "version": 1, "MAIN": {}, "COM": {} });
        assert!(matches!(
            coerce_legacy_v1_full(&mut v),
            Err(ConfiguratorError::Migration(_))
        ));
    }

    #[test]
    fn normalizes_khz_and_mhz100_com_values() {
        let mut v = json!({
            "version": 2,
            "COM": { "default_preset": 1, "VHF": [118300, 11830, 119.3], "UHF": [251000] }
        });
        coerce_legacy_v1_full(&mut v).unwrap();
        assert_eq!(v["COM"]["VHF"], json!([118.3, 118.3, 119.3]));
        assert_eq!(v["COM"]["UHF"], json!([251.0]));
    }

    #[test]
    fn leaves_mhz_range_values_byte_identical() {
        let mut v = json!({ "version": 2, "COM": { "VHF": [118, 118.3], "UHF": [] } });
        coerce_legacy_v1_full(&mut v).unwrap();
        assert_eq!(
            serde_json::to_string(&v["COM"]["VHF"]).unwrap(),
            "[118,118.3]"
        );
    }

    #[test]
    fn wraps_flat_boundaries_into_rings() {
        let mut v = json!({
            "version": 2,
            "ZONES": { "training_zones": [
                { "name": "flat", "boundary": [{ "lat": 1.0, "lon": 2.0 }] },
                { "name": "nested", "boundary": [[{ "lat": 3.0, "lon": 4.0 }]] },
                { "name": "empty", "boundary": [] }
            ], "no_fly_zones": [] }
        });
        coerce_legacy_v1_full(&mut v).unwrap();
        let zones = &v["ZONES"]["training_zones"];
        assert_eq!(zones[0]["boundary"], json!([[{ "lat": 1.0, "lon": 2.0 }]]));
        assert_eq!(zones[1]["boundary"], json!([[{ "lat": 3.0, "lon": 4.0 }]]));
        assert_eq!(zones[2]["boundary"], json!([]));
    }

    #[test]
    fn tolerates_missing_sections() {
        let mut v = json!({ "version": 2, "MAIN": {}, "IFF": {} });
        coerce_legacy_v1_full(&mut v).unwrap();
        assert_eq!(v, json!({ "version": 2, "MAIN": {}, "IFF": {} }));
    }
}
