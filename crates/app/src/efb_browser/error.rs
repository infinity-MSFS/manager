//! Why a browser session could not start or keep going.
//!
//! None of these reach the IPC layer: the EFB hears about them as the
//! `reason` of a `browser.gone` frame, so `Display` is written for the crew
//! to read on the panel.

use super::engine::ENGINE_ENV;

/// A failure talking to the engine over the DevTools protocol.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CdpError {
    #[error("could not connect to the browser engine: {0}")]
    Connect(#[source] Box<tokio_tungstenite::tungstenite::Error>),
    #[error("the browser engine closed its connection")]
    Closed,
    #[error("the browser engine did not answer {method} in time")]
    Timeout { method: String },
    /// A normal reply carrying an `error` member: the engine understood and
    /// refused, which is different from a transport failure.
    #[error("{method}: {message}")]
    Remote { method: String, message: String },
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum BrowserError {
    #[error(
        "No Chromium-based browser found. Install Microsoft Edge or Google Chrome, \
         or set {ENGINE_ENV} to a browser executable."
    )]
    NoEngine,
    #[error("could not prepare the browser profile: {0}")]
    Profile(#[source] std::io::Error),
    #[error("could not start {exe}: {source}")]
    Spawn {
        exe: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{exe} exited before it was ready ({status})")]
    ExitedEarly { exe: String, status: String },
    #[error("{exe} did not report a DevTools port within {secs}s")]
    NoDevToolsPort { exe: String, secs: u64 },
    #[error(transparent)]
    Cdp(#[from] CdpError),
    /// The engine answered but the reply lacked a field every Chromium sends.
    #[error("the browser engine answered {method} without a {field}")]
    Protocol {
        method: &'static str,
        field: &'static str,
    },
}
