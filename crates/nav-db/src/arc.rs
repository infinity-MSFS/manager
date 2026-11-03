//! Turning airspace boundaries into drawable rings. Pure geometry.
//!
//! ARINC 424 boundaries are points joined by great-circle lines, except
//! that a point with an arc origin means "follow a circle about the origin
//! from the previous point to the next". Expanding those arcs into short
//! straight steps gives a polygon a map can fill.

use infinity_ipc::nav::{BoundaryPointLL, RestrictiveAirspaceBoundaryPoint};

const EARTH_RADIUS_NM: f64 = 3440.065;
/// Points per arc in [`expand_training_areas`].
const TRAINING_AREA_ARC_RESOLUTION: usize = 16;

/// Where you end up starting at `(lat, lon)` and travelling `distance_nm`
/// on `bearing_deg`.
pub fn destination_point(
    lat: f64,
    lon: f64,
    bearing_deg: f64,
    distance_nm: f64,
) -> BoundaryPointLL {
    let lat1 = lat.to_radians();
    let lon1 = lon.to_radians();
    let bearing = bearing_deg.to_radians();
    let dr = distance_nm / EARTH_RADIUS_NM;
    let lat2 = (lat1.sin() * dr.cos() + lat1.cos() * dr.sin() * bearing.cos()).asin();
    let lon2 =
        lon1 + (bearing.sin() * dr.sin() * lat1.cos()).atan2(dr.cos() - lat1.sin() * lat2.sin());
    BoundaryPointLL {
        lat: lat2.to_degrees(),
        lon: lon2.to_degrees(),
    }
}

/// One ring per ground-level band, arcs densified. Mirrors the v1 zones
/// map: bands that start above the ground are dropped (unless nothing is
/// left), and in rings mixing straight and arc points the arc points are
/// dropped, because ARINC uses small arcs to carve exclusions (Holloman,
/// Timberon) that read as noise at map scale. Backs
/// `nav_expand_training_areas`.
pub fn expand_training_areas(
    points: Vec<RestrictiveAirspaceBoundaryPoint>,
) -> Vec<Vec<BoundaryPointLL>> {
    // Points arrive ordered by ring then sequence; split on ring changes.
    let mut groups: Vec<Vec<RestrictiveAirspaceBoundaryPoint>> = Vec::new();
    for p in points {
        match groups.last_mut() {
            Some(g)
                if g.first()
                    .is_some_and(|f| f.multiple_code == p.multiple_code) =>
            {
                g.push(p)
            }
            _ => groups.push(vec![p]),
        }
    }

    let on_ground = |g: &Vec<RestrictiveAirspaceBoundaryPoint>| {
        g.first()
            .is_some_and(|p| is_ground_level(p.lower_limit.as_deref()))
    };
    if groups.iter().any(on_ground) {
        groups.retain(on_ground);
    }

    groups
        .into_iter()
        .filter_map(|pts| {
            let kept: Vec<&RestrictiveAirspaceBoundaryPoint> = if pts.iter().any(|p| !p.is_arc) {
                pts.iter().filter(|p| !p.is_arc).collect()
            } else {
                pts.iter().collect()
            };
            let ring = expand_ring(&kept, TRAINING_AREA_ARC_RESOLUTION, true);
            (ring.len() >= 3).then_some(ring)
        })
        .collect()
}

/// The whole boundary as a single ring in sequence order, each arc
/// expanded to `resolution` points. Arc points missing their origin or
/// radius are skipped.
pub fn normalize_boundary_ring(
    mut points: Vec<RestrictiveAirspaceBoundaryPoint>,
    resolution: usize,
) -> Vec<BoundaryPointLL> {
    points.sort_by_key(|p| p.sequence_number);
    let refs: Vec<&RestrictiveAirspaceBoundaryPoint> = points.iter().collect();
    expand_ring(&refs, resolution, false)
}

