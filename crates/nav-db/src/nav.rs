//! Queries against an aircraft's Navigraph-format navigation database
//! (`<work>/NavigationData/nav.s3db`).

use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Row, ToSql};

use crate::approach;
use crate::arc::normalize_boundary_ring;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::helpers::{clean_runway_number, get_f64, get_i32, haversine_nm, navaid_type_label};
use infinity_ipc::nav::{
    ApproachData, AvailableApproachTypes, BoundaryPointLL, RestrictiveAirspaceBoundaryPoint,
    RestrictiveAirspaceData, RestrictiveAirspaceDataWithDistance, RunwayData, RunwayDetails,
    WaypointData,
};

/// Restrictive airspace is US-only and limited to MOAs, warning and
/// restricted areas: the kinds a training aircraft is assigned to.
const AIRSPACE_FILTER: &str = "area_code = 'USA' AND restrictive_type IN ('M', 'W', 'R')";

/// Default result count for [`NavDb::nearest_waypoints`].
pub const DEFAULT_NEAREST_WAYPOINTS: usize = 5;
/// Default arc resolution for [`NavDb::restrictive_airspace_boundary_normalized`].
pub const DEFAULT_ARC_RESOLUTION: u32 = 24;

/// A read-only handle to one navigation database.
///
/// Every query is synchronous SQLite work: call it from a blocking context
/// (`tokio::task::spawn_blocking`), not directly on the async runtime. The
/// handle is `Send + Sync`; share it in an `Arc` and reopen when
/// [`is_stale`](Self::is_stale) says the file was replaced by a navdata
/// update. On Windows an open handle can stop that update from replacing
/// the file, so drop cached handles before installing navdata.
#[derive(Debug)]
pub struct NavDb {
    db: Db,
}

