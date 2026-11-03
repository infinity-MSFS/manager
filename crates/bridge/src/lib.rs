//! Loopback WebSocket bridges between the manager and in-sim WASM gauges.
//!
//! Each catalog [`BridgeSpec`] becomes one `infinity-bridge-host` server
//! owned by a [`BridgeManager`]. Servers bind `127.0.0.1:<port>` and serve
//! the gauge socket at `/<namespace>`, so the DC-10 gauge that dials
//! `ws://127.0.0.1:9876/dc10` keeps working unchanged.
//!
//! Gauge events from every bridge are merged into one stream
//! ([`BridgeManager::subscribe`]) tagged with their namespace; the app
//! forwards them to the UI as `bridge:<namespace>:<event>`
//! ([`BridgeEvent::channel`]). Connection changes have a stream of their own
//! ([`BridgeManager::subscribe_status`]).
//!
//! Nothing here knows about any product, entitlement or auth: the bridge is
//! transport only, so builds that add DRM layer it on top.

mod error;
mod manager;
mod shutdown;

pub use error::BridgeError;
pub use infinity_bridge_host::{ErrorKind, HelloPayload};
pub use infinity_ipc::catalog::{BridgeKind, BridgeSpec};
pub use manager::{
    BridgeConfig, BridgeEvent, BridgeManager, BridgeStatus, ConnectionChange, DEFAULT_CMD_ATTEMPTS,
    DEFAULT_CMD_TIMEOUT,
};
