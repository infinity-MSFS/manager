//! Navigation database shapes. Field names are v3's wire format, which
//! configurator widgets and the in-sim gauges read: snake_case, and
//! `ApproachData` keeps the gauge's spellings. Do not rename.

//! Response shapes. These are snake_case on the wire, not camelCase: the
//! configurator widgets read `runway_identifier`, `area_code`, `is_arc` and
//! so on, and `ApproachData` is forwarded verbatim to the aircraft gauge,
//! typos (`Approch_type`) included. Do not add `rename_all`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaypointData {
    pub identifier: String,
    pub latitude: f64,
    pub longitude: f64,
    /// `Airport`, `Enroute`, `Terminal`, `VHF NAVAID`, `TACAN` or `DME`.
    #[serde(rename = "type")]
    pub waypoint_type: String,
    /// Airport name. Nearest-waypoint results append ` (12.3 nm)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestrictiveAirspaceData {
    pub name: String,
    pub designation: String,
    #[serde(rename = "type")]
    pub airspace_type: String,
}

/// Unlike [`RestrictiveAirspaceData`] the type field keeps its Rust name on
/// the wire; the map context menu and zones page read `airspace_type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestrictiveAirspaceDataWithDistance {
    pub name: String,
    pub designation: String,
    pub airspace_type: String,
    /// Nautical miles.
    pub distance: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestrictiveAirspaceBoundaryPoint {
    pub sequence_number: i32,
    pub latitude: f64,
    pub longitude: f64,
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub boundary_type: Option<String>,
    /// Set when the point carries an arc origin; the arc helpers densify it.
    pub is_arc: bool,
    pub arc_bearing: Option<f64>,
    pub arc_distance: Option<f64>,
    pub arc_origin_latitude: Option<f64>,
    pub arc_origin_longitude: Option<f64>,
    /// Ring id: one airspace can stack several altitude bands.
    pub multiple_code: Option<String>,
    pub lower_limit: Option<String>,
    pub upper_limit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunwayData {
    pub runway_identifier: String,
    pub airport_identifier: String,
}

/// Enough of a runway to seed a manual approach where no procedure exists.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunwayDetails {
    pub runway_identifier: String,
    pub airport_identifier: String,
    pub latitude: f64,
    pub longitude: f64,
    /// Landing threshold elevation, feet.
    pub elevation_ft: i32,
    /// Degrees; missing for a handful of runways.
    pub true_bearing: Option<f64>,
    pub magnetic_bearing: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailableApproachTypes {
    pub has_ils: bool,
    pub has_vor: bool,
}

/// An approach resolved for the gauge. Field names are the gauge's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApproachData {
    #[serde(rename = "Approch_type")]
    pub approch_type: String,
    #[serde(rename = "Airport_icao")]
    pub airport_icao: String,
    /// `Localizer`, `VOR` or `Unknown`.
    pub navaid_type: String,
    /// MHz; 0 when the recommended navaid is unknown.
    pub navaid_freq: f64,
    pub navaid_course: i32,
    pub rwy_lon: f64,
    pub rwy_lat: f64,
    pub rwy_elev: i32,
    /// Nautical miles from the threshold; 0 when the fix is absent.
    pub fip_distance: f64,
    pub faf_distance: f64,
    /// Glideslope, positive degrees.
    pub app_gs: f64,
    #[serde(default)]
    pub fip_lat: f64,
    #[serde(default)]
    pub fip_lon: f64,
    #[serde(default)]
    pub faf_lat: f64,
    #[serde(default)]
    pub faf_lon: f64,
}

/// One vertex of a densified airspace ring.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BoundaryPointLL {
    pub lat: f64,
    pub lon: f64,
}

/// Military training route family, from `mtr_segments.mtr_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MtrFamily {
    /// Visual route, `mtr_type = 0`.
    Vr,
    /// Instrument route, `mtr_type = 1`.
    Ir,
}

