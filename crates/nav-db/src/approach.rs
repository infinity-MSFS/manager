//! Approach resolution: from a runway and approach type to the fixes,
//! course, glideslope and navaid the gauge flies.
//!
//! An instrument approach in `tbl_pf_iaps` is a set of legs grouped by
//! route type: `A` legs are transitions, and the final approach legs carry
//! the procedure's own prefix (`I` for ILS, `D` for VOR/DME). The final
//! approach fix is coded `E??F` and the intermediate fix `E??I` in the
//! ARINC 424 waypoint description.

use rusqlite::{Connection, OptionalExtension};

use crate::error::{Error, Result};
use crate::helpers::{clean_runway_number, haversine_km, haversine_nm, normalize_approach_prefix};
use infinity_ipc::nav::ApproachData;

/// A VOR further than this from the runway is a namesake elsewhere in the
/// world, not the approach's navaid.
const MAX_NAVAID_KM: f64 = 200.0;

#[derive(Debug, Clone)]
struct Leg {
    route_type: String,
    description: Option<String>,
    lat: Option<f64>,
    lon: Option<f64>,
    course: Option<f64>,
    vertical_angle: Option<f64>,
    navaid: Option<String>,
    navaid_table: Option<String>,
}

impl Leg {
    fn is_fix(&self, kind: char) -> bool {
        self.description
            .as_deref()
            .map(str::trim)
            .is_some_and(|c| c.len() == 4 && c.starts_with('E') && c.ends_with(kind))
    }

    fn position(&self) -> Option<(f64, f64)> {
        Some((self.lat?, self.lon?))
    }
}

pub(crate) fn resolve(
    conn: &Connection,
    airport_icao: &str,
    runway_identifier: &str,
    approach_type: &str,
) -> Result<ApproachData> {
    if airport_icao.trim().is_empty()
        || runway_identifier.trim().is_empty()
        || approach_type.trim().is_empty()
    {
        return Err(Error::BadInput(
            "airport_icao/runway_identifier/approach_type cannot be empty".into(),
        ));
    }
    let airport = airport_icao.trim().to_uppercase();
    let runway_num = clean_runway_number(runway_identifier);
    if runway_num.is_empty() {
        return Err(Error::BadInput(
            "Unable to parse runway number from runway_identifier".into(),
        ));
    }
    let prefix = normalize_approach_prefix(approach_type)
        .ok_or_else(|| Error::BadInput("Unable to normalize approach_type".into()))?;

    let (rwy_lon, rwy_lat, rwy_elev) = runway(conn, &airport, runway_identifier)?;
    let legs = legs(conn, &airport, &runway_num, prefix)?;
    tracing::debug!(?legs, "approach legs");

    let (transitions, finals) = split(&legs, prefix);
    let fip = transitions.iter().chain(&finals).find(|l| l.is_fix('I'));
    let faf = finals.iter().chain(&transitions).find(|l| l.is_fix('F'));

    let (fip_lat, fip_lon) = fip.and_then(Leg::position).unwrap_or((0.0, 0.0));
    let (faf_lat, faf_lon) = faf.and_then(Leg::position).unwrap_or((0.0, 0.0));
    let distance = |lat: f64, lon: f64| {
        if lat != 0.0 || lon != 0.0 {
            haversine_nm(rwy_lat, rwy_lon, lat, lon)
        } else {
            0.0
        }
    };

    // VOR/DME procedures often leave the FAF course empty; any course
    // further along the procedure is the inbound one.
    let navaid_course = faf
        .and_then(|l| l.course)
        .or_else(|| finals.iter().rev().find_map(|l| l.course))
        .or_else(|| transitions.iter().rev().find_map(|l| l.course))
        .or_else(|| legs.iter().rev().find_map(|l| l.course))
        .map_or(0, |c| c.round() as i32);

    let (navaid_type, navaid_freq) = recommended_navaid(
        conn,
        &airport,
        rwy_lat,
        rwy_lon,
        faf.or_else(|| finals.first()).or_else(|| legs.first()),
    )?;

    Ok(ApproachData {
        approch_type: approach_type.trim().to_uppercase(),
        airport_icao: airport,
        navaid_type: navaid_type.into(),
        navaid_freq,
        navaid_course,
        rwy_lon,
        rwy_lat,
        rwy_elev,
        fip_distance: distance(fip_lat, fip_lon),
        faf_distance: distance(faf_lat, faf_lon),
        app_gs: faf.and_then(|l| l.vertical_angle).unwrap_or(0.0).abs(),
        fip_lat,
        fip_lon,
        faf_lat,
        faf_lon,
    })
}

/// Threshold longitude, latitude and elevation.
fn runway(conn: &Connection, airport: &str, runway_identifier: &str) -> Result<(f64, f64, i32)> {
    conn.prepare_cached(
        "SELECT runway_longitude, runway_latitude, landing_threshold_elevation
         FROM tbl_pg_runways
         WHERE airport_identifier = ?1 AND runway_identifier = ?2
         LIMIT 1",
    )?
    .query_row([airport, runway_identifier], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })
    .optional()?
    .ok_or_else(|| Error::BadInput("Runway not found".into()))
}

