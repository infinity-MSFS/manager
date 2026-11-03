use std::path::{Path, PathBuf};

use infinity_ipc::{IpcError, IpcErrorCode};

pub type Result<T, E = FlirError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum FlirError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("FLIR settings: {0}")]
    Json(#[from] serde_json::Error),
    #[error("FLIR preview: {0}")]
    Encode(String),
}

impl FlirError {
    pub(crate) fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

impl From<FlirError> for IpcError {
    fn from(e: FlirError) -> Self {
        let code = match &e {
            FlirError::Io { .. } => IpcErrorCode::IoError,
            FlirError::Json(_) => IpcErrorCode::ValidationFailed,
            FlirError::Encode(_) => IpcErrorCode::Internal,
        };
        Self::new(code, e.to_string())
    }
}
