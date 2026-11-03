//! Distance math, identifier cleanup and tolerant column reads.

use rusqlite::Row;
use rusqlite::types::Type;

const EARTH_RADIUS_NM: f64 = 3440.065;
const EARTH_RADIUS_KM: f64 = 6371.0;

/// Great-circle distance in nautical miles.
pub fn haversine_nm(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    EARTH_RADIUS_NM * central_angle(lat1, lon1, lat2, lon2)
}

/// Great-circle distance in kilometres.
pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    EARTH_RADIUS_KM * central_angle(lat1, lon1, lat2, lon2)
}

pub fn km_to_nm(km: f64) -> f64 {
    km / 1.852
}

fn central_angle(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    2.0 * a.sqrt().atan2((1.0 - a).sqrt())
}

/// The leading digits of a runway identifier: `RW15L` -> `15`. Procedure
/// identifiers carry the number but not the side.
pub fn clean_runway_number(runway_identifier: &str) -> String {
    runway_identifier
        .trim()
        .trim_start_matches("RW")
        .trim_start_matches("rw")
        .chars()
        .take_while(char::is_ascii_digit)
        .collect()
}

/// The ARINC 424 procedure-identifier prefix for an approach type: `ILS`
/// procedures are `I15L`, VOR/DME `D33R`, and so on. Unknown types fall
/// back to their first letter.
pub fn normalize_approach_prefix(approach_type: &str) -> Option<char> {
    let t = approach_type.trim().to_uppercase();
    match t.as_str() {
        "ILS" | "ILZ" => Some('I'),
        "VOR" => Some('D'),
        "VORTAC" => Some('S'),
        "TACAN" => Some('T'),
        _ => t.chars().next(),
    }
}

/// Classifies a `tbl_d_vhfnavaids` row from its ARINC 424 class. The first
/// character is the VOR component, the second the DME/TACAN one.
pub(crate) fn navaid_type_label(class: Option<&str>) -> &'static str {
    let class = class.unwrap_or("");
    if class.starts_with('V') {
        return "VHF NAVAID";
    }
    match class.chars().nth(1) {
        Some('T' | 'M') => "TACAN",
        Some('D') => "DME",
        _ => "VHF NAVAID",
    }
}

/// Reads a number that some navdata releases store as text.
pub(crate) fn get_f64(row: &Row<'_>, index: usize) -> rusqlite::Result<f64> {
    row.get::<_, f64>(index)
        .or_else(|e| match row.get::<_, String>(index) {
            Ok(s) => s.trim().parse().map_err(|p| {
                rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(p))
            }),
            Err(_) => Err(e),
        })
}

pub(crate) fn get_i32(row: &Row<'_>, index: usize) -> rusqlite::Result<i32> {
    row.get::<_, i32>(index)
        .or_else(|e| match row.get::<_, String>(index) {
            Ok(s) => s.trim().parse().map_err(|p| {
                rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(p))
            }),
            Err(_) => Err(e),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runway_numbers_drop_prefix_and_side() {
        assert_eq!(clean_runway_number("RW15L"), "15");
        assert_eq!(clean_runway_number(" rw09 "), "09");
        assert_eq!(clean_runway_number("33"), "33");
        assert_eq!(clean_runway_number("RWL"), "");
    }

    #[test]
    fn approach_prefixes() {
        assert_eq!(normalize_approach_prefix("ils"), Some('I'));
        assert_eq!(normalize_approach_prefix("ILZ"), Some('I'));
        assert_eq!(normalize_approach_prefix("VOR"), Some('D'));
        assert_eq!(normalize_approach_prefix("VORTAC"), Some('S'));
        assert_eq!(normalize_approach_prefix("TACAN"), Some('T'));
        assert_eq!(normalize_approach_prefix("rnav"), Some('R'));
        assert_eq!(normalize_approach_prefix("  "), None);
    }

    #[test]
    fn navaid_classes() {
        assert_eq!(navaid_type_label(Some("VDHW")), "VHF NAVAID");
        assert_eq!(navaid_type_label(Some(" T")), "TACAN");
        assert_eq!(navaid_type_label(Some(" M")), "TACAN");
        assert_eq!(navaid_type_label(Some(" D")), "DME");
        assert_eq!(navaid_type_label(None), "VHF NAVAID");
    }

    #[test]
    fn one_degree_of_latitude_is_sixty_miles() {
        let nm = haversine_nm(0.0, 0.0, 1.0, 0.0);
        assert!((nm - 60.04).abs() < 0.05, "{nm}");
        let km = haversine_km(0.0, 0.0, 1.0, 0.0);
        assert!((km_to_nm(km) - 60.04).abs() < 0.05, "{km}");
    }

    #[test]
    fn numbers_stored_as_text_still_read() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let (f, i): (f64, i32) = conn
            .query_row("SELECT ' 29.5 ', '42'", [], |r| {
                Ok((get_f64(r, 0)?, get_i32(r, 1)?))
            })
            .unwrap();
        assert_eq!((f, i), (29.5, 42));
        let bad = conn.query_row("SELECT 'x'", [], |r| get_f64(r, 0));
        assert!(bad.is_err());
        let null = conn.query_row("SELECT NULL", [], |r| get_f64(r, 0));
        assert!(null.is_err());
    }
}
