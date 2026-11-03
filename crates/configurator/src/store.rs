//! Per-product config store: `<app_data>/configs/<productId>.json`.
//!
//! The file format, `.bak` rotation and the one-shot v1 import are shared
//! with v3, which reads and writes the same directory.

pub use infinity_ipc::dto::ConfigEnvelope;

use std::path::{Path, PathBuf};

use infinity_ipc::catalog::ProductId;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::sync::Mutex;

use crate::error::{ConfiguratorError, IoContext, Result};
use crate::fsio::{read_json, to_pretty, with_suffix, write_atomic};

/// Outcome of the v1 import, kept in `migration.v2tov3.json` so support can
/// see what happened on a user's machine.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationReport {
    pub ran_at: String,
    pub migrated_configs: Vec<String>,
    pub skipped: Vec<String>,
    pub errors: Vec<String>,
}

const MIGRATION_MARKER: &str = "migration.v2tov3.json";

/// Folder names v1 used under `%APPDATA%`.
const V1_APP_DATA_NAMES: [&str; 3] = [
    "com.infinity-simulations.manager",
    "com.infinity.infinity-manager",
    "Infinity Manager",
];

#[derive(Debug)]
pub struct ConfigStore {
    app_data: PathBuf,
    write: Mutex<()>,
}

impl ConfigStore {
    pub fn new(app_data: &Path) -> Self {
        Self {
            app_data: app_data.to_path_buf(),
            write: Mutex::new(()),
        }
    }

    fn configs_dir(&self) -> PathBuf {
        self.app_data.join("configs")
    }

    fn config_path(&self, product_id: &str) -> Result<PathBuf> {
        validate_product_id(product_id)?;
        Ok(self.configs_dir().join(format!("{product_id}.json")))
    }

    /// The saved config, or `None` if the product has never been saved.
    pub async fn get(&self, product_id: &str) -> Result<Option<ConfigEnvelope>> {
        read_json(&self.config_path(product_id)?).await
    }

    /// Like [`Self::get`] but a missing config is
    /// [`ConfiguratorError::NotFound`], for callers that need one to exist.
    pub async fn load(&self, product_id: &str) -> Result<ConfigEnvelope> {
        self.get(product_id)
            .await?
            .ok_or_else(|| ConfiguratorError::NotFound(product_id.to_owned()))
    }

    pub async fn exists(&self, product_id: &str) -> Result<bool> {
        Ok(fs::try_exists(self.config_path(product_id)?)
            .await
            .unwrap_or(false))
    }

    /// Writes `<file>.tmp`, moves the current file to `<file>.bak`, then
    /// renames the tmp into place. The `.bak` is the user's one-step undo
    /// if a save turns out bad, so rotation failure only warns.
    pub async fn save(&self, env: &ConfigEnvelope) -> Result<()> {
        let path = self.config_path(&env.product_id)?;
        let json = to_pretty(env)?;
        let _guard = self.write.lock().await;

        let dir = self.configs_dir();
        fs::create_dir_all(&dir).await.at(&dir)?;
        let tmp = with_suffix(&path, "tmp");
        fs::write(&tmp, &json).await.at(&tmp)?;
        if fs::try_exists(&path).await.unwrap_or(false) {
            let bak = with_suffix(&path, "bak");
            if let Err(e) = fs::rename(&path, &bak).await {
                tracing::warn!(path = %path.display(), error = %e, "config .bak rotation failed");
            }
        }
        fs::rename(&tmp, &path).await.at(&path)?;
        tracing::info!(product_id = %env.product_id, "config saved");
        Ok(())
    }

