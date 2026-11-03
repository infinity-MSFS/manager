use std::path::{Path, PathBuf};

use infinity_ipc::{IpcError, IpcErrorCode};

use crate::space;

pub(crate) type Result<T, E = Error> = std::result::Result<T, E>;

/// Why a job stopped early at the user's request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    Pause,
    Cancel,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("stopped")]
    Stopped(Stop),

    #[error("the download server answered HTTP {0}")]
    Http(reqwest::StatusCode),

    #[error("download failed: {0}")]
    Network(#[from] reqwest::Error),

    #[error("the download ended early: got {got} of {expected} bytes")]
    Truncated { got: u64, expected: u64 },

    #[error("the download was damaged on the way (its checksum does not match)")]
    Checksum { expected: String, actual: String },

    #[error("the package archive is unusable: {0}")]
    Archive(String),

    /// The download is not the kind of archive the catalog says it is.
    /// Not retried: downloading the same file again would not change it.
    #[error("the package archive is unusable: {0}")]
    Format(String),

    /// An archive format this build cannot install.
    #[error("{0}")]
    Unsupported(String),

    #[error(
        "not enough space on {} to {what}: needs {}, {} free",
        space::volume_label(path),
        space::human_bytes(*needed),
        space::human_bytes(*available)
    )]
    DiskFull {
        path: PathBuf,
        what: &'static str,
        needed: u64,
        available: u64,
    },

    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("{0}")]
    Internal(String),
}

impl Error {
    /// Worth retrying from where the download left off.
    pub(crate) fn is_transient(&self) -> bool {
        match self {
            // A connection cut mid-body surfaces as a decode error.
            Self::Network(e) => !e.is_builder() && !e.is_redirect(),
            Self::Truncated { .. } => true,
            Self::Http(s) => s.is_server_error() || *s == reqwest::StatusCode::REQUEST_TIMEOUT,
            _ => false,
        }
    }
}

/// Attaches the path to an IO error.
pub(crate) trait IoContext<T> {
    fn at(self, path: &Path) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn at(self, path: &Path) -> Result<T> {
        self.map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })
    }
}

impl From<zip::result::ZipError> for Error {
    fn from(e: zip::result::ZipError) -> Self {
        match e {
            zip::result::ZipError::Io(source) => Self::Io {
                path: PathBuf::from("archive"),
                source,
            },
            other => Self::Archive(other.to_string()),
        }
    }
}

impl From<Error> for IpcError {
    fn from(e: Error) -> Self {
        let message = e.to_string();
        match e {
            Error::Stopped(Stop::Cancel) => Self::new(IpcErrorCode::JobCancelled, message),
            Error::Stopped(Stop::Pause) => Self::internal("job paused"),
            Error::Http(status) => {
                let code = match status.as_u16() {
                    404 => IpcErrorCode::CdnNotFound,
                    401 | 403 => IpcErrorCode::CdnForbidden,
                    408 | 504 => IpcErrorCode::CdnTimeout,
                    429 => IpcErrorCode::RateLimited,
                    _ => IpcErrorCode::NetworkUnreachable,
                };
                Self::new(code, message)
            }
            Error::Network(ref n) if n.is_timeout() => Self::new(IpcErrorCode::CdnTimeout, message),
            Error::Network(_) | Error::Truncated { .. } => {
                Self::new(IpcErrorCode::NetworkUnreachable, message)
            }
            Error::Checksum { expected, actual } => {
                Self::new(IpcErrorCode::CdnSignatureInvalid, message).with_details(
                    serde_json::json!({ "expected_sha256": expected, "actual_sha256": actual }),
                )
            }
            Error::Archive(_) | Error::Format(_) => {
                Self::new(IpcErrorCode::ManifestInvalid, message)
            }
            Error::Unsupported(_) => Self::unsupported(message),
            Error::DiskFull {
                path,
                needed,
                available,
                ..
            } => disk_full(message, &path, Some((needed, available))),
            Error::Io { path, source } if space::is_disk_full(&source) => {
                disk_full(message, &path, None)
            }
            Error::Io { path, source } => {
                let message = if source.kind() == std::io::ErrorKind::PermissionDenied {
                    format!("{message}. If the simulator is running, close it and try again.")
                } else {
                    message
                };
                Self::new(IpcErrorCode::IoError, message)
                    .with_details(serde_json::json!({ "path": path.display().to_string() }))
            }
            Error::Internal(_) => Self::internal(message),
        }
    }
}

fn disk_full(message: String, path: &Path, figures: Option<(u64, u64)>) -> IpcError {
    let mut details = serde_json::json!({ "path": path.display().to_string() });
    if let Some((needed, available)) = figures {
        details["needed_bytes"] = needed.into();
        details["available_bytes"] = available.into();
    }
    IpcError::new(IpcErrorCode::DiskFull, message).with_details(details)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_statuses_map_to_stable_codes() {
        let code =
            |s: u16| IpcError::from(Error::Http(reqwest::StatusCode::from_u16(s).unwrap())).code;
        assert_eq!(code(404), IpcErrorCode::CdnNotFound);
        assert_eq!(code(403), IpcErrorCode::CdnForbidden);
        assert_eq!(code(429), IpcErrorCode::RateLimited);
        assert_eq!(code(500), IpcErrorCode::NetworkUnreachable);
    }

    #[test]
    fn disk_full_carries_figures() {
        let err = IpcError::from(Error::DiskFull {
            path: PathBuf::from("/data"),
            what: "download this package",
            needed: 10,
            available: 5,
        });
        assert_eq!(err.code, IpcErrorCode::DiskFull);
        let details = err.details.unwrap();
        assert_eq!(details["needed_bytes"], 10);
        assert_eq!(details["available_bytes"], 5);
    }

    #[test]
    fn only_wire_failures_are_transient() {
        assert!(
            Error::Truncated {
                got: 1,
                expected: 2
            }
            .is_transient()
        );
        assert!(Error::Http(reqwest::StatusCode::BAD_GATEWAY).is_transient());
        assert!(!Error::Http(reqwest::StatusCode::FORBIDDEN).is_transient());
        assert!(!Error::Archive("x".into()).is_transient());
    }
}
