//! The configurator schema contract (v3 `schema.ts`): groups of fields,
//! addressed by dotted paths into one JSON value tree. The schema is opaque
//! JSON so authors can add widget props without touching the contract;
//! these helpers read it leniently.

use serde_json::{Map, Value};

/// Resolves `"a.b.c"` against `value`. Array segments index by number, as
/// JS property access does. `None` when any segment is missing.
pub(crate) fn path_get<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return None;
    }
    path.split('.').try_fold(value, |cur, part| match cur {
        Value::Object(map) => map.get(part),
        Value::Array(items) => part.parse::<usize>().ok().and_then(|i| items.get(i)),
        _ => None,
    })
}

/// [`path_get`], cloned, with `Null` for a missing path.
pub(crate) fn path_value(value: &Value, path: &str) -> Value {
    path_get(value, path).cloned().unwrap_or(Value::Null)
}

/// Sets `path` to `next`, creating intermediate objects. As in v3, any
/// intermediate (or root) that is not an object is replaced by one. An
/// empty path replaces the whole value.
pub(crate) fn path_set(value: &mut Value, path: &str, next: Value) {
    if path.is_empty() {
        *value = next;
        return;
    }
    let mut cur = value;
    let mut parts = path.split('.').peekable();
    while let Some(part) = parts.next() {
        let Some(map) = object_mut(cur) else {
            return;
        };
        if parts.peek().is_none() {
            map.insert(part.to_owned(), next);
            return;
        }
        cur = map.entry(part.to_owned()).or_insert(Value::Null);
    }
}

/// `value` as a mutable object, replacing it with an empty one first if it
/// is anything else. Always `Some`.
pub(crate) fn object_mut(value: &mut Value) -> Option<&mut Map<String, Value>> {
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
    value.as_object_mut()
}

/// The path of a field inside a group, for field ids that may already
/// carry the group prefix.
pub(crate) fn field_path(group_id: &str, field_id: &str) -> String {
    if group_id.is_empty()
        || field_id == group_id
        || field_id
            .strip_prefix(group_id)
            .is_some_and(|rest| rest.starts_with('.'))
    {
        field_id.to_owned()
    } else {
        format!("{group_id}.{field_id}")
    }
}

fn join(group_path: &str, relative: &str) -> String {
    if group_path.is_empty() {
        relative.to_owned()
    } else {
        format!("{group_path}.{relative}")
    }
}

/// A sibling field's value relative to a group path: `("APP.APP_500",
/// "Airport_icao")` reads `APP.APP_500.Airport_icao`.
pub(crate) fn read_sibling<'a>(
    root: &'a Value,
    group_path: &str,
    relative: &str,
) -> Option<&'a Value> {
    path_get(root, &join(group_path, relative))
}

/// Writes several sibling fields relative to `group_path`.
pub(crate) fn write_siblings<I>(root: &mut Value, group_path: &str, updates: I)
where
    I: IntoIterator<Item = (String, Value)>,
{
    for (relative, value) in updates {
        path_set(root, &join(group_path, &relative), value);
    }
}

// ---- Lenient readers for schema nodes and values.

/// A string prop on a schema node.
pub(crate) fn str_prop<'a>(node: &'a Value, key: &str) -> Option<&'a str> {
    node.get(key).and_then(Value::as_str)
}

/// A numeric prop on a schema node.
pub(crate) fn num_prop(node: &Value, key: &str) -> Option<f64> {
    node.get(key).and_then(Value::as_f64)
}

/// A field's label, falling back to its id.
pub(crate) fn label_of(node: &Value) -> String {
    str_prop(node, "label")
        .or_else(|| str_prop(node, "title"))
        .or_else(|| str_prop(node, "id"))
        .unwrap_or_default()
        .to_owned()
}