impl NavDb {
    /// Opens the database at `path`. Fails with [`Error::NotFound`] when
    /// the file does not exist, which for a work folder means the aircraft
    /// has not been loaded since its navdata was installed.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            db: Db::open(path.into())?,
        })
    }

    pub fn path(&self) -> &Path {
        self.db.path()
    }

    /// True once the file on disk differs from the one opened.
    pub fn is_stale(&self) -> bool {
        self.db.is_stale()
    }

    /// AIRAC cycle, e.g. `2410`. Backs `nav_cycle_info`.
    pub fn cycle(&self) -> Result<String> {
        self.db.with(|conn| {
            conn.prepare_cached("SELECT cycle FROM tbl_hdr_header LIMIT 1")?
                .query_row([], |r| r.get::<_, String>(0))
                .optional()?
                .ok_or_else(|| Error::Internal("the navigation database has no header".into()))
        })
    }

    /// Backs `nav_is_valid_icao`.
    pub fn is_valid_icao(&self, icao: &str) -> Result<bool> {
        if icao.trim().is_empty() {
            return Ok(false);
        }
        self.db.with(|conn| {
            let n: i64 = conn
                .prepare_cached(
                    "SELECT COUNT(*) FROM tbl_pa_airports WHERE airport_identifier = ?1",
                )?
                .query_row([icao.to_uppercase()], |r| r.get(0))?;
            Ok(n > 0)
        })
    }

    /// The airport's runways in identifier order. Backs
    /// `nav_runways_for_airport`.
    pub fn runways_for_airport(&self, icao: &str) -> Result<Vec<RunwayData>> {
        if icao.trim().is_empty() {
            return Ok(Vec::new());
        }
        self.db.with(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT DISTINCT runway_identifier, airport_identifier
                 FROM tbl_pg_runways
                 WHERE airport_identifier = ?1
                 ORDER BY runway_identifier ASC",
            )?;
            let rows = stmt.query_map([icao.to_uppercase()], |r| {
                Ok(RunwayData {
                    runway_identifier: r.get(0)?,
                    airport_identifier: r.get(1)?,
                })
            })?;
            Ok(rows.filter_map(std::result::Result::ok).collect())
        })
    }

    /// Threshold position, elevation and bearings of one runway. Works for
    /// runways with no published approach, where
    /// [`approach_data`](Self::approach_data) fails. Backs
    /// `nav_runway_details`.
    pub fn runway_details(
        &self,
        airport_icao: &str,
        runway_identifier: &str,
    ) -> Result<RunwayDetails> {
        self.db.with(|conn| {
            conn.prepare_cached(
                "SELECT runway_identifier, airport_identifier, runway_latitude,
                        runway_longitude, landing_threshold_elevation,
                        runway_true_bearing, runway_magnetic_bearing
                 FROM tbl_pg_runways
                 WHERE airport_identifier = ?1 AND runway_identifier = ?2
                 LIMIT 1",
            )?
            .query_row(
                [airport_icao.to_uppercase().as_str(), runway_identifier],
                |r| {
                    Ok(RunwayDetails {
                        runway_identifier: r.get(0)?,
                        airport_identifier: r.get(1)?,
                        latitude: r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                        longitude: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                        elevation_ft: r.get(4)?,
                        true_bearing: r.get(5)?,
                        magnetic_bearing: r.get(6)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| Error::BadInput("Runway not found".into()))
        })
    }

    /// Up to 20 four-letter airport identifiers containing `query`. Backs
    /// `nav_airports_by_name` (which, despite the name, matches the ICAO).
    pub fn airports_by_name(&self, query: &str) -> Result<Vec<String>> {
        self.db.with(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT airport_identifier FROM tbl_pa_airports
                 WHERE airport_identifier LIKE ?1 AND LENGTH(airport_identifier) = 4
                 LIMIT 20",
            )?;
            let rows = stmt.query_map([format!("%{}%", query.to_uppercase())], |r| r.get(0))?;
            Ok(rows.filter_map(std::result::Result::ok).collect())
        })
    }

    /// Whether the runway has an ILS (`I..`) or VOR (`D..`) procedure.
    /// Backs `nav_available_approach_types`.
    pub fn available_approach_types(
        &self,
        airport_icao: &str,
        runway_identifier: &str,
    ) -> Result<AvailableApproachTypes> {
        let none = AvailableApproachTypes {
            has_ils: false,
            has_vor: false,
        };
        if airport_icao.trim().is_empty() {
            return Ok(none);
        }
        let runway = clean_runway_number(runway_identifier);
        if runway.is_empty() {
            return Ok(none);
        }
        self.db.with(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT DISTINCT procedure_identifier
                 FROM tbl_pf_iaps
                 WHERE airport_identifier = ?1
                   AND (procedure_identifier LIKE 'I' || ?2 || '%'
                        OR procedure_identifier LIKE 'D' || ?2 || '%')",
            )?;
            let rows = stmt.query_map([airport_icao.to_uppercase(), runway], |r| {
                r.get::<_, String>(0)
            })?;
            let mut found = none;
            for p in rows.filter_map(std::result::Result::ok) {
                match p.chars().next().map(|c| c.to_ascii_uppercase()) {
                    Some('I') => found.has_ils = true,
                    Some('D') => found.has_vor = true,
                    _ => {}
                }
            }
            Ok(found)
        })
    }

    /// Waypoints of every kind whose identifier starts with `term`, exact
    /// matches first, at most 30. Terms under three characters return
    /// nothing so typing does not scan the whole database. Backs
    /// `nav_search_waypoints`.
    pub fn search_waypoints(&self, term: &str) -> Result<Vec<WaypointData>> {
        if term.trim().is_empty() || term.len() < 3 {
            return Ok(Vec::new());
        }
        let upper = term.to_uppercase();
        let pattern = format!("{upper}%");
        let mut found = self.db.with(|conn| {
            let mut out = Vec::new();
            for (source, limit) in [
                (Source::Navaid, 10),
                (Source::Enroute, 20),
                (Source::Terminal, 20),
                (Source::Airport, 10),
            ] {
                let filter = format!("{} LIKE ?1 LIMIT {limit}", source.id_col());
                out.extend(source.query(conn, &filter, &[&pattern]));
            }
            Ok(out)
        })?;
        found.sort_by(|a, b| {
            let exact = |w: &WaypointData| !w.identifier.eq_ignore_ascii_case(&upper);
            exact(a)
                .cmp(&exact(b))
                .then_with(|| a.identifier.cmp(&b.identifier))
                .then_with(|| a.waypoint_type.cmp(&b.waypoint_type))
                .then_with(|| a.area_code.cmp(&b.area_code))
        });
        found.truncate(30);
        Ok(found)
    }

    /// Waypoints within half a degree of a point, nearest first, one per
    /// identifier. Each result's `name` gains a ` (12.3 nm)` suffix, which
    /// the map context menu shows as is. Backs `nav_nearest_waypoints`;
    /// `None` means [`DEFAULT_NEAREST_WAYPOINTS`].
    pub fn nearest_waypoints(
        &self,
        latitude: f64,
        longitude: f64,
        max_results: Option<usize>,
    ) -> Result<Vec<WaypointData>> {
        let bounds = [
            latitude - 0.5,
            latitude + 0.5,
            longitude - 0.5,
            longitude + 0.5,
        ];
        let candidates = self.db.with(|conn| {
            let mut out = Vec::new();
            for (source, limit) in [
                (Source::Airport, 500),
                (Source::Navaid, 800),
                (Source::Enroute, 1500),
                (Source::Terminal, 1500),
            ] {
                let (lat, lon) = source.lat_lon_cols();
                let filter =
                    format!("{lat} BETWEEN ?1 AND ?2 AND {lon} BETWEEN ?3 AND ?4 LIMIT {limit}");
                let params: Vec<&dyn ToSql> = bounds.iter().map(|b| b as &dyn ToSql).collect();
                out.extend(source.query(conn, &filter, &params));
            }
            Ok(out)
        })?;

        let mut measured: Vec<(f64, WaypointData)> = candidates
            .into_iter()
            .map(|mut w| {
                let d = haversine_nm(latitude, longitude, w.latitude, w.longitude);
                if d.is_finite() {
                    w.name = Some(match w.name.take().filter(|n| !n.is_empty()) {
                        Some(n) => format!("{n} ({d:.1} nm)"),
                        None => format!("({d:.1} nm)"),
                    });
                }
                (if d.is_finite() { d } else { f64::MAX }, w)
            })
            .collect();
        measured.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));

        let mut seen = HashSet::new();
        let mut out: Vec<WaypointData> = measured
            .into_iter()
            .map(|(_, w)| w)
            .filter(|w| seen.insert(w.identifier.clone()))
            .collect();
        out.truncate(max_results.unwrap_or(DEFAULT_NEAREST_WAYPOINTS));
        Ok(out)
    }

    /// One waypoint by exact identifier. When several share it, prefers
    /// `preferred_area_code`, then anything in or around the US (the
    /// aircraft's home region), then the first navaid/enroute/terminal/
    /// airport match in that order. Backs `nav_waypoint_by_identifier`.
    pub fn waypoint_by_identifier(
        &self,
        identifier: &str,
        preferred_area_code: Option<&str>,
    ) -> Result<Option<WaypointData>> {
        if identifier.trim().is_empty() {
            return Ok(None);
        }
        let upper = identifier.to_uppercase();
        let candidates = self.db.with(|conn| {
            let mut out = Vec::new();
            for source in [
                Source::Navaid,
                Source::Enroute,
                Source::Terminal,
                Source::Airport,
            ] {
                let filter = format!("{} = ?1", source.id_col());
                out.extend(source.query(conn, &filter, &[&upper]));
            }
            Ok(out)
        })?;
        Ok(pick_waypoint(candidates, preferred_area_code))
    }

    /// Raw boundary points of an airspace, ordered by ring then sequence.
    /// Backs `nav_restrictive_airspace_boundary`.
    pub fn restrictive_airspace_boundary(
        &self,
        designation: &str,
    ) -> Result<Vec<RestrictiveAirspaceBoundaryPoint>> {
        self.db.with(|conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT seqno, latitude, longitude, restrictive_airspace_name, restrictive_type,
                        arc_bearing, arc_distance, arc_origin_latitude, arc_origin_longitude,
                        multiple_code, lower_limit, upper_limit
                 FROM tbl_ur_restrictive_airspace
                 WHERE {AIRSPACE_FILTER} AND restrictive_airspace_designation = ?1
                 ORDER BY multiple_code ASC, seqno ASC"
            ))?;
            let rows = stmt.query_map([designation], |r| {
                let arc_origin_latitude = get_f64(r, 7).ok();
                let arc_origin_longitude = get_f64(r, 8).ok();
                Ok(RestrictiveAirspaceBoundaryPoint {
                    sequence_number: get_i32(r, 0)?,
                    latitude: get_f64(r, 1)?,
                    longitude: get_f64(r, 2)?,
                    name: r.get(3).ok(),
                    boundary_type: r.get(4).ok(),
                    is_arc: arc_origin_latitude.is_some() && arc_origin_longitude.is_some(),
                    arc_bearing: get_f64(r, 5).ok(),
                    arc_distance: get_f64(r, 6).ok(),
                    arc_origin_latitude,
                    arc_origin_longitude,
                    multiple_code: r.get(9).ok(),
                    lower_limit: r.get(10).ok(),
                    upper_limit: r.get(11).ok(),
                })
            })?;
            Ok(rows.filter_map(std::result::Result::ok).collect())
        })
    }

    /// The boundary as one ring with arcs densified, `resolution` points
    /// per arc (default [`DEFAULT_ARC_RESOLUTION`], clamped to 8..=128).
    /// Backs `nav_restrictive_airspace_boundary_normalized`.
    pub fn restrictive_airspace_boundary_normalized(
        &self,
        designation: &str,
        resolution: Option<u32>,
    ) -> Result<Vec<BoundaryPointLL>> {
        let raw = self.restrictive_airspace_boundary(designation)?;
        let resolution = resolution.unwrap_or(DEFAULT_ARC_RESOLUTION).clamp(8, 128);
        Ok(normalize_boundary_ring(raw, resolution as usize))
    }

    /// Up to five airspaces with a boundary point within
    /// `max_distance_nm`, nearest first. Backs
    /// `nav_nearby_restrictive_airspace`.
    pub fn nearby_restrictive_airspace(
        &self,
        latitude: f64,
        longitude: f64,
        max_distance_nm: f64,
    ) -> Result<Vec<RestrictiveAirspaceDataWithDistance>> {
        let lat_pad = max_distance_nm / 60.0;
        let lon_pad = max_distance_nm / (60.0 * latitude.to_radians().cos().abs());
        let rows = self.db.with(|conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT DISTINCT restrictive_airspace_name, restrictive_airspace_designation,
                        restrictive_type, latitude, longitude
                 FROM tbl_ur_restrictive_airspace
                 WHERE {AIRSPACE_FILTER}
                   AND latitude BETWEEN ?1 AND ?2
                   AND longitude BETWEEN ?3 AND ?4
                 LIMIT 100"
            ))?;
            let rows = stmt.query_map(
                [
                    latitude - lat_pad,
                    latitude + lat_pad,
                    longitude - lon_pad,
                    longitude + lon_pad,
                ],
                |r| {
                    Ok(RestrictiveAirspaceDataWithDistance {
                        name: r.get(0)?,
                        designation: r.get(1)?,
                        airspace_type: r.get(2)?,
                        distance: haversine_nm(latitude, longitude, get_f64(r, 3)?, get_f64(r, 4)?),
                    })
                },
            )?;
            Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
        })?;

        let mut near: Vec<_> = rows
            .into_iter()
            .filter(|a| a.distance <= max_distance_nm)
            .collect();
        near.sort_by(|a, b| {
            a.distance
                .partial_cmp(&b.distance)
                .unwrap_or(Ordering::Equal)
        });
        let mut seen = HashSet::new();
        near.retain(|a| seen.insert(a.designation.clone()));
        near.truncate(5);
        Ok(near)
    }

    /// Up to 50 airspaces whose name contains `term`. Backs
    /// `nav_search_restrictive_airspace`.
    pub fn search_restrictive_airspace(&self, term: &str) -> Result<Vec<RestrictiveAirspaceData>> {
        self.db.with(|conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT DISTINCT restrictive_airspace_name, restrictive_airspace_designation,
                        restrictive_type
                 FROM tbl_ur_restrictive_airspace
                 WHERE {AIRSPACE_FILTER} AND restrictive_airspace_name LIKE ?1
                 ORDER BY restrictive_airspace_name
                 LIMIT 50"
            ))?;
            let rows = stmt.query_map([format!("%{term}%")], |r| {
                Ok(RestrictiveAirspaceData {
                    name: r.get(0)?,
                    designation: r.get(1)?,
                    airspace_type: r.get(2)?,
                })
            })?;
            Ok(rows.filter_map(std::result::Result::ok).collect())
        })
    }

    /// Backs `nav_is_valid_restrictive_airspace`.
    pub fn is_valid_restrictive_airspace(&self, designation: &str) -> Result<bool> {
        self.db.with(|conn| {
            let n: i64 = conn
                .prepare_cached(&format!(
                    "SELECT COUNT(*) FROM tbl_ur_restrictive_airspace
                     WHERE {AIRSPACE_FILTER} AND restrictive_airspace_designation = ?1"
                ))?
                .query_row([designation], |r| r.get(0))?;
            Ok(n > 0)
        })
    }

    /// Resolves an approach (`ILS`, `VOR`, ...) to a runway into what the
    /// gauge flies: fixes, distances, course, glideslope and the navaid to
    /// tune. Backs `nav_approach_data`.
    pub fn approach_data(
        &self,
        airport_icao: &str,
        runway_identifier: &str,
        approach_type: &str,
    ) -> Result<ApproachData> {
        self.db
            .with(|conn| approach::resolve(conn, airport_icao, runway_identifier, approach_type))
    }
}

