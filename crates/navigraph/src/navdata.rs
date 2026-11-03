//! Navdata packages (`GET /v1/navdata/packages`), the `cycle.json` that
//! records what is installed, and AIRAC arithmetic.

pub use infinity_ipc::navigraph::CycleJson;

use serde::Deserialize;

use crate::secret::Secret;

/// One file of a package. `signed_url` is a short-lived download link.
#[derive(Debug, Clone, Deserialize)]
pub struct NavdataPackageFile {
    #[serde(default)]
    pub key: String,
    /// SHA-256 of the file, hex.
    #[serde(default)]
    pub hash: String,
    pub signed_url: Secret,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NavdataPackage {
    #[serde(default)]
    pub package_id: String,
    pub cycle: String,
    #[serde(default)]
    pub revision: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub package_status: String,
    #[serde(default)]
    pub files: Vec<NavdataPackageFile>,
}

impl NavdataPackage {
    /// The zip if there is one, else the first file.
    pub fn primary_file(&self) -> Option<&NavdataPackageFile> {
        self.files
            .iter()
            .find(|f| f.key.to_ascii_lowercase().ends_with(".zip"))
            .or_else(|| self.files.first())
    }

    /// Revision with Navigraph's implicit default: empty means `"1"`.
    pub fn revision_or_default(&self) -> &str {
        revision_or_default(&self.revision)
    }
}

fn revision_or_default(rev: &str) -> &str {
    if rev.is_empty() { "1" } else { rev }
}

/// The hash as lowercase hex if it is a well-formed SHA-256.
pub fn sha256_hex(hash: &str) -> Option<String> {
    let h = hash.trim();
    (h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())).then(|| h.to_ascii_lowercase())
}

/// The `cycle.json` describing an installed package.
pub(crate) fn cycle_json_for(pkg: &NavdataPackage) -> CycleJson {
    CycleJson {
        cycle: pkg.cycle.clone(),
        revision: pkg.revision_or_default().to_owned(),
        name: pkg.description.clone(),
        format: pkg.format.clone(),
        validity_period: airac_validity(&pkg.cycle).unwrap_or_default(),
    }
}

/// The first `current` package matching the product's `format` (exactly,
/// ignoring case) and `package_match` (substring of the description,
/// ignoring case). `None` filters match anything.
pub fn select_package<'a>(
    packages: &'a [NavdataPackage],
    format: Option<&str>,
    package_match: Option<&str>,
) -> Option<&'a NavdataPackage> {
    let needle = package_match.map(str::to_ascii_lowercase);
    packages
        .iter()
        .filter(|p| p.package_status.eq_ignore_ascii_case("current"))
        .filter(|p| format.is_none_or(|f| p.format.eq_ignore_ascii_case(f)))
        .find(|p| {
            needle
                .as_deref()
                .is_none_or(|m| p.description.to_ascii_lowercase().contains(m))
        })
}

/// Whether `latest` differs from what is installed, by cycle then revision.
pub fn is_update(installed: &CycleJson, latest: &NavdataPackage) -> bool {
    installed.cycle != latest.cycle
        || revision_or_default(&installed.revision) != latest.revision_or_default()
}

