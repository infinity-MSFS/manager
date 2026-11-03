//! The two path syntaxes catalog authors write.
//!
//! **Patterns** (output pipeline ops) are dotted segments plus `*` for every
//! key of an object and `[*]` for every index of an array:
//!
//! - `META.created_at`, `DECLUTTER.*`, `APP.*.Approch_type`
//! - `ZONES.training_zones[*].boundary`
//!
//! Walks are best-effort: a step that doesn't match the current shape ends
//! that branch silently, because optional sections are normal.
//!
//! **Dotted paths** (migrations, export transforms) are plain `a.b.c` keys
//! with no wildcard meaning; v3 treated `*` and `[` literally there and
//! existing catalogs rely on that, so the two stay separate.

use serde_json::{Map, Value};

use crate::error::{ConfiguratorError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathStep {
    Key(String),
    /// `*`
    WildKey,
    /// `[*]`
    WildIdx,
}

/// The concrete leaf a pattern walk landed on, relative to its parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeafKey {
    Key(String),
    Idx(usize),
}

pub fn parse_path(path: &str) -> Result<Vec<PathStep>> {
    if path.is_empty() {
        return Err(invalid("empty"));
    }
    let mut out = Vec::new();
    for seg in path.split('.') {
        if seg.is_empty() {
            return Err(invalid(format!("empty segment in `{path}`")));
        }
        let (head, has_idx) = match seg.find('[') {
            Some(pos) => {
                let (h, t) = seg.split_at(pos);
                if t != "[*]" {
                    return Err(invalid(format!(
                        "only `[*]` array index supported, got `{t}` in `{path}`"
                    )));
                }
                (h, true)
            }
            None => (seg, false),
        };
        match head {
            "" => {}
            "*" => out.push(PathStep::WildKey),
            k => out.push(PathStep::Key(k.to_owned())),
        }
        if has_idx {
            out.push(PathStep::WildIdx);
        }
    }
    Ok(out)
}

/// Calls `f(parent, leaf)` for every leaf `pattern` matches. The callback
/// gets the parent container so it can replace the leaf *and* write siblings
/// (polygon expansion adds a `rings` field next to `boundary`).
pub fn walk_mut<F>(root: &mut Value, pattern: &str, mut f: F) -> Result<()>
where
    F: FnMut(&mut Value, &LeafKey) -> Result<()>,
{
    let steps = parse_path(pattern)?;
    if steps.is_empty() {
        return Err(invalid("empty steps"));
    }
    walk_inner(root, &steps, &mut f)
}

fn walk_inner<F>(parent: &mut Value, steps: &[PathStep], f: &mut F) -> Result<()>
where
    F: FnMut(&mut Value, &LeafKey) -> Result<()>,
{
    let Some((head, rest)) = steps.split_first() else {
        return Ok(());
    };
    let is_leaf = rest.is_empty();
    match head {
        PathStep::Key(k) if is_leaf => {
            if parent.as_object().is_some_and(|m| m.contains_key(k)) {
                f(parent, &LeafKey::Key(k.clone()))?;
            }
        }
        PathStep::Key(k) => {
            if let Some(child) = parent.get_mut(k.as_str()) {
                walk_inner(child, rest, f)?;
            }
        }
        PathStep::WildKey if is_leaf => {
            let keys: Vec<String> = parent
                .as_object()
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            for k in keys {
                f(parent, &LeafKey::Key(k))?;
            }
        }
        PathStep::WildKey => {
            if let Some(map) = parent.as_object_mut() {
                for child in map.values_mut() {
                    walk_inner(child, rest, f)?;
                }
            }
        }
        PathStep::WildIdx if is_leaf => {
            let len = parent.as_array().map_or(0, Vec::len);
            for i in 0..len {
                f(parent, &LeafKey::Idx(i))?;
            }
        }
        PathStep::WildIdx => {
            if let Some(arr) = parent.as_array_mut() {
                for child in arr {
                    walk_inner(child, rest, f)?;
                }
            }
        }
    }
    Ok(())
}

/// Sets a wildcard-free pattern, creating intermediate objects (and
/// replacing non-object intermediates) as needed.
pub fn set_path_unchecked(root: &mut Value, pattern: &str, new: Value) -> Result<()> {
    let mut keys = Vec::new();
    for step in parse_path(pattern)? {
        match step {
            PathStep::Key(k) => keys.push(k),
            PathStep::WildKey | PathStep::WildIdx => {
                return Err(invalid(format!(
                    "set_path_unchecked: wildcard not allowed in `{pattern}`"
                )));
            }
        }
    }
    set_keys(root, keys, new)
}

/// Reads a wildcard-free pattern; `None` if any step is missing.
pub(crate) fn get_pattern<'a>(root: &'a Value, pattern: &str) -> Result<Option<&'a Value>> {
    let mut cur = root;
    for step in parse_path(pattern)? {
        let PathStep::Key(k) = step else {
            return Ok(None);
        };
        match cur.get(k.as_str()) {
            Some(next) => cur = next,
            None => return Ok(None),
        }
    }
    Ok(Some(cur))
}

