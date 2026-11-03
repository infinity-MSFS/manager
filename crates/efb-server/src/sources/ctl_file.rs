use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::Value;

use crate::providers::{CtlStore, ProviderError};

/// Controls bindings in one JSON file, written atomically.
#[derive(Debug, Clone)]
pub struct FileCtlStore {
    path: PathBuf,
}

impl FileCtlStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// `<app_data>/t38-controls.json`, where v3 kept them.
    pub fn in_app_data(app_data_dir: &Path) -> Self {
        Self::new(app_data_dir.join("t38-controls.json"))
    }
}

#[async_trait]
impl CtlStore for FileCtlStore {
    async fn load(&self) -> Value {
        let Ok(text) = tokio::fs::read_to_string(&self.path).await else {
            return Value::Null;
        };
        serde_json::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "efb-ctl: stored bindings unreadable; treating as empty");
            Value::Null
        })
    }

    async fn save(&self, blob: Value) -> Result<(), ProviderError> {
        if !blob.is_object() {
            return Err(ProviderError::new(
                "validation",
                "bindings blob must be an object",
            ));
        }
        let internal =
            |what: &str, e: std::io::Error| ProviderError::new("internal", format!("{what}: {e}"));
        let text = serde_json::to_string_pretty(&blob)
            .map_err(|e| ProviderError::new("internal", e.to_string()))?;
        if let Some(dir) = self.path.parent() {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|e| internal("create dir", e))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        tokio::fs::write(&tmp, text)
            .await
            .map_err(|e| internal("write", e))?;
        tokio::fs::rename(&tmp, &self.path)
            .await
            .map_err(|e| internal("rename", e))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn round_trips_and_rejects_non_objects() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCtlStore::in_app_data(dir.path());
        assert_eq!(store.load().await, Value::Null);
        let blob = json!({ "version": 1, "bindings": [] });
        store.save(blob.clone()).await.unwrap();
        assert_eq!(store.load().await, blob);
        assert_eq!(store.save(json!([1])).await.unwrap_err().code, "validation");
        assert!(dir.path().join("t38-controls.json").is_file());
    }
}