/// The four tables a waypoint can come from. Each reads into
/// [`WaypointData`] its own way.
#[derive(Debug, Clone, Copy)]
enum Source {
    Navaid,
    Enroute,
    Terminal,
    Airport,
}

impl Source {
    fn id_col(self) -> &'static str {
        match self {
            Self::Navaid => "navaid_identifier",
            Self::Enroute | Self::Terminal => "waypoint_identifier",
            Self::Airport => "airport_identifier",
        }
    }

    fn lat_lon_cols(self) -> (&'static str, &'static str) {
        match self {
            // Navaids without a VOR are positioned by their DME.
            Self::Navaid => (
                "COALESCE(navaid_latitude, dme_latitude)",
                "COALESCE(navaid_longitude, dme_longitude)",
            ),
            Self::Enroute | Self::Terminal => ("waypoint_latitude", "waypoint_longitude"),
            Self::Airport => ("airport_ref_latitude", "airport_ref_longitude"),
        }
    }

    /// `SELECT .. FROM .. WHERE <base> AND`, to be followed by a filter.
    fn select(self) -> &'static str {
        match self {
            // ILS, MLS and precision DMEs have no VOR position and are not
            // something to fly to; they are tuned through the approach.
            Self::Navaid => {
                "SELECT navaid_identifier,
                        COALESCE(navaid_latitude, dme_latitude),
                        COALESCE(navaid_longitude, dme_longitude),
                        navaid_class
                 FROM tbl_d_vhfnavaids
                 WHERE COALESCE(navaid_latitude, dme_latitude) IS NOT NULL
                   AND (navaid_latitude IS NOT NULL
                        OR SUBSTR(navaid_class, 2, 1) NOT IN ('I', 'N', 'P')) AND"
            }
            Self::Enroute => {
                "SELECT waypoint_identifier, waypoint_latitude, waypoint_longitude, area_code
                 FROM tbl_ea_enroute_waypoints WHERE"
            }
            Self::Terminal => {
                "SELECT waypoint_identifier, waypoint_latitude, waypoint_longitude,
                        area_code, region_code
                 FROM tbl_pc_terminal_waypoints WHERE"
            }
            Self::Airport => {
                "SELECT airport_identifier, airport_ref_latitude, airport_ref_longitude,
                        airport_name
                 FROM tbl_pa_airports
                 WHERE LENGTH(airport_identifier) = 4 AND"
            }
        }
    }

    fn read(self, r: &Row<'_>) -> rusqlite::Result<WaypointData> {
        let (waypoint_type, name, area_code) = match self {
            Self::Navaid => {
                let class: Option<String> = r.get(3).ok();
                (
                    navaid_type_label(class.as_deref()),
                    None,
                    Some(String::new()),
                )
            }
            Self::Enroute => ("Enroute", None, r.get::<_, String>(3).ok()),
            Self::Terminal => (
                "Terminal",
                None,
                r.get::<_, String>(3)
                    .or_else(|_| r.get::<_, String>(4))
                    .ok(),
            ),
            Self::Airport => ("Airport", r.get(3).ok(), Some(String::new())),
        };
        Ok(WaypointData {
            identifier: r.get(0)?,
            latitude: get_f64(r, 1)?,
            longitude: get_f64(r, 2)?,
            waypoint_type: waypoint_type.to_owned(),
            name,
            area_code,
        })
    }

    /// Runs the source's query with `filter`. A table missing from an older
    /// navdata release, or a malformed row, drops out rather than failing
    /// the whole search.
    fn query(self, conn: &Connection, filter: &str, params: &[&dyn ToSql]) -> Vec<WaypointData> {
        let sql = format!("{} {filter}", self.select());
        let mut stmt = match conn.prepare_cached(&sql) {
            Ok(stmt) => stmt,
            Err(e) => {
                tracing::debug!(source = ?self, error = %e, "waypoint table unavailable");
                return Vec::new();
            }
        };
        match stmt.query_map(params, |r| self.read(r)) {
            Ok(rows) => rows.filter_map(std::result::Result::ok).collect(),
            Err(e) => {
                tracing::debug!(source = ?self, error = %e, "waypoint query failed");
                Vec::new()
            }
        }
    }
}

fn pick_waypoint(
    candidates: Vec<WaypointData>,
    preferred_area_code: Option<&str>,
) -> Option<WaypointData> {
    if candidates.len() <= 1 {
        return candidates.into_iter().next();
    }
    let area_is =
        |w: &WaypointData, f: &dyn Fn(&str) -> bool| w.area_code.as_deref().is_some_and(f);
    if let Some(pref) = preferred_area_code
        && let Some(i) = candidates
            .iter()
            .position(|w| area_is(w, &|a| a.eq_ignore_ascii_case(pref)))
    {
        return candidates.into_iter().nth(i);
    }
    let in_us = |w: &WaypointData| {
        area_is(w, &|a| a.starts_with('K'))
            || (w.longitude < 0.0 && w.longitude > -180.0 && w.latitude > 15.0 && w.latitude < 72.0)
    };
    let i = candidates.iter().position(in_us).unwrap_or(0);
    candidates.into_iter().nth(i)
}

#[cfg(test)]
mod tests;
