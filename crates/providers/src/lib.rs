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
}

impl Providers {
    pub fn community(catalog_url: &str, artifact_base: Option<url::Url>) -> IpcResult<Self> {
        Ok(Self {
            edition: Edition::Community,
            identity: Arc::new(community::NoIdentity),
            content: Arc::new(community::HttpContent::new(catalog_url, artifact_base)?),
            sim_services: Vec::new(),
            channels: vec![Channel::Stable],
        })
    }

    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            accounts: self.identity.available(),
            store: self.content.supports_entitlements(),
            drm: self.sim_services.iter().any(|s| s.provides_drm()),
            channels: self.channels.clone(),
        }
    }
}