pub(crate) fn get_dotted<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return None;
    }
    path.split('.')
        .try_fold(root, |cur, key| cur.as_object()?.get(key))
}

pub(crate) fn set_dotted(root: &mut Value, path: &str, new: Value) -> Result<()> {
    if path.is_empty() {
        return Err(invalid("empty"));
    }
    set_keys(root, path.split('.').map(str::to_owned), new)
}

pub(crate) fn take_dotted(root: &mut Value, path: &str) -> Result<Option<Value>> {
    if path.is_empty() {
        return Err(invalid("empty"));
    }
    let (parents, last) = match path.rsplit_once('.') {
        Some((p, l)) => (Some(p), l),
        None => (None, path),
    };
    let mut cur = root;
    for key in parents.into_iter().flat_map(|p| p.split('.')) {
        match cur.get_mut(key) {
            Some(next) => cur = next,
            None => return Ok(None),
        }
    }
    Ok(cur.as_object_mut().and_then(|m| m.remove(last)))
}

fn set_keys(root: &mut Value, keys: impl IntoIterator<Item = String>, new: Value) -> Result<()> {
    let mut keys = keys.into_iter().peekable();
    let mut cur = root;
    while let Some(key) = keys.next() {
        if !cur.is_object() {
            *cur = Value::Object(Map::new());
        }
        let Value::Object(map) = cur else {
            return Err(invalid(format!("not an object at `{key}`")));
        };
        if keys.peek().is_none() {
            map.insert(key, new);
            return Ok(());
        }
        cur = map.entry(key).or_insert_with(|| Value::Object(Map::new()));
    }
    Err(invalid("empty"))
}

fn invalid(msg: impl Into<String>) -> ConfiguratorError {
    ConfiguratorError::InvalidPath(msg.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_simple_and_wildcard_paths() {
        assert_eq!(
            parse_path("META.created_at").unwrap(),
            vec![
                PathStep::Key("META".into()),
                PathStep::Key("created_at".into())
            ]
        );
        assert_eq!(
            parse_path("APP.*.Approch_type").unwrap(),
            vec![
                PathStep::Key("APP".into()),
                PathStep::WildKey,
                PathStep::Key("Approch_type".into()),
            ]
        );
        assert_eq!(
            parse_path("ZONES.training_zones[*].boundary").unwrap(),
            vec![
                PathStep::Key("ZONES".into()),
                PathStep::Key("training_zones".into()),
                PathStep::WildIdx,
                PathStep::Key("boundary".into()),
            ]
        );
    }

    #[test]
    fn rejects_unsupported_syntax() {
        assert!(parse_path("a[3].b").is_err());
        assert!(parse_path("").is_err());
        assert!(parse_path("a..b").is_err());
    }

    #[test]
    fn walk_visits_each_wild_key() {
        let mut v = json!({ "D": { "a": "1", "b": "1", "c": "0" } });
        walk_mut(&mut v, "D.*", |parent, leaf| {
            let LeafKey::Key(k) = leaf else { panic!() };
            let flipped = if parent[k.as_str()] == "1" { "0" } else { "1" };
            parent[k.as_str()] = json!(flipped);
            Ok(())
        })
        .unwrap();
        assert_eq!(v, json!({ "D": { "a": "0", "b": "0", "c": "1" } }));
    }

    #[test]
    fn walk_visits_each_array_item() {
        let mut v = json!({ "Z": { "t": [{ "b": 1 }, { "b": 2 }] } });
        let mut count = 0;
        walk_mut(&mut v, "Z.t[*].b", |parent, leaf| {
            count += 1;
            let LeafKey::Key(k) = leaf else { panic!() };
            parent[k.as_str()] = json!("hit");
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 2);
        assert_eq!(v["Z"]["t"][1]["b"], "hit");
    }

    #[test]
    fn walk_skips_missing_branches() {
        let mut v = json!({ "X": 1, "META": {} });
        walk_mut(&mut v, "DECLUTTER.*", |_, _| panic!("should not fire")).unwrap();
        walk_mut(&mut v, "META.created_at", |_, _| panic!("should not fire")).unwrap();
    }

    #[test]
    fn set_path_creates_intermediates_and_rejects_wildcards() {
        let mut v = json!({ "META": 3 });
        set_path_unchecked(&mut v, "META.created_at", json!("now")).unwrap();
        assert_eq!(v, json!({ "META": { "created_at": "now" } }));
        assert!(set_path_unchecked(&mut v, "a.*.b", json!(0)).is_err());
    }

    #[test]
    fn dotted_paths_are_literal() {
        let mut v = json!({ "a": { "b": 1 } });
        assert_eq!(get_dotted(&v, "a.b"), Some(&json!(1)));
        assert_eq!(get_dotted(&v, "a.*"), None);
        set_dotted(&mut v, "x.y", json!(2)).unwrap();
        assert_eq!(take_dotted(&mut v, "x.y").unwrap(), Some(json!(2)));
        assert_eq!(take_dotted(&mut v, "nope.y").unwrap(), None);
        assert_eq!(v, json!({ "a": { "b": 1 }, "x": {} }));
    }
}
