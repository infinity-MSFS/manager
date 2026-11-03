//! The approach slot model and its pure rules (v3 `ApproachesPage.tsx`,
//! which ported v1's `ApproachCard` and `arcUtils`).
//!
//! Wire shape: `APP.APP_<n>` objects with v1's field names, `Approch_type`
//! typo included.

use serde_json::{Map, Value};

use crate::configurator::schema::{num_at, number, str_at};

pub(super) const SLOT_BASE: u32 = 500;
pub(super) const SLOT_COUNT: usize = 10;

pub(super) fn slot_key(slot: u32) -> String {
    format!("APP_{slot}")
}

/// One saved approach (v1 `ApproachConfig`).
#[derive(Debug, Clone, PartialEq, Default)]
pub(super) struct Config {
    /// `SCA`, `ILS` or `VOR`.
    pub approach_type: String,
    pub icao: String,
    pub runway: String,
    pub navaid_type: String,
    pub navaid_freq: f64,
    pub navaid_course: f64,
    pub rwy_lat: f64,
    pub rwy_lon: f64,
    pub rwy_elev: f64,
    pub app_gs: f64,
    pub fip_distance: Option<f64>,
    pub faf_distance: Option<f64>,
    pub fip_lat: Option<f64>,
    pub fip_lon: Option<f64>,
    pub faf_lat: Option<f64>,
    pub faf_lon: Option<f64>,
}

impl Config {
    /// A saved slot; anything but an object is no slot.
    pub(super) fn from_value(v: &Value) -> Option<Self> {
        if !v.is_object() {
            return None;
        }
        let n = |k: &str| num_at(v, k).unwrap_or(0.0);
        let o = |k: &str| num_at(v, k);
        Some(Self {
            approach_type: str_at(v, "Approch_type"),
            icao: str_at(v, "Airport_icao"),
            runway: str_at(v, "runway_name"),
            navaid_type: str_at(v, "navaid_type"),
            navaid_freq: n("navaid_freq"),
            navaid_course: n("navaid_course"),
            rwy_lat: n("rwy_lat"),
            rwy_lon: n("rwy_lon"),
            rwy_elev: n("rwy_elev"),
            app_gs: n("app_gs"),
            fip_distance: o("FIP_distance"),
            faf_distance: o("FAF_distance"),
            fip_lat: o("FIP_lat"),
            fip_lon: o("FIP_lon"),
            faf_lat: o("FAF_lat"),
            faf_lon: o("FAF_lon"),
        })
    }

    /// The wire object. Missing optionals are omitted, as v3's JSON was;
    /// whole numbers stay integers for the gauge's integer fields.
    pub(super) fn to_value(&self) -> Value {
        let mut m = Map::new();
        let mut s = |k: &str, v: &str| {
            m.insert(k.to_owned(), Value::String(v.to_owned()));
        };
        s("Approch_type", &self.approach_type);
        s("Airport_icao", &self.icao);
        s("navaid_type", &self.navaid_type);
        s("runway_name", &self.runway);
        for (k, v) in [
            ("navaid_freq", Some(self.navaid_freq)),
            ("navaid_course", Some(self.navaid_course)),
            ("rwy_lon", Some(self.rwy_lon)),
            ("rwy_lat", Some(self.rwy_lat)),
            ("rwy_elev", Some(self.rwy_elev)),
            ("FIP_distance", self.fip_distance),
            ("FAF_distance", self.faf_distance),
            ("FIP_lat", self.fip_lat),
            ("FIP_lon", self.fip_lon),
            ("FAF_lat", self.faf_lat),
            ("FAF_lon", self.faf_lon),
            ("app_gs", Some(self.app_gs)),
        ] {
            if let Some(v) = v {
                m.insert(k.to_owned(), number(v));
            }
        }
        Value::Object(m)
    }
}

/// A card's working copy: edited freely, written on Save.
#[derive(Debug, Clone, PartialEq, Default)]
pub(super) struct Draft {
    pub c: Config,
    /// Also marks an opened empty slot as visible.
    pub expanded: bool,
    /// Local time of the last nav-database fill.
    pub last_sync: Option<String>,
}

