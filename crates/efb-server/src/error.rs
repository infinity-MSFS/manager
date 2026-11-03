use std::net::SocketAddr;

use infinity_ipc::{IpcError, IpcErrorCode};

#[derive(Debug, thiserror::Error)]
pub enum EfbError {
    #[error("could not listen on {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        source: std::io::Error,
    },

    #[error("the EFB server is already running")]
    AlreadyRunning,

    #[error("the EFB server is not running")]
    NotRunning,

    /// No secret yet: [`crate::EfbServer::set_secret`] or `start` first.
    #[error("no pairing secret is set")]
    NoSecret,

    #[error("pairing secret too short (need at least 16 bytes, got {0})")]
    InvalidSecret(usize),

    #[error("stored pairing secret is not hex")]
    SecretEncoding,

    /// `|` separates the token's fields.
    #[error("device name must not contain '|'")]
    InvalidDeviceId,
}

impl From<EfbError> for IpcError {
    fn from(e: EfbError) -> Self {
        let message = e.to_string();
        match e {
            EfbError::Bind { addr, source } => {
                let message = if source.kind() == std::io::ErrorKind::AddrInUse {
                    format!("{message}. Another program is using port {}.", addr.port())
                } else {
                    message
                };
                Self::new(IpcErrorCode::IoError, message)
                    .with_details(serde_json::json!({ "addr": addr.to_string() }))
            }
            EfbError::InvalidDeviceId => Self::new(IpcErrorCode::ValidationFailed, message),
            EfbError::AlreadyRunning
            | EfbError::NotRunning
            | EfbError::NoSecret
            | EfbError::InvalidSecret(_)
            | EfbError::SecretEncoding => Self::internal(message),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn port_in_use_names_the_port() {
        let err = IpcError::from(EfbError::Bind {
            addr: "127.0.0.1:28012".parse().unwrap(),
            source: std::io::ErrorKind::AddrInUse.into(),
        });
        assert_eq!(err.code, IpcErrorCode::IoError);
        assert!(err.message.contains("28012"), "{}", err.message);
    }
}
