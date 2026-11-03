//! Corridor geometry for FAA military training routes (v3
//! `lib/mtrCorridor.ts`).
//!
//! A route arrives as segments, each a centreline polyline with left and
//! right half-widths in nautical miles. The segments are chained into
//! continuous centrelines by shared endpoints (joints take the mean width,
//! so the corridor tapers), every centreline step becomes a trapezoid,
//! every turn gets bevel triangles on both sides, and the union of all of
//! them is the corridor. The union uses the vendored `polygon-clipping`
//! (Martinez-Rueda), as v3 did.
//!
//! Local flat earth: 1 NM is 1/60 degree of latitude, longitude scaled by
//! cos(lat) at the chain's first vertex. Fine for corridor widths of tens
//! of miles at CONUS latitudes.

use infinity_ipc::IpcError;
use infinity_ipc::nav::{MtrLatLon, MtrSegment};
use wasm_bindgen::JsValue;

use super::assets;
use super::zones::LatLon;

const NM_PER_DEGREE_LAT: f64 = 60.0;
/// Endpoint match tolerance, about half a mile: FAA vertices meant to
/// coincide agree to ~6 dp.
const JOIN_TOLERANCE: f64 = 0.005;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ChainVertex {
    pub lat: f64,
    pub lon: f64,
    pub width_left: f64,
    pub width_right: f64,
}

#[derive(Debug, Clone, Copy)]
struct Frame {
    cos_lat: f64,
}