    /// Removes the saved config. Returns whether there was one.
    pub async fn delete(&self, product_id: &str) -> Result<bool> {
        let path = self.config_path(product_id)?;
        let _guard = self.write.lock().await;
        match fs::remove_file(&path).await {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e).at(&path),
        }
    }

    /// Products with a saved config, sorted.
    pub async fn list(&self) -> Result<Vec<ProductId>> {
        let dir = self.configs_dir();
        let mut out = Vec::new();
        let mut rd = match fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e).at(&dir),
        };
        while let Some(entry) = rd.next_entry().await.at(&dir)? {
            let name = entry.file_name();
            // `x.json.bak`, `x.json.tmp` and friends aren't configs.
            if let Some(id) = name.to_string_lossy().strip_suffix(".json")
                && validate_product_id(id).is_ok()
            {
                out.push(id.to_owned());
            }
        }
        out.sort();
        Ok(out)
    }

    /// One-shot import of v1's single global `<v1_app_data>/config.json`
    /// as `configs/<target_product_id>.json`. Idempotent via a marker file
    /// (a second run returns the first run's report) and non-destructive:
    /// the v1 file is renamed to `config.json.v2.bak`, never deleted.
    pub async fn migrate_v1_config(
        &self,
        v1_app_data: &Path,
        target_product_id: &str,
        target_schema_version: u32,
    ) -> Result<MigrationReport> {
        validate_product_id(target_product_id)?;
        let marker = self.app_data.join(MIGRATION_MARKER);
        match read_json::<MigrationReport>(&marker).await {
            Ok(Some(report)) => return Ok(report),
            // v3 treated an unreadable marker as "already ran" too.
            Err(ConfiguratorError::Corrupt { .. }) => return Ok(MigrationReport::default()),
            Ok(None) => {}
            Err(e) => return Err(e),
        }

        let mut report = MigrationReport {
            ran_at: unix_stamp(),
            ..MigrationReport::default()
        };
        let legacy = v1_app_data.join("config.json");
        match read_json::<serde_json::Value>(&legacy).await {
            Ok(None) => report
                .skipped
                .push(format!("legacy config not found: {}", legacy.display())),
            Err(ConfiguratorError::Corrupt { source, .. }) => {
                report.errors.push(format!("legacy config parse: {source}"));
            }
            Err(e) => return Err(e),
            Ok(Some(value)) => {
                self.save(&ConfigEnvelope {
                    schema_version: target_schema_version,
                    product_id: target_product_id.to_owned(),
                    value,
                })
                .await?;
                report.migrated_configs.push(target_product_id.to_owned());
                let bak = legacy.with_extension("json.v2.bak");
                if let Err(e) = fs::rename(&legacy, &bak).await {
                    report
                        .errors
                        .push(format!("rename legacy → bak failed: {e}"));
                }
            }
        }

        write_atomic(&marker, &to_pretty(&report)?).await?;
        tracing::info!(?report, "v1 config import finished");
        Ok(report)
    }
}

/// Finds v1's data folder under `appdata` (the `%APPDATA%` root, passed in
/// so nothing here reads the environment). `None` means nothing to import.
pub async fn find_v1_app_data(appdata: &Path) -> Option<PathBuf> {
    for name in V1_APP_DATA_NAMES {
        let dir = appdata.join(name);
        if fs::metadata(dir.join("config.json"))
            .await
            .is_ok_and(|m| m.is_file())
        {
            return Some(dir);
        }
    }
    None
}

/// Product ids name files, so they're restricted to the characters v1
/// allowed; anything else could traverse out of `configs/`.
pub fn validate_product_id(id: &str) -> Result<()> {
    let ok = !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(ConfiguratorError::InvalidProductId(id.to_owned()))
    }
}

