//! Byte-for-byte parity with the T-38 `sendConfigToAircraft` payload the
//! WASM gauge consumes. The input is a v1 manager capture; the golden is the
//! current wire (flight plans in the v2 bundle shape), identical to v3's.
//! If this diverges the gauge refuses the payload: fix the pipeline, not the
//! fixture.

#![allow(clippy::unwrap_used, clippy::panic)]

use infinity_configurator::{EmitContext, Op, PickRing, parse_ops, run_pipeline};
use serde_json::{Value, json};

const INPUT: &str = include_str!("fixtures/t38c_send_input.json");
const GOLDEN: &str = include_str!("fixtures/t38c_send_payload.golden.json");
/// The capture's timestamp, the only runtime-variable part.
const FROZEN_NOW: &str = "2026-05-07T23:05:09.411Z";

fn t38_send_pipeline() -> Vec<Op> {
    vec![
        // The gauge's `set_pending_config` gate needs a numeric root
        // `version` (2 = current DTC wire) before it parses anything.
        Op::InjectConstant {
            path: "version".into(),
            value: json!(2),
        },
        Op::InjectTimestamp {
            path: "META.created_at".into(),
        },
        Op::InvertBoolStrings {
            path: "DECLUTTER.*".into(),
        },
        Op::InvertBoolStrings {
            path: "MFD_DECLUTTER.*".into(),
        },
        Op::ClampEnum {
            path: "APP.*.Approch_type".into(),
            allowed: vec!["SCA".into(), "ILS".into(), "VOR".into()],
            default: "VOR".into(),
        },
        Op::ExpandPolygonRings {
            path: "ZONES.training_zones[*].boundary".into(),
            rings_field: "rings".into(),
            pick_ring: PickRing::Largest,
        },
        Op::BuildFlightPlanBundleV2 {
            path: "FLIGHT_PLANS".into(),
            variant: "t38_classic".into(),
        },
    ]
}

fn emit(ops: &[Op]) -> String {
    let input: Value = serde_json::from_str(INPUT).unwrap();
    let ctx = EmitContext {
        now_iso: FROZEN_NOW.into(),
    };
    let out = run_pipeline(input, ops, &ctx).unwrap();
    // Sinks write `to_vec_pretty`; the golden adds a trailing newline.
    serde_json::to_string_pretty(&out).unwrap() + "\n"
}

fn golden() -> String {
    GOLDEN.replace("\r\n", "\n")
}

#[test]
fn t38_send_payload_is_byte_identical_to_golden() {
    let ours = emit(&t38_send_pipeline());
    if ours != golden() {
        let line = ours
            .lines()
            .zip(golden().lines())
            .position(|(a, b)| a != b)
            .map_or(0, |i| i + 1);
        panic!("emit output diverges from golden at line {line}");
    }
}

/// The same pipeline as a catalog author writes it in `outputs[].transforms`.
#[test]
fn catalog_json_pipeline_matches_golden() {
    let raw = json!([
        { "op": "injectConstant", "path": "version", "value": 2 },
        { "op": "injectTimestamp", "path": "META.created_at" },
        { "op": "invertBoolStrings", "path": "DECLUTTER.*" },
        { "op": "invertBoolStrings", "path": "MFD_DECLUTTER.*" },
        { "op": "clampEnum", "path": "APP.*.Approch_type",
          "allowed": ["SCA", "ILS", "VOR"], "default": "VOR" },
        { "op": "expandPolygonRings", "path": "ZONES.training_zones[*].boundary" },
        { "op": "buildFlightPlanBundleV2", "path": "FLIGHT_PLANS", "variant": "t38_classic" }
    ]);
    let ops = parse_ops(raw.as_array().unwrap()).unwrap();
    assert_eq!(ops, t38_send_pipeline());
    assert_eq!(emit(&ops), golden());
}