impl Frame {
    fn at(lat: f64) -> Self {
        Self {
            cos_lat: lat.to_radians().cos(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Normal {
    nx: f64,
    ny: f64,
}

/// Unit left normal (port side) of a to b in local NM space.
fn unit_normal(a: (f64, f64), b: (f64, f64), frame: Frame) -> Option<Normal> {
    let dx = (b.1 - a.1) * frame.cos_lat * NM_PER_DEGREE_LAT;
    let dy = (b.0 - a.0) * NM_PER_DEGREE_LAT;
    let m = dx.hypot(dy);
    (m >= 1e-9).then(|| Normal {
        nx: -dy / m,
        ny: dx / m,
    })
}

/// `from` moved `dist_nm` along (nx, ny). Returns (lat, lon).
fn offset(from: (f64, f64), nx: f64, ny: f64, dist_nm: f64, frame: Frame) -> (f64, f64) {
    if dist_nm <= 0.0 {
        return from;
    }
    let dy = ny * dist_nm / NM_PER_DEGREE_LAT;
    let dx = if frame.cos_lat > 1e-9 {
        nx * dist_nm / (NM_PER_DEGREE_LAT * frame.cos_lat)
    } else {
        0.0
    };
    (from.0 + dy, from.1 + dx)
}

fn same(a: &MtrLatLon, b: &ChainVertex) -> bool {
    (a.lat - b.lat).abs() < JOIN_TOLERANCE && (a.lon - b.lon).abs() < JOIN_TOLERANCE
}

fn vertex(p: &MtrLatLon, wl: f64, wr: f64) -> ChainVertex {
    ChainVertex {
        lat: p.lat,
        lon: p.lon,
        width_left: wl,
        width_right: wr,
    }
}

struct Part<'a> {
    points: &'a [MtrLatLon],
    wl: f64,
    wr: f64,
}

#[allow(clippy::cast_precision_loss)]
fn widths(seg: &MtrSegment) -> (f64, f64) {
    (seg.width_left_nm as f64, seg.width_right_nm as f64)
}

/// Joins every polyline of every segment into continuous centrelines,
/// reversing parts as needed. Disconnected pieces become separate chains.
pub(crate) fn chain_segments(segments: &[MtrSegment]) -> Vec<Vec<ChainVertex>> {
    let mut parts = Vec::new();
    for seg in segments {
        let (wl, wr) = widths(seg);
        if wl <= 0.0 && wr <= 0.0 {
            continue;
        }
        for line in &seg.polylines {
            if line.len() >= 2 {
                parts.push(Part {
                    points: line,
                    wl,
                    wr,
                });
            }
        }
    }
    let mut used = vec![false; parts.len()];
    let mut chains = Vec::new();

    while let Some(seed) = used.iter().position(|u| !u) {
        used[seed] = true;
        let p = &parts[seed];
        let mut chain: Vec<ChainVertex> = p.points.iter().map(|q| vertex(q, p.wl, p.wr)).collect();

        let mut extended = true;
        while extended {
            extended = false;
            // Forward: a part whose end meets the tail.
            for (i, p) in parts.iter().enumerate() {
                if used[i] {
                    continue;
                }
                let (Some(first), Some(last), Some(tail)) =
                    (p.points.first(), p.points.last(), chain.last_mut())
                else {
                    continue;
                };
                let forward = if same(first, tail) {
                    Some(true)
                } else if same(last, tail) {
                    Some(false)
                } else {
                    None
                };
                let Some(forward) = forward else { continue };
                tail.width_left = (tail.width_left + p.wl) / 2.0;
                tail.width_right = (tail.width_right + p.wr) / 2.0;
                let rest: Vec<ChainVertex> = if forward {
                    p.points
                        .iter()
                        .skip(1)
                        .map(|q| vertex(q, p.wl, p.wr))
                        .collect()
                } else {
                    p.points
                        .iter()
                        .rev()
                        .skip(1)
                        .map(|q| vertex(q, p.wl, p.wr))
                        .collect()
                };
                chain.extend(rest);
                used[i] = true;
                extended = true;
            }
            // Backward: a part whose end meets the head.
            for (i, p) in parts.iter().enumerate() {
                if used[i] {
                    continue;
                }
                let (Some(first), Some(last), Some(head)) =
                    (p.points.first(), p.points.last(), chain.first_mut())
                else {
                    continue;
                };
                let natural = if same(last, head) {
                    Some(true)
                } else if same(first, head) {
                    Some(false)
                } else {
                    None
                };
                let Some(natural) = natural else { continue };
                head.width_left = (head.width_left + p.wl) / 2.0;
                head.width_right = (head.width_right + p.wr) / 2.0;
                let n = p.points.len();
                let prepend: Vec<ChainVertex> = if natural {
                    p.points[..n - 1]
                        .iter()
                        .map(|q| vertex(q, p.wl, p.wr))
                        .collect()
                } else {
                    p.points[1..]
                        .iter()
                        .rev()
                        .map(|q| vertex(q, p.wl, p.wr))
                        .collect()
                };
                chain.splice(0..0, prepend);
                used[i] = true;
                extended = true;
            }
        }
        chains.push(chain);
    }
    chains
}

/// A closed ring of `[lon, lat]` pairs, polygon-clipping's convention.
pub(crate) type Ring = Vec<[f64; 2]>;

/// One trapezoid per centreline step, plus left and right bevel triangles
/// at every turning vertex to fill the outside of the turn.
pub(crate) fn trapezoids(chain: &[ChainVertex]) -> Vec<Ring> {
    let Some(first) = chain.first() else {
        return Vec::new();
    };
    if chain.len() < 2 {
        return Vec::new();
    }
    let frame = Frame::at(first.lat);
    let ll = |v: &ChainVertex| (v.lat, v.lon);
    let pair = |p: (f64, f64)| [p.1, p.0];
    let normals: Vec<Option<Normal>> = chain
        .windows(2)
        .map(|w| unit_normal(ll(&w[0]), ll(&w[1]), frame))
        .collect();
    let mut out = Vec::new();

    for (w, n) in chain.windows(2).zip(&normals) {
        let Some(n) = n else { continue };
        let (a, b) = (&w[0], &w[1]);
        let a_l = offset(ll(a), n.nx, n.ny, a.width_left, frame);
        let b_l = offset(ll(b), n.nx, n.ny, b.width_left, frame);
        let b_r = offset(ll(b), -n.nx, -n.ny, b.width_right, frame);
        let a_r = offset(ll(a), -n.nx, -n.ny, a.width_right, frame);
        out.push(vec![pair(a_l), pair(b_l), pair(b_r), pair(a_r), pair(a_l)]);
    }

    for (i, v) in chain.iter().enumerate().take(chain.len() - 1).skip(1) {
        let (Some(Some(n_in)), Some(Some(n_out))) = (normals.get(i - 1), normals.get(i)) else {
            continue;
        };
        // Straight through: no wedge, and a sliver confuses the union.
        if n_in.nx * n_out.nx + n_in.ny * n_out.ny > 0.9999 {
            continue;
        }
        let apex = pair(ll(v));
        for (sign, width) in [(1.0, v.width_left), (-1.0, v.width_right)] {
            let c_in = offset(ll(v), sign * n_in.nx, sign * n_in.ny, width, frame);
            let c_out = offset(ll(v), sign * n_out.nx, sign * n_out.ny, width, frame);
            out.push(vec![apex, pair(c_in), pair(c_out), apex]);
        }
    }
    out
}

/// Closes a ring (last point equal to the first).
pub(crate) fn close(ring: &mut Vec<LatLon>) {
    if let (Some(first), Some(last)) = (ring.first().copied(), ring.last().copied())
        && (first.lat != last.lat || first.lon != last.lon)
    {
        ring.push(first);
    }
}

/// Unions rings with polygon-clipping. Each input is one single-ring
/// polygon; output rings are outer rings then holes, each closed.
async fn union(rings: Vec<Ring>) -> Result<Vec<Vec<LatLon>>, IpcError> {
    if rings.is_empty() {
        return Ok(Vec::new());
    }
    assets::ensure(assets::POLYGON_CLIPPING_JS).await?;
    let (Some(lib), Some(f)) = (
        assets::global("polygonClipping"),
        assets::global_fn("polygonClipping", "union"),
    ) else {
        return Err(IpcError::internal("polygon-clipping did not load"));
    };
    let ser = serde_wasm_bindgen::Serializer::json_compatible();
    let args = js_sys::Array::new();
    for ring in rings {
        let polygon = vec![ring];
        let js = serde::Serialize::serialize(&polygon, &ser)
            .map_err(|e| IpcError::internal(format!("corridor: {e}")))?;
        args.push(&js);
    }
    let result: JsValue = f
        .apply(&lib, &args)
        .map_err(|e| IpcError::internal(format!("corridor union failed: {e:?}")))?;
    let multi: Vec<Vec<Vec<Vec<f64>>>> = serde_wasm_bindgen::from_value(result)
        .map_err(|e| IpcError::internal(format!("corridor union: {e}")))?;
    let mut out = Vec::new();
    for polygon in multi {
        for ring in polygon {
            let mut pts: Vec<LatLon> = ring
                .iter()
                .map(|p| LatLon {
                    lat: p.get(1).copied().unwrap_or_default(),
                    lon: p.first().copied().unwrap_or_default(),
                })
                .collect();
            close(&mut pts);
            out.push(pts);
        }
    }
    Ok(out)
}

/// The route's corridor as clean closed rings (v3
/// `expandMtrAsConnectedPath`). Rings under four points are dropped.
pub(crate) async fn connected_path(segments: &[MtrSegment]) -> Result<Vec<Vec<LatLon>>, IpcError> {
    let all: Vec<Ring> = chain_segments(segments)
        .iter()
        .flat_map(|c| trapezoids(c))
        .collect();
    Ok(union(all)
        .await?
        .into_iter()
        .filter(|r| r.len() >= 4)
        .collect())
}

#[cfg(test)]
mod tests {
    use infinity_ipc::nav::MtrFamily;

    use super::*;

    fn seg(id: i64, wl: i64, wr: i64, line: &[(f64, f64)]) -> MtrSegment {
        MtrSegment {
            object_id: id,
            ident_full: "VR1".into(),
            family: MtrFamily::Vr,
            lower_ft: None,
            lower_code: None,
            upper_ft: None,
            upper_code: None,
            width_left_nm: wl,
            width_right_nm: wr,
            polylines: vec![
                line.iter()
                    .map(|&(lat, lon)| MtrLatLon { lat, lon })
                    .collect(),
            ],
        }
    }

    #[test]
    fn chains_join_reversed_parts_and_average_widths() {
        let segs = [
            seg(1, 4, 4, &[(0.0, 0.0), (0.0, 1.0)]),
            // Stored backwards: its end meets the first part's tail.
            seg(2, 2, 2, &[(0.0, 2.0), (0.0, 1.0)]),
            // Meets the head.
            seg(3, 6, 6, &[(-1.0, 0.0), (0.0, 0.0)]),
        ];
        let chains = chain_segments(&segs);
        assert_eq!(chains.len(), 1);
        let c = &chains[0];
        let lons: Vec<f64> = c.iter().map(|v| v.lon).collect();
        assert_eq!(lons, vec![0.0, 0.0, 1.0, 2.0]);
        assert_eq!(c[0].lat, -1.0);
        // Joint between 1 (4 nm) and 2 (2 nm).
        assert_eq!(c[2].width_left, 3.0);
        // Joint between 3 (6 nm) and 1 (4 nm).
        assert_eq!(c[1].width_left, 5.0);
    }

    #[test]
    fn zero_width_segments_and_disconnected_pieces() {
        let segs = [
            seg(1, 0, 0, &[(0.0, 0.0), (0.0, 1.0)]),
            seg(2, 1, 1, &[(5.0, 5.0), (5.0, 6.0)]),
            seg(3, 1, 1, &[(9.0, 9.0), (9.0, 10.0)]),
        ];
        assert_eq!(chain_segments(&segs).len(), 2);
        assert!(chain_segments(&[]).is_empty());
    }

    #[test]
    fn a_straight_step_is_one_trapezoid_of_the_right_width() {
        let chain = [
            ChainVertex {
                lat: 0.0,
                lon: 0.0,
                width_left: 3.0,
                width_right: 6.0,
            },
            ChainVertex {
                lat: 1.0,
                lon: 0.0,
                width_left: 3.0,
                width_right: 6.0,
            },
        ];
        let t = trapezoids(&chain);
        assert_eq!(t.len(), 1);
        let ring = &t[0];
        assert_eq!(ring.len(), 5);
        assert_eq!(ring.first(), ring.last());
        // Heading north: left is west (negative lon), 3 nm = 0.05 deg at the equator.
        assert!((ring[0][0] + 0.05).abs() < 1e-9);
        assert!((ring[3][0] - 0.1).abs() < 1e-9);
        assert!((ring[1][1] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn turns_add_two_bevels_and_straight_joints_none() {
        let v = |lat: f64, lon: f64| ChainVertex {
            lat,
            lon,
            width_left: 1.0,
            width_right: 1.0,
        };
        assert_eq!(
            trapezoids(&[v(0.0, 0.0), v(1.0, 0.0), v(2.0, 0.0)]).len(),
            2
        );
        assert_eq!(
            trapezoids(&[v(0.0, 0.0), v(1.0, 0.0), v(1.0, 1.0)]).len(),
            4
        );
        assert!(trapezoids(&[v(0.0, 0.0)]).is_empty());
    }

    #[test]
    fn close_appends_the_first_point_once() {
        let mut r = vec![LatLon { lat: 0.0, lon: 0.0 }, LatLon { lat: 1.0, lon: 0.0 }];
        close(&mut r);
        assert_eq!(r.len(), 3);
        close(&mut r);
        assert_eq!(r.len(), 3);
    }
}
