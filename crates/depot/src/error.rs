pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A manifest this build cannot read: newer schema or hash.
    #[error("{0}")]
    Unsupported(String),

    /// A manifest that is malformed or describes an impossible package.
    #[error("the package manifest is invalid: {0}")]
    Invalid(String),

    /// The manifest bytes are not the ones the catalog promised.
    #[error("the package manifest does not match the catalog (expected {expected}, got {actual})")]
    ManifestHash { expected: String, actual: String },

    /// Chunk bytes that do not decode, or decode to something else.
    #[error("chunk {chunk} is damaged: {reason}")]
    Chunk { chunk: String, reason: String },

    /// A package folder the builder refuses.
    #[error("{0}")]
    Package(String),

    #[error("{}: {source}", path.display())]
    Io {
        path: std::path::PathBuf,
        source: std::io::Error,
    },

    #[error("cancelled")]
    Cancelled,
}

#[cfg(feature = "build")]
pub(crate) trait IoContext<T> {
    fn at(self, path: &std::path::Path) -> Result<T>;
}

#[cfg(feature = "build")]
impl<T> IoContext<T> for std::io::Result<T> {
    fn at(self, path: &std::path::Path) -> Result<T> {
        self.map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Self::Io {
            path: std::path::PathBuf::new(),
            source,
        }
    }
}
