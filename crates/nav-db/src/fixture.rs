//! Small databases with the columns the queries read, built per test.

use std::path::PathBuf;

use rusqlite::Connection;
use tempfile::TempDir;

fn build(name: &str, sql: &str) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(sql).unwrap();
    (dir, path)
}

/// Randolph AFB (KRND) and surroundings, plus namesakes abroad to test
/// disambiguation.
pub(crate) fn nav_db() -> (TempDir, PathBuf) {
    build("nav.s3db", NAV_SQL)
}

pub(crate) fn mtr_db() -> (TempDir, PathBuf) {
    build("faa_mtr.sqlite", MTR_SQL)
}

const NAV_SQL: &str = "
CREATE TABLE tbl_hdr_header (cycle TEXT);
INSERT INTO tbl_hdr_header VALUES ('2410');

CREATE TABLE tbl_pa_airports (
    airport_identifier TEXT, airport_ref_latitude REAL, airport_ref_longitude REAL,
    airport_name TEXT);
INSERT INTO tbl_pa_airports VALUES
    ('KRND', 29.5297, -98.2789, 'RANDOLPH AFB'),
    ('KSAT', 29.5337, -98.4698, 'SAN ANTONIO INTL'),
    ('RND', 29.5, -98.3, 'NOT AN ICAO'),
    ('KRNX', '29.6', '-98.2', NULL);

CREATE TABLE tbl_pg_runways (
    airport_identifier TEXT, runway_identifier TEXT, runway_latitude REAL,
    runway_longitude REAL, landing_threshold_elevation INTEGER,
    runway_true_bearing REAL, runway_magnetic_bearing REAL);
INSERT INTO tbl_pg_runways VALUES
    ('KRND', 'RW33R', 29.51, -98.27, 740, 330.1, 326.0),
    ('KRND', 'RW15L', 29.54, -98.29, 750, 150.1, 146.0),
    ('KRND', 'RW15R', 29.54, -98.28, 752, NULL, NULL);

CREATE TABLE tbl_pf_iaps (
    airport_identifier TEXT, procedure_identifier TEXT, route_type TEXT, seqno INTEGER,
    waypoint_description_code TEXT, waypoint_latitude REAL, waypoint_longitude REAL,
    course REAL, vertical_angle REAL, recommended_navaid TEXT,
    recommended_navaid_ref_table TEXT);
INSERT INTO tbl_pf_iaps VALUES
    ('KRND', 'I15L', 'I', 20, 'GY M', 29.54, -98.29, 151.0, NULL, NULL, NULL),
    ('KRND', 'I15L', 'I', 10, 'E  F', 29.62, -98.34, 151.4, -3.0, 'IRND', 'PI'),
    ('KRND', 'I15L', 'A', 10, 'E  I', 29.70, -98.40, 150.0, NULL, NULL, NULL),
    ('KRND', 'D33R', 'D', 10, 'E  F', 29.45, -98.22, NULL, NULL, 'RND', 'D'),
    ('KRND', 'D33R', 'D', 20, 'GY M', 29.51, -98.27, 326.4, NULL, NULL, NULL),
    ('KRND', 'R15R', 'R', 10, 'E  F', 29.60, -98.30, 150.0, NULL, 'XXX', 'Q');

CREATE TABLE tbl_pi_localizers_glideslopes (
    airport_identifier TEXT, llz_identifier TEXT, llz_frequency REAL);
INSERT INTO tbl_pi_localizers_glideslopes VALUES ('KRND', 'IRND', 110.3);

CREATE TABLE tbl_d_vhfnavaids (
    navaid_identifier TEXT, navaid_latitude REAL, navaid_longitude REAL,
    dme_latitude REAL, dme_longitude REAL, navaid_class TEXT, navaid_frequency REAL);
INSERT INTO tbl_d_vhfnavaids VALUES
    ('RND', 29.52, -98.28, 29.52, -98.28, 'VD', 112.2),
    ('RND', 50.0, 10.0, 50.0, 10.0, 'VD', 999.0),
    ('RNDT', NULL, NULL, 29.53, -98.27, ' T', 0.0),
    ('IRND', NULL, NULL, 29.54, -98.29, ' I', 0.0);

