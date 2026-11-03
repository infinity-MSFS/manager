//! ISO-8601 stamps without pulling `chrono`/`time` into the workspace. The
//! format matches JavaScript's `new Date().toISOString()`
//! (`YYYY-MM-DDTHH:MM:SS.sssZ`, UTC, ms precision) because that's what the
//! gauges were written against.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_iso8601_ms() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    epoch_ms_to_iso8601(ms)
}

pub fn epoch_ms_to_iso8601(ms: i64) -> String {
    const DAY_MS: i64 = 86_400_000;
    let days = ms.div_euclid(DAY_MS);
    let in_day = ms.rem_euclid(DAY_MS);
    let (y, m, d) = civil_from_days(days);
    let h = in_day / 3_600_000;
    let mn = (in_day / 60_000) % 60;
    let s = (in_day / 1000) % 60;
    let millis = in_day % 1000;
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mn:02}:{s:02}.{millis:03}Z")
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 to a proleptic
/// Gregorian `(year, month, day)`.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_captured_golden_timestamp() {
        assert_eq!(
            epoch_ms_to_iso8601(1_778_195_109_411),
            "2026-05-07T23:05:09.411Z"
        );
    }

    #[test]
    fn unix_epoch_is_1970() {
        assert_eq!(epoch_ms_to_iso8601(0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn handles_leap_day() {
        assert_eq!(
            epoch_ms_to_iso8601(1_709_210_096_789),
            "2024-02-29T12:34:56.789Z"
        );
    }

    #[test]
    fn handles_pre_epoch() {
        assert_eq!(epoch_ms_to_iso8601(-1), "1969-12-31T23:59:59.999Z");
    }
}