impl Draft {
    pub(super) fn blank(committed: Option<&Config>) -> Self {
        let c = committed.cloned().unwrap_or_else(|| Config {
            approach_type: "SCA".to_owned(),
            navaid_type: "V".to_owned(),
            ..Config::default()
        });
        let mut c = c;
        if c.approach_type.is_empty() {
            c.approach_type = "SCA".to_owned();
        }
        if c.navaid_type.is_empty() {
            c.navaid_type = "V".to_owned();
        }
        Self {
            c,
            expanded: false,
            last_sync: None,
        }
    }

    /// Something typed in: keeps the card on screen.
    pub(super) fn has_something(&self) -> bool {
        !self.c.icao.is_empty() || !self.c.runway.is_empty() || self.c.navaid_course != 0.0
    }

    /// Nothing that a share-load would overwrite.
    pub(super) fn is_empty(&self) -> bool {
        self.c.icao.is_empty() && self.c.runway.is_empty() && self.c.navaid_freq == 0.0
    }

    /// Drops everything derived from the airport/runway/type selection, so
    /// a previous selection's data cannot survive into a new one.
    pub(super) fn clear_nav(&mut self) {
        let c = &mut self.c;
        c.navaid_type = "V".to_owned();
        c.navaid_freq = 0.0;
        c.navaid_course = 0.0;
        c.rwy_lat = 0.0;
        c.rwy_lon = 0.0;
        c.rwy_elev = 0.0;
        c.app_gs = 0.0;
        c.fip_distance = None;
        c.faf_distance = None;
        c.fip_lat = None;
        c.fip_lon = None;
        c.faf_lat = None;
        c.faf_lon = None;
        self.last_sync = None;
    }

    /// What Save writes: an SCA always carries navaid type `V`.
    pub(super) fn to_config(&self) -> Config {
        let mut c = self.c.clone();
        if c.approach_type == "SCA" {
            c.navaid_type = "V".to_owned();
        }
        c
    }
}

pub(super) fn clamp_icao(v: &str) -> String {
    v.to_uppercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(4)
        .collect()
}

pub(super) fn normalize360(deg: f64) -> f64 {
    deg.rem_euclid(360.0)
}

fn near(a: Option<f64>, b: Option<f64>, eps: f64) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => (a - b).abs() <= eps,
        (None, None) => true,
        _ => false,
    }
}

/// Great-circle destination from (lat, lon) degrees, `dist_nm`, bearing
/// degrees true (v1 `destinationPointNm`).
pub(super) fn destination_point_nm(lat: f64, lon: f64, dist_nm: f64, bearing: f64) -> (f64, f64) {
    const R: f64 = 3440.065;
    let p1 = lat.to_radians();
    let l1 = lon.to_radians();
    let t = bearing.to_radians();
    let d = dist_nm / R;
    let p2 = (p1.sin() * d.cos() + p1.cos() * d.sin() * t.cos()).asin();
    let l2 = l1 + (t.sin() * d.sin() * p1.cos()).atan2(d.cos() - p1.sin() * p2.sin());
    (p2.to_degrees(), l2.to_degrees())
}

fn positive(v: Option<f64>) -> Option<f64> {
    v.filter(|n| n.is_finite() && *n > 0.0)
}

/// For an SCA, FIP and FAF lie on the reciprocal of the inbound course at
/// their distances from the threshold. `Some` with the updated config when
/// anything moved, `None` when it is already right (or not an SCA yet).
pub(super) fn derive_sca(c: &Config) -> Option<Config> {
    if c.approach_type != "SCA"
        || !c.rwy_lat.is_finite()
        || c.rwy_lat == 0.0
        || !c.rwy_lon.is_finite()
        || c.rwy_lon == 0.0
        || !c.navaid_course.is_finite()
        || c.navaid_course <= 0.0
    {
        return None;
    }
    let reciprocal = normalize360(c.navaid_course + 180.0);
    let at = |d: Option<f64>| {
        positive(d).map(|d| destination_point_nm(c.rwy_lat, c.rwy_lon, d, reciprocal))
    };
    let mut next = c.clone();
    let mut changed = false;
    let mut place = |pt: Option<(f64, f64)>, lat: &mut Option<f64>, lon: &mut Option<f64>| match pt
    {
        Some((la, lo)) => {
            if !near(*lat, Some(la), 1e-8) || !near(*lon, Some(lo), 1e-8) {
                *lat = Some(la);
                *lon = Some(lo);
                changed = true;
            }
        }
        None => {
            if lat.is_some() || lon.is_some() {
                *lat = None;
                *lon = None;
                changed = true;
            }
        }
    };
    place(at(c.fip_distance), &mut next.fip_lat, &mut next.fip_lon);
    place(at(c.faf_distance), &mut next.faf_lat, &mut next.faf_lon);
    changed.then_some(next)
}

