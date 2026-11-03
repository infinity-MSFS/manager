use super::*;
use crate::fixture;

fn db() -> (tempfile::TempDir, NavDb) {
    let (dir, path) = fixture::nav_db();
    let db = NavDb::open(path).unwrap();
    (dir, db)
}

fn ids(wps: &[WaypointData]) -> Vec<(&str, &str)> {
    wps.iter()
        .map(|w| (w.identifier.as_str(), w.waypoint_type.as_str()))
        .collect()
}

#[test]
fn open_reports_a_missing_file() {
    let tmp = tempfile::tempdir().unwrap();
    let err = NavDb::open(tmp.path().join("nav.s3db")).unwrap_err();
    assert!(matches!(err, Error::NotFound { .. }), "{err:?}");
}

#[test]
fn notices_the_file_being_replaced() {
    let (_dir, db) = db();
    assert!(!db.is_stale());
    Connection::open(db.path())
        .unwrap()
        .execute_batch("CREATE TABLE extra (x BLOB); INSERT INTO extra VALUES (zeroblob(65536));")
        .unwrap();
    assert!(db.is_stale());
}

#[test]
fn cycle_and_icao() {
    let (_dir, db) = db();
    assert_eq!(db.cycle().unwrap(), "2410");
    assert!(db.is_valid_icao("krnd").unwrap());
    assert!(!db.is_valid_icao("KXXX").unwrap());
    assert!(!db.is_valid_icao("  ").unwrap());
}

#[test]
fn runways_in_identifier_order() {
    let (_dir, db) = db();
    let rwys: Vec<_> = db
        .runways_for_airport("krnd")
        .unwrap()
        .into_iter()
        .map(|r| r.runway_identifier)
        .collect();
    assert_eq!(rwys, ["RW15L", "RW15R", "RW33R"]);
    assert!(db.runways_for_airport("").unwrap().is_empty());
}

#[test]
fn runway_details_with_and_without_bearings() {
    let (_dir, db) = db();
    let d = db.runway_details("krnd", "RW15L").unwrap();
    assert_eq!(
        (d.latitude, d.longitude, d.elevation_ft),
        (29.54, -98.29, 750)
    );
    assert_eq!(d.true_bearing, Some(150.1));
    let r = db.runway_details("KRND", "RW15R").unwrap();
    assert_eq!((r.true_bearing, r.magnetic_bearing), (None, None));
    assert!(matches!(
        db.runway_details("KRND", "RW01"),
        Err(Error::BadInput(_))
    ));
}

#[test]
fn airports_by_name_only_returns_four_letter_idents() {
    let (_dir, db) = db();
    let mut found = db.airports_by_name("rn").unwrap();
    found.sort();
    assert_eq!(found, ["KRND", "KRNX"]);
}

#[test]
fn approach_types_by_runway_number() {
    let (_dir, db) = db();
    let t = |rwy| db.available_approach_types("krnd", rwy).unwrap();
    assert_eq!(
        t("RW15L"),
        AvailableApproachTypes {
            has_ils: true,
            has_vor: false
        }
    );
    // Procedures carry the number, not the side.
    assert!(t("RW15R").has_ils);
    assert_eq!(
        t("RW33R"),
        AvailableApproachTypes {
            has_ils: false,
            has_vor: true
        }
    );
    assert_eq!(
        t("RWXX"),
        AvailableApproachTypes {
            has_ils: false,
            has_vor: false
        }
    );
    assert!(!db.available_approach_types("", "RW15L").unwrap().has_ils);
}

#[test]
fn search_puts_exact_matches_first() {
    let (_dir, db) = db();
    let found = db.search_waypoints("rnd").unwrap();
    assert_eq!(
        ids(&found),
        [
            ("RND", "VHF NAVAID"),
            ("RND", "VHF NAVAID"),
            ("RNDAL", "Enroute"),
            ("RNDAL", "Terminal"),
            ("RNDFA", "Terminal"),
            ("RNDT", "TACAN"),
        ]
    );
    let rndt = &found[5];
    assert_eq!((rndt.latitude, rndt.longitude), (29.53, -98.27));
    // Terminal waypoints without an area fall back to their region.
    assert_eq!(found[4].area_code.as_deref(), Some("K4"));
    assert_eq!(found[0].area_code.as_deref(), Some(""));
}

