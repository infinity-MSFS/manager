//! Pure flight-plan logic (v3 `FlightPlansPage.tsx` helpers): catalog
//! custom waypoints, `DEST XX` lookups against the `DST` section, the
//! compiled route string and route-token parsing.

use infinity_ipc::nav::WaypointData;
use serde_json::{Map, Value, json};

use crate::configurator::schema::{num_at, str_at};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Waypoint {
    pub ident: String,
    pub lat: f64,
    pub lon: f64,
}

impl Waypoint {
    pub(crate) fn to_value(&self) -> Value {
        json!({ "ident": self.ident, "lat": self.lat, "lon": self.lon })
    }
}

impl From<&WaypointData> for Waypoint {
    fn from(w: &WaypointData) -> Self {
        Self {
            ident: w.identifier.clone(),
            lat: w.latitude,
            lon: w.longitude,
        }
    }
}

/// An off-database fix from the group's `customWaypoints`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CustomWaypoint {
    pub ident: String,
    pub lat: f64,
    pub lon: f64,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DestEntry {
    pub number: String,
    pub lat: f64,
    pub lon: f64,
}

fn finite(v: &Value, key: &str) -> Option<f64> {
    v.get(key)?.as_f64().filter(|n| n.is_finite())
}

pub(crate) fn read_custom_waypoints(group: &Value) -> Vec<CustomWaypoint> {
    group
        .get("customWaypoints")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let ident = item.get("ident")?.as_str()?.trim();
                    if ident.is_empty() {
                        return None;
                    }
                    Some(CustomWaypoint {
                        ident: ident.to_uppercase(),
                        lat: finite(item, "lat")?,
                        lon: finite(item, "lon")?,
                        name: item.get("name").and_then(Value::as_str).map(str::to_owned),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Destinations from the config's `DST` section.
pub(crate) fn read_destinations(root: &Value) -> Vec<DestEntry> {
    root.get("DST")
        .and_then(Value::as_object)
        .map(|dst| {
            dst.iter()
                .filter_map(|(num, d)| {
                    Some(DestEntry {
                        number: num.clone(),
                        lat: finite(d, "lat")?,
                        lon: finite(d, "lon")?,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn pad2(s: &str) -> String {
    format!("{s:0>2}")
}

fn find_dest<'a>(dests: &'a [DestEntry], num: &str) -> Option<&'a DestEntry> {
    let want = pad2(num);
    dests.iter().find(|d| pad2(&d.number) == want)
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// The destination number in a search term: bare digits (`12`) or a
/// `DEST` prefix (`D`, `DE`, `DES`, `DEST`) with digits (`DEST 12`, `D12`).
pub(crate) fn dest_number(term: &str) -> Option<String> {
    let rest = ["DEST", "DES", "DE", "D"]
        .iter()
        .find_map(|p| term.strip_prefix(p))
        .map_or(term, str::trim_start);
    all_digits(rest).then(|| pad2(rest))
}

/// A partial spelling of `DEST` with no digits yet: show a hint instead of
/// every destination.
pub(crate) fn is_dest_prefix(term: &str) -> bool {
    !term.is_empty() && "DEST".starts_with(term)
}

/// Suggestions from outside the navigation database: custom waypoints by
/// prefix, then the destination the term names.
pub(crate) fn local_suggestions(
    term: &str,
    customs: &[CustomWaypoint],
    dests: &[DestEntry],
) -> Vec<WaypointData> {
    let mut out: Vec<WaypointData> = customs
        .iter()
        .filter(|c| c.ident.starts_with(term))
        .map(|c| WaypointData {
            identifier: c.ident.clone(),
            latitude: c.lat,
            longitude: c.lon,
            waypoint_type: "CUSTOM".to_owned(),
            name: Some(c.name.clone().unwrap_or_else(|| c.ident.clone())),
            area_code: None,
        })
        .collect();
    if let Some(num) = dest_number(term)
        && let Some(d) = find_dest(dests, &num)
    {
        out.push(WaypointData {
            identifier: format!("DEST {num}"),
            latitude: d.lat,
            longitude: d.lon,
            waypoint_type: "DESTINATION".to_owned(),
            name: Some(format!("Destination {num}")),
            area_code: None,
        });
    }
    out
}

/// Local hits first, then database hits not already listed, capped at 12.
pub(crate) fn merge_results(local: Vec<WaypointData>, db: Vec<WaypointData>) -> Vec<WaypointData> {
    let mut out = local;
    for r in db {
        if !out.iter().any(|l| l.identifier == r.identifier) {
            out.push(r);
        }
    }
    out.truncate(12);
    out
}

pub(crate) fn is_destination_ident(ident: &str) -> bool {
    ident
        .trim()
        .to_uppercase()
        .strip_prefix("DEST")
        .map(str::trim_start)
        .is_some_and(all_digits)
}

#[allow(clippy::cast_possible_truncation)]
fn dms(deg: f64, is_lat: bool) -> String {
    let hemi = match (is_lat, deg >= 0.0) {
        (true, true) => 'N',
        (true, false) => 'S',
        (false, true) => 'E',
        (false, false) => 'W',
    };
    let abs = deg.abs();
    let mut d = abs.floor() as i64;
    let min_f = (abs - abs.floor()) * 60.0;
    let mut m = min_f.floor() as i64;
    let mut s = ((min_f - min_f.floor()) * 60.0).round() as i64;
    if s == 60 {
        s = 0;
        m += 1;
    }
    if m == 60 {
        m = 0;
        d += 1;
    }
    if is_lat {
        format!("{d:02}{m:02}{s:02}{hemi}")
    } else {
        format!("{d:03}{m:02}{s:02}{hemi}")
    }
}

/// Compact ARINC lat/lon, v1's `formatLatLonCompact`: `DDMMSSN` then
/// `DDDMMSSE`.
pub(crate) fn format_lat_lon_compact(lat: f64, lon: f64) -> String {
    format!("{}{}", dms(lat, true), dms(lon, false))
}

fn normalize_token(token: &str) -> String {
    token.split_whitespace().collect::<String>().to_uppercase()
}

/// The plan as one route line (`KSPS BULME ELECT`); destinations collapse
/// to compact lat/lon so off-database fixes survive the round trip.
pub(crate) fn compile(waypoints: &[Value]) -> String {
    waypoints
        .iter()
        .filter_map(|w| {
            let ident = str_at(w, "ident");
            if ident.trim().is_empty() {
                return None;
            }
            let lat = num_at(w, "lat").filter(|n| n.is_finite());
            let lon = num_at(w, "lon").filter(|n| n.is_finite());
            Some(match (is_destination_ident(&ident), lat, lon) {
                (true, Some(lat), Some(lon)) => format_lat_lon_compact(lat, lon),
                _ => normalize_token(&ident),
            })
        })
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_lat_lon_token(t: &str) -> bool {
    let b = t.as_bytes();
    b.len() == 15
        && b[..6].iter().all(u8::is_ascii_digit)
        && matches!(b[6], b'N' | b'S')
        && b[7..14].iter().all(u8::is_ascii_digit)
        && matches!(b[14], b'E' | b'W')
}

/// A pasted (upper-case) token resolved without the database, or `None` to
/// look it up. Compact lat/lon passes through as an ident at 0/0, as v3.
pub(crate) fn resolve_local(
    token: &str,
    customs: &[CustomWaypoint],
    dests: &[DestEntry],
) -> Option<Waypoint> {
    if is_lat_lon_token(token) {
        return Some(Waypoint {
            ident: token.to_owned(),
            lat: 0.0,
            lon: 0.0,
        });
    }
    if let Some(num) = token.strip_prefix("DEST").filter(|n| all_digits(n)) {
        let num = pad2(num);
        if let Some(d) = find_dest(dests, &num) {
            return Some(Waypoint {
                ident: format!("DEST {num}"),
                lat: d.lat,
                lon: d.lon,
            });
        }
    }
    customs.iter().find(|c| c.ident == token).map(|c| Waypoint {
        ident: c.ident.clone(),
        lat: c.lat,
        lon: c.lon,
    })
}

/// Upper-cased route tokens.
pub(crate) fn tokens(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_uppercase).collect()
}

/// The lowest free two-digit plan key (`00`, `01`, ...).
pub(crate) fn next_plan_key(plans: &Map<String, Value>) -> String {
    (0u32..)
        .map(|i| format!("{i:02}"))
        .find(|k| !plans.contains_key(k))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dests() -> Vec<DestEntry> {
        vec![DestEntry {
            number: "1".into(),
            lat: 35.0,
            lon: -106.0,
        }]
    }

    #[test]
    fn arinc_compaction() {
        assert_eq!(
            format_lat_lon_compact(35.21735, -106.71284),
            "351302N1064246W"
        );
        assert_eq!(format_lat_lon_compact(-0.5, 0.25), "003000S0001500E");
        // 59.99... seconds carry through the minute and the degree.
        assert_eq!(format_lat_lon_compact(9.999_999_9, 0.0), "100000N0000000E");
    }

    #[test]
    fn dest_terms() {
        assert_eq!(dest_number("12").as_deref(), Some("12"));
        assert_eq!(dest_number("DEST 1").as_deref(), Some("01"));
        assert_eq!(dest_number("D7").as_deref(), Some("07"));
        assert_eq!(dest_number("DEN"), None);
        assert_eq!(dest_number("DEST"), None);
        assert!(is_dest_prefix("DE"));
        assert!(!is_dest_prefix("DEN"));
        assert!(is_destination_ident("dest 05"));
        assert!(!is_destination_ident("DESTX"));
    }

    #[test]
    fn suggestions_and_merge() {
        let customs = vec![CustomWaypoint {
            ident: "MIKKE".into(),
            lat: 1.0,
            lon: 2.0,
            name: None,
        }];
        let s = local_suggestions("MI", &customs, &dests());
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name.as_deref(), Some("MIKKE"));
        let s = local_suggestions("DEST 01", &customs, &dests());
        assert_eq!(s[0].identifier, "DEST 01");
        assert_eq!(merge_results(s.clone(), s).len(), 1);
    }

    #[test]
    fn compile_and_resolve() {
        let wps = vec![
            json!({ "ident": "KSPS", "lat": 33.9, "lon": -98.5 }),
            json!({ "ident": "DEST 01", "lat": 35.0, "lon": -106.0 }),
            json!({ "ident": " ", "lat": 0, "lon": 0 }),
        ];
        assert_eq!(compile(&wps), "KSPS 350000N1060000W");
        assert_eq!(tokens(" ksps  dest1 "), vec!["KSPS", "DEST1"]);
        let r = resolve_local("DEST1", &[], &dests());
        assert_eq!(r.map(|w| w.ident).as_deref(), Some("DEST 01"));
        let r = resolve_local("350000N1060000W", &[], &dests());
        assert_eq!(r.map(|w| w.lat), Some(0.0));
        assert_eq!(resolve_local("KSPS", &[], &dests()), None);
    }

    #[test]
    fn readers_skip_bad_entries() {
        let g = json!({ "customWaypoints": [
            { "ident": " beavr ", "lat": 1, "lon": 2, "name": "Beaver" },
            { "ident": "", "lat": 1, "lon": 2 },
            { "ident": "X", "lat": "a", "lon": 2 }
        ]});
        let c = read_custom_waypoints(&g);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].ident, "BEAVR");
        let root = json!({ "DST": { "201": { "lat": 1, "lon": 2 }, "202": { "lat": 1 } } });
        assert_eq!(read_destinations(&root).len(), 1);
        let plans = json!({ "00": {}, "02": {} });
        let empty = Map::new();
        assert_eq!(next_plan_key(plans.as_object().unwrap_or(&empty)), "01");
    }
}
