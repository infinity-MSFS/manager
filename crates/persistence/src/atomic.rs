use std::path::{Path, PathBuf};

use infinity_ipc::{IpcError, IpcErrorCode, IpcResult};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::fs;

pub(crate) async fn read_json_or_default<T>(path: &Path) -> IpcResult<T>
where
    T: DeserializeOwned + Default,
{
    let bytes = match fs::read(path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(e) => return Err(io(path, &e)),
    };
    match serde_json::from_slice(&bytes) {
        Ok(v) => Ok(v),
        Err(e) => {
            let aside = with_suffix(path, "corrupt");
            tracing::warn!(path = %path.display(), error = %e, "unreadable state file moved aside");
            fs::rename(path, &aside).await.map_err(|e| io(path, &e))?;
            Ok(T::default())
        }
    }
}

pub(crate) async fn write_json<T: Serialize>(path: &Path, value: &T) -> IpcResult<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).await.map_err(|e| io(dir, &e))?;
    }
    let json = serde_json::to_vec_pretty(value)
        .map_err(|e| IpcError::internal(format!("encode {}: {e}", path.display())))?;
    let tmp = with_suffix(path, "tmp");
    fs::write(&tmp, &json).await.map_err(|e| io(&tmp, &e))?;
    fs::rename(&tmp, path).await.map_err(|e| io(path, &e))
}

fn with_suffix(path: &Path, suffux: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".");
    name.push(suffux);
    PathBuf::from(name)
}

fn io(path: &Path, e: &std::io::Error) -> IpcError {
    IpcError::new(IpcErrorCode::IoError, format!("{}: {e}", path.display()))
        .with_details(serde_json::json!({"path": path.display().to_string()}))
}
