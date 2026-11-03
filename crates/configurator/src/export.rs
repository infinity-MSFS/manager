//! Schema-driven export: applies each field's `exportTransform` to the
//! stored value, producing what the deprecated `exportFile` sink writes.
//!
//! Only top-level groups with `fields[]` are walked (the T-38 shape); `list`
//! item schemas and `dependsOn` chains are not, as in v3. Fields missing from
//! the value are left alone.

use serde_json::Value;

use crate::error::{ConfiguratorError, Result};
use crate::path::{get_dotted, set_dotted};
use crate::transforms::apply_transform;

pub fn apply_export_transforms(schema: &Value, mut value: Value) -> Result<Value> {
    let groups = schema.as_array().ok_or_else(|| {
        ConfiguratorError::SchemaInvalid("schema top-level must be an array".into())
    })?;

    for group in groups.iter().filter_map(Value::as_object) {
        let group_id = group
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| ConfiguratorError::SchemaInvalid("group missing id".into()))?;
        let Some(fields) = group.get("fields").and_then(Value::as_array) else {
            continue;
        };
        for field in fields {
            let (Some(field_id), Some(transform)) = (
                field.get("id").and_then(Value::as_str),
                field.get("exportTransform").and_then(Value::as_str),
            ) else {
                continue;
            };
            let path = field_path(group_id, field_id);
            if let Some(current) = get_dotted(&value, &path) {
                let new = apply_transform(transform, current)?;
                set_dotted(&mut value, &path, new)?;
            }
        }
    }
    Ok(value)
}

/// Authors write both `heading_bug` and `MAIN.heading_bug` inside group
/// `MAIN`; accept either.
fn field_path(group_id: &str, field_id: &str) -> String {
    let qualified = field_id == group_id
        || field_id
            .strip_prefix(group_id)
            .is_some_and(|rest| rest.starts_with('.'));
    if qualified {
        field_id.to_owned()
    } else {
        format!("{group_id}.{field_id}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn com_schema(transform: &str) -> Value {
        json!([{ "id": "COM", "fields": [
            { "id": "ils_freq", "type": "freq", "exportTransform": transform },
            { "id": "COM.uhf", "type": "freq", "exportTransform": transform },
            { "id": "plain", "type": "int" }
        ] }])
    }

    #[test]
    fn applies_transforms_to_short_and_qualified_ids() {
        let value = json!({ "COM": { "ils_freq": 122.4, "uhf": 251.0, "plain": 1 } });
        let out = apply_export_transforms(&com_schema("mhzToKhzInt"), value).unwrap();
        assert_eq!(
            out,
            json!({ "COM": { "ils_freq": 122_400, "uhf": 251_000, "plain": 1 } })
        );
    }

    #[test]
    fn ignores_missing_paths() {
        let value = json!({ "MAIN": { "x": 1 } });
        let out = apply_export_transforms(&com_schema("mhzToKhzInt"), value.clone()).unwrap();
        assert_eq!(out, value);
    }

    #[test]
    fn unknown_transform_errors() {
        let value = json!({ "COM": { "ils_freq": 1 } });
        assert!(matches!(
            apply_export_transforms(&com_schema("noSuchThing"), value),
            Err(ConfiguratorError::UnknownTransform(_))
        ));
    }

    #[test]
    fn field_paths() {
        assert_eq!(field_path("MAIN", "x"), "MAIN.x");
        assert_eq!(field_path("MAIN", "MAIN.x"), "MAIN.x");
        assert_eq!(field_path("MAIN", "MAINx"), "MAIN.MAINx");
        assert_eq!(field_path("MAIN", "MAIN"), "MAIN");
    }
}
