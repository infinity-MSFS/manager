use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use infinity_ipc::catalog::{Catalog, Product};
use infinity_ipc::dto::{Channel, ProductRef};
use infinity_ipc::{IpcError, IpcErrorCode, IpcResult};
use infinity_providers::Providers;
use tokio::sync::Mutex;

/// How long a fetched catalog answers product lookups. The library fetches
/// fresh on every visit; this only spares the per-keystroke configurator
/// saves a network round trip.
const CATALOG_TTL: Duration = Duration::from_secs(300);

#[derive(Debug)]
pub(crate) struct AppState {
    pub(crate) providers: Providers,
    catalogs: Mutex<HashMap<Channel, (Instant, Arc<Catalog>)>>,
}

impl AppState {
    pub(crate) fn new(providers: Providers) -> Self {
        Self {
            providers,
            catalogs: Mutex::new(HashMap::new()),
        }
    }

    /// A channel's catalog from the provider, refreshing the cache.
    pub(crate) async fn fetch_catalog(&self, channel: Channel) -> IpcResult<Arc<Catalog>> {
        if !self.providers.channels.contains(&channel) {
            return Err(IpcError::new(
                IpcErrorCode::AuthForbidden,
                format!(
                    "channel {} is not available in this build",
                    channel.as_str()
                ),
            ));
        }
        let catalog = Arc::new(self.providers.content.catalog(channel).await?);
        self.catalogs
            .lock()
            .await
            .insert(channel, (Instant::now(), catalog.clone()));
        Ok(catalog)
    }

    /// A channel's catalog, from the cache while it is fresh.
    pub(crate) async fn catalog(&self, channel: Channel) -> IpcResult<Arc<Catalog>> {
        let cached = self.catalogs.lock().await.get(&channel).cloned();
        match cached {
            Some((at, catalog)) if at.elapsed() < CATALOG_TTL => Ok(catalog),
            _ => self.fetch_catalog(channel).await,
        }
    }

    pub(crate) async fn product(&self, at: &ProductRef) -> IpcResult<Product> {
        self.catalog(at.channel)
            .await?
            .products
            .iter()
            .find(|p| p.id == at.product_id)
            .cloned()
            .ok_or_else(|| {
                IpcError::new(
                    IpcErrorCode::ProductUnknown,
                    format!(
                        "{} is not on the {} channel",
                        at.product_id,
                        at.channel.as_str()
                    ),
                )
            })
    }
}