/// Expands each arc point between its neighbours. `keep_incomplete` keeps
/// an arc point lacking origin or radius as a plain vertex instead of
/// dropping it; the two callers differ here as v3 did.
fn expand_ring(
    pts: &[&RestrictiveAirspaceBoundaryPoint],
    resolution: usize,
    keep_incomplete: bool,
) -> Vec<BoundaryPointLL> {
    let n = pts.len();
    let mut out = Vec::with_capacity(n);
    for (i, p) in pts.iter().enumerate() {
        let vertex = BoundaryPointLL {
            lat: p.latitude,
            lon: p.longitude,
        };
        if !p.is_arc {
            out.push(vertex);
            continue;
        }
        let (Some(olat), Some(olon), Some(radius)) = (
            p.arc_origin_latitude,
            p.arc_origin_longitude,
            p.arc_distance,
        ) else {
            if keep_incomplete {
                out.push(vertex);
            }
            continue;
        };
        let prev = pts[(i + n - 1) % n];
        let next = pts[(i + 1) % n];
        let start = bearing(olat, olon, prev.latitude, prev.longitude);
        let end = bearing(olat, olon, next.latitude, next.longitude);
        out.extend(arc(
            olat,
            olon,
            radius,
            start,
            end,
            p.arc_bearing,
            resolution,
        ));
    }
    out
}

/// Initial great-circle bearing from the origin to a point, 0..360.
fn bearing(origin_lat: f64, origin_lon: f64, to_lat: f64, to_lon: f64) -> f64 {
    let lat1 = origin_lat.to_radians();
    let lat2 = to_lat.to_radians();
    let dlon = (to_lon - origin_lon).to_radians();
    let y = dlon.sin() * lat2.cos();
    let x = lat1.cos() * lat2.sin() - lat1.sin() * lat2.cos() * dlon.cos();
    norm360(y.atan2(x).to_degrees())
}

fn norm360(a: f64) -> f64 {
    a.rem_euclid(360.0)
}

/// Degrees turned going clockwise (increasing bearing) from `from` to `to`.
fn sweep_increasing(from: f64, to: f64) -> f64 {
    norm360(to - from)
}

/// Degrees turned going anticlockwise (decreasing bearing).
fn sweep_decreasing(from: f64, to: f64) -> f64 {
    norm360(from - to)
}

/// `resolution` points on the circle from `start` to `end`. The direction
/// is the one whose sweep contains `hint` (the point's own bearing from the
/// origin, which lies on the arc), else the shorter one.
fn arc(
    olat: f64,
    olon: f64,
    radius_nm: f64,
    start: f64,
    end: f64,
    hint: Option<f64>,
    resolution: usize,
) -> Vec<BoundaryPointLL> {
    let (start, end) = (norm360(start), norm360(end));
    let up = sweep_increasing(start, end);
    let down = sweep_decreasing(start, end);
    let increasing = match hint.map(norm360) {
        Some(h) => {
            let on_up = sweep_increasing(start, h) <= up + 1e-9;
            let on_down = sweep_decreasing(start, h) <= down + 1e-9;
            match (on_up, on_down) {
                (true, false) => true,
                (false, true) => false,
                _ => up <= down,
            }
        }
        None => up <= down,
    };
    let (total, sign) = if increasing { (up, 1.0) } else { (down, -1.0) };
    let steps = resolution.max(2);
    (0..steps)
        .map(|i| {
            let t = i as f64 / (steps - 1) as f64;
            destination_point(olat, olon, start + sign * total * t, radius_nm)
        })
        .collect()
}

