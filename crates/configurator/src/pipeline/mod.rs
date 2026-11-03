//! The output pipeline: path-targeted ops an `outputs[]` entry applies to the
//! stored value before it goes to its sink. Authors reference ops by their
//! `op` tag; the tags and field names are a catalog contract.

mod flight_plan;
mod polygon;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ConfiguratorError, Result};
use crate::path::{LeafKey, set_path_unchecked, walk_mut};

pub use flight_plan::FLIGHT_PLAN_WIRE_VERSION;

/// Runtime inputs to the ops. Passed in rather than read inside so goldens
/// can freeze the clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitContext {
    /// Written by [`Op::InjectTimestamp`]; see [`crate::now_iso8601_ms`].
    pub now_iso: String,
}

impl EmitContext {
    pub fn now() -> Self {
        Self {
            now_iso: crate::time::now_iso8601_ms(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PickRing {
    #[default]
    Largest,
    First,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum Op {
    /// Sets `path` to [`EmitContext::now_iso`], creating it if missing.
    InjectTimestamp { path: String },
    /// Sets `path` to `value`, creating it if missing. For wire fields the
    /// stored config doesn't carry, e.g. the T-38 DTC's root `version`, which
    /// the gauge checks before parsing anything else.
    InjectConstant { path: String, value: Value },
    /// Flips `"0"`/`"1"` string leaves. Anything else is left alone rather
    /// than coerced, since it signals a schema bug.
    InvertBoolStrings { path: String },
    /// Replaces leaves outside `allowed` (or non-strings) with `default`.
    ClampEnum {
        path: String,
        allowed: Vec<String>,
        default: String,
    },
    /// Turns a polygon (nested rings or legacy flat ring) into the dual wire
    /// shape older gauges need: `path` becomes one flat ring and a sibling
    /// `rings_field` holds every ring.
    ///
    /// v3 only honoured the snake_case spellings (its camelCase rename
    /// covered variant names, not fields, so `ringsField` was silently
    /// ignored); both are accepted now.
    #[serde(rename_all = "camelCase")]
    ExpandPolygonRings {
        path: String,
        #[serde(default = "default_rings_field", alias = "rings_field")]
        rings_field: String,
        #[serde(default, alias = "pick_ring")]
        pick_ring: PickRing,
    },
    /// Rewrites the legacy `{"00": {name, waypoints}}` flight-plan map into
    /// the gauge's `FlightPlanBundleV2`. See [`flight_plan`].
    BuildFlightPlanBundleV2 { path: String, variant: String },
}

fn default_rings_field() -> String {
    "rings".into()
}

/// Parses an output's opaque `transforms[]`.
pub fn parse_ops(raw: &[Value]) -> Result<Vec<Op>> {
    raw.iter()
        .map(|op| {
            Op::deserialize(op).map_err(|e| {
                ConfiguratorError::SchemaInvalid(format!("invalid output transform: {e}"))
            })
        })
        .collect()
}

/// Applies `ops` in order; each op sees the previous one's output.
pub fn run_pipeline(mut value: Value, ops: &[Op], ctx: &EmitContext) -> Result<Value> {
    for op in ops {
        apply(&mut value, op, ctx)?;
    }
    Ok(value)
}

fn apply(value: &mut Value, op: &Op, ctx: &EmitContext) -> Result<()> {
    match op {
        Op::InjectTimestamp { path } => {
            set_path_unchecked(value, path, Value::String(ctx.now_iso.clone()))
        }
        Op::InjectConstant { path, value: v } => set_path_unchecked(value, path, v.clone()),
        Op::InvertBoolStrings { path } => walk_mut(value, path, |parent, leaf| {
            let (map, key) = object_leaf(parent, leaf, "invertBoolStrings")?;
            let next = match map.get(key).and_then(Value::as_str) {
                Some("0") => "1",
                Some("1") => "0",
                _ => return Ok(()),
            };
            map.insert(key.to_owned(), next.into());
            Ok(())
        }),
        Op::ClampEnum {
            path,
            allowed,
            default,
        } => walk_mut(value, path, |parent, leaf| {
            let (map, key) = object_leaf(parent, leaf, "clampEnum")?;
            let cur = map.get(key).and_then(Value::as_str).unwrap_or("");
            if !allowed.iter().any(|a| a == cur) {
                map.insert(key.to_owned(), default.as_str().into());
            }
            Ok(())
        }),
        Op::ExpandPolygonRings {
            path,
            rings_field,
            pick_ring,
        } => walk_mut(value, path, |parent, leaf| {
            let (map, key) = object_leaf(parent, leaf, "expandPolygonRings")?;
            polygon::expand(map, key, rings_field, *pick_ring);
            Ok(())
        }),
        Op::BuildFlightPlanBundleV2 { path, variant } => flight_plan::build(value, path, variant),
    }
}

/// Every leaf op addresses an object key; array-index leaves are a
/// catalog mistake.
fn object_leaf<'a>(
    parent: &'a mut Value,
    leaf: &'a LeafKey,
    op: &str,
) -> Result<(&'a mut serde_json::Map<String, Value>, &'a str)> {
    let LeafKey::Key(key) = leaf else {
        return Err(ConfiguratorError::InvalidPath(format!(
            "{op}: array leaves not supported"
        )));
    };
    let map = parent
        .as_object_mut()
        .ok_or_else(|| ConfiguratorError::InvalidPath(format!("{op}: parent must be an object")))?;
    Ok((map, key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(value: Value, op: Op) -> Value {
        run_pipeline(
            value,
            &[op],
            &EmitContext {
                now_iso: "Z".into(),
            },
        )
        .unwrap()
    }

    #[test]
    fn inject_timestamp_creates_or_overwrites() {
        let op = || Op::InjectTimestamp {
            path: "META.created_at".into(),
        };
        assert_eq!(
            run(json!({}), op()),
            json!({ "META": { "created_at": "Z" } })
        );
        let existing = json!({ "META": { "created_at": "" } });
        assert_eq!(
            run(existing, op()),
            json!({ "META": { "created_at": "Z" } })
        );
    }

    #[test]
    fn inject_constant_creates_or_overwrites() {
        let op = || Op::InjectConstant {
            path: "version".into(),
            value: json!(2),
        };
        assert_eq!(run(json!({}), op()), json!({ "version": 2 }));
        assert_eq!(run(json!({ "version": 1 }), op()), json!({ "version": 2 }));
    }

    #[test]
    fn invert_bool_strings_flips_only_bool_strings() {
        let out = run(
            json!({ "D": { "a": "1", "b": "0", "c": 1, "d": "yes" } }),
            Op::InvertBoolStrings { path: "D.*".into() },
        );
        assert_eq!(
            out,
            json!({ "D": { "a": "0", "b": "1", "c": 1, "d": "yes" } })
        );
    }

    #[test]
    fn clamp_enum_replaces_outsiders() {
        let out = run(
            json!({ "APP": {
                "A": { "Approch_type": "ILS" },
                "B": { "Approch_type": "FOO" },
                "C": { "Approch_type": 3 }
            } }),
            Op::ClampEnum {
                path: "APP.*.Approch_type".into(),
                allowed: vec!["SCA".into(), "ILS".into(), "VOR".into()],
                default: "VOR".into(),
            },
        );
        assert_eq!(out["APP"]["A"]["Approch_type"], "ILS");
        assert_eq!(out["APP"]["B"]["Approch_type"], "VOR");
        assert_eq!(out["APP"]["C"]["Approch_type"], "VOR");
    }

    #[test]
    fn array_leaves_are_rejected() {
        let err = run_pipeline(
            json!({ "L": ["0", "1"] }),
            &[Op::InvertBoolStrings {
                path: "L[*]".into(),
            }],
            &EmitContext { now_iso: "".into() },
        );
        assert!(matches!(err, Err(ConfiguratorError::InvalidPath(_))));
    }

    #[test]
    fn ops_run_in_order() {
        let ops = vec![
            Op::InjectTimestamp {
                path: "META.created_at".into(),
            },
            Op::ClampEnum {
                path: "META.created_at".into(),
                allowed: vec!["allowed".into()],
                default: "fallback".into(),
            },
        ];
        let ctx = EmitContext {
            now_iso: "Z".into(),
        };
        let out = run_pipeline(json!({ "META": {} }), &ops, &ctx).unwrap();
        assert_eq!(out["META"]["created_at"], "fallback");
    }

    #[test]
    fn parses_catalog_wire_format() {
        let raw = json!([
            { "op": "injectConstant", "path": "version", "value": 2 },
            { "op": "injectTimestamp", "path": "META.created_at" },
            { "op": "invertBoolStrings", "path": "DECLUTTER.*" },
            { "op": "clampEnum", "path": "APP.*.Approch_type",
              "allowed": ["SCA","ILS","VOR"], "default": "VOR" },
            { "op": "expandPolygonRings", "path": "ZONES.training_zones[*].boundary",
              "ringsField": "rings", "pickRing": "largest" },
            { "op": "expandPolygonRings", "path": "x", "rings_field": "r", "pick_ring": "first" },
            { "op": "buildFlightPlanBundleV2", "path": "FLIGHT_PLANS",
              "variant": "t38_classic" }
        ]);
        let ops = parse_ops(raw.as_array().unwrap()).unwrap();
        assert_eq!(ops.len(), 7);
        assert_eq!(
            ops[5],
            Op::ExpandPolygonRings {
                path: "x".into(),
                rings_field: "r".into(),
                pick_ring: PickRing::First,
            }
        );
        let bad = parse_ops(&[json!({ "op": "nope" })]);
        assert!(matches!(bad, Err(ConfiguratorError::SchemaInvalid(_))));
    }
}
