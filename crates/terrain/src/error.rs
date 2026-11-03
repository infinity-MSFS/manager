use std::path::PathBuf;

use infinity_ipc::{IpcError, IpcErrorCode};

pub type Result<T, E = TerrainError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum TerrainError {
    #[error("the terrain database is not loaded")]
    NotLoaded,
    #[error("terrain engine: {0}")]
    Engine(String),
    #[error("could not open the terrain database {}: {message}", path.display())]
    Database { path: PathBuf, message: String },
    #[error("terrain render failed: {0}")]
    Render(String),
    #[error("bad download URL {0}")]
    Url(String),
    #[error("the terrain download failed: {0}")]
    Network(#[from] reqwest::Error),
    #[error("the terrain download failed: the server answered {0}")]
    Http(reqwest::StatusCode),
    #[error("the terrain download ended early ({received} of {expected} bytes)")]
    Truncated { received: u64, expected: u64 },
    #[error("the terrain download is damaged (expected sha256 {expected}, got {actual})")]
    Checksum { expected: String, actual: String },
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Attaches the path an I/O error happened at.
pub(crate) trait IoContext<T> {
    fn at(self, path: &std::path::Path) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn at(self, path: &std::path::Path) -> Result<T> {
        self.map_err(|source| TerrainError::Io {
            path: path.to_path_buf(),
            source,
        })
    }
}

impl From<TerrainError> for IpcError {
    fn from(e: TerrainError) -> Self {
        let message = e.to_string();
        let code = match &e {
            TerrainError::NotLoaded | TerrainError::Database { .. } | TerrainError::Io { .. } => {
                IpcErrorCode::IoError
            }
            TerrainError::Engine(_) | TerrainError::Render(_) => IpcErrorCode::Internal,
            TerrainError::Url(_) => IpcErrorCode::ValidationFailed,
            TerrainError::Network(n) if n.is_timeout() => IpcErrorCode::CdnTimeout,
            TerrainError::Network(_) | TerrainError::Truncated { .. } => {
                IpcErrorCode::NetworkUnreachable
            }
            TerrainError::Http(status) => match status.as_u16() {
                404 => IpcErrorCode::CdnNotFound,
                401 | 403 => IpcErrorCode::CdnForbidden,
                408 | 504 => IpcErrorCode::CdnTimeout,
                429 => IpcErrorCode::RateLimited,
                _ => IpcErrorCode::NetworkUnreachable,
            },
            TerrainError::Checksum { .. } => IpcErrorCode::CdnSignatureInvalid,
        };
        Self::new(code, message)
    }
}
