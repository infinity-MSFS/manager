//! Schema sanity checks.
//!
//! Deliberately shallow, as in v3: the schema must be an array of nodes with
//! non-empty string `id`s, and a group's `fields[]` must have unique string
//! ids. Widget props, `dependsOn` references and value bounds are the
//! renderer's business; rejecting them here would break catalogs v3 accepts.

use std::collections::HashSet;

use serde_json::Value;

use crate::error::{ConfiguratorError, Result};

pub fn validate_schema(schema: &Value) -> Result<()> {
    let nodes = schema
        .as_array()
        .ok_or_else(|| invalid("top-level schema must be an array".into()))?;
    for (i, node) in nodes.iter().enumerate() {
        validate_node(node, &format!("[{i}]"))?;
    }
    Ok(())
}

fn validate_node(node: &Value, at: &str) -> Result<()> {
    let obj = node
        .as_object()
        .ok_or_else(|| invalid(format!("{at}: each schema node must be an object")))?;
    let id = obj
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("{at}: missing string `id`")))?;
    if id.is_empty() {
        return Err(invalid(format!("{at}: empty `id`")));
    }

    let Some(fields) = obj.get("fields").and_then(Value::as_array) else {
        return Ok(());
    };
    let mut seen = HashSet::new();
    for (j, child) in fields.iter().enumerate() {
        let cid = child
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid(format!("{at}.fields[{j}]: missing string `id`")))?;
        if !seen.insert(cid) {
            return Err(invalid(format!("{at}: duplicate field id `{cid}`")));
        }
    }
    Ok(())
}

fn invalid(msg: String) -> ConfiguratorError {
    ConfiguratorError::SchemaInvalid(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rejects(schema: Value) -> bool {
        matches!(
            validate_schema(&schema),
            Err(ConfiguratorError::SchemaInvalid(_))
        )
    }

    #[test]
    fn accepts_minimal_schema_and_custom_pages() {
        validate_schema(&json!([
            { "id": "MAIN", "title": "Main", "fields": [
                { "id": "heading_bug", "type": "int" },
                { "id": "ils_freq", "type": "freq" }
            ] },
            { "id": "FLIGHT_PLANS", "page": "flightPlans" }
        ]))
        .unwrap();
    }

    #[test]
    fn rejects_bad_shapes() {
        assert!(rejects(json!({})));
        assert!(rejects(json!(["MAIN"])));
        assert!(rejects(json!([{ "title": "no id" }])));
        assert!(rejects(json!([{ "id": "" }])));
        assert!(rejects(
            json!([{ "id": "M", "fields": [{ "type": "int" }] }])
        ));
        assert!(rejects(json!([{ "id": "M", "fields": [
            { "id": "x", "type": "int" },
            { "id": "x", "type": "bool" }
        ] }])));
    }
}
