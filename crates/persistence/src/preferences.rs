use std::path::{Path, PathBuf};

use infinity_ipc::IpcResult;
use infinity_ipc::dto::Preferences;
use tokio::sync::Mutex;

use crate::atomic::{read_json_or_default, write_json};

#[derive(Debug)]
pub struct PreferencesStore {
    path: PathBuf,
    write: Mutex<()>,
}

impl PreferencesStore {
    pub fn new(app_data: &Path) -> Self {
        Self {
            path: app_data.join("preferences.json"),
            write: Mutex::new(()),
        }
    }

    pub async fn load(&self) -> IpcResult<Preferences> {
        read_json_or_default(&self.path).await
    }

    pub async fn save(&self, prefs: &Preferences) -> IpcResult<()> {
        let _guard = self.write.lock().await;
        write_json(&self.path, prefs).await
    }
}

#[cfg(test)]
mod tests {
    use infinity_ipc::dto::MotionPreference;

    use super::*;

    #[tokio::test]
    async fn round_trips_and_keeps_v3_fields() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("preferences.json"),
            r#"{ "channel": "beta", "installDir": "D:\\Community" }"#,
        )
        .unwrap();

        let store = PreferencesStore::new(dir.path());
        let mut prefs = store.load().await.unwrap();
        prefs.motion = MotionPreference::Reduced;
        store.save(&prefs).await.unwrap();

        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("preferences.json")).unwrap())
                .unwrap();
        assert_eq!(raw["channel"], "beta");
        assert_eq!(raw["installDir"], "D:\\Community");
        assert_eq!(raw["motion"], "reduced");
    }

    #[tokio::test]
    async fn missing_file_is_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let prefs = PreferencesStore::new(dir.path()).load().await.unwrap();
        assert_eq!(prefs, Preferences::default());
    }

    #[tokio::test]
    async fn corrupt_file_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preferences.json");
        std::fs::write(&path, "{ not json").unwrap();
        let prefs = PreferencesStore::new(dir.path()).load().await.unwrap();
        assert_eq!(prefs, Preferences::default());
        assert!(dir.path().join("preferences.json.corrupt").exists());
        assert!(!path.exists());
    }
}