/// `unix:<secs>`, the `ranAt` format v3 wrote.
fn unix_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    format!("unix:{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID: &str = "aerodynamics-t38c";

    fn env(value: serde_json::Value) -> ConfigEnvelope {
        ConfigEnvelope {
            schema_version: 1,
            product_id: ID.into(),
            value,
        }
    }

    #[tokio::test]
    async fn save_load_list_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(tmp.path());
        assert_eq!(store.get(ID).await.unwrap(), None);
        assert!(matches!(
            store.load(ID).await,
            Err(ConfiguratorError::NotFound(_))
        ));

        store
            .save(&env(json!({ "heading_bug": 270 })))
            .await
            .unwrap();
        assert!(store.exists(ID).await.unwrap());
        assert_eq!(store.load(ID).await.unwrap().value["heading_bug"], 270);
        assert_eq!(store.list().await.unwrap(), vec![ID]);

        assert!(store.delete(ID).await.unwrap());
        assert!(!store.delete(ID).await.unwrap());
        assert!(store.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn reads_v3_files_and_writes_the_same_format() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("configs");
        std::fs::create_dir_all(&dir).unwrap();
        let v3 = "{\n  \"schemaVersion\": 3,\n  \"productId\": \"aerodynamics-t38c\",\n  \"value\": {\n    \"b\": 1,\n    \"a\": 2\n  }\n}";
        std::fs::write(dir.join("aerodynamics-t38c.json"), v3).unwrap();

        let store = ConfigStore::new(tmp.path());
        let loaded = store.load(ID).await.unwrap();
        assert_eq!(loaded.schema_version, 3);
        store.save(&loaded).await.unwrap();
        let written = std::fs::read_to_string(dir.join("aerodynamics-t38c.json")).unwrap();
        assert_eq!(written, v3);
    }

    #[tokio::test]
    async fn save_rotates_bak_and_leaves_no_tmp() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(tmp.path());
        store.save(&env(json!({ "v": 1 }))).await.unwrap();
        store.save(&env(json!({ "v": 2 }))).await.unwrap();
        store.save(&env(json!({ "v": 3 }))).await.unwrap();

        let dir = tmp.path().join("configs");
        let bak: ConfigEnvelope =
            serde_json::from_slice(&std::fs::read(dir.join("aerodynamics-t38c.json.bak")).unwrap())
                .unwrap();
        assert_eq!(bak.value["v"], 2);
        assert!(!dir.join("aerodynamics-t38c.json.tmp").exists());
        assert_eq!(store.list().await.unwrap(), vec![ID]);
    }

    #[tokio::test]
    async fn corrupt_file_is_an_error_not_a_reset() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("configs");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("aerodynamics-t38c.json"), "not json").unwrap();
        let store = ConfigStore::new(tmp.path());
        assert!(matches!(
            store.get(ID).await,
            Err(ConfiguratorError::Corrupt { .. })
        ));
    }

    #[tokio::test]
    async fn rejects_unsafe_product_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(tmp.path());
        for id in ["../../etc/passwd", "", "a.b", "a/b", "a\\b"] {
            let mut e = env(json!({}));
            e.product_id = id.into();
            assert!(matches!(
                store.save(&e).await,
                Err(ConfiguratorError::InvalidProductId(_))
            ));
        }
    }

    #[tokio::test]
    async fn migrate_v1_config_writes_envelope_and_marker() {
        let v1 = tempfile::tempdir().unwrap();
        let app = tempfile::tempdir().unwrap();
        std::fs::write(
            v1.path().join("config.json"),
            br#"{"main":{"heading_bug":42}}"#,
        )
        .unwrap();

        let store = ConfigStore::new(app.path());
        let report = store.migrate_v1_config(v1.path(), ID, 1).await.unwrap();
        assert_eq!(report.migrated_configs, vec![ID]);
        assert!(report.ran_at.starts_with("unix:"));
        assert_eq!(
            store.load(ID).await.unwrap().value["main"]["heading_bug"],
            42
        );
        assert!(v1.path().join("config.json.v2.bak").exists());
        assert!(app.path().join(MIGRATION_MARKER).exists());

        let again = store.migrate_v1_config(v1.path(), ID, 1).await.unwrap();
        assert_eq!(again, report);
    }

    #[tokio::test]
    async fn migrate_v1_config_records_skips_and_parse_errors() {
        let v1 = tempfile::tempdir().unwrap();
        let app = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(app.path());
        let report = store.migrate_v1_config(v1.path(), ID, 1).await.unwrap();
        assert_eq!(report.skipped.len(), 1);

        let app = tempfile::tempdir().unwrap();
        std::fs::write(v1.path().join("config.json"), "{").unwrap();
        let store = ConfigStore::new(app.path());
        let report = store.migrate_v1_config(v1.path(), ID, 1).await.unwrap();
        assert_eq!(report.errors.len(), 1);
        assert!(!store.exists(ID).await.unwrap());
    }

    #[tokio::test]
    async fn finds_v1_app_data() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(find_v1_app_data(root.path()).await, None);
        let dir = root.path().join("Infinity Manager");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), "{}").unwrap();
        assert_eq!(find_v1_app_data(root.path()).await, Some(dir));
    }
}
