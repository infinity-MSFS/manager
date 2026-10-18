use async_trait::async_trait;
use infinity_ipc::IpcResult;
use infinity_ipc::catalog::{Catalog, PresignArtifact, Product};
use infinity_ipc::dto::Channel;

#[async_trait]
pub trait ContentProvider: Send + Sync + std::fmt::Debug {
    fn supports_entitlements(&self) -> bool {
        false
    }

    async fn catalog(&self, channel: Channel) -> IpcResult<Catalog>;

    async fn resolve_download(
        &self,
        product: &Product,
        artifact: PresignArtifact,
    ) -> IpcResult<DownloadUrl>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadUrl {
    pub url: url::Url,
    pub expires_at: Option<u64>,
}
