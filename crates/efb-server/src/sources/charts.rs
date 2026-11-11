//! Navigraph's charts index (`GET /v2/charts/{icao}`) in the hub's model.
//!
//! Lenient by design: a chart with a field missing or mistyped is kept with
//! that field empty, a chart without an id is dropped, and a georeference
//! that does not check out is simply absent (no own-ship), never wrong.

use serde_json::Value;

use crate::charts::{ChartCategory, ChartEntry, GeoRect, GeoRef, PixelRect};

fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|f| f.is_finite())
}

fn text(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The last path segment of an image URL, without its query: the key the
/// image route takes back to `GET /v2/charts/{icao}/{file}`, so the bearer
/// token and signed URL never leave the manager.
fn file_of(v: Option<&Value>) -> Option<String> {
    let url = v?.as_str()?;
    let path = url.split(['?', '#']).next()?;
    path.rsplit('/')
        .next()
        .filter(|f| !f.is_empty())
        .map(str::to_owned)
}

fn pixels(v: &Value) -> Option<PixelRect> {
    Some(PixelRect {
        x1: num(v.get("x1"))?,
        y1: num(v.get("y1"))?,
        x2: num(v.get("x2"))?,
        y2: num(v.get("y2"))?,
    })
}

fn latlng(v: &Value) -> Option<GeoRect> {
    Some(GeoRect {
        lat1: num(v.get("lat1"))?,
        lng1: num(v.get("lng1"))?,
        lat2: num(v.get("lat2"))?,
        lng2: num(v.get("lng2"))?,
    })
}

fn georef(c: &Value) -> Option<GeoRef> {
    if c.get("is_georeferenced").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let boxes = c.get("bounding_boxes")?;
    let plan = boxes.get("planview")?;
    let insets = boxes
        .get("insets")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|i| i.get("pixels").and_then(pixels))
                .collect()
        })
        .unwrap_or_default();
    GeoRef::checked(
        pixels(plan.get("pixels")?)?,
        latlng(plan.get("latlng")?)?,
        insets,
    )
}

fn dimension(v: Option<&Value>) -> Option<u32> {
    num(v)
        .filter(|f| *f >= 1.0 && *f <= f64::from(u32::MAX))
        .map(|f| f.round() as u32)
}

pub fn navigraph_entries(raw: &Value) -> Vec<ChartEntry> {
    let Some(charts) = raw.get("charts").and_then(Value::as_array) else {
        return Vec::new();
    };
    charts
        .iter()
        .filter_map(|c| {
            let id = text(c.get("id")).filter(|s| !s.is_empty())?;
            Some(ChartEntry {
                index: text(c.get("index_number")).unwrap_or_default(),
                name: text(c.get("name")).unwrap_or_else(|| id.clone()),
                category: ChartCategory::from_code(
                    c.get("category").and_then(Value::as_str).unwrap_or(""),
                ),
                runways: strings(c.get("relevant_runways")),
                procedures: strings(c.get("procedures")),
                day: file_of(c.get("image_day_url")),
                night: file_of(c.get("image_night_url")),
                width: dimension(c.get("width")),
                height: dimension(c.get("height")),
                georef: georef(c),
                id,
            })
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_full_chart_maps_every_field() {
        let raw = json!({ "charts": [{
            "id": "c1", "name": "ILS OR LOC RWY 28L", "index_number": "11-1",
            "category": "APP",
            "image_day_url": "https://api.navigraph.com/v2/charts/KSFO/abc_d.png?sig=1",
            "image_night_url": "https://api.navigraph.com/v2/charts/KSFO/abc_n.png",
            "width": 1700, "height": "2200",
            "procedures": ["I28L", " "], "relevant_runways": ["28L"],
            "is_georeferenced": true,
            "bounding_boxes": {
                "planview": {
                    "pixels": { "x1": 100, "y1": 1900, "x2": 1600, "y2": 300 },
                    "latlng": { "lng1": -122.5, "lat1": 37.5, "lng2": -122.2, "lat2": 37.7 }
                },
                "insets": [{ "pixels": { "x1": 0, "y1": 0, "x2": 10, "y2": 10 } }, { "bad": 1 }]
            }
        }]});
        let c = &navigraph_entries(&raw)[0];
        assert_eq!(c.id, "c1");
        assert_eq!(c.index, "11-1");
        assert_eq!(c.category, ChartCategory::Approach);
        assert_eq!(c.day.as_deref(), Some("abc_d.png"));
        assert_eq!(c.night.as_deref(), Some("abc_n.png"));
        assert_eq!((c.width, c.height), (Some(1700), Some(2200)));
        assert_eq!(c.procedures, vec!["I28L"]);
        assert_eq!(c.runways, vec!["28L"]);
        let g = c.georef.as_ref().unwrap();
        assert_eq!(g.pixels.y1, 1900.0);
        assert_eq!(g.latlng.lng1, -122.5);
        assert_eq!(g.insets.len(), 1);
    }

    #[test]
    fn missing_and_broken_fields_degrade_instead_of_failing() {
        let raw = json!({ "charts": [
            { "id": "bare" },
            { "name": "no id" },
            { "id": 42, "image_day_url": null, "category": "XYZ",
              "is_georeferenced": true,
              "bounding_boxes": { "planview": {
                  "pixels": { "x1": 0, "y1": 0, "x2": 0, "y2": 10 },
                  "latlng": { "lng1": 1, "lat1": 1, "lng2": 2, "lat2": 2 } } } },
            { "id": "flag-off", "is_georeferenced": false, "bounding_boxes": {} },
        ]});
        let charts = navigraph_entries(&raw);
        assert_eq!(charts.len(), 3);
        assert_eq!(charts[0].name, "bare");
        assert_eq!(charts[0].day, None);
        assert_eq!(charts[1].id, "42");
        assert_eq!(charts[1].category, ChartCategory::Reference);
        assert!(charts[1].georef.is_none(), "zero-width box");
        assert!(charts[2].georef.is_none());
        assert!(navigraph_entries(&json!({})).is_empty());
        assert!(navigraph_entries(&json!({ "charts": 3 })).is_empty());
    }
}
