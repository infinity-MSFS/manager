//! Frames shared with `rescript-efb-mirror` clients.
//!
//! `rescript-efb-mirror/DESIGN.md` is the contract; a change here is a
//! protocol change, so bump [`PROTOCOL_VERSION`] and update the client.
//! `Map` rather than `HashMap` keeps key order on the wire, which helps when
//! chasing a frame mismatch in dev tools.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Both sides pin it; a mismatch closes with
/// [`close_code::STATE_RESET`] on the client side.
pub const PROTOCOL_VERSION: u32 = 1;

/// WebSocket close codes the server sends.
pub mod close_code {
    /// The manager forgot this device (or rotated the secret).
    pub const FORGOTTEN: u16 = 4001;
    /// No frame, ping included, for the idle timeout.
    pub const IDLE: u16 = 4408;
    pub const STATE_RESET: u16 = 4409;
    /// The server is stopping.
    pub const GOING_AWAY: u16 = 1001;
}

/// Minted per connection; only used to keep a `cmd` from echoing back to
/// its sender. Never on the wire.
pub type ClientId = u64;

/// Frames a client sends.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum Incoming {
    /// The in-sim publisher pushing simvar deltas. Mirrors never send it.
    Sim {
        data: Map<String, Value>,
    },
    /// A UI state write. Rebroadcast to everyone, sender included: clients
    /// commit on the rebroadcast.
    Ui {
        data: Map<String, Value>,
    },
    /// One-shot event, rebroadcast to everyone but the sender unless a
    /// provider answers it.
    Cmd {
        name: String,
        #[serde(default)]
        data: Value,
    },
    /// The client saw a `seq` gap and wants a fresh snapshot.
    Resync,
    Ping,
}

/// Frames the server sends.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum Outgoing {
    /// Sent on connect, on `resync`, and when a client fell too far behind.
    Snapshot {
        proto: u32,
        sim_seq: u64,
        ui_seq: u64,
        sim: Map<String, Value>,
        ui: Map<String, Value>,
    },
    Sim {
        seq: u64,
        data: Map<String, Value>,
    },
    Ui {
        seq: u64,
        data: Map<String, Value>,
    },
    Cmd {
        name: String,
        data: Value,
    },
    Pong,
}

/// An outgoing frame plus who caused it, so a client task can skip a `cmd`
/// it sent itself.
#[derive(Debug, Clone)]
pub struct Broadcast {
    pub frame: Outgoing,
    pub origin: Option<ClientId>,
}

impl Broadcast {
    pub(crate) fn is_for(&self, client: ClientId) -> bool {
        !matches!(self.frame, Outgoing::Cmd { .. }) || self.origin != Some(client)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn frames_match_the_design_doc() {
        let snap = Outgoing::Snapshot {
            proto: 1,
            sim_seq: 2,
            ui_seq: 3,
            sim: Map::new(),
            ui: Map::new(),
        };
        assert_eq!(
            serde_json::to_value(&snap).unwrap(),
            json!({ "t": "snapshot", "proto": 1, "sim_seq": 2, "ui_seq": 3, "sim": {}, "ui": {} })
        );
        assert_eq!(
            serde_json::to_value(Outgoing::Pong).unwrap(),
            json!({ "t": "pong" })
        );
        let cmd: Incoming = serde_json::from_str(r#"{"t":"cmd","name":"refresh"}"#).unwrap();
        assert!(matches!(
            cmd,
            Incoming::Cmd {
                data: Value::Null,
                ..
            }
        ));
        let resync: Incoming = serde_json::from_str(r#"{"t":"resync"}"#).unwrap();
        assert!(matches!(resync, Incoming::Resync));
    }
}
