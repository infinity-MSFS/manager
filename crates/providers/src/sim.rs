use std::sync::Arc;

use async_trait::async_trait;
use infinity_ipc::IpcResult;

use crate::IdentityProvider;

#[async_trait]
pub trait SimService: Send + Sync + std::fmt::Debug {
    fn name(&self) -> &'static str;

    fn provides_drm(&self) -> bool {
        false
    }

    /// An aircraft depends on this service right now, so quitting the
    /// manager should be confirmed first.
    fn in_use(&self) -> bool {
        false
    }

    async fn start(&self, ctx: SimContext) -> IpcResult<()>;

    async fn stop(&self) -> IpcResult<()>;
}

#[derive(Debug, Clone)]
pub struct SimContext {
    pub identity: Arc<dyn IdentityProvider>,
}