fn finite_pos(v: f64) -> bool {
    v.is_finite() && v > 0.0
}

/// Why a draft cannot be saved yet (v1 `isCompleteDraft`, same wording).
pub(super) fn missing(c: &Config) -> Vec<&'static str> {
    let mut r = Vec::new();
    if c.icao.chars().count() != 4 {
        r.push("Airport ICAO is required.");
    }
    if c.runway.is_empty() {
        r.push("Runway is required.");
    }
    if c.approach_type.is_empty() {
        r.push("Approach type is required.");
    }
    if c.approach_type != "SCA" && c.navaid_type.is_empty() {
        r.push("Navaid type is required.");
    }
    if !finite_pos(c.navaid_freq) {
        r.push("Navaid frequency must be > 0.");
    }
    if !finite_pos(c.navaid_course) {
        r.push("Approach course must be > 0.");
    }
    if !c.rwy_lat.is_finite() || c.rwy_lat == 0.0 {
        r.push("Runway latitude is required.");
    }
    if !c.rwy_lon.is_finite() || c.rwy_lon == 0.0 {
        r.push("Runway longitude is required.");
    }
    if !c.rwy_elev.is_finite() {
        r.push("Runway elevation is required.");
    }
    if c.approach_type != "VOR" && !finite_pos(c.app_gs) {
        r.push("Glideslope angle must be > 0.");
    }
    if c.approach_type == "SCA" {
        let fip = positive(c.fip_distance);
        let faf = positive(c.faf_distance);
        if fip.is_none() {
            r.push("FIP distance is required for SCA.");
        }
        if faf.is_none() {
            r.push("FAP distance is required for SCA.");
        }
        let has = |v: Option<f64>| v.is_some_and(f64::is_finite);
        if fip.is_some() && !(has(c.fip_lat) && has(c.fip_lon)) {
            r.push("FIP lat/lon is missing (check runway/course/distances).");
        }
        if faf.is_some() && !(has(c.faf_lat) && has(c.faf_lon)) {
            r.push("FAP lat/lon is missing (check runway/course/distances).");
        }
    }
    r
}

/// Draft against saved, floats within 1e-9 (FIP/FAF are recomputed).
pub(super) fn configs_equal(a: &Config, b: Option<&Config>) -> bool {
    let Some(b) = b else { return false };
    let e = |x: f64, y: f64| (x - y).abs() <= 1e-9;
    let o = |x: Option<f64>, y: Option<f64>| near(x, y, 1e-9);
    a.approach_type == b.approach_type
        && a.icao == b.icao
        && a.navaid_type == b.navaid_type
        && a.runway == b.runway
        && e(a.navaid_freq, b.navaid_freq)
        && e(a.navaid_course, b.navaid_course)
        && e(a.rwy_lat, b.rwy_lat)
        && e(a.rwy_lon, b.rwy_lon)
        && e(a.rwy_elev, b.rwy_elev)
        && e(a.app_gs, b.app_gs)
        && o(a.fip_distance, b.fip_distance)
        && o(a.faf_distance, b.faf_distance)
        && o(a.fip_lat, b.fip_lat)
        && o(a.fip_lon, b.fip_lon)
        && o(a.faf_lat, b.faf_lat)
        && o(a.faf_lon, b.faf_lon)
}

/// The card heading.
pub(super) fn header_title(slot: u32, draft: &Config, committed: Option<&Config>) -> String {
    if let Some(c) = committed {
        format!("{} {} {}", c.icao, c.approach_type, c.runway)
            .trim()
            .to_owned()
    } else if !draft.icao.is_empty() {
        let rwy = if draft.runway.is_empty() {
            "—"
        } else {
            &draft.runway
        };
        format!("{} {} {rwy}", draft.icao, draft.approach_type)
    } else {
        format!("Approach {slot}")
    }
}

