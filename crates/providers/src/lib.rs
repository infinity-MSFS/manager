pub mod community;
mod content;
mod identity;
mod sim;

pub use content::{ContentProvider, DownloadUrl};
pub use identity::{AccessToken, IdentityProvider};
pub use sim::{SimContext, SimService};

use std::sync::Arc;

use infinity_ipc::IpcResult;
use infinity_ipc::dto::{Capabilities, Channel, Edition};

#[derive(Debug, Clone)]
pub struct Providers {
    pub edition: Edition,
    pub identity: Arc<dyn IdentityProvider>,
    pub content: Arc<dyn ContentProvider>,
    pub sim_services: Vec<Arc<dyn SimService>>,
    pub channels: Vec<Channel>,
    /// Navigraph linking, when this build has app credentials for it.
    pub navigraph: Option<NavigraphAccess>,
}

/// What Navigraph linking needs from the build.
#[derive(Debug, Clone)]
pub struct NavigraphAccess {
    pub credentials: Arc<dyn infinity_navigraph::CredentialsSource>,
    /// Where the account's refresh token is kept. `None` keeps it in a
    /// file in app data.
    pub tokens: Option<Arc<dyn infinity_navigraph::RefreshTokenStore>>,
}

impl Providers {
    pub fn community(
        catalog_url: &str,
        changelog_url: Option<&str>,
        efb_docs_base: Option<url::Url>,
        artifact_base: Option<url::Url>,
    ) -> IpcResult<Self> {
        let mut content = community::HttpContent::new(catalog_url, artifact_base)?;
        if let Some(template) = changelog_url {
            content = content.with_changelog_url(template)?;
        }
        if let Some(base) = efb_docs_base {
            content = content.with_efb_docs(base);
        }
        Ok(Self {
            edition: Edition::Community,
            identity: Arc::new(community::NoIdentity),
            content: Arc::new(content),
            sim_services: Vec::new(),
            channels: vec![Channel::Stable],
            navigraph: None,
        })
    }

    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            accounts: self.identity.available(),
            store: self.content.supports_entitlements(),
            drm: self.sim_services.iter().any(|s| s.provides_drm()),
            channels: self.channels.clone(),
            navigraph: self.navigraph.is_some(),
        }
    }
}
