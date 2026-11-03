//! What is in a package folder, without the registry's help.

use std::path::{Path, PathBuf};

use infinity_ipc::dto::InstalledPackage;
use serde::Deserialize;

/// Written into each package the manager installs, recording the product
/// version. Same name and fields as v3, which reads it too.
pub(crate) const MARKER_FILE: &str = "infinity.json";

/// A product the rescan should look for.
#[derive(Debug, Clone)]
pub struct Wanted {
    pub product_id: String,
    pub package_dir_name: String,
}

/// The installed version: the manager's marker, else the sim manifest's
/// `package_version`. `None` when neither says.
pub fn package_version(package: &Path) -> Option<String> {
    #[derive(Deserialize)]
    struct Marker {
        version: Option<String>,
    }
    #[derive(Deserialize)]
    struct SimManifest {
        package_version: Option<String>,
    }
    let read = |name: &str| std::fs::read(package.join(name)).ok();
    let marker = read(MARKER_FILE)
        .and_then(|b| serde_json::from_slice::<Marker>(&b).ok())
        .and_then(|m| m.version);
    let manifest = || {
        read("manifest.json")
            .and_then(|b| serde_json::from_slice::<SimManifest>(&b).ok())
            .and_then(|m| m.package_version)
    };
    marker
        .or_else(manifest)
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

pub(crate) fn marker_json(version: &str, package_dir_name: &str) -> Vec<u8> {
    serde_json::to_vec_pretty(&serde_json::json!({
        "version": version,
        "project_name": package_dir_name,
    }))
    .unwrap_or_default()
}

/// Reconciles the registry with the disk. Blocking.
///
/// Records whose folder is gone are dropped. Each wanted product found in
/// `install_dir` is recorded, keeping the original install time when the
/// version is unchanged. Records for products not in `wanted` (another
/// channel's catalog, say) are kept while their folder exists.
pub fn rescan(
    known: Vec<InstalledPackage>,
    install_dir: &Path,
    wanted: &[Wanted],
    now: u64,
) -> Vec<InstalledPackage> {
    let mut out: Vec<InstalledPackage> = known
        .into_iter()
        .filter(|p| Path::new(&p.path).is_dir())
        .collect();
    for want in wanted {
        let path: PathBuf = install_dir.join(&want.package_dir_name);
        if !path.is_dir() {
            continue;
        }
        let version = package_version(&path);
        let path = path.to_string_lossy().into_owned();
        let previous = out.iter().position(|p| p.product_id == want.product_id);
        let installed_at = previous
            .map(|i| &out[i])
            .filter(|p| p.version == version && p.path == path)
            .map_or(Some(now), |p| p.installed_at);
        let record = InstalledPackage {
            product_id: want.product_id.clone(),
            version,
            path,
            package_dir_name: want.package_dir_name.clone(),
            installed_at,
        };
        match previous {
            Some(i) => out[i] = record,
            None => out.push(record),
        }
    }
    out.sort_by(|a, b| a.product_id.cmp(&b.product_id));
    out
}

/// Total bytes of the files under `root`. Unreadable entries are skipped:
/// this feeds a size label, not an accounting. Blocking.
pub fn dir_size(root: &Path) -> u64 {
    let mut total = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            match entry.file_type() {
                Ok(t) if t.is_dir() => pending.push(entry.path()),
                Ok(t) if t.is_file() => total += entry.metadata().map_or(0, |m| m.len()),
                _ => {}
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn version_prefers_the_marker() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("manifest.json"),
            r#"{"package_version":"0.1.0"}"#,
        )
        .unwrap();
        assert_eq!(package_version(tmp.path()).as_deref(), Some("0.1.0"));
        fs::write(tmp.path().join(MARKER_FILE), marker_json("2.0.14", "pkg")).unwrap();
        assert_eq!(package_version(tmp.path()).as_deref(), Some("2.0.14"));
    }

    #[test]
    fn sizes_a_tree() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("a/b")).unwrap();
        fs::write(tmp.path().join("x"), [0; 10]).unwrap();
        fs::write(tmp.path().join("a/b/y"), [0; 5]).unwrap();
        assert_eq!(dir_size(tmp.path()), 15);
        assert_eq!(dir_size(&tmp.path().join("missing")), 0);
    }

    #[test]
    fn unreadable_package_has_no_version() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(MARKER_FILE), "not json").unwrap();
        assert_eq!(package_version(tmp.path()), None);
    }

    #[test]
    fn rescan_records_finds_and_forgets() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        fs::create_dir_all(dir.join("t38")).unwrap();
        fs::write(
            dir.join("t38").join(MARKER_FILE),
            marker_json("2.0.0", "t38"),
        )
        .unwrap();
        fs::create_dir_all(dir.join("other")).unwrap();

        let record = |id: &str, version: Option<&str>, at| InstalledPackage {
            product_id: id.into(),
            version: version.map(Into::into),
            path: dir.join(id).to_string_lossy().into_owned(),
            package_dir_name: id.into(),
            installed_at: Some(at),
        };
        let known = vec![
            record("t38", Some("2.0.0"), 1),
            record("gone", Some("1.0.0"), 1),
            record("other", None, 1),
        ];
        let wanted = [
            Wanted {
                product_id: "t38".into(),
                package_dir_name: "t38".into(),
            },
            Wanted {
                product_id: "dc10".into(),
                package_dir_name: "dc10".into(),
            },
        ];
        let out = rescan(known, dir, &wanted, 99);
        assert_eq!(
            out,
            vec![record("other", None, 1), record("t38", Some("2.0.0"), 1)]
        );

        fs::write(
            dir.join("t38").join(MARKER_FILE),
            marker_json("2.1.0", "t38"),
        )
        .unwrap();
        let out = rescan(out, dir, &wanted, 99);
        assert_eq!(out[1], record("t38", Some("2.1.0"), 99));
    }
}
