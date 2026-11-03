use std::path::PathBuf;

use infinity_ipc::{IpcError, IpcErrorCode};

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The work folder exists but the aircraft has not written its
    /// navigation database there yet.
    #[error("navigation database not found at {}", path.display())]
    NotFound { path: PathBuf },

    /// No sim has a work folder for the package, which usually means the
    /// aircraft has never been loaded.
    #[error("no work folder for package `{package_prefix}`; load the aircraft in the sim once")]
    WorkDirMissing { package_prefix: String },

    /// The query arguments cannot name anything, or name something the
    /// database does not have (an unknown runway, a runway with no
    /// published approach).
    #[error("{0}")]
    BadInput(String),

    #[error("navigation database query failed: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("{0}")]
    Internal(String),
}

impl From<Error> for IpcError {
    fn from(e: Error) -> Self {
        let message = e.to_string();
        match e {
            // The configurator's cycle indicator reads this code as "boot the
            // aircraft once first" rather than a failure.
            Error::NotFound { path } => Self::new(IpcErrorCode::PackageDirNotFound, message)
                .with_details(serde_json::json!({ "path": path.display().to_string() })),
            Error::WorkDirMissing { .. } => Self::new(IpcErrorCode::PackageDirNotFound, message),
            Error::BadInput(_) => Self::new(IpcErrorCode::ValidationFailed, message),
            Error::Sqlite(_) | Error::Internal(_) => Self::internal(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_map_to_stable_codes() {
        let code = |e: Error| IpcError::from(e).code;
        assert_eq!(
            code(Error::NotFound {
                path: PathBuf::from("nav.s3db")
            }),
            IpcErrorCode::PackageDirNotFound
        );
        assert_eq!(
            code(Error::WorkDirMissing {
                package_prefix: "x".into()
            }),
            IpcErrorCode::PackageDirNotFound
        );
        assert_eq!(
            code(Error::BadInput("Runway not found".into())),
            IpcErrorCode::ValidationFailed
        );
        assert_eq!(
            code(Error::Sqlite(rusqlite::Error::QueryReturnedNoRows)),
            IpcErrorCode::Internal
        );
    }

    #[test]
    fn not_found_carries_the_path() {
        let err = IpcError::from(Error::NotFound {
            path: PathBuf::from("nav.s3db"),
        });
        assert_eq!(err.details.unwrap()["path"], "nav.s3db");
    }
}
