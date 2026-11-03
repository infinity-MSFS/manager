//! Schema-version migrations: a contiguous `from -> from + 1` ladder of
//! declarative ops that brings a stored config up to the catalog's version.
//!
//! ```jsonc
//! [{ "from": 1, "to": 2, "ops": [
//!     { "op": "rename", "path": "APP.APP_500.Approch_type", "to": "APP.APP_500.approach_type" },
//!     { "op": "defaultIfMissing", "path": "MAIN.declutter_level", "value": 0 }
//! ] }]
//! ```
//!
//! Paths are literal dotted keys (see [`crate::path`]).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ConfiguratorError, Result};
use crate::path::{get_dotted, set_dotted, take_dotted};
use crate::transforms::apply_transform;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Migration {
    pub from: u32,
    pub to: u32,
    pub ops: Vec<MigrationOp>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum MigrationOp {
    Rename {
        path: String,
        to: String,
    },
    Remove {
        path: String,
    },
    DefaultIfMissing {
        path: String,
        value: Value,
    },
    RemoveIf {
        path: String,
        equals: Value,
    },
    /// Runs a named [`crate::transforms`] function on the value at `path`.
    Transform {
        path: String,
        transform: String,
    },
}

/// Parses a catalog's opaque `migrations[]`.
pub fn parse_migrations(raw: &[Value]) -> Result<Vec<Migration>> {
    raw.iter()
        .map(|m| {
            Migration::deserialize(m).map_err(|e| {
                ConfiguratorError::SchemaInvalid(format!("invalid migration entry: {e}"))
            })
        })
        .collect()
}

/// Steps `value` from `from_version` to `target_version`. Downward
/// migrations, gaps in the ladder and steps that skip a version are errors;
/// a stored config must never be half-migrated.
pub fn migrate(
    value: Value,
    migrations: &[Migration],
    from_version: u32,
    target_version: u32,
) -> Result<Value> {
    if from_version == target_version {
        return Ok(value);
    }
    if from_version > target_version {
        return Err(fail(format!(
            "downward migration not supported: {from_version} → {target_version}"
        )));
    }

    let mut sorted: Vec<&Migration> = migrations.iter().collect();
    sorted.sort_by_key(|m| m.from);

    let mut current = value;
    let mut at = from_version;
    for m in sorted {
        if m.from < at {
            continue;
        }
        if m.from > at {
            return Err(fail(format!(
                "missing migration step: have {at}, next available is {}→{}",
                m.from, m.to
            )));
        }
        if m.to != m.from + 1 {
            return Err(fail(format!(
                "migration must step by 1: {} → {}",
                m.from, m.to
            )));
        }
        for op in &m.ops {
            apply(&mut current, op)?;
        }
        at = m.to;
        if at == target_version {
            return Ok(current);
        }
    }

    Err(fail(format!(
        "no migration path from {from_version} to {target_version} (stopped at {at})"
    )))
}

fn apply(value: &mut Value, op: &MigrationOp) -> Result<()> {
    match op {
        MigrationOp::Rename { path, to } => {
            if let Some(v) = take_dotted(value, path)? {
                set_dotted(value, to, v)?;
            }
        }
        MigrationOp::Remove { path } => {
            take_dotted(value, path)?;
        }
        MigrationOp::DefaultIfMissing {
            path,
            value: default,
        } => {
            if get_dotted(value, path).is_none() {
                set_dotted(value, path, default.clone())?;
            }
        }
        MigrationOp::RemoveIf { path, equals } => {
            if get_dotted(value, path) == Some(equals) {
                take_dotted(value, path)?;
            }
        }
        MigrationOp::Transform { path, transform } => match get_dotted(value, path) {
            Some(current) => {
                let new = apply_transform(transform, current)?;
                set_dotted(value, path, new)?;
            }
            None => tracing::warn!(%path, transform, "migrate: transform target missing, skipped"),
        },
    }
    Ok(())
}

fn fail(msg: String) -> ConfiguratorError {
    ConfiguratorError::Migration(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn step(from: u32, ops: Vec<MigrationOp>) -> Migration {
        Migration {
            from,
            to: from + 1,
            ops,
        }
    }

    fn rename(path: &str, to: &str) -> MigrationOp {
        MigrationOp::Rename {
            path: path.into(),
            to: to.into(),
        }
    }

    #[test]
    fn rename_moves_value() {
        let value = json!({ "APP": { "APP_500": { "Approch_type": "Stored" } } });
        let m = step(
            1,
            vec![rename(
                "APP.APP_500.Approch_type",
                "APP.APP_500.approach_type",
            )],
        );
        let out = migrate(value, &[m], 1, 2).unwrap();
        assert_eq!(
            out,
            json!({ "APP": { "APP_500": { "approach_type": "Stored" } } })
        );
    }

    #[test]
    fn default_if_missing_inserts_but_keeps_existing() {
        let m = step(
            1,
            vec![MigrationOp::DefaultIfMissing {
                path: "MAIN.lvl".into(),
                value: json!(0),
            }],
        );
        let out = migrate(json!({ "MAIN": {} }), std::slice::from_ref(&m), 1, 2).unwrap();
        assert_eq!(out["MAIN"]["lvl"], 0);
        let out = migrate(json!({ "MAIN": { "lvl": 3 } }), &[m], 1, 2).unwrap();
        assert_eq!(out["MAIN"]["lvl"], 3);
    }

    #[test]
    fn remove_if_remove_and_transform() {
        let m = step(
            1,
            vec![
                MigrationOp::RemoveIf {
                    path: "a".into(),
                    equals: json!("x"),
                },
                MigrationOp::RemoveIf {
                    path: "b".into(),
                    equals: json!("x"),
                },
                MigrationOp::Remove { path: "c".into() },
                MigrationOp::Transform {
                    path: "f".into(),
                    transform: "mhzToKhzInt".into(),
                },
                MigrationOp::Transform {
                    path: "missing".into(),
                    transform: "boolToInt".into(),
                },
            ],
        );
        let input = json!({ "a": "x", "b": "y", "c": 1, "f": 122.4 });
        let out = migrate(input, &[m], 1, 2).unwrap();
        assert_eq!(out, json!({ "b": "y", "f": 122_400 }));
    }

    #[test]
    fn ladder_runs_in_order_regardless_of_declaration_order() {
        let migrations = vec![
            step(2, vec![rename("b", "c")]),
            step(1, vec![rename("a", "b")]),
        ];
        let out = migrate(json!({ "a": 7 }), &migrations, 1, 3).unwrap();
        assert_eq!(out, json!({ "c": 7 }));
    }

    #[test]
    fn broken_ladders_error() {
        let is_migration = |r: Result<Value>| matches!(r, Err(ConfiguratorError::Migration(_)));
        assert!(is_migration(migrate(json!({}), &[step(2, vec![])], 1, 3)));
        assert!(is_migration(migrate(json!({}), &[], 3, 1)));
        let skip = Migration {
            from: 1,
            to: 3,
            ops: vec![],
        };
        assert!(is_migration(migrate(json!({}), &[skip], 1, 3)));
        assert!(is_migration(migrate(json!({}), &[step(1, vec![])], 1, 3)));
    }

    #[test]
    fn no_op_when_already_at_target() {
        let value = json!({ "x": 1 });
        assert_eq!(migrate(value.clone(), &[], 5, 5).unwrap(), value);
    }

    #[test]
    fn parses_catalog_wire_format() {
        let raw = vec![json!({ "from": 1, "to": 2, "ops": [
            { "op": "rename", "path": "a", "to": "b" },
            { "op": "defaultIfMissing", "path": "c", "value": 0 },
            { "op": "removeIf", "path": "d", "equals": false },
            { "op": "remove", "path": "e" },
            { "op": "transform", "path": "f", "transform": "boolToInt" }
        ] })];
        assert_eq!(parse_migrations(&raw).unwrap()[0].ops.len(), 5);
        let bad = parse_migrations(&[json!({ "from": 1 })]);
        assert!(matches!(bad, Err(ConfiguratorError::SchemaInvalid(_))));
    }
}