fn is_ground_level(limit: Option<&str>) -> bool {
    let Some(t) = limit.map(str::trim).filter(|t| !t.is_empty()) else {
        return true;
    };
    let t = t.to_ascii_uppercase();
    matches!(t.as_str(), "GND" | "SFC" | "MSL") || t.parse::<i32>() == Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::haversine_nm;

    fn point(seq: i32, lat: f64, lon: f64) -> RestrictiveAirspaceBoundaryPoint {
        RestrictiveAirspaceBoundaryPoint {
            sequence_number: seq,
            latitude: lat,
            longitude: lon,
            name: None,
            boundary_type: None,
            is_arc: false,
            arc_bearing: None,
            arc_distance: None,
            arc_origin_latitude: None,
            arc_origin_longitude: None,
            multiple_code: Some("A".into()),
            lower_limit: Some("GND".into()),
            upper_limit: None,
        }
    }

    fn arc_point(
        seq: i32,
        lat: f64,
        lon: f64,
        origin: (f64, f64),
        radius: f64,
    ) -> RestrictiveAirspaceBoundaryPoint {
        RestrictiveAirspaceBoundaryPoint {
            is_arc: true,
            arc_origin_latitude: Some(origin.0),
            arc_origin_longitude: Some(origin.1),
            arc_distance: Some(radius),
            ..point(seq, lat, lon)
        }
    }

    #[test]
    fn destination_point_goes_north() {
        let p = destination_point(0.0, 0.0, 0.0, 60.0);
        assert!((p.lat - 0.9994).abs() < 1e-3, "{p:?}");
        assert!(p.lon.abs() < 1e-9);
    }

    #[test]
    fn arcs_stay_on_their_circle() {
        let origin = (30.0, -98.0);
        let north = destination_point(origin.0, origin.1, 0.0, 10.0);
        let east = destination_point(origin.0, origin.1, 90.0, 10.0);
        let pts = vec![
            point(10, north.lat, north.lon),
            arc_point(20, east.lat, east.lon, origin, 10.0),
            point(30, 29.0, -98.0),
        ];
        let ring = normalize_boundary_ring(pts, 8);
        assert_eq!(ring.len(), 1 + 8 + 1);
        for p in &ring[1..9] {
            let r = haversine_nm(origin.0, origin.1, p.lat, p.lon);
            assert!((r - 10.0).abs() < 0.01, "{r}");
        }
        // Starts at the previous point's bearing (north), swings east.
        assert!((ring[1].lat - north.lat).abs() < 1e-6);
        assert!(ring[4].lon > origin.1);
    }

    #[test]
    fn hint_picks_the_long_way_round() {
        // From bearing 0 to 90, but the arc point sits at 225: go the long
        // way through the south-west.
        let pts = arc(0.0, 0.0, 10.0, 0.0, 90.0, Some(225.0), 5);
        let mid = pts[2];
        assert!(mid.lat < 0.0 && mid.lon < 0.0, "{mid:?}");
        let short = arc(0.0, 0.0, 10.0, 0.0, 90.0, None, 5);
        assert!(short[2].lat > 0.0 && short[2].lon > 0.0);
    }

    #[test]
    fn normalize_skips_incomplete_arcs_and_sorts() {
        let mut broken = point(15, 9.0, 9.0);
        broken.is_arc = true;
        let ring =
            normalize_boundary_ring(vec![point(20, 2.0, 2.0), broken, point(10, 1.0, 1.0)], 8);
        assert_eq!(
            ring,
            vec![
                BoundaryPointLL { lat: 1.0, lon: 1.0 },
                BoundaryPointLL { lat: 2.0, lon: 2.0 }
            ]
        );
        assert!(normalize_boundary_ring(Vec::new(), 8).is_empty());
    }

    #[test]
    fn training_areas_keep_ground_rings_and_drop_exclusion_arcs() {
        let mut upper: Vec<_> = (0..4)
            .map(|i| RestrictiveAirspaceBoundaryPoint {
                multiple_code: Some("B".into()),
                lower_limit: Some("18000".into()),
                ..point(i, 5.0 + f64::from(i), 5.0)
            })
            .collect();
        let mut pts = vec![
            point(10, 0.0, 0.0),
            point(20, 0.0, 1.0),
            arc_point(25, 0.5, 1.2, (0.5, 1.0), 5.0),
            point(30, 1.0, 1.0),
        ];
        pts.append(&mut upper);
        let rings = expand_training_areas(pts);
        assert_eq!(rings.len(), 1);
        assert_eq!(rings[0].len(), 3);
    }

    #[test]
    fn training_areas_fall_back_to_upper_rings_and_expand_pure_arcs() {
        let origin = (0.0, 0.0);
        let ring: Vec<_> = [0.0, 120.0, 240.0]
            .iter()
            .zip(1..)
            .map(|(b, i)| {
                let p = destination_point(origin.0, origin.1, *b, 5.0);
                RestrictiveAirspaceBoundaryPoint {
                    lower_limit: Some("5000".into()),
                    ..arc_point(i, p.lat, p.lon, origin, 5.0)
                }
            })
            .collect();
        let rings = expand_training_areas(ring);
        assert_eq!(rings.len(), 1);
        assert_eq!(rings[0].len(), 3 * TRAINING_AREA_ARC_RESOLUTION);
        assert!(expand_training_areas(Vec::new()).is_empty());
    }

    #[test]
    fn ground_level_limits() {
        for g in [
            None,
            Some(""),
            Some("gnd"),
            Some("SFC"),
            Some("MSL"),
            Some("0"),
        ] {
            assert!(is_ground_level(g), "{g:?}");
        }
        assert!(!is_ground_level(Some("500")));
        assert!(!is_ground_level(Some("FL180")));
    }
}
