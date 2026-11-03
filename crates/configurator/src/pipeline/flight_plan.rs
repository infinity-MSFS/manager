//! `buildFlightPlanBundleV2`: legacy HashMap-keyed flight plans to the
//! gauge's v2 bundle.
//!
//! | `variant`                      | payload `kind`   |
//! |--------------------------------|------------------|
//! | `t38_classic`, `t38_modern`    | `t38_steerpoint` |
//! | `uns1`                         | `uns1_waypoint`  |
//!
//! T-38 waypoints whose ident is a 3-digit 200–509 or 600–610 are
//! destination numbers; everything else is an ICAO ident. Payload fields not
//! in the legacy shape get the store-side defaults.

use serde_json::{Map, Value, json};

use crate::error::{ConfiguratorError, Result};
use crate::path::{get_pattern, set_path_unchecked};

pub const FLIGHT_PLAN_WIRE_VERSION: u32 = 2;

#[derive(Clone, Copy)]
enum PayloadKind {
    T38Steerpoint,
    Uns1Waypoint,
}

pub(super) fn build(root: &mut Value, path: &str, variant: &str) -> Result<()> {
    let kind = match variant {
        "t38_classic" | "t38_modern" => PayloadKind::T38Steerpoint,
        "uns1" => PayloadKind::Uns1Waypoint,
        other => {
            return Err(ConfiguratorError::InvalidPath(format!(
                "buildFlightPlanBundleV2: unknown variant `{other}`"
            )));
        }
    };

    // A missing or non-object section still emits an (empty) bundle so the
    // gauge always sees the v2 shape.
    let legacy = match get_pattern(root, path)? {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };

    let mut plans: Vec<(u64, Value)> = legacy
        .into_iter()
        .filter_map(|(key, plan)| {
            let index = parse_plan_index(&key)?;
            let plan = plan.as_object()?;
            let name = plan.get("name").and_then(Value::as_str).unwrap_or("");
            let waypoints: Vec<Value> = plan
                .get("waypoints")
                .and_then(Value::as_array)
                .map(|wps| wps.iter().filter_map(|wp| waypoint(wp, kind)).collect())
                .unwrap_or_default();
            Some((
                index,
                json!({ "index": index, "name": name, "waypoints": waypoints }),
            ))
        })
        .collect();
    // Stable sort by index so equal configs give byte-identical output.
    plans.sort_by_key(|(index, _)| *index);
    let plans: Vec<Value> = plans.into_iter().map(|(_, p)| p).collect();

    let bundle = json!({
        "version": FLIGHT_PLAN_WIRE_VERSION,
        "variant": variant,
        "plans": plans,
    });
    set_path_unchecked(root, path, bundle)
}