/// `YYYY-MM-DD/YYYY-MM-DD` effective/expiry dates of AIRAC cycle `YYCC`.
pub fn airac_validity(cycle: &str) -> Option<String> {
    if cycle.len() != 4 || !cycle.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let yy: i64 = cycle[..2].parse().ok()?;
    let ord: i64 = cycle[2..].parse().ok()?;
    if ord == 0 {
        return None;
    }
    let year = 2000 + yy;

    // Any known effective date anchors the 28-day grid.
    let anchor = days_from_civil(2026, 1, 22);
    let jan1 = days_from_civil(year, 1, 1);
    let first = jan1 + (anchor - jan1).rem_euclid(28);
    let effective = first + (ord - 1) * 28;
    let expiry = effective + 27;

    let (ey, em, ed) = civil_from_days(effective);
    let (xy, xm, xd) = civil_from_days(expiry);
    Some(format!("{ey:04}-{em:02}-{ed:02}/{xy:04}-{xm:02}-{xd:02}"))
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn pkg(
        cycle: &str,
        revision: &str,
        description: &str,
        status: &str,
    ) -> NavdataPackage {
        NavdataPackage {
            package_id: String::new(),
            cycle: cycle.into(),
            revision: revision.into(),
            description: description.into(),
            format: "dfdv2".into(),
            package_status: status.into(),
            files: vec![],
        }
    }

    #[test]
    fn airac_validity_matches_navigraph_cycle_json() {
        assert_eq!(
            airac_validity("2606").as_deref(),
            Some("2026-06-11/2026-07-08")
        );
        assert_eq!(
            airac_validity("2501").as_deref(),
            Some("2025-01-23/2025-02-19")
        );
        assert_eq!(
            airac_validity("2513").as_deref(),
            Some("2025-12-25/2026-01-21")
        );
        assert!(airac_validity("26").is_none());
        assert!(airac_validity("26xx").is_none());
        assert!(airac_validity("2600").is_none());
    }

    #[test]
    fn selection_filters_status_format_and_description() {
        let packages = vec![
            pkg("2605", "1", "Aero Dynamics T-38C DFD v2", "outdated"),
            pkg("2606", "1", "Aero Dynamics T-38C DFD v2", "current"),
            pkg("2606", "2", "Aero Dynamics T-38N DFD v2", "current"),
        ];
        let c = select_package(&packages, Some("dfdv2"), Some("t-38n")).unwrap();
        assert_eq!(c.revision, "2");
        let first_current = select_package(&packages, None, None).unwrap();
        assert_eq!(first_current.description, "Aero Dynamics T-38C DFD v2");
        assert_eq!(first_current.cycle, "2606");
        assert!(select_package(&packages, Some("dfdv1"), None).is_none());
    }

    #[test]
    fn update_compares_cycle_then_revision() {
        let latest = pkg("2606", "2", "x", "current");
        let mut installed = crate::navdata::cycle_json_for(&pkg("2606", "2", "x", "current"));
        assert!(!is_update(&installed, &latest));
        installed.cycle = "2605".into();
        assert!(is_update(&installed, &latest));
        installed.cycle = "2606".into();
        installed.revision = "1".into();
        assert!(is_update(&installed, &latest));
        installed.revision = String::new();
        assert!(is_update(&installed, &latest));
        let latest_r1 = pkg("2606", "", "x", "current");
        assert!(!is_update(&installed, &latest_r1));
    }

    #[test]
    fn cycle_json_round_trips_hub_format() {
        let hub = r#"{
          "cycle": "2606",
          "revision": "1",
          "name": "Aero Dynamics T-38C DFD v2",
          "format": "dfdv2",
          "validityPeriod": "2026-06-11/2026-07-08"
        }"#;
        let parsed: CycleJson = serde_json::from_str(hub).unwrap();
        assert_eq!(parsed.cycle, "2606");
        assert_eq!(parsed.validity_period, "2026-06-11/2026-07-08");
        let out = serde_json::to_string(&parsed).unwrap();
        assert!(out.contains("\"validityPeriod\""));
    }

    #[test]
    fn cycle_json_from_package_defaults_revision() {
        let c = crate::navdata::cycle_json_for(&pkg("2606", "", "T-38C", "current"));
        assert_eq!(c.revision, "1");
        assert_eq!(c.name, "T-38C");
        assert_eq!(c.validity_period, "2026-06-11/2026-07-08");
    }

    #[test]
    fn primary_file_prefers_zip() {
        let mut p = pkg("2606", "1", "x", "current");
        p.files = vec![
            NavdataPackageFile {
                key: "readme.txt".into(),
                hash: String::new(),
                signed_url: "https://x/readme".into(),
            },
            NavdataPackageFile {
                key: "navdata.ZIP".into(),
                hash: "AB".repeat(32),
                signed_url: "https://x/zip".into(),
            },
        ];
        assert_eq!(p.primary_file().unwrap().key, "navdata.ZIP");
        assert_eq!(sha256_hex(&p.files[1].hash).unwrap(), "ab".repeat(32));
        assert!(sha256_hex("not-a-hash").is_none());
        assert!(!format!("{p:?}").contains("https://x/zip"));
    }

    #[test]
    fn packages_parse_navigraph_shape() {
        let body = r#"[{"package_id":"abc","cycle":"2606","revision":"1",
            "description":"T-38C DFD v2","format":"dfdv2","package_status":"current",
            "files":[{"key":"t38.zip","hash":"","signed_url":"https://cdn/t38.zip?sig=1"}],
            "extra":true}]"#;
        let pkgs: Vec<NavdataPackage> = serde_json::from_str(body).unwrap();
        assert_eq!(
            pkgs[0].files[0].signed_url.expose(),
            "https://cdn/t38.zip?sig=1"
        );
    }
}