#[test]
fn search_ignores_short_terms_and_ils_navaids() {
    let (_dir, db) = db();
    assert!(db.search_waypoints("rn").unwrap().is_empty());
    assert!(db.search_waypoints("IRN").unwrap().is_empty());
    let airports = db.search_waypoints("KRN").unwrap();
    assert_eq!(ids(&airports), [("KRND", "Airport"), ("KRNX", "Airport")]);
    assert_eq!(airports[0].name.as_deref(), Some("RANDOLPH AFB"));
}

#[test]
fn nearest_waypoints_sorted_unique_and_annotated() {
    let (_dir, db) = db();
    let near = db.nearest_waypoints(29.5297, -98.2789, None).unwrap();
    assert_eq!(near.len(), DEFAULT_NEAREST_WAYPOINTS);
    assert_eq!(near[0].identifier, "KRND");
    assert_eq!(near[0].name.as_deref(), Some("RANDOLPH AFB (0.0 nm)"));

    let all = db.nearest_waypoints(29.5297, -98.2789, Some(100)).unwrap();
    let mut idents: Vec<_> = all.iter().map(|w| w.identifier.as_str()).collect();
    let dist = |w: &WaypointData| haversine_nm(29.5297, -98.2789, w.latitude, w.longitude);
    assert!(all.windows(2).all(|p| dist(&p[0]) <= dist(&p[1])));
    idents.sort_unstable();
    assert_eq!(
        idents,
        [
            "KRND", "KRNX", "KSAT", "RND", "RNDAL", "RNDFA", "RNDT", "WAYPT"
        ]
    );
    let krnx = all.iter().find(|w| w.identifier == "KRNX").unwrap();
    assert!(krnx.name.as_deref().unwrap().starts_with('('));
}

#[test]
fn waypoint_by_identifier_disambiguates() {
    let (_dir, db) = db();
    let w = db.waypoint_by_identifier("waypt", None).unwrap().unwrap();
    assert_eq!(w.area_code.as_deref(), Some("USA"));
    let w = db
        .waypoint_by_identifier("WAYPT", Some("eur"))
        .unwrap()
        .unwrap();
    assert_eq!(w.latitude, 50.0);
    let w = db
        .waypoint_by_identifier("RND", Some("nowhere"))
        .unwrap()
        .unwrap();
    assert_eq!(w.latitude, 29.52);
    assert_eq!(
        db.waypoint_by_identifier("KRND", None)
            .unwrap()
            .unwrap()
            .waypoint_type,
        "Airport"
    );
    assert!(db.waypoint_by_identifier("NOPE", None).unwrap().is_none());
    assert!(db.waypoint_by_identifier(" ", None).unwrap().is_none());
}