CREATE TABLE tbl_ea_enroute_waypoints (
    waypoint_identifier TEXT, waypoint_latitude REAL, waypoint_longitude REAL,
    area_code TEXT);
INSERT INTO tbl_ea_enroute_waypoints VALUES
    ('RNDAL', 29.60, -98.30, 'USA'),
    ('WAYPT', 50.0, 10.0, 'EUR'),
    ('WAYPT', 30.0, -98.0, 'USA');

CREATE TABLE tbl_pc_terminal_waypoints (
    waypoint_identifier TEXT, waypoint_latitude REAL, waypoint_longitude REAL,
    region_code TEXT, area_code TEXT);
INSERT INTO tbl_pc_terminal_waypoints VALUES
    ('RNDFA', 29.62, -98.34, 'K4', NULL),
    ('RNDAL', 29.61, -98.31, 'K4', 'USA');

CREATE TABLE tbl_ur_restrictive_airspace (
    area_code TEXT, restrictive_type TEXT, restrictive_airspace_designation TEXT,
    restrictive_airspace_name TEXT, seqno INTEGER, latitude REAL, longitude REAL,
    arc_bearing REAL, arc_distance REAL, arc_origin_latitude REAL,
    arc_origin_longitude REAL, multiple_code TEXT, lower_limit TEXT, upper_limit TEXT);
INSERT INTO tbl_ur_restrictive_airspace VALUES
    ('USA', 'M', 'RND1', 'RANDOLPH 1 MOA', 20, 30.0, -97.0, NULL, NULL, NULL, NULL, 'A', 'GND', '17999'),
    ('USA', 'M', 'RND1', 'RANDOLPH 1 MOA', 10, 30.0, -98.0, NULL, NULL, NULL, NULL, 'A', 'GND', '17999'),
    ('USA', 'M', 'RND1', 'RANDOLPH 1 MOA', 30, 29.0, -97.0, 90.0, 30.0, 29.5, -97.5, 'A', 'GND', '17999'),
    ('USA', 'M', 'RND1', 'RANDOLPH 1 MOA', 40, 29.0, -98.0, NULL, NULL, NULL, NULL, 'A', 'GND', '17999'),
    ('USA', 'R', 'R-6312', 'COTULLA RESTRICTED', 10, 28.5, -99.0, NULL, NULL, NULL, NULL, 'A', 'GND', 'UNL'),
    ('USA', 'A', 'ALERT1', 'RANDOLPH ALERT', 10, 29.6, -98.3, NULL, NULL, NULL, NULL, 'A', 'GND', 'UNL'),
    ('CAN', 'M', 'CYR1', 'RANDOLPH NORTH', 10, 29.6, -98.3, NULL, NULL, NULL, NULL, 'A', 'GND', 'UNL');
";

const MTR_SQL: &str = "
CREATE TABLE mtr_segments (
    object_id INTEGER PRIMARY KEY, ident TEXT NOT NULL, mtr_type INTEGER,
    lower_val REAL, lower_uom TEXT, lower_code TEXT,
    upper_val REAL, upper_uom TEXT, upper_code TEXT,
    width_left_nm INTEGER, width_right_nm INTEGER);
INSERT INTO mtr_segments VALUES
    (1, '1064', 0, 100, 'FT', 'HEI', 1500, 'FT', 'ALT', 4, NULL),
    (2, '1064', 0, 500, 'FT', 'ALT', 180, 'FL', 'STD', NULL, NULL),
    (3, '002', 1, 3000, 'FT', 'ALT', 10000, 'FT', 'ALT', 5, 5),
    (4, '1001', 1, 200, 'FT', 'HEI', 1500, 'FT', 'HEI', 2, 2);

CREATE TABLE mtr_segment_points (
    object_id INTEGER NOT NULL, part_index INTEGER NOT NULL, point_index INTEGER NOT NULL,
    lon REAL NOT NULL, lat REAL NOT NULL,
    PRIMARY KEY (object_id, part_index, point_index));
INSERT INTO mtr_segment_points VALUES
    (1, 1, 0, -98.0, 30.2),
    (1, 0, 1, -98.0, 30.1),
    (1, 0, 0, -98.0, 30.0),
    (2, 0, 0, -98.0, 30.3),
    (3, 0, 0, -98.0, 30.25),
    (4, 0, 0, -120.0, 45.0);
";
