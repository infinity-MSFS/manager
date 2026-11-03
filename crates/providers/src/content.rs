use async_trait::async_trait;
use infinity_ipc::catalog::{
    Catalog, ChangelogEntry, EfbDocMeta, EntitlementsResponse, LiveryCatalog, PresignArtifact,
    Product,
};
use infinity_ipc::dto::Channel;
use infinity_ipc::{IpcError, IpcResult};

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

    /// Release notes, newest first.
    async fn changelog(&self, _product_id: &str) -> IpcResult<Vec<ChangelogEntry>> {
        Ok(Vec::new())
    }

    /// Liveries published for an aircraft, with download URLs.
    async fn liveries(&self, _product_id: &str) -> IpcResult<LiveryCatalog> {
        Err(IpcError::unsupported("this build has no livery source"))
    }

    /// What the signed-in account owns. `None` when this build has no
    /// store: then only free products are available.
    async fn entitlements(&self) -> IpcResult<Option<EntitlementsResponse>> {
        Ok(None)
    }

    /// Manuals published for the EFB's documents tab.
    async fn efb_docs(&self) -> IpcResult<Vec<EfbDocMeta>> {
        Ok(Vec::new())
    }

    /// One published document's PDF bytes.
    async fn efb_doc(&self, _id: &str) -> IpcResult<Vec<u8>> {
        Err(IpcError::unsupported("this build has no document source"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadUrl {
    pub url: url::Url,
    pub expires_at: Option<u64>,
}
