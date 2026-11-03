use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::fs;

use crate::error::{ConfiguratorError, IoContext, Result};

/// Pretty JSON, the format every file this crate writes has always used.
pub(crate) fn to_pretty<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(value).map_err(|e| ConfiguratorError::Internal(e.to_string()))
}

/// Reads and parses `path`; `None` if it doesn't exist.
pub(crate) async fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let bytes = match fs::read(path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).at(path),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|source| ConfiguratorError::Corrupt {
            path: path.to_path_buf(),
            source,
        })
}

/// Writes `bytes` to `<path>.tmp` then renames over `path`, so a crash
/// mid-write never leaves a truncated file behind.
pub(crate) async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).await.at(dir)?;
    }
    let tmp = with_suffix(path, "tmp");
    fs::write(&tmp, bytes).await.at(&tmp)?;
    fs::rename(&tmp, path).await.at(path)
}

pub(crate) fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".");
    name.push(suffix);
    PathBuf::from(name)
}
