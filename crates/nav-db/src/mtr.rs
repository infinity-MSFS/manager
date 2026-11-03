//! Queries against the bundled FAA Military Training Route database
//! (`faa_mtr.sqlite`, built by the T-38 repo's `mtr_importer` from the FAA
//! AIS MTRSegment feature service).
//!
//! FAA route idents are numeric (`002`, `1064`); `mtr_type` gives the
//! family, and the display label `VR1064` / `IR002` is put together here.
//! Each route is several `mtr_segments` rows, each with a polyline in
//! `mtr_segment_points`.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::{Row, params};

use crate::db::Db;
use crate::error::{Error, Result};
use crate::helpers::haversine_nm;
use infinity_ipc::nav::{MtrFamily, MtrLatLon, MtrRouteSummary, MtrRouteWithDistance, MtrSegment};

/// Result cap `mtr_nearby` used.
pub const DEFAULT_NEARBY_ROUTES: usize = 50;

const SUMMARY_COLUMNS: &str = "s.ident, s.mtr_type, COUNT(*),
     MIN(CASE WHEN s.lower_uom = 'FT' THEN s.lower_val END),
     MAX(CASE WHEN s.upper_uom = 'FT' THEN s.upper_val END)";

/// A read-only handle to the MTR database. Same threading rules as
/// [`NavDb`](crate::NavDb): blocking, `Send + Sync`, run on the blocking
/// pool. The file ships with the app, so one handle lives for the session.
#[derive(Debug)]
pub struct MtrDb {
    db: Db,
}

impl MtrDb {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            db: Db::open(path.into())?,
        })
    }

    pub fn path(&self) -> &Path {
        self.db.path()
    }

    /// Routes whose ident contains the query, by family then ident. A
    /// leading `VR` or `IR` restricts the family; an empty query lists
    /// every route. Backs `mtr_search`.
    pub fn search(&self, query: &str) -> Result<Vec<MtrRouteSummary>> {
        let q = query.trim().to_ascii_uppercase();
        let (family, ident) = match split_family(&q) {
            Some((family, rest)) => (Some(family_to_db(family)), rest.trim()),
            None => (None, q.as_str()),
        };
        let pattern = (!ident.is_empty()).then(|| format!("%{ident}%"));
        self.db.with(|conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT {SUMMARY_COLUMNS}
                 FROM mtr_segments s
                 WHERE (?1 IS NULL OR s.mtr_type = ?1)
                   AND (?2 IS NULL OR s.ident LIKE ?2)
                 GROUP BY s.ident, s.mtr_type
                 ORDER BY s.mtr_type, s.ident"
            ))?;
            let rows = stmt.query_map(params![family, pattern], read_summary)?;
            Ok(rows.filter_map(std::result::Result::ok).collect())
        })
    }

    /// Routes with a centreline vertex within `max_distance_nm`, nearest
    /// first, at most `max_results` (0 for no cap). Vertex spacing on the
    /// FAA charts is close enough that nearest vertex stands in for
    /// distance to the corridor. Backs `mtr_nearby` (with
    /// [`DEFAULT_NEARBY_ROUTES`]).
    pub fn nearby(
        &self,
        lat: f64,
        lon: f64,
        max_distance_nm: f64,
        max_results: usize,
    ) -> Result<Vec<MtrRouteWithDistance>> {
        // A degree of latitude is 60 nm; longitude shrinks with cos(lat).
        // At least half a degree so a nearby vertex is never boxed out.
        let lat_pad = (max_distance_nm / 60.0).max(0.5);
        let lon_pad = (max_distance_nm / 60.0 / lat.to_radians().cos().max(0.05)).max(0.5);

        let mut out = self.db.with(|conn| {
            let mut points = conn.prepare_cached(
                "SELECT s.ident, s.mtr_type, p.lat, p.lon
                 FROM mtr_segment_points p
                 JOIN mtr_segments s ON s.object_id = p.object_id
                 WHERE p.lat BETWEEN ?1 AND ?2 AND p.lon BETWEEN ?3 AND ?4",
            )?;
            let rows = points.query_map(
                params![lat - lat_pad, lat + lat_pad, lon - lon_pad, lon + lon_pad],
                |r| {
                    Ok((
                        (r.get::<_, String>(0)?, r.get::<_, i64>(1)?),
                        haversine_nm(lat, lon, r.get(2)?, r.get(3)?),
                    ))
                },
            )?;
            let mut closest: BTreeMap<(String, i64), f64> = BTreeMap::new();
            for (route, d) in rows.filter_map(std::result::Result::ok) {
                if d <= max_distance_nm {
                    let best = closest.entry(route).or_insert(f64::INFINITY);
                    *best = best.min(d);
                }
            }

            let mut summary = conn.prepare_cached(&format!(
                "SELECT {SUMMARY_COLUMNS}
                 FROM mtr_segments s
                 WHERE s.ident = ?1 AND s.mtr_type = ?2
                 GROUP BY s.ident, s.mtr_type"
            ))?;
            let mut out = Vec::with_capacity(closest.len());
            for ((ident, mtr_type), distance_nm) in closest {
                if let Ok(route) = summary.query_row(params![ident, mtr_type], read_summary) {
                    out.push(MtrRouteWithDistance { route, distance_nm });
                }
            }
            Ok(out)
        })?;

        out.sort_by(|a, b| {
            a.distance_nm
                .partial_cmp(&b.distance_nm)
                .unwrap_or(Ordering::Equal)
        });
        if max_results > 0 {
            out.truncate(max_results);
        }
        Ok(out)
    }

    /// Every segment of one route (`VR1064`, `IR002`) with its geometry.
    /// Backs `mtr_segments_by_ident`.
    pub fn segments_by_ident(&self, ident_full: &str) -> Result<Vec<MtrSegment>> {
        let upper = ident_full.trim().to_ascii_uppercase();
        let (family, ident) = split_family(&upper)
            .ok_or_else(|| Error::BadInput(format!("invalid MTR ident: {ident_full}")))?;
        self.db.with(|conn| {
            let mut seg_stmt = conn.prepare_cached(
                "SELECT object_id, ident, mtr_type,
                        lower_val, lower_uom, lower_code,
                        upper_val, upper_uom, upper_code,
                        width_left_nm, width_right_nm
                 FROM mtr_segments
                 WHERE ident = ?1 AND mtr_type = ?2
                 ORDER BY object_id",
            )?;
            let mut segments: Vec<MtrSegment> = seg_stmt
                .query_map(params![ident, family_to_db(family)], |r| {
                    let family = family_from_db(r.get(2)?);
                    Ok(MtrSegment {
                        object_id: r.get(0)?,
                        ident_full: format!("{}{}", family.prefix(), r.get::<_, String>(1)?),
                        family,
                        lower_ft: feet(r.get(3)?, r.get(4)?),
                        lower_code: r.get(5)?,
                        upper_ft: feet(r.get(6)?, r.get(7)?),
                        upper_code: r.get(8)?,
                        width_left_nm: r.get::<_, Option<i64>>(9)?.unwrap_or(0),
                        width_right_nm: r.get::<_, Option<i64>>(10)?.unwrap_or(0),
                        polylines: Vec::new(),
                    })
                })?
                .filter_map(std::result::Result::ok)
                .collect();

            let mut pts_stmt = conn.prepare_cached(
                "SELECT part_index, lat, lon
                 FROM mtr_segment_points
                 WHERE object_id = ?1
                 ORDER BY part_index, point_index",
            )?;
            for seg in &mut segments {
                let rows = pts_stmt.query_map([seg.object_id], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        MtrLatLon {
                            lat: r.get(1)?,
                            lon: r.get(2)?,
                        },
                    ))
                })?;
                for (part, point) in rows.filter_map(std::result::Result::ok) {
                    let Ok(part) = usize::try_from(part) else {
                        continue;
                    };
                    if seg.polylines.len() <= part {
                        seg.polylines.resize_with(part + 1, Vec::new);
                    }
                    seg.polylines[part].push(point);
                }
            }
            Ok(segments)
        })
    }
}

