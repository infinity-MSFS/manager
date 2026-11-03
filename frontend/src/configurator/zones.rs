//! Zone shapes shared by the zone widgets and the Zones page: v1's
//! `TrainingZoneConfig` and `NoFlyZoneConfig`, plus the lookups that turn
//! an airspace or a military training route into boundary rings.

use infinity_ipc::IpcError;
use infinity_ipc::commands as cmd;
use infinity_ipc::nav::{MtrFamily, NavAirspace};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::mtr;
use super::state::Configurator;
use crate::ipc;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ZoneColor {
    #[default]
    Yellow,
    Cyan,
    Green,
    Magenta,
    Orange,
    Red,
}

impl ZoneColor {
    pub(crate) const ALL: [Self; 6] = [
        Self::Yellow,
        Self::Cyan,
        Self::Green,
        Self::Magenta,
        Self::Orange,
        Self::Red,
    ];

    pub(crate) fn css(self) -> &'static str {
        match self {
            Self::Yellow => "#facc15",
            Self::Cyan => "#22d3ee",
            Self::Green => "#22c55e",
            Self::Magenta => "#e879f9",
            Self::Orange => "#fb923c",
            Self::Red => "#ef4444",
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Yellow => "yellow",
            Self::Cyan => "cyan",
            Self::Green => "green",
            Self::Magenta => "magenta",
            Self::Orange => "orange",
            Self::Red => "red",
        }
    }
}

/// Lenient: an unknown colour name reads as the default rather than
/// failing the whole zone.
fn color_or_default<'de, D: serde::Deserializer<'de>>(d: D) -> Result<ZoneColor, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or_default())
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct TrainingZone {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub designation: String,
    #[serde(default, rename = "type")]
    pub zone_type: String,
    #[serde(default, deserialize_with = "color_or_default")]
    pub color: ZoneColor,
    #[serde(default)]
    pub boundary: Vec<Vec<LatLon>>,
    /// Fields this editor does not know, kept on write.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl TrainingZone {
    pub(crate) fn title(&self, index: usize) -> String {
        if !self.name.is_empty() {
            self.name.clone()
        } else if !self.designation.is_empty() {
            self.designation.clone()
        } else {
            format!("Zone {}", index + 1)
        }
    }

    pub(crate) fn ring_summary(&self) -> Option<String> {
        let rings = self.boundary.len();
        let points: usize = self.boundary.iter().map(Vec::len).sum();
        (rings > 0).then(|| {
            format!(
                "{rings} ring{}, {points} pts",
                if rings == 1 { "" } else { "s" }
            )
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct NoFlyZone {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub lat: f64,
    #[serde(default)]
    pub lon: f64,
    /// Nautical miles.
    #[serde(default)]
    pub radius: f64,
    #[serde(default = "magenta", deserialize_with = "color_or_default")]
    pub color: ZoneColor,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn magenta() -> ZoneColor {
    ZoneColor::Magenta
}

impl NoFlyZone {
    pub(crate) fn blank(index: usize) -> Self {
        Self {
            name: format!("No-Fly Zone {}", index + 1),
            lat: 0.0,
            lon: 0.0,
            radius: 5.0,
            color: ZoneColor::Magenta,
            extra: Map::new(),
        }
    }
}

/// Reads an array of zones, skipping entries that are not objects.
pub(crate) fn read_list<T: for<'de> Deserialize<'de>>(value: &Value) -> Vec<T> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|z| serde_json::from_value(z.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn write_list<T: Serialize>(zones: &[T]) -> Value {
    Value::Array(
        zones
            .iter()
            .filter_map(|z| serde_json::to_value(z).ok())
            .collect(),
    )
}

/// `M` / `W` / `R` as words.
pub(crate) fn airspace_type_label(t: &str) -> String {
    match t {
        "M" => "MOA".to_owned(),
        "W" => "Warning".to_owned(),
        "R" => "Restricted".to_owned(),
        "" => "—".to_owned(),
        other => other.to_owned(),
    }
}

/// Every ring of an airspace, arcs densified (R-5107B has four).
pub(crate) async fn airspace_rings(
    cfg: Configurator,
    designation: &str,
) -> Result<Vec<Vec<LatLon>>, IpcError> {
    let raw = ipc::invoke::<cmd::NavAirspaceBoundary>(&NavAirspace {
        product: cfg.product_ref(),
        designation: designation.to_owned(),
        resolution: None,
    })
    .await?;
    let rings = ipc::invoke::<cmd::NavExpandTrainingAreas>(&raw).await?;
    Ok(rings
        .into_iter()
        .map(|r| {
            r.into_iter()
                .map(|p| LatLon {
                    lat: p.lat,
                    lon: p.lon,
                })
                .collect()
        })
        .collect())
}

/// An airspace as a new training zone.
pub(crate) async fn airspace_zone(
    cfg: Configurator,
    name: String,
    designation: String,
    zone_type: String,
) -> Result<TrainingZone, IpcError> {
    let boundary = airspace_rings(cfg, &designation).await?;
    Ok(TrainingZone {
        name,
        designation,
        zone_type,
        color: ZoneColor::Yellow,
        boundary,
        extra: Map::new(),
    })
}

/// A military training route as a new training zone: its corridor,
/// unioned into clean rings. `Ok(None)` when no segment has a width.
pub(crate) async fn mtr_zone(
    ident_full: String,
    family: MtrFamily,
) -> Result<Option<TrainingZone>, IpcError> {
    let segments = ipc::invoke::<cmd::MtrSegments>(&ident_full).await?;
    let rings = mtr::connected_path(&segments).await?;
    if rings.is_empty() {
        return Ok(None);
    }
    Ok(Some(TrainingZone {
        name: ident_full.clone(),
        designation: ident_full,
        zone_type: "MTR".to_owned(),
        color: if family == MtrFamily::Ir {
            ZoneColor::Cyan
        } else {
            ZoneColor::Green
        },
        boundary: rings,
        extra: Map::new(),
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn zones_round_trip_and_keep_unknown_fields() {
        let v = json!([
            { "name": "A", "designation": "R-1", "type": "R", "color": "cyan",
              "boundary": [[{ "lat": 1.0, "lon": 2.0 }]], "extra": 5 },
            "junk",
            { "name": "B", "color": "chartreuse" }
        ]);
        let zones: Vec<TrainingZone> = read_list(&v);
        assert_eq!(zones.len(), 2);
        assert_eq!(zones[0].color, ZoneColor::Cyan);
        assert_eq!(zones[1].color, ZoneColor::Yellow);
        assert_eq!(zones[0].ring_summary().as_deref(), Some("1 ring, 1 pts"));
        let back = write_list(&zones);
        assert_eq!(back[0]["extra"], json!(5));
        assert_eq!(back[0]["type"], json!("R"));
    }

    #[test]
    fn no_fly_defaults_to_magenta() {
        let z: Vec<NoFlyZone> =
            read_list(&json!([{ "name": "x", "lat": 1, "lon": 2, "radius": 3 }]));
        assert_eq!(z[0].color, ZoneColor::Magenta);
        assert_eq!(NoFlyZone::blank(1).name, "No-Fly Zone 2");
        assert_eq!(airspace_type_label("M"), "MOA");
    }
}
