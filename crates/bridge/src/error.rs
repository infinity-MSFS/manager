use infinity_bridge_host::ErrorKind;
use infinity_ipc::{IpcError, IpcErrorCode};

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("bridge {0} is already running")]
    AlreadyRunning(String),

    #[error("no bridge is running for {0}")]
    NotFound(String),

    #[error("nothing is connected to bridge {0}")]
    NotConnected(String),

    #[error("bridge {namespace} could not listen on {addr}: {source}")]
    Bind {
        namespace: String,
        addr: std::net::SocketAddr,
        source: std::io::Error,
    },

    /// A command reached the transport and came back unsuccessful. `kind`
    /// separates "nothing answered" from "the gauge answered no": the
    /// difference between retrying and reporting. Kept structured because a
    /// debug-formatted wire error once reached the UI as
    /// `bridgeError {kind: timeout, ...}`, naming the symptom and burying the
    /// cause.
    #[error("bridge command failed [{kind}]: {message}")]
    Command { kind: ErrorKind, message: String },
}

impl BridgeError {
    pub(crate) fn from_wire(e: &infinity_bridge_host::BridgeError) -> Self {
        Self::Command {
            kind: e.kind(),
            message: e.message().to_owned(),
        }
    }

    /// Whether re-issuing the same command could plausibly succeed.
    ///
    /// The far end is a flight sim: it drops the socket on menu transitions,
    /// stalls through loading screens, and brings its WASM module up seconds
    /// after the panel that fronts it. Treating that as a hard failure made
    /// the configurator look broken at random.
    pub fn is_retryable(&self) -> bool {
        match self {
            // The bridge exists; nothing is on the other end yet.
            Self::NotConnected(_) => true,
            Self::Command { kind, message } => match kind {
                ErrorKind::Timeout | ErrorKind::Transport | ErrorKind::NoClients => true,
                // The relay answered, but only to say the far side was not
                // reachable. Both of its own failure modes clear by
                // themselves: it re-binds CommBus every second, and the module
                // finishes loading shortly after the panel. Anything else here
                // is the gauge refusing on the merits (`INVALID_EQUIP_IDX`...)
                // and must not be replayed.
                ErrorKind::Application => {
                    message.starts_with("WASM_TIMEOUT")
                        || message.starts_with("COMMBUS_NOT_")
                        || message.starts_with("STATE_BUSY")
                }
                ErrorKind::Protocol => false,
            },
            // Configuration, not conditions.
            Self::AlreadyRunning(_) | Self::NotFound(_) | Self::Bind { .. } => false,
        }
    }

    fn kind_tag(&self) -> &'static str {
        match self {
            Self::AlreadyRunning(_) => "already_running",
            Self::NotFound(_) => "not_found",
            Self::NotConnected(_) => "not_connected",
            Self::Bind { .. } => "bind",
            Self::Command { kind, .. } => match kind {
                ErrorKind::Transport => "transport",
                ErrorKind::Timeout => "timeout",
                ErrorKind::Protocol => "protocol",
                ErrorKind::Application => "application",
                ErrorKind::NoClients => "no_clients",
            },
        }
    }
}

/// The UI gets the readable message plus `details.kind` and
/// `details.retryable`, which is what it needs to choose between "waiting for
/// the sim" and an error.
impl From<BridgeError> for IpcError {
    fn from(e: BridgeError) -> Self {
        let code = match &e {
            BridgeError::Bind { .. } => IpcErrorCode::IoError,
            _ => IpcErrorCode::Internal,
        };
        let details = serde_json::json!({ "kind": e.kind_tag(), "retryable": e.is_retryable() });
        IpcError::new(code, e.to_string()).with_details(details)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use infinity_bridge_host::BridgeError as WireError;

    #[test]
    fn transport_outcomes_are_retried() {
        for wire in [
            WireError::timeout("no ack received within 8s"),
            WireError::transport("all gauge connections dropped before ack"),
            WireError::no_clients("no gauges connected"),
        ] {
            let e = BridgeError::from_wire(&wire);
            assert!(e.is_retryable(), "{e}");
        }
        assert!(BridgeError::NotConnected("dc10".into()).is_retryable());
    }

    #[test]
    fn a_refusal_from_the_gauge_is_not_retried() {
        // Replaying these can only produce the same answer, and for a command
        // with a side effect it would produce it twice.
        for msg in ["VARIANT_UNRESOLVED", "INVALID_EQUIP_IDX: 240", "CMD_FAILED"] {
            let e = BridgeError::from_wire(&WireError::application(msg));
            assert!(!e.is_retryable(), "{msg} should not be retried");
        }
        assert!(!BridgeError::from_wire(&WireError::protocol("bad JSON")).is_retryable());
    }

    #[test]
    fn the_relays_own_unreachable_answers_are_retried() {
        for msg in [
            "WASM_TIMEOUT: no response from WASM gauge",
            "COMMBUS_NOT_BOUND: response listener never attached",
            "COMMBUS_NOT_READY: CommBus not initialized",
            "STATE_BUSY: equipment state is being updated",
        ] {
            let e = BridgeError::from_wire(&WireError::application(msg));
            assert!(e.is_retryable(), "{msg} should be retried");
        }
    }

    #[test]
    fn configuration_faults_are_never_retried() {
        assert!(!BridgeError::NotFound("dc10".into()).is_retryable());
        assert!(!BridgeError::AlreadyRunning("dc10".into()).is_retryable());
        let bind = BridgeError::Bind {
            namespace: "dc10".into(),
            addr: "127.0.0.1:9876".parse().unwrap(),
            source: std::io::ErrorKind::AddrInUse.into(),
        };
        assert!(!bind.is_retryable());
    }

    #[test]
    fn the_message_names_the_cause_not_just_the_symptom() {
        let e = BridgeError::from_wire(&WireError::application("COMMBUS_NOT_BOUND: nope"));
        let shown = e.to_string();
        assert!(shown.contains("APPLICATION"), "{shown}");
        assert!(shown.contains("COMMBUS_NOT_BOUND"), "{shown}");
        assert!(!shown.contains("kind:"), "no debug formatting: {shown}");
    }

    #[test]
    fn ipc_error_carries_kind_and_retryability() {
        let err = IpcError::from(BridgeError::from_wire(&WireError::timeout("slow")));
        let details = err.details.unwrap();
        assert_eq!(details["kind"], "timeout");
        assert_eq!(details["retryable"], true);
    }
}