fn read_summary(r: &Row<'_>) -> rusqlite::Result<MtrRouteSummary> {
    let ident: String = r.get(0)?;
    let family = family_from_db(r.get::<_, Option<i64>>(1)?.unwrap_or(0));
    Ok(MtrRouteSummary {
        ident_full: format!("{}{ident}", family.prefix()),
        is_low_level: family == MtrFamily::Ir && ident.len() == 4,
        ident,
        family,
        segment_count: r.get(2)?,
        min_lower_ft: r.get(3)?,
        max_upper_ft: r.get(4)?,
    })
}

/// Altitudes in other units (flight levels) are left out rather than
/// passed off as feet.
fn feet(val: Option<f64>, uom: Option<String>) -> Option<f64> {
    val.filter(|_| uom.as_deref() == Some("FT"))
}

/// Splits `VR1064` into its family and ident. Expects uppercase.
fn split_family(s: &str) -> Option<(MtrFamily, &str)> {
    if let Some(rest) = s.strip_prefix("VR") {
        Some((MtrFamily::Vr, rest))
    } else {
        s.strip_prefix("IR").map(|rest| (MtrFamily::Ir, rest))
    }
}

/// `mtr_segments.mtr_type`: 0 is visual, 1 instrument.
fn family_from_db(v: i64) -> MtrFamily {
    if v == 1 { MtrFamily::Ir } else { MtrFamily::Vr }
}

