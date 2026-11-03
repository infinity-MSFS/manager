//! v1 `.infinity` import against the captured v1 export. Modern v1 exports
//! already store MHz COM floats and nested rings, so coercion must be a
//! byte-identical no-op on the fixture; an "old" variant with legacy COM
//! encodings and flat boundaries must coerce back to the fixture's shape.

#![allow(clippy::unwrap_used, clippy::panic)]

use infinity_configurator::{LegacyShare, coerce_legacy_v1_full, detect_legacy_share};
use serde_json::Value;

const V1_EXPORT: &str = include_str!("fixtures/t38c_send_input.json");

fn fixture() -> Value {
    serde_json::from_str(V1_EXPORT).unwrap()
}

#[test]
fn v1_full_export_detects_and_coerces_identically() {
    let original = fixture();
    assert_eq!(detect_legacy_share(&original), Some(LegacyShare::V1Full));

    let mut coerced = original.clone();
    coerce_legacy_v1_full(&mut coerced).unwrap();
    assert_eq!(
        serde_json::to_string(&coerced).unwrap(),
        serde_json::to_string(&original).unwrap()
    );
}

#[test]
fn older_v1_encodings_coerce_to_modern_shape() {
    let mut old = fixture();
    for item in old["COM"]["VHF"].as_array_mut().unwrap() {
        *item = Value::from((item.as_f64().unwrap() * 1_000.0).round() as i64);
    }
    for zone in old["ZONES"]["training_zones"].as_array_mut().unwrap() {
        zone["boundary"] = zone["boundary"][0].clone();
    }

    coerce_legacy_v1_full(&mut old).unwrap();

    let modern = fixture();
    let ours = old["COM"]["VHF"].as_array().unwrap();
    let theirs = modern["COM"]["VHF"].as_array().unwrap();
    assert_eq!(ours.len(), theirs.len());
    for (a, b) in ours.iter().zip(theirs) {
        assert_eq!(a.as_f64(), b.as_f64());
    }
    let ours = old["ZONES"]["training_zones"].as_array().unwrap();
    let theirs = modern["ZONES"]["training_zones"].as_array().unwrap();
    assert_eq!(ours.len(), theirs.len());
    for (a, b) in ours.iter().zip(theirs) {
        assert_eq!(a["boundary"].as_array().unwrap().len(), 1);
        assert_eq!(a["boundary"][0], b["boundary"][0]);
    }
}
