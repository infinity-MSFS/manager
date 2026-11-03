use std::future::Future;

use infinity_ipc::IpcResult;
use infinity_ipc::catalog::BridgeSpec;
use serde_json::Value;

/// Where `bridge` outputs go. Implemented by the app over its bridge
/// manager, so this crate doesn't depend on the bridge crate. The
/// implementation owns the timeout; v3 used the bridge's default command
/// timeout so the relay's own (shorter) diagnosis always arrives first.
pub trait BridgeSink: Sync {
    /// Sends `payload` as `command` to the running bridge `bridge` (resolved
    /// from the product's `bridges[]` by the output's `bridgeRef`) and
    /// returns its response.
    fn command(
        &self,
        bridge: &BridgeSpec,
        command: &str,
        payload: Value,
    ) -> impl Future<Output = IpcResult<Value>> + Send;
}

/// For callers that know no bridge output will run (and for tests).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoBridge;

impl BridgeSink for NoBridge {
    async fn command(&self, bridge: &BridgeSpec, _: &str, _: Value) -> IpcResult<Value> {
        Err(infinity_ipc::IpcError::unsupported(format!(
            "bridge `{}` is not available",
            bridge.id
        )))
    }
}