fn legs(conn: &Connection, airport: &str, runway_num: &str, prefix: char) -> Result<Vec<Leg>> {
    let mut stmt = conn.prepare_cached(
        "SELECT route_type, seqno, waypoint_description_code, waypoint_latitude,
                waypoint_longitude, course, vertical_angle, recommended_navaid,
                recommended_navaid_ref_table
         FROM tbl_pf_iaps
         WHERE airport_identifier = ?1 AND procedure_identifier LIKE ?2
         ORDER BY route_type ASC, seqno ASC",
    )?;
    let rows = stmt.query_map([airport, &format!("{prefix}{runway_num}%")], |r| {
        // Read so a leg without a sequence number drops out, as in v1.
        let _seqno: i32 = r.get(1)?;
        Ok(Leg {
            route_type: r.get(0)?,
            description: r.get(2)?,
            lat: r.get(3)?,
            lon: r.get(4)?,
            course: r.get(5)?,
            vertical_angle: r.get(6)?,
            navaid: r.get(7)?,
            navaid_table: r.get(8)?,
        })
    })?;
    let legs: Vec<Leg> = rows.filter_map(std::result::Result::ok).collect();
    if legs.is_empty() {
        return Err(Error::BadInput(
            "No approach waypoints found (tbl_pf_iaps)".into(),
        ));
    }
    Ok(legs)
}

/// Transitions and final-approach legs. A procedure whose finals are coded
/// under some other route type gets every non-transition leg as finals.
fn split(legs: &[Leg], prefix: char) -> (Vec<Leg>, Vec<Leg>) {
    let route = |l: &Leg| l.route_type.trim().to_uppercase();
    let prefix = prefix.to_string();
    let transitions: Vec<Leg> = legs.iter().filter(|l| route(l) == "A").cloned().collect();
    let mut finals: Vec<Leg> = legs
        .iter()
        .filter(|l| route(l) == prefix)
        .cloned()
        .collect();
    if finals.is_empty() {
        finals = legs.iter().filter(|l| route(l) != "A").cloned().collect();
    }
    (transitions, finals)
}

/// The navaid to tune: a VOR from `tbl_d_vhfnavaids` (`D`) or a localizer
/// from `tbl_pi_localizers_glideslopes` (`PI`). Frequency 0 when it cannot
/// be found.
fn recommended_navaid(
    conn: &Connection,
    airport: &str,
    rwy_lat: f64,
    rwy_lon: f64,
    leg: Option<&Leg>,
) -> Result<(&'static str, f64)> {
    let id = leg
        .and_then(|l| l.navaid.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let table = leg
        .and_then(|l| l.navaid_table.as_deref())
        .map(|t| t.trim().to_lowercase());
    let (Some(id), Some(table)) = (id, table) else {
        return Ok(("Unknown", 0.0));
    };
    match table.as_str() {
        "d" => Ok((
            "VOR",
            closest_vor_freq(conn, id, rwy_lat, rwy_lon)?.unwrap_or(0.0),
        )),
        "pi" => Ok((
            "Localizer",
            localizer_freq(conn, airport, id)?.unwrap_or(0.0),
        )),
        _ => Ok(("Unknown", 0.0)),
    }
}

/// VOR identifiers repeat worldwide, so take the nearest one in range.
fn closest_vor_freq(
    conn: &Connection,
    id: &str,
    rwy_lat: f64,
    rwy_lon: f64,
) -> Result<Option<f64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT navaid_frequency,
                COALESCE(navaid_latitude, dme_latitude),
                COALESCE(navaid_longitude, dme_longitude)
         FROM tbl_d_vhfnavaids
         WHERE navaid_identifier = ?1
           AND COALESCE(navaid_latitude, dme_latitude) IS NOT NULL
           AND (navaid_latitude IS NOT NULL
                OR SUBSTR(navaid_class, 2, 1) NOT IN ('I', 'N', 'P'))",
    )?;
    let rows = stmt.query_map([id], |r| {
        Ok((
            r.get::<_, f64>(0)?,
            r.get::<_, f64>(1)?,
            r.get::<_, f64>(2)?,
        ))
    })?;
    let best = rows
        .filter_map(std::result::Result::ok)
        .map(|(freq, lat, lon)| (freq, haversine_km(rwy_lat, rwy_lon, lat, lon)))
        .filter(|&(_, d)| d <= MAX_NAVAID_KM)
        .min_by(|a, b| a.1.total_cmp(&b.1));
    Ok(best.map(|(freq, _)| freq))
}

fn localizer_freq(conn: &Connection, airport: &str, id: &str) -> Result<Option<f64>> {
    Ok(conn
        .prepare_cached(
            "SELECT llz_frequency
             FROM tbl_pi_localizers_glideslopes
             WHERE airport_identifier = ?1 AND llz_identifier = ?2
             LIMIT 1",
        )?
        .query_row([airport, id], |r| r.get::<_, f64>(0))
        .optional()?)
}
