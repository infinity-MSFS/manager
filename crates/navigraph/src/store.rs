//! The two seams a build plugs in: where the Navigraph app credentials come
//! from, and where the user's refresh token is kept. The community build
//! uses the local implementations here; the official build supplies its own.

use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use infinity_ipc::{IpcError, IpcErrorCode, IpcResult};
use serde::{Deserialize, Serialize};

use crate::client::AppCredentials;
use crate::secret::Secret;

/// Supplies the Navigraph app's OAuth client. Called lazily, at most once
/// per [`TokenManager`](crate::TokenManager) unless the result is an error.
#[async_trait]
pub trait CredentialsSource: Send + Sync + std::fmt::Debug {
    async fn fetch(&self) -> IpcResult<AppCredentials>;
}

/// Persists the user's Navigraph refresh token. Navigraph rotates the token
/// on every refresh, so `save` is called after each exchange; a store that
/// loses a write forces a re-link on the next cold start.
#[async_trait]
pub trait RefreshTokenStore: Send + Sync + std::fmt::Debug {
    /// The stored token, `None` when nothing is linked.
    async fn load(&self) -> IpcResult<Option<Secret>>;
    /// Replaces the stored token.
    async fn save(&self, refresh_token: &Secret) -> IpcResult<()>;
    /// Unlinks. Idempotent.
    async fn clear(&self) -> IpcResult<()>;
}

/// Credentials known up front, e.g. from a developer's own Navigraph app.
#[derive(Debug, Clone)]
pub struct StaticCredentials(pub AppCredentials);

#[async_trait]
impl CredentialsSource for StaticCredentials {
    async fn fetch(&self) -> IpcResult<AppCredentials> {
        Ok(self.0.clone())
    }
}

/// No credentials: linking reports why instead of failing obscurely.
#[derive(Debug, Clone, Default)]
pub struct NoCredentials;

#[async_trait]
impl CredentialsSource for NoCredentials {
    async fn fetch(&self) -> IpcResult<AppCredentials> {
        Err(IpcError::unsupported(
            "Navigraph integration is not available in this build",
        ))
    }
}

/// Keeps the token for the life of the process.
#[derive(Debug, Default)]
pub struct MemoryTokenStore(Mutex<Option<Secret>>);

#[async_trait]
impl RefreshTokenStore for MemoryTokenStore {
    async fn load(&self) -> IpcResult<Option<Secret>> {
        Ok(self.0.lock().map_err(poisoned)?.clone())
    }

    async fn save(&self, refresh_token: &Secret) -> IpcResult<()> {
        *self.0.lock().map_err(poisoned)? = Some(refresh_token.clone());
        Ok(())
    }

    async fn clear(&self) -> IpcResult<()> {
        *self.0.lock().map_err(poisoned)? = None;
        Ok(())
    }
}

fn poisoned<T>(_: std::sync::PoisonError<T>) -> IpcError {
    IpcError::internal("token store lock poisoned")
}

/// Keeps the token in a JSON file (`{"refresh_token": "..."}`, the shape the
/// v3 vault used), written atomically and readable only by the user on Unix.
#[derive(Debug, Clone)]
pub struct FileTokenStore {
    path: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct StoredToken {
    refresh_token: String,
}

impl FileTokenStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn io_err(&self, e: &std::io::Error) -> IpcError {
        IpcError::new(
            IpcErrorCode::IoError,
            format!("navigraph token file {}: {e}", self.path.display()),
        )
    }
}

#[async_trait]
impl RefreshTokenStore for FileTokenStore {
    async fn load(&self) -> IpcResult<Option<Secret>> {
        let bytes = match tokio::fs::read(&self.path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(self.io_err(&e)),
        };
        match serde_json::from_slice::<StoredToken>(&bytes) {
            Ok(t) if !t.refresh_token.is_empty() => Ok(Some(Secret::new(t.refresh_token))),
            Ok(_) => Ok(None),
            Err(e) => {
                tracing::warn!(path = %self.path.display(), error = %e, "navigraph: token file unreadable");
                Ok(None)
            }
        }
    }

    async fn save(&self, refresh_token: &Secret) -> IpcResult<()> {
        let body = serde_json::to_vec(&StoredToken {
            refresh_token: refresh_token.expose().to_owned(),
        })
        .map_err(|e| IpcError::internal(e.to_string()))?;
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| self.io_err(&e))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        tokio::fs::write(&tmp, body)
            .await
            .map_err(|e| self.io_err(&e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
                .await
                .map_err(|e| self.io_err(&e))?;
        }
        tokio::fs::rename(&tmp, &self.path)
            .await
            .map_err(|e| self.io_err(&e))
    }

    async fn clear(&self) -> IpcResult<()> {
        match tokio::fs::remove_file(&self.path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(self.io_err(&e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn file_store_round_trips_and_clears() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileTokenStore::new(tmp.path().join("nested").join("navigraph.json"));
        assert!(store.load().await.unwrap().is_none());
        store.save(&Secret::new("rt-1")).await.unwrap();
        store.save(&Secret::new("rt-2")).await.unwrap();
        assert_eq!(store.load().await.unwrap().unwrap().expose(), "rt-2");
        let raw =
            std::fs::read_to_string(tmp.path().join("nested").join("navigraph.json")).unwrap();
        assert_eq!(raw, r#"{"refresh_token":"rt-2"}"#);
        store.clear().await.unwrap();
        store.clear().await.unwrap();
        assert!(store.load().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn corrupt_file_reads_as_unlinked() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("navigraph.json");
        std::fs::write(&path, b"not json").unwrap();
        assert!(FileTokenStore::new(path).load().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn memory_store_round_trips() {
        let store = MemoryTokenStore::default();
        store.save(&Secret::new("x")).await.unwrap();
        assert_eq!(store.load().await.unwrap().unwrap().expose(), "x");
        store.clear().await.unwrap();
        assert!(store.load().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn no_credentials_is_unsupported() {
        let err = NoCredentials.fetch().await.unwrap_err();
        assert_eq!(err.code, IpcErrorCode::Unsupported);
    }
}
