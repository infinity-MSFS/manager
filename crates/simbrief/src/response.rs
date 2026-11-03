//! The subset of the SimBrief JSON OFP the importer needs. Everything is
//! `#[serde(default)]` so upstream schema tweaks do not break parsing.

use serde::Deserialize;

/// Top-level OFP. SimBrief sends most numbers as strings.
#[derive(Debug, Clone, Deserialize)]
pub struct SimBriefOfp {
    #[serde(default)]
    pub general: SimBriefGeneral,
    #[serde(default)]
    pub origin: SimBriefAirport,
    #[serde(default)]
    pub destination: SimBriefAirport,
    /// Per-fix navlog; airway segments are already expanded to one fix per
    /// join, with `via_airway` set for display.
    #[serde(default)]
    pub navlog: SimBriefNavlog,
    /// Present on lookup failures (`{"fetch": {"status": "Error: ..."}}`)
    /// and, as `"Success"`, on success.
    #[serde(default)]
    pub fetch: Option<SimBriefFetchStatus>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SimBriefGeneral {
    #[serde(default)]
    pub ofp_id: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SimBriefAirport {
    #[serde(default)]
    pub icao_code: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SimBriefNavlog {
    /// An array on multi-fix flights, a bare object on one-fix legs; see
    /// [`parse_navlog_fixes`].
    #[serde(default)]
    pub fix: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SimBriefOfpFix {
    #[serde(default)]
    pub ident: String,
    /// Decimal degrees, as a string.
    #[serde(default)]
    pub pos_lat: String,
    /// Decimal degrees, as a string.
    #[serde(default)]
    pub pos_long: String,
    /// Feet MSL, as a string. `"0"` on airports.
    #[serde(default)]
    pub altitude_feet: String,
    /// `apt`, `wpt`, `vor`, `ndb`...
    #[serde(default, rename = "type")]
    pub fix_type: String,
    /// Airway name, empty if direct. Display only.
    #[serde(default)]
    pub via_airway: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SimBriefFetchStatus {
    #[serde(default)]
    pub status: String,
}

/// Navlog fixes from either the array or the single-object shape. Entries
/// that do not parse are skipped.
pub fn parse_navlog_fixes(raw: &serde_json::Value) -> Vec<SimBriefOfpFix> {
    match raw {
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|v| SimBriefOfpFix::deserialize(v).ok())
            .collect(),
        serde_json::Value::Object(_) => SimBriefOfpFix::deserialize(raw)
            .map(|f| vec![f])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// A string-encoded float; `""` and garbage are `0.0`.
pub(crate) fn parse_f64(s: &str) -> f64 {
    s.trim().parse::<f64>().unwrap_or(0.0)
}

/// A string-encoded integer; `""` and garbage are `0`.
pub(crate) fn parse_i32(s: &str) -> i32 {
    s.trim().parse::<i32>().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navlog_accepts_array_object_and_garbage() {
        let arr = serde_json::json!([{ "ident": "A" }, { "ident": "B" }, 7]);
        assert_eq!(parse_navlog_fixes(&arr).len(), 2);
        let obj = serde_json::json!({ "ident": "A", "pos_lat": "1" });
        assert_eq!(parse_navlog_fixes(&obj)[0].ident, "A");
        assert!(parse_navlog_fixes(&serde_json::json!("x")).is_empty());
        assert!(parse_navlog_fixes(&serde_json::Value::Null).is_empty());
    }

    #[test]
    fn lenient_numbers() {
        assert_eq!(parse_f64(" 34.5 "), 34.5);
        assert_eq!(parse_f64(""), 0.0);
        assert_eq!(parse_i32("18000"), 18_000);
        assert_eq!(parse_i32("FL180"), 0);
    }
}