impl MtrFamily {
    /// Ident prefix: `VR` or `IR`.
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Vr => "VR",
            Self::Ir => "IR",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MtrRouteSummary {
    /// Display ident: `VR1064`, `IR002`.
    pub ident_full: String,
    /// Numeric ident as stored.
    pub ident: String,
    pub family: MtrFamily,
    /// FAA AP/1B low-level route: IR with a four-digit ident, no segment
    /// above 1,500 ft AGL.
    pub is_low_level: bool,
    pub segment_count: i64,
    /// Lowest floor in feet over the segments given in feet.
    pub min_lower_ft: Option<f64>,
    /// Highest ceiling in feet over the segments given in feet.
    pub max_upper_ft: Option<f64>,
}

/// A route summary flattened together with its distance from the query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MtrRouteWithDistance {
    #[serde(flatten)]
    pub route: MtrRouteSummary,
    /// Nautical miles to the route's nearest centreline vertex.
    pub distance_nm: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MtrSegment {
    pub object_id: i64,
    pub ident_full: String,
    pub family: MtrFamily,
    /// `None` when missing or not given in feet.
    pub lower_ft: Option<f64>,
    /// `ALT`, `STD`, `HEI` or `SFC`.
    pub lower_code: Option<String>,
    pub upper_ft: Option<f64>,
    pub upper_code: Option<String>,
    pub width_left_nm: i64,
    pub width_right_nm: i64,
    /// One polyline per geometry part, vertices in order.
    pub polylines: Vec<Vec<MtrLatLon>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MtrLatLon {
    pub lat: f64,
    pub lon: f64,
}

// ---- Command arguments. These are v4's own (camelCase): only the results
// above carry the v3 wire format.

use crate::dto::ProductRef;

/// An airport in the product's navigation database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavAirport {
    pub product: ProductRef,
    pub icao: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavRunway {
    pub product: ProductRef,
    pub icao: String,
    pub runway: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavApproach {
    pub product: ProductRef,
    pub icao: String,
    pub runway: String,
    /// `ILS` or `VOR`.
    pub approach_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavSearch {
    pub product: ProductRef,
    pub query: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavNearest {
    pub product: ProductRef,
    pub lat: f64,
    pub lon: f64,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavWaypoint {
    pub product: ProductRef,
    pub identifier: String,
    /// Breaks ties between same-named waypoints in different regions.
    pub area_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavAirspace {
    pub product: ProductRef,
    pub designation: String,
    /// Points per arc for the normalized boundary.
    pub resolution: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavNearby {
    pub product: ProductRef,
    pub lat: f64,
    pub lon: f64,
    pub max_nm: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MtrNearby {
    pub lat: f64,
    pub lon: f64,
    pub max_nm: f64,
    /// `None` returns up to 50; `0` returns all.
    pub limit: Option<usize>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn approach_data_keeps_the_gauge_field_names() {
        let a = ApproachData {
            approch_type: "ILS".into(),
            airport_icao: "KRND".into(),
            navaid_type: "Localizer".into(),
            navaid_freq: 110.3,
            navaid_course: 151,
            rwy_lon: 1.0,
            rwy_lat: 2.0,
            rwy_elev: 700,
            fip_distance: 0.0,
            faf_distance: 0.0,
            app_gs: 3.0,
            fip_lat: 0.0,
            fip_lon: 0.0,
            faf_lat: 0.0,
            faf_lon: 0.0,
        };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["Approch_type"], "ILS");
        assert_eq!(v["Airport_icao"], "KRND");
        assert_eq!(v["navaid_course"], 151);
    }

    #[test]
    fn waypoint_renames_type_and_omits_missing_fields() {
        let w = WaypointData {
            identifier: "RND".into(),
            latitude: 1.0,
            longitude: 2.0,
            waypoint_type: "VHF NAVAID".into(),
            name: None,
            area_code: Some(String::new()),
        };
        assert_eq!(
            serde_json::to_value(&w).unwrap(),
            json!({ "identifier": "RND", "latitude": 1.0, "longitude": 2.0,
                    "type": "VHF NAVAID", "area_code": "" })
        );
    }

    #[test]
    fn airspace_type_field_differs_between_search_and_nearby() {
        let s = RestrictiveAirspaceData {
            name: "n".into(),
            designation: "d".into(),
            airspace_type: "M".into(),
        };
        assert_eq!(serde_json::to_value(&s).unwrap()["type"], "M");
        let n = RestrictiveAirspaceDataWithDistance {
            name: "n".into(),
            designation: "d".into(),
            airspace_type: "M".into(),
            distance: 1.0,
        };
        assert_eq!(serde_json::to_value(&n).unwrap()["airspace_type"], "M");
    }

    #[test]
    fn mtr_route_with_distance_is_flat() {
        let r = MtrRouteWithDistance {
            route: MtrRouteSummary {
                ident_full: "IR1064".into(),
                ident: "1064".into(),
                family: MtrFamily::Ir,
                is_low_level: true,
                segment_count: 2,
                min_lower_ft: Some(100.0),
                max_upper_ft: None,
            },
            distance_nm: 3.5,
        };
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            json!({ "ident_full": "IR1064", "ident": "1064", "family": "ir",
                    "is_low_level": true, "segment_count": 2,
                    "min_lower_ft": 100.0, "max_upper_ft": null, "distance_nm": 3.5 })
        );
    }
}