#[test]
fn boundary_points_in_sequence_with_arcs_flagged() {
    let (_dir, db) = db();
    let pts = db.restrictive_airspace_boundary("RND1").unwrap();
    let seqs: Vec<_> = pts.iter().map(|p| p.sequence_number).collect();
    assert_eq!(seqs, [10, 20, 30, 40]);
    assert!(pts[2].is_arc && !pts[0].is_arc);
    assert_eq!(pts[2].arc_distance, Some(30.0));
    assert_eq!(pts[0].boundary_type.as_deref(), Some("M"));
    assert_eq!(pts[0].lower_limit.as_deref(), Some("GND"));
    assert!(
        db.restrictive_airspace_boundary("ALERT1")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn normalized_boundary_expands_arcs_at_clamped_resolution() {
    let (_dir, db) = db();
    let ring = db
        .restrictive_airspace_boundary_normalized("RND1", None)
        .unwrap();
    assert_eq!(ring.len(), 3 + DEFAULT_ARC_RESOLUTION as usize);
    let coarse = db
        .restrictive_airspace_boundary_normalized("RND1", Some(1))
        .unwrap();
    assert_eq!(coarse.len(), 3 + 8);
}

#[test]
fn airspace_search_and_validity_skip_other_kinds() {
    let (_dir, db) = db();
    let found = db.search_restrictive_airspace("RANDOLPH").unwrap();
    assert_eq!(
        found,
        [RestrictiveAirspaceData {
            name: "RANDOLPH 1 MOA".into(),
            designation: "RND1".into(),
            airspace_type: "M".into(),
        }]
    );
    assert!(db.is_valid_restrictive_airspace("RND1").unwrap());
    assert!(!db.is_valid_restrictive_airspace("ALERT1").unwrap());
    assert!(!db.is_valid_restrictive_airspace("CYR1").unwrap());
}

#[test]
fn nearby_airspace_one_entry_per_designation() {
    let (_dir, db) = db();
    let near = db.nearby_restrictive_airspace(29.6, -98.3, 100.0).unwrap();
    let names: Vec<_> = near.iter().map(|a| a.designation.as_str()).collect();
    assert_eq!(names, ["RND1", "R-6312"]);
    assert!(near[0].distance < near[1].distance && near[1].distance <= 100.0);
    assert!(
        db.nearby_restrictive_airspace(29.6, -98.3, 20.0)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn ils_approach_uses_the_localizer() {
    let (_dir, db) = db();
    let a = db.approach_data("krnd", "RW15L", "ils").unwrap();
    assert_eq!(a.approch_type, "ILS");
    assert_eq!(a.airport_icao, "KRND");
    assert_eq!(
        (a.navaid_type.as_str(), a.navaid_freq),
        ("Localizer", 110.3)
    );
    assert_eq!(a.navaid_course, 151);
    assert_eq!(a.app_gs, 3.0);
    assert_eq!((a.rwy_lat, a.rwy_lon, a.rwy_elev), (29.54, -98.29, 750));
    assert_eq!((a.fip_lat, a.fip_lon), (29.70, -98.40));
    assert_eq!((a.faf_lat, a.faf_lon), (29.62, -98.34));
    let faf = haversine_nm(29.54, -98.29, 29.62, -98.34);
    assert!((a.faf_distance - faf).abs() < 1e-9);
    assert!(a.fip_distance > a.faf_distance);
}

#[test]
fn vor_approach_tunes_the_nearest_namesake_and_finds_a_course() {
    let (_dir, db) = db();
    let a = db.approach_data("KRND", "RW33R", "VOR").unwrap();
    assert_eq!((a.navaid_type.as_str(), a.navaid_freq), ("VOR", 112.2));
    // The FAF has no course; the next leg's is used.
    assert_eq!(a.navaid_course, 326);
    assert_eq!((a.fip_lat, a.fip_lon, a.fip_distance), (0.0, 0.0, 0.0));
    assert!(a.faf_distance > 0.0);
}

#[test]
fn unknown_navaid_tables_give_no_frequency() {
    let (_dir, db) = db();
    let a = db.approach_data("KRND", "RW15R", "RNAV").unwrap();
    assert_eq!((a.navaid_type.as_str(), a.navaid_freq), ("Unknown", 0.0));
    assert_eq!(a.navaid_course, 150);
}

#[test]
fn approach_input_errors() {
    let (_dir, db) = db();
    for (apt, rwy, kind) in [
        ("", "RW15L", "ILS"),
        ("KRND", "RWXX", "ILS"),
        ("KRND", "RW01", "ILS"),
        ("KRND", "RW33R", "ILS"),
    ] {
        let err = db.approach_data(apt, rwy, kind).unwrap_err();
        assert!(
            matches!(err, Error::BadInput(_)),
            "{apt} {rwy} {kind}: {err:?}"
        );
    }
}

#[test]
fn shared_across_threads() {
    let (_dir, db) = db();
    let db = std::sync::Arc::new(db);
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let db = db.clone();
            std::thread::spawn(move || db.cycle().unwrap())
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().unwrap(), "2410");
    }
}

#[test]
fn pick_prefers_k_area_codes() {
    let wp = |lat: f64, area: &str| WaypointData {
        identifier: "X".into(),
        latitude: lat,
        longitude: 10.0,
        waypoint_type: "Enroute".into(),
        name: None,
        area_code: Some(area.into()),
    };
    let picked = pick_waypoint(vec![wp(1.0, "EUR"), wp(2.0, "K2")], None).unwrap();
    assert_eq!(picked.latitude, 2.0);
    let first = pick_waypoint(vec![wp(1.0, "EUR"), wp(2.0, "PAC")], None).unwrap();
    assert_eq!(first.latitude, 1.0);
    assert!(pick_waypoint(Vec::new(), None).is_none());
}