/// `"00"` -> 0, `"FPL_3"` -> 3, `"FLIGHT_PLAN_07"` -> 7; every digit in the
/// key counts. No digits means the entry isn't a plan.
fn parse_plan_index(key: &str) -> Option<u64> {
    let digits: String = key.chars().filter(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn waypoint(wp: &Value, kind: PayloadKind) -> Option<Value> {
    let ident = wp.get("ident").and_then(Value::as_str).unwrap_or("").trim();
    if ident.is_empty() {
        return None;
    }
    let payload = match kind {
        PayloadKind::T38Steerpoint => {
            let (source, destination) = match t38_destination(ident) {
                Some(n) => ("destination", n),
                None => ("icao", 0),
            };
            json!({
                "kind": "t38_steerpoint",
                "source": source,
                "destination": destination,
                "assigned_dest_number": 0,
                "tot": -1,
                "steering": "fby",
            })
        }
        PayloadKind::Uns1Waypoint => json!({
            "kind": "uns1_waypoint",
            "role": "enroute",
            "leg": "direct",
            "vnav": {
                "alt_at_ft": null,
                "alt_above_ft": null,
                "alt_below_ft": null,
                "speed_kts": null,
            },
            "procedure": "",
        }),
    };
    Some(json!({
        "ident": ident,
        "lat": wp.get("lat").and_then(Value::as_f64),
        "lon": wp.get("lon").and_then(Value::as_f64),
        "alt_ft": wp.get("alt_ft").and_then(Value::as_i64),
        "payload": payload,
    }))
}

fn t38_destination(ident: &str) -> Option<i64> {
    if ident.len() != 3 || !ident.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: i64 = ident.parse().ok()?;
    ((200..=509).contains(&n) || (600..=610).contains(&n)).then_some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: Value, variant: &str) -> Value {
        let mut v = input;
        build(&mut v, "FLIGHT_PLANS", variant).unwrap();
        v
    }

    #[test]
    fn icao_waypoints() {
        let out = run(
            json!({ "FLIGHT_PLANS": { "00": { "name": "Flight Plan 00", "waypoints": [
                { "ident": "KABQ", "lat": 35.04, "lon": -106.61 },
                { "ident": "  ", "lat": 0.0, "lon": 0.0 }
            ] } } }),
            "t38_classic",
        );
        assert_eq!(
            out["FLIGHT_PLANS"],
            json!({ "version": 2, "variant": "t38_classic", "plans": [{
                "index": 0, "name": "Flight Plan 00", "waypoints": [{
                    "ident": "KABQ", "lat": 35.04, "lon": -106.61, "alt_ft": null,
                    "payload": {
                        "kind": "t38_steerpoint", "source": "icao", "destination": 0,
                        "assigned_dest_number": 0, "tot": -1, "steering": "fby"
                    }
                }]
            }] })
        );
    }

    #[test]
    fn destination_idents_decode() {
        let out = run(
            json!({ "FLIGHT_PLANS": { "00": { "name": "d", "waypoints": [
                { "ident": "201" }, { "ident": "605" }, { "ident": "199" }, { "ident": "611" }
            ] } } }),
            "t38_classic",
        );
        let wps = &out["FLIGHT_PLANS"]["plans"][0]["waypoints"];
        assert_eq!(wps[0]["payload"]["source"], "destination");
        assert_eq!(wps[0]["payload"]["destination"], 201);
        assert_eq!(wps[1]["payload"]["destination"], 605);
        assert_eq!(wps[2]["payload"]["source"], "icao");
        assert_eq!(wps[3]["payload"]["source"], "icao");
    }

    #[test]
    fn plans_sorted_by_index_with_varied_keys() {
        let out = run(
            json!({ "FLIGHT_PLANS": {
                "FPL_3": { "name": "three", "waypoints": [] },
                "00": { "name": "zero", "waypoints": [] },
                "notes": { "name": "skipped" },
                "FLIGHT_PLAN_07": { "name": "seven", "waypoints": [] }
            } }),
            "t38_classic",
        );
        let names: Vec<&str> = out["FLIGHT_PLANS"]["plans"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["zero", "three", "seven"]);
        assert_eq!(out["FLIGHT_PLANS"]["plans"][2]["index"], 7);
    }

    #[test]
    fn missing_section_emits_empty_bundle() {
        let out = run(json!({}), "t38_modern");
        assert_eq!(
            out["FLIGHT_PLANS"],
            json!({ "version": 2, "variant": "t38_modern", "plans": [] })
        );
    }

    #[test]
    fn uns1_variant() {
        let out = run(
            json!({ "FLIGHT_PLANS": { "00": { "name": "u", "waypoints": [{ "ident": "BULME" }] } } }),
            "uns1",
        );
        let payload = &out["FLIGHT_PLANS"]["plans"][0]["waypoints"][0]["payload"];
        assert_eq!(payload["kind"], "uns1_waypoint");
        assert_eq!(payload["role"], "enroute");
        assert_eq!(payload["vnav"]["speed_kts"], Value::Null);
    }

    #[test]
    fn unknown_variant_errors() {
        let mut v = json!({ "FLIGHT_PLANS": {} });
        assert!(matches!(
            build(&mut v, "FLIGHT_PLANS", "nope"),
            Err(ConfiguratorError::InvalidPath(_))
        ));
    }
}
