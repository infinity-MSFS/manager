use serde::{Deserialize, Serialize};

pub type IpcResult<T> = Result<T, IpcError>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("[{code}] {message}")]
pub struct IpcError {
    pub code: IpcErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl IpcError {
    pub fn new(code: IpcErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    #[must_use]
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(IpcErrorCode::Internal, message)
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(IpcErrorCode::Unsupported, message)
    }
}

/// Stable errors used across all managers, add freely, dont rename or delete
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum IpcErrorCode {
    // Network / CDN
    NetworkUnreachable,
    CdnNotFound,
    CdnForbidden,
    CdnTimeout,
    CdnSignatureInvalid,
    PresignFailed,

    // Auth
    AuthRequired,
    AuthExpired,
    AuthForbidden,

    // Entitlement
    NotOwned,

    // Product / catalog
    CatalogInvalid,
    ProductUnknown,
    ManifestInvalid,

    // Filesystem / sim
    SimNotFound,
    PackageDirNotFound,
    IoError,
    /// `details`: `{ path, needed_bytes, available_bytes }`.
    DiskFull,

    // Configurator
    SchemaInvalid,
    MigrationFailed,
    ValidationFailed,

    // Jobs
    JobNotFound,
    JobCancelled,

    RateLimited,

    /// The operation does not exist in this build or on this platform, e.g.
    /// a store feature in a community build. The UI explains rather than
    /// offering a retry
    Unsupported,

    Internal,
}

impl std::fmt::Display for IpcErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::String(s)) => f.write_str(&s),
            _ => write!(f, "{self:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_serialize_snake_case() {
        let json = serde_json::to_string(&IpcErrorCode::PackageDirNotFound).unwrap();
        assert_eq!(json, "\"package_dir_not_found\"");
        assert_eq!(IpcErrorCode::DiskFull.to_string(), "disk_full");
    }

    #[test]
    fn envelope_omits_empty_details() {
        let err = IpcError::new(IpcErrorCode::NotOwned, "nope");
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "code": "not_owned", "message": "nope" })
        );
        let back: IpcError = serde_json::from_value(json).unwrap();
        assert_eq!(back, err);
    }
}