fn family_to_db(family: MtrFamily) -> i64 {
    match family {
        MtrFamily::Vr => 0,
        MtrFamily::Ir => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture;

    fn db() -> (tempfile::TempDir, MtrDb) {
        let (dir, path) = fixture::mtr_db();
        let db = MtrDb::open(path).unwrap();
        (dir, db)
    }

    fn idents(routes: &[MtrRouteSummary]) -> Vec<&str> {
        routes.iter().map(|r| r.ident_full.as_str()).collect()
    }

    #[test]
    fn search_lists_everything_by_family_then_ident() {
        let (_dir, db) = db();
        assert_eq!(
            idents(&db.search("").unwrap()),
            ["VR1064", "IR002", "IR1001"]
        );
    }

    #[test]
    fn search_filters_by_family_and_ident() {
        let (_dir, db) = db();
        assert_eq!(idents(&db.search("ir").unwrap()), ["IR002", "IR1001"]);
        assert_eq!(idents(&db.search(" IR 100 ").unwrap()), ["IR1001"]);
        assert_eq!(idents(&db.search("10").unwrap()), ["VR1064", "IR1001"]);
        assert!(db.search("VR002").unwrap().is_empty());
    }

    #[test]
    fn summary_aggregates_segments_in_feet_only() {
        let (_dir, db) = db();
        let routes = db.search("VR1064").unwrap();
        let r = &routes[0];
        assert_eq!(r.ident, "1064");
        assert_eq!(r.family, MtrFamily::Vr);
        assert!(!r.is_low_level);
        assert_eq!(r.segment_count, 2);
        assert_eq!(r.min_lower_ft, Some(100.0));
        // The FL ceiling on one segment is not feet.
        assert_eq!(r.max_upper_ft, Some(1500.0));

        let ll = &db.search("IR1001").unwrap()[0];
        assert!(ll.is_low_level);
        assert!(!db.search("IR002").unwrap()[0].is_low_level);
    }

    #[test]
    fn nearby_folds_segments_into_routes_nearest_first() {
        let (_dir, db) = db();
        let near = db.nearby(30.0, -98.0, 30.0, DEFAULT_NEARBY_ROUTES).unwrap();
        assert_eq!(
            idents(&near.iter().map(|r| r.route.clone()).collect::<Vec<_>>()),
            ["VR1064", "IR002"]
        );
        assert!(near[0].distance_nm < 1.0);
        assert_eq!(near[0].route.segment_count, 2);
        assert!(near[1].distance_nm > near[0].distance_nm && near[1].distance_nm <= 30.0);

        assert_eq!(db.nearby(30.0, -98.0, 30.0, 1).unwrap().len(), 1);
        assert!(db.nearby(0.0, 0.0, 30.0, 0).unwrap().is_empty());
    }

    #[test]
    fn segments_carry_ordered_polylines_per_part() {
        let (_dir, db) = db();
        let segs = db.segments_by_ident("vr1064").unwrap();
        assert_eq!(segs.len(), 2);
        let first = &segs[0];
        assert_eq!(first.ident_full, "VR1064");
        assert_eq!(first.width_left_nm, 4);
        assert_eq!(first.width_right_nm, 0);
        assert_eq!(first.lower_code.as_deref(), Some("HEI"));
        assert_eq!(first.polylines.len(), 2);
        assert_eq!(
            first.polylines[0],
            [
                MtrLatLon {
                    lat: 30.0,
                    lon: -98.0
                },
                MtrLatLon {
                    lat: 30.1,
                    lon: -98.0
                }
            ]
        );
        assert_eq!(segs[1].upper_ft, None);
        assert_eq!(segs[1].upper_code.as_deref(), Some("STD"));
    }

    #[test]
    fn segments_reject_idents_without_a_family() {
        let (_dir, db) = db();
        assert!(matches!(
            db.segments_by_ident("1064"),
            Err(Error::BadInput(_))
        ));
        assert!(db.segments_by_ident("IR9999").unwrap().is_empty());
    }

    /// Runs against the real FAA database. Point `INFINITY_MTR_DB` at a
    /// copy (v3 ships it as `crates/app/resources/faa_mtr.sqlite`).
    #[test]
    #[ignore = "needs INFINITY_MTR_DB"]
    fn smoke_real_mtr_database() {
        let Some(path) = std::env::var_os("INFINITY_MTR_DB") else {
            return;
        };
        let db = MtrDb::open(PathBuf::from(path)).unwrap();
        let all = db.search("").unwrap();
        assert!(all.len() > 100, "{}", all.len());
        let first = &all[0];
        let segs = db.segments_by_ident(&first.ident_full).unwrap();
        assert_eq!(segs.len() as i64, first.segment_count);
        let vertex = segs[0].polylines[0][0];
        let near = db
            .nearby(vertex.lat, vertex.lon, 5.0, DEFAULT_NEARBY_ROUTES)
            .unwrap();
        assert!(near.iter().any(|r| r.route.ident_full == first.ident_full));
    }
}
