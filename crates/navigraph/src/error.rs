use std::path::{Path, PathBuf};

use infinity_ipc::{IpcError, IpcErrorCode};

pub type Result<T, E = NavigraphError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NavigraphError {
    #[error("No Navigraph account linked. Link one in Settings.")]
    NotLinked,

    #[error("Navigraph session expired. Re-link your account in Settings.")]
    RelinkRequired,

    #[error(
        "Navigraph refused the request. Check your subscription, or re-link your account in \
         Settings (links made before navdata support need a refresh)."
    )]
    NoSubscription,

    #[error("Navigraph link not started")]
    LinkNotStarted,

    #[error("Navigraph app credentials unavailable: {}", .0.message)]
    Credentials(IpcError),

    #[error("Navigraph token store: {}", .0.message)]
    Store(IpcError),

    #[error("network error talking to Navigraph: {0}")]
    Network(#[from] reqwest::Error),

    #[error("Navigraph API error (HTTP {status}): {body}")]
    Api { status: u16, body: String },

    #[error("No matching Navigraph navdata package for this product.")]
    NoMatchingPackage,

    #[error("Navigraph package lists no files")]
    NoPackageFiles,

    #[error("{product} work directory not found. Is the aircraft installed?")]
    WorkDirNotFound { product: String },

    #[error("the navdata download server answered HTTP {0}")]
    Download(u16),

    #[error("the navdata download does not match its checksum (expected {expected}, got {actual})")]
    Checksum { expected: String, actual: String },

    #[error("the navdata archive is unusable: {0}")]
    Archive(String),

    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("{0}")]
    Internal(String),
}

impl NavigraphError {
    /// Stable machine-readable reason, carried in `IpcError.details.reason`.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::NotLinked => "not_linked",
            Self::RelinkRequired => "relink_required",
            Self::NoSubscription => "no_subscription",
            Self::LinkNotStarted => "link_not_started",
            Self::Credentials(_) => "credentials",
            Self::Store(_) => "store",
            Self::Network(_) => "network",
            Self::Api { .. } => "api",
            Self::NoMatchingPackage => "no_matching_package",
            Self::NoPackageFiles => "no_package_files",
            Self::WorkDirNotFound { .. } => "work_dir_not_found",
            Self::Download(_) => "download",
            Self::Checksum { .. } => "checksum",
            Self::Archive(_) => "archive",
            Self::Io { .. } => "io",
            Self::Internal(_) => "internal",
        }
    }
}

impl From<NavigraphError> for IpcError {
    fn from(e: NavigraphError) -> Self {
        let message = e.to_string();
        let reason = e.reason();
        let code = match &e {
            NavigraphError::NotLinked => IpcErrorCode::AuthRequired,
            NavigraphError::RelinkRequired => IpcErrorCode::AuthExpired,
            NavigraphError::NoSubscription => IpcErrorCode::AuthForbidden,
            NavigraphError::LinkNotStarted => IpcErrorCode::ValidationFailed,
            // The seam implementations pick the code (e.g. auth_required
            // when the user is signed out of the Infinity account).
            NavigraphError::Credentials(inner) | NavigraphError::Store(inner) => inner.code,
            NavigraphError::Network(n) if n.is_timeout() => IpcErrorCode::CdnTimeout,
            NavigraphError::Network(_) => IpcErrorCode::NetworkUnreachable,
            NavigraphError::Api { status: 429, .. } | NavigraphError::Download(429) => {
                IpcErrorCode::RateLimited
            }
            NavigraphError::Api { .. } => IpcErrorCode::NetworkUnreachable,
            NavigraphError::NoMatchingPackage => IpcErrorCode::ProductUnknown,
            NavigraphError::NoPackageFiles => IpcErrorCode::ManifestInvalid,
            NavigraphError::WorkDirNotFound { .. } => IpcErrorCode::PackageDirNotFound,
            NavigraphError::Download(404) => IpcErrorCode::CdnNotFound,
            NavigraphError::Download(401 | 403) => IpcErrorCode::CdnForbidden,
            NavigraphError::Download(408 | 504) => IpcErrorCode::CdnTimeout,
            NavigraphError::Download(_) => IpcErrorCode::NetworkUnreachable,
            NavigraphError::Checksum { .. } => IpcErrorCode::CdnSignatureInvalid,
            NavigraphError::Archive(_) => IpcErrorCode::ManifestInvalid,
            NavigraphError::Io { .. } => IpcErrorCode::IoError,
            NavigraphError::Internal(_) => IpcErrorCode::Internal,
        };
        let mut details = serde_json::json!({ "reason": reason });
        if let NavigraphError::Io { path, .. } = &e {
            details["path"] = path.display().to_string().into();
        }
        IpcError::new(code, message).with_details(details)
    }
}

/// Attaches the path to an IO error.
pub(crate) trait IoContext<T> {
    fn at(self, path: &Path) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn at(self, path: &Path) -> Result<T> {
        self.map_err(|source| NavigraphError::Io {
            path: path.to_path_buf(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_states_map_to_stable_codes() {
        let code = |e: NavigraphError| IpcError::from(e).code;
        assert_eq!(code(NavigraphError::NotLinked), IpcErrorCode::AuthRequired);
        assert_eq!(
            code(NavigraphError::RelinkRequired),
            IpcErrorCode::AuthExpired
        );
        assert_eq!(
            code(NavigraphError::NoSubscription),
            IpcErrorCode::AuthForbidden
        );
        assert_eq!(
            code(NavigraphError::NoMatchingPackage),
            IpcErrorCode::ProductUnknown
        );
        assert_eq!(
            code(NavigraphError::WorkDirNotFound {
                product: "T-38C".into()
            }),
            IpcErrorCode::PackageDirNotFound
        );
        assert_eq!(
            code(NavigraphError::Download(403)),
            IpcErrorCode::CdnForbidden
        );
    }

    #[test]
    fn seam_errors_keep_their_code() {
        let inner = IpcError::new(IpcErrorCode::AuthRequired, "signed out");
        let err = IpcError::from(NavigraphError::Credentials(inner));
        assert_eq!(err.code, IpcErrorCode::AuthRequired);
        assert!(err.message.contains("signed out"));
        assert_eq!(err.details.unwrap()["reason"], "credentials");
    }
}
