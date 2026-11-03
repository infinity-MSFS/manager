use serde_json::{Value, json};

/// Reshapes a Navigraph charts index into what the EFB's chart list reads:
/// `{ "charts": [{ id, index_number, name, category, day_file, night_file,
/// width, height, is_georeferenced }] }`. Image URLs become bare file names,
/// which the client fetches back through `/efb/nav/chart/{icao}/{file}` so
/// the bearer token never leaves the manager.
pub fn charts_index_for_efb(raw: &Value) -> Value {
    let file = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .and_then(|u| u.rsplit('/').next())
            .and_then(|f| f.split('?').next())
            .map_or(Value::Null, |f| json!(f))
    };
    let field = |c: &Value, k: &str| c.get(k).cloned().unwrap_or(Value::Null);
    let charts: Vec<Value> = raw
        .get("charts")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|c| {
                    json!({
                        "id": field(c, "id"),
                        "index_number": field(c, "index_number"),
                        "name": field(c, "name"),
                        "category": field(c, "category"),
                        "day_file": file(c.get("image_day_url")),
                        "night_file": file(c.get("image_night_url")),
                        "width": field(c, "width"),
                        "height": field(c, "height"),
                        "is_georeferenced": c.get("is_georeferenced").cloned().unwrap_or(json!(false)),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    json!({ "charts": charts })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_become_file_names() {
        let raw = json!({ "charts": [{
            "id": "c1", "name": "ILS 28L",
            "image_day_url": "https://charts.api/v2/KSFO/abc_d.png?sig=1",
            "image_night_url": null,
        }]});
        let c = &charts_index_for_efb(&raw)["charts"][0];
        assert_eq!(c["day_file"], "abc_d.png");
        assert_eq!(c["night_file"], Value::Null);
        assert_eq!(c["is_georeferenced"], false);
        assert_eq!(c["width"], Value::Null);
        assert_eq!(charts_index_for_efb(&json!({})), json!({ "charts": [] }));
    }
}
