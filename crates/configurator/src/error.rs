use std::path::{Path, PathBuf};

use infinity_ipc::{IpcError, IpcErrorCode};

pub type Result<T, E = ConfiguratorError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum ConfiguratorError {
    #[error("Migration failed: {0}")]
    Migration(String),

    #[error("Validation failed: {0}")]
    Validation(String),

    #[error("Unknown export transform: {0}")]
    UnknownTransform(String),

    #[error("Transform failed for `{transform}`: {message}")]
    TransformFailed { transform: String, message: String },

    #[error("Invalid path: {0}")]
    InvalidPath(String),

    #[error("Schema invalid: {0}")]
    SchemaInvalid(String),

    #[error("Invalid product id: {0}")]
    InvalidProductId(String),

    #[error("Config not found: {0}")]
    NotFound(String),

    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    /// A file on disk that should hold JSON doesn't parse.
    #[error("{}: {source}", path.display())]
    Corrupt {
        path: PathBuf,
        source: serde_json::Error,
    },

    /// Failures from the caller's collaborators (work-dir resolution, bridge
    /// sink) pass through untouched so their codes survive.
    #[error(transparent)]
    Upstream(#[from] IpcError),

    #[error("{0}")]
    Internal(String),
}

/// Attaches the path to an IO error.
pub(crate) trait IoContext<T> {
    fn at(self, path: &Path) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn at(self, path: &Path) -> Result<T> {
        self.map_err(|source| ConfiguratorError::Io {
            path: path.to_path_buf(),
            source,
        })
    }
}

impl From<ConfiguratorError> for IpcError {
    fn from(e: ConfiguratorError) -> Self {
        use ConfiguratorError as E;
        let message = e.to_string();
        let code = match e {
            E::Upstream(inner) => return inner,
            E::Io { path, .. } => {
                return Self::new(IpcErrorCode::IoError, message)
                    .with_details(serde_json::json!({ "path": path.display().to_string() }));
            }
            E::Migration(_) => IpcErrorCode::MigrationFailed,
            E::SchemaInvalid(_) => IpcErrorCode::SchemaInvalid,
            // v3 reported an unparseable config file as a validation failure.
            E::Validation(_)
            | E::UnknownTransform(_)
            | E::TransformFailed { .. }
            | E::InvalidPath(_)
            | E::Corrupt { .. } => IpcErrorCode::ValidationFailed,
            E::InvalidProductId(_) | E::NotFound(_) => IpcErrorCode::ProductUnknown,
            E::Internal(_) => IpcErrorCode::Internal,
        };
        Self::new(code, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_to_stable_codes() {
        let code = |e: ConfiguratorError| IpcError::from(e).code;
        assert_eq!(
            code(ConfiguratorError::Migration("x".into())),
            IpcErrorCode::MigrationFailed
        );
        assert_eq!(
            code(ConfiguratorError::SchemaInvalid("x".into())),
            IpcErrorCode::SchemaInvalid
        );
        assert_eq!(
            code(ConfiguratorError::UnknownTransform("x".into())),
            IpcErrorCode::ValidationFailed
        );
        assert_eq!(
            code(ConfiguratorError::NotFound("x".into())),
            IpcErrorCode::ProductUnknown
        );
    }

    #[test]
    fn upstream_errors_pass_through() {
        let inner = IpcError::new(IpcErrorCode::PackageDirNotFound, "no sim");
        assert_eq!(
            IpcError::from(ConfiguratorError::from(inner.clone())),
            inner
        );
    }

    #[test]
    fn io_errors_carry_the_path() {
        let err = IpcError::from(ConfiguratorError::Io {
            path: PathBuf::from("/x/y.json"),
            source: std::io::Error::other("boom"),
        });
        assert_eq!(err.code, IpcErrorCode::IoError);
        assert!(err.details.is_some());
    }
}
