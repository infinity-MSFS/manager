use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use infinity_ipc::IpcResult;
use infinity_ipc::catalog::ProductId;
use infinity_ipc::dto::InstalledPackage;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::atomic::{read_json_or_default, write_json};

/// `installed.json`: what the manager believes is installed, keyed by
/// product. The file format is shared with v3, which reads the same file.
#[derive(Debug)]
pub struct InstalledStore {
    path: PathBuf,
    write: Mutex<()>,
}

impl InstalledStore {
    pub fn new(app_data: &Path) -> Self {
        Self {
            path: app_data.join("installed.json"),
            write: Mutex::new(()),
        }
    }

    pub async fn list(&self) -> IpcResult<Vec<InstalledPackage>> {
        let file: File = read_json_or_default(&self.path).await?;
        Ok(file.items.into_values().map(Record::into_dto).collect())
    }

    pub async fn get(&self, product_id: &str) -> IpcResult<Option<InstalledPackage>> {
        let file: File = read_json_or_default(&self.path).await?;
        Ok(file.items.get(product_id).cloned().map(Record::into_dto))
    }

    pub async fn upsert(&self, package: InstalledPackage) -> IpcResult<()> {
        self.update(|items| {
            items.insert(package.product_id.clone(), Record::from_dto(package));
        })
        .await
    }

    pub async fn remove(&self, product_id: &str) -> IpcResult<()> {
        self.update(|items| {
            items.remove(product_id);
        })
        .await
    }

    /// Replaces every record in one write.
    pub async fn replace_all(&self, packages: Vec<InstalledPackage>) -> IpcResult<()> {
        self.update(|items| {
            *items = packages
                .into_iter()
                .map(|p| (p.product_id.clone(), Record::from_dto(p)))
                .collect();
        })
        .await
    }

    async fn update(&self, f: impl FnOnce(&mut BTreeMap<ProductId, Record>)) -> IpcResult<()> {
        let _guard = self.write.lock().await;
        let mut file: File = read_json_or_default(&self.path).await?;
        f(&mut file.items);
        write_json(&self.path, &file).await
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    items: BTreeMap<ProductId, Record>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    product_id: ProductId,
    /// v3 wrote `"legacy"` or `"unknown"` when it could not tell.
    version: String,
    /// v3 writes `"unix:<seconds>"`.
    #[serde(default)]
    installed_at: String,
    install_path: PathBuf,
    package_dir_name: String,
}

const UNKNOWN_VERSIONS: [&str; 3] = ["", "legacy", "unknown"];

impl Record {
    fn into_dto(self) -> InstalledPackage {
        let version = Some(self.version).filter(|v| !UNKNOWN_VERSIONS.contains(&v.trim()));
        InstalledPackage {
            installed_at: self
                .installed_at
                .strip_prefix("unix:")
                .and_then(|s| s.parse().ok()),
            product_id: self.product_id,
            version,
            path: self.install_path.to_string_lossy().into_owned(),
            package_dir_name: self.package_dir_name,
        }
    }

    fn from_dto(p: InstalledPackage) -> Self {
        Self {
            product_id: p.product_id,
            version: p.version.unwrap_or_else(|| "unknown".to_owned()),
            installed_at: p
                .installed_at
                .map(|s| format!("unix:{s}"))
                .unwrap_or_default(),
            install_path: PathBuf::from(p.path),
            package_dir_name: p.package_dir_name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(id: &str, version: Option<&str>) -> InstalledPackage {
        InstalledPackage {
            product_id: id.to_owned(),
            version: version.map(str::to_owned),
            path: format!("D:\\Community\\{id}"),
            package_dir_name: id.to_owned(),
            installed_at: Some(1_700_000_000),
        }
    }

    #[tokio::test]
    async fn reads_v3_records() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("installed.json"),
            r#"{ "items": {
                "aerodynamics-t38c": { "productId": "aerodynamics-t38c", "version": "2.0.14",
                    "installedAt": "unix:1700000000", "installPath": "D:\\Community\\aerodynamics-t38c",
                    "packageDirName": "aerodynamics-t38c" },
                "aerodynamics-dc10": { "productId": "aerodynamics-dc10", "version": "legacy",
                    "installedAt": "unix:x", "installPath": "D:\\Community\\aerodynamics-dc10",
                    "packageDirName": "aerodynamics-dc10" }
            } }"#,
        )
        .unwrap();
        let store = InstalledStore::new(dir.path());
        let t38 = store.get("aerodynamics-t38c").await.unwrap().unwrap();
        assert_eq!(t38.version.as_deref(), Some("2.0.14"));
        assert_eq!(t38.installed_at, Some(1_700_000_000));
        let dc10 = store.get("aerodynamics-dc10").await.unwrap().unwrap();
        assert_eq!(dc10.version, None);
        assert_eq!(dc10.installed_at, None);
    }

    #[tokio::test]
    async fn upsert_remove_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = InstalledStore::new(dir.path());
        assert!(store.list().await.unwrap().is_empty());

        store.upsert(package("a", Some("1.0.0"))).await.unwrap();
        store.upsert(package("b", None)).await.unwrap();
        store.upsert(package("a", Some("1.1.0"))).await.unwrap();
        let all = store.list().await.unwrap();
        assert_eq!(all, vec![package("a", Some("1.1.0")), package("b", None)]);

        store.remove("a").await.unwrap();
        store.remove("missing").await.unwrap();
        assert_eq!(store.list().await.unwrap(), vec![package("b", None)]);

        store.replace_all(vec![package("c", None)]).await.unwrap();
        assert_eq!(store.list().await.unwrap(), vec![package("c", None)]);
    }

    #[tokio::test]
    async fn writes_a_file_v3_can_read() {
        let dir = tempfile::tempdir().unwrap();
        let store = InstalledStore::new(dir.path());
        store.upsert(package("a", None)).await.unwrap();
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("installed.json")).unwrap())
                .unwrap();
        let a = &raw["items"]["a"];
        assert_eq!(a["version"], "unknown");
        assert_eq!(a["installedAt"], "unix:1700000000");
        assert_eq!(a["installPath"], "D:\\Community\\a");
    }
}