/// The `fields` array of a group, or nothing.
pub(crate) fn fields_of(group: &Value) -> Vec<Value> {
    group
        .get("fields")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// A JS-like display of a scalar: strings bare, numbers without a
/// trailing `.0`.
pub(crate) fn display_scalar(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A number from a JSON value, also accepting numeric strings.
pub(crate) fn as_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// A JSON number for `n`, as an integer when it is one. Non-finite values
/// (which JSON cannot hold) become `0`.
pub(crate) fn number(n: f64) -> Value {
    if !n.is_finite() {
        return Value::from(0);
    }
    #[allow(clippy::cast_possible_truncation)]
    if n.fract() == 0.0 && n.abs() < 9.0e15 {
        return Value::from(n as i64);
    }
    Value::from(n)
}

/// The value at `key` of an object as a string, or empty.
pub(crate) fn str_at(value: &Value, key: &str) -> String {
    value.get(key).map(display_scalar).unwrap_or_default()
}

/// The value at `key` of an object as a number.
pub(crate) fn num_at(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(as_number)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn path_get_walks_objects_and_arrays() {
        let v = json!({ "a": { "b": [ { "c": 1 } ] } });
        assert_eq!(path_get(&v, "a.b.0.c"), Some(&json!(1)));
        assert_eq!(path_get(&v, "a.x"), None);
        assert_eq!(path_get(&v, ""), None);
        assert_eq!(path_get(&v, "a.b.c"), None);
        assert_eq!(path_value(&v, "nope"), Value::Null);
    }

    #[test]
    fn path_set_creates_and_replaces_intermediates() {
        let mut v = json!({ "a": 1, "keep": true });
        path_set(&mut v, "a.b.c", json!("x"));
        assert_eq!(v, json!({ "a": { "b": { "c": "x" } }, "keep": true }));

        let mut v = json!([1, 2]);
        path_set(&mut v, "k", json!(3));
        assert_eq!(v, json!({ "k": 3 }));

        let mut v = json!({ "list": [1] });
        path_set(&mut v, "list.0", json!(9));
        assert_eq!(
            v,
            json!({ "list": { "0": 9 } }),
            "arrays become objects, as v3"
        );

        let mut v = json!({});
        path_set(&mut v, "", json!(5));
        assert_eq!(v, json!(5));
    }

    #[test]
    fn field_path_respects_prefixed_ids() {
        assert_eq!(field_path("MAIN", "heading"), "MAIN.heading");
        assert_eq!(field_path("MAIN", "MAIN.heading"), "MAIN.heading");
        assert_eq!(field_path("MAIN", "MAIN"), "MAIN");
        assert_eq!(field_path("MAIN", "MAINX"), "MAIN.MAINX");
        assert_eq!(field_path("", "x"), "x");
    }

    #[test]
    fn siblings_read_and_write_relative_to_the_group() {
        let mut v = json!({ "APP": { "APP_500": { "Airport_icao": "KRND" } } });
        assert_eq!(
            read_sibling(&v, "APP.APP_500", "Airport_icao"),
            Some(&json!("KRND"))
        );
        write_siblings(
            &mut v,
            "APP.APP_500",
            [
                ("rwy_lat".to_owned(), json!(1.5)),
                ("x".to_owned(), json!(2)),
            ],
        );
        assert_eq!(v["APP"]["APP_500"]["rwy_lat"], json!(1.5));
        assert_eq!(v["APP"]["APP_500"]["Airport_icao"], json!("KRND"));
        write_siblings(&mut v, "", [("top".to_owned(), json!(true))]);
        assert_eq!(v["top"], json!(true));
    }

    #[test]
    fn numbers_round_trip_like_js() {
        assert_eq!(number(3.0), json!(3));
        assert_eq!(number(2.5), json!(2.5));
        assert_eq!(number(f64::NAN), json!(0));
        assert_eq!(display_scalar(&json!(3)), "3");
        assert_eq!(display_scalar(&json!("a")), "a");
        assert_eq!(as_number(&json!(" 4.5 ")), Some(4.5));
    }
}