/// A runway's true bearing as an inbound course: rounded, 0 becomes 360.
pub(super) fn course_from_bearing(bearing: f64) -> f64 {
    let c = bearing.round();
    if c == 0.0 { 360.0 } else { c }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sca() -> Config {
        Config {
            approach_type: "SCA".into(),
            icao: "KPHX".into(),
            runway: "08".into(),
            navaid_type: "V".into(),
            navaid_freq: 110.3,
            navaid_course: 90.0,
            rwy_lat: 33.0,
            rwy_lon: -112.0,
            rwy_elev: 1100.0,
            app_gs: 3.0,
            fip_distance: Some(10.0),
            faf_distance: Some(5.0),
            ..Config::default()
        }
    }

    #[test]
    fn destination_point_moves_along_the_bearing() {
        let (lat, lon) = destination_point_nm(0.0, 0.0, 60.0, 0.0);
        assert!((lat - 1.0).abs() < 0.01);
        assert!(lon.abs() < 1e-9);
        let (lat, lon) = destination_point_nm(0.0, 0.0, 60.0, 270.0);
        assert!(lat.abs() < 1e-6);
        assert!((lon + 1.0).abs() < 0.01);
    }

    #[test]
    fn sca_points_sit_on_the_reciprocal_and_settle() {
        let next = derive_sca(&sca()).unwrap_or_default();
        // Course 090: the fixes are west of the threshold.
        assert!(next.fip_lon.is_some_and(|l| l < -112.0));
        assert!(
            next.faf_lon
                .is_some_and(|l| l < -112.0 && l > next.fip_lon.unwrap_or(0.0))
        );
        assert_eq!(derive_sca(&next), None, "stable once placed");
        let mut no_dist = next.clone();
        no_dist.fip_distance = None;
        let cleared = derive_sca(&no_dist).unwrap_or_default();
        assert_eq!(cleared.fip_lat, None);
        let mut ils = sca();
        ils.approach_type = "ILS".into();
        assert_eq!(derive_sca(&ils), None);
    }

    #[test]
    fn completeness_matches_v1() {
        let done = derive_sca(&sca()).unwrap_or_default();
        assert!(missing(&done).is_empty());
        assert!(
            missing(&sca()).contains(&"FIP lat/lon is missing (check runway/course/distances).")
        );
        let blank = Draft::blank(None).c;
        let m = missing(&blank);
        assert_eq!(m[0], "Airport ICAO is required.");
        assert!(m.contains(&"FIP distance is required for SCA."));
        let mut vor = done.clone();
        vor.approach_type = "VOR".into();
        vor.app_gs = 0.0;
        assert!(!missing(&vor).contains(&"Glideslope angle must be > 0."));
    }

    #[test]
    fn wire_round_trip_keeps_names_and_integers() {
        let c = derive_sca(&sca()).unwrap_or_default();
        let v = c.to_value();
        assert_eq!(v["Approch_type"], json!("SCA"));
        assert_eq!(v["rwy_elev"], json!(1100));
        assert_eq!(v["navaid_course"], json!(90));
        let mut no_faf = c.clone();
        no_faf.faf_distance = None;
        assert!(no_faf.to_value().get("FAF_distance").is_none());
        let back = Config::from_value(&v);
        assert!(configs_equal(&c, back.as_ref()));
        assert!(!configs_equal(&c, None));
        assert_eq!(Config::from_value(&json!("x")), None);
    }

    #[test]
    fn drafts_clear_and_convert() {
        let mut d = Draft::blank(Some(&sca()));
        assert!(d.has_something());
        d.c.navaid_type = "ILS".into();
        assert_eq!(d.to_config().navaid_type, "V");
        d.clear_nav();
        assert_eq!(d.c.navaid_freq, 0.0);
        assert_eq!(d.c.fip_distance, None);
        assert_eq!(d.c.icao, "KPHX");
        assert!(Draft::blank(None).is_empty());
        assert_eq!(clamp_icao("k-ph x9z"), "KPHX");
        assert_eq!(course_from_bearing(0.2), 360.0);
        assert_eq!(course_from_bearing(89.6), 90.0);
        assert_eq!(normalize360(-90.0), 270.0);
        assert_eq!(
            header_title(503, &Draft::blank(None).c, None),
            "Approach 503"
        );
        assert_eq!(header_title(503, &sca(), Some(&sca())), "KPHX SCA 08");
    }
}
