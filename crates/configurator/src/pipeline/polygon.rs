use serde_json::{Map, Value};

use super::PickRing;

/// Rewrites `map[key]` to the chosen flat ring and adds `rings_field` (all
/// rings, nested) right after it. Missing, empty or non-array values are
/// left alone.
pub(super) fn expand(map: &mut Map<String, Value>, key: &str, rings_field: &str, pick: PickRing) {
    let Some(arr) = map.get(key).and_then(Value::as_array) else {
        return;
    };
    let Some(first) = arr.first() else {
        return;
    };
    let rings: Vec<Vec<Value>> = if first.is_array() {
        arr.iter().filter_map(|v| v.as_array().cloned()).collect()
    } else {
        vec![arr.clone()]
    };

    let chosen = match pick {
        PickRing::First => rings.first(),
        // `max_by_key` keeps the *last* of equal-length rings; v3 did the
        // same and the wire output must not change.
        PickRing::Largest => rings.iter().max_by_key(|r| r.len()),
    }
    .cloned()
    .unwrap_or_default();

    map.insert(key.to_owned(), Value::Array(chosen));
    let nested = Value::Array(rings.into_iter().map(Value::Array).collect());
    insert_after(map, key, rings_field, nested);
}

/// Inserts `new_key` directly after `anchor`, matching v1's wire order
/// (`…, boundary, rings`). `serde_json::Map` has no positional insert, so
/// the map is rebuilt; an existing `new_key` is updated in place.
fn insert_after(map: &mut Map<String, Value>, anchor: &str, new_key: &str, new_val: Value) {
    if let Some(slot) = map.get_mut(new_key) {
        *slot = new_val;
        return;
    }
    let mut new_val = Some(new_val);
    let mut rebuilt = Map::with_capacity(map.len() + 1);
    for (k, v) in std::mem::take(map) {
        let is_anchor = k == anchor;
        rebuilt.insert(k, v);
        if is_anchor && let Some(val) = new_val.take() {
            rebuilt.insert(new_key.to_owned(), val);
        }
    }
    *map = rebuilt;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn expand_zone(zone: Value, pick: PickRing) -> Value {
        let Value::Object(mut map) = zone else {
            panic!("zone must be an object")
        };
        expand(&mut map, "boundary", "rings", pick);
        Value::Object(map)
    }

    #[test]
    fn nested_input_picks_largest_ring() {
        let a = json!([{ "lat": 0.0 }, { "lat": 1.0 }]);
        let b = json!([{ "lat": 2.0 }, { "lat": 3.0 }, { "lat": 4.0 }]);
        let out = expand_zone(json!({ "boundary": [a, b] }), PickRing::Largest);
        assert_eq!(out["boundary"], b);
        assert_eq!(out["rings"], json!([a, b]));
        let out = expand_zone(json!({ "boundary": [a, b] }), PickRing::First);
        assert_eq!(out["boundary"], a);
    }

    #[test]
    fn flat_input_is_wrapped() {
        let ring = json!([{ "lat": 0.0 }, { "lat": 1.0 }]);
        let out = expand_zone(json!({ "boundary": ring }), PickRing::Largest);
        assert_eq!(out["boundary"], ring);
        assert_eq!(out["rings"], json!([ring]));
    }

    #[test]
    fn rings_field_follows_boundary() {
        let out = expand_zone(
            json!({ "color": "y", "boundary": [[{ "lat": 0.0 }]], "name": "z" }),
            PickRing::Largest,
        );
        let keys: Vec<&str> = out
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["color", "boundary", "rings", "name"]);
    }

    #[test]
    fn empty_or_missing_boundary_is_untouched() {
        let zone = json!({ "boundary": [], "other": 1 });
        assert_eq!(expand_zone(zone.clone(), PickRing::Largest), zone);
        let zone = json!({ "other": 1 });
        assert_eq!(expand_zone(zone.clone(), PickRing::Largest), zone);
    }
}
