//! Wire payload emitted to the WASM gauge and the EFB.
//!
//! Must stay byte-compatible with the receiver in
//! `wasm/shared/flight-plan-import/src/sources_simbrief.rs` (T-38C-WASM);
//! bump [`SIMBRIEF_WIRE_VERSION`] in lockstep. Field names are snake_case
//! on the wire.

use serde::{Deserialize, Serialize};

pub const SIMBRIEF_WIRE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimBriefWaypoint {
    pub ident: String,
    pub lat: f64,
    pub lon: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt_ft: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimBriefPayload {
    pub version: u32,
    #[serde(default)]
    pub ofp_id: String,
    #[serde(default)]
    pub origin: String,
    #[serde(default)]
    pub destination: String,
    /// Per-fix enroute waypoints, in order. Never airway-collapsed.
    #[serde(default)]
    pub waypoints: Vec<SimBriefWaypoint>,
}

impl SimBriefPayload {
    pub fn empty() -> Self {
        Self {
            version: SIMBRIEF_WIRE_VERSION,
            ofp_id: String::new(),
            origin: String::new(),
            destination: String::new(),
            waypoints: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.waypoints.is_empty() && self.origin.is_empty() && self.destination.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_shape_is_snake_case_and_omits_missing_altitude() {
        let p = SimBriefPayload {
            ofp_id: "1".into(),
            origin: "KEDW".into(),
            waypoints: vec![
                SimBriefWaypoint {
                    ident: "DAG".into(),
                    lat: 1.5,
                    lon: -2.0,
                    alt_ft: Some(18_000),
                },
                SimBriefWaypoint {
                    ident: "KLSV".into(),
                    lat: 0.0,
                    lon: 0.0,
                    alt_ft: None,
                },
            ],
            ..SimBriefPayload::empty()
        };
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            serde_json::json!({
                "version": 1, "ofp_id": "1", "origin": "KEDW", "destination": "",
                "waypoints": [
                    { "ident": "DAG", "lat": 1.5, "lon": -2.0, "alt_ft": 18000 },
                    { "ident": "KLSV", "lat": 0.0, "lon": 0.0 }
                ]
            })
        );
        assert!(SimBriefPayload::empty().is_empty());
        assert!(!p.is_empty());
    }
}
