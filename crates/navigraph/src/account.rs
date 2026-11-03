//! The user's Navigraph link: device-flow linking, status and unlinking,
//! plus authenticated access for the rest of the crate. One per app.

use std::sync::Arc;

use tokio::sync::Mutex;

use crate::client::{DeviceAuth, NavigraphClient, Pkce, Poll, TileCookies, token_scopes};
use crate::error::{NavigraphError, Result};
use crate::navdata::NavdataPackage;
use crate::secret::Secret;
use crate::store::{CredentialsSource, RefreshTokenStore};
use crate::token::TokenManager;
use infinity_ipc::navigraph::{LinkPoll, LinkStart, NavigraphStatus};

#[derive(Debug)]
struct PendingLink {
    pkce: Pkce,
    auth: DeviceAuth,
}

#[derive(Debug)]
pub struct Navigraph {
    client: NavigraphClient,
    tokens: TokenManager,
    pending: Mutex<Option<PendingLink>>,
}

impl Navigraph {
    pub fn new(
        client: NavigraphClient,
        store: Arc<dyn RefreshTokenStore>,
        credentials: Arc<dyn CredentialsSource>,
    ) -> Self {
        let tokens = TokenManager::new(store, Arc::new(client.clone()), credentials);
        Self::with_tokens(client, tokens)
    }

    pub fn with_tokens(client: NavigraphClient, tokens: TokenManager) -> Self {
        Self {
            client,
            tokens,
            pending: Mutex::new(None),
        }
    }

    pub fn client(&self) -> &NavigraphClient {
        &self.client
    }

    pub fn tokens(&self) -> &TokenManager {
        &self.tokens
    }

    /// Starts a device-flow link, replacing any attempt in progress.
    pub async fn link_start(&self) -> Result<LinkStart> {
        let creds = self.tokens.credentials().await?;
        let pkce = Pkce::generate();
        let auth = self.client.device_authorization(&creds, &pkce).await?;
        let start = LinkStart {
            user_code: auth.user_code.clone(),
            verification_uri_complete: auth.verification_uri_complete.clone(),
            expires_in: auth.expires_in,
            interval: auth.interval.max(1),
        };
        *self.pending.lock().await = Some(PendingLink { pkce, auth });
        Ok(start)
    }

    /// Polls the attempt started by [`link_start`](Self::link_start) once.
    /// On success the refresh token is persisted; if the store refuses it the
    /// link still works for this session and the write is retried later.
    pub async fn link_poll(&self) -> Result<LinkPoll> {
        let (pkce, device_code) = {
            let pending = self.pending.lock().await;
            let link = pending.as_ref().ok_or(NavigraphError::LinkNotStarted)?;
            (link.pkce.clone(), link.auth.device_code.clone())
        };
        let creds = self.tokens.credentials().await?;

        match self
            .client
            .poll_device_token(&creds, &pkce, &device_code)
            .await?
        {
            Poll::Pending => Ok(LinkPoll::Pending),
            Poll::SlowDown => {
                tracing::info!("navigraph: poll slow_down");
                Ok(LinkPoll::SlowDown)
            }
            Poll::Denied => {
                tracing::info!("navigraph: link denied by user");
                self.pending.lock().await.take();
                Ok(LinkPoll::Denied)
            }
            Poll::Expired => {
                tracing::info!("navigraph: link code expired");
                self.pending.lock().await.take();
                Ok(LinkPoll::Expired)
            }
            Poll::Ready(tokens) => {
                self.pending.lock().await.take();
                match self.tokens.store().save(&tokens.refresh_token).await {
                    Ok(()) => {
                        tracing::info!("navigraph: linked; refresh token stored");
                        self.tokens.prime(*tokens).await;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "navigraph: linked but token store write failed; holding token in memory");
                        self.tokens.prime_unpersisted(*tokens).await;
                    }
                }
                Ok(LinkPoll::Linked)
            }
        }
    }

    /// Never fails: a store that cannot be read is reported in
    /// `vault_error`.
    pub async fn status(&self) -> NavigraphStatus {
        let ts = self.tokens.status().await;
        let (linked, vault_error) = if ts.session_live {
            (true, None)
        } else {
            match self.tokens.store().load().await {
                Ok(found) => (found.is_some(), None),
                Err(e) => {
                    tracing::warn!(error = %e, "navigraph: token store status check failed");
                    (false, Some(e.message))
                }
            }
        };
        NavigraphStatus {
            linked,
            relink_required: ts.relink_required,
            session_live: ts.session_live,
            vault_error,
        }
    }

    pub async fn unlink(&self) -> Result<()> {
        self.tokens
            .store()
            .clear()
            .await
            .map_err(NavigraphError::Store)?;
        self.tokens.clear().await;
        self.pending.lock().await.take();
        Ok(())
    }

    pub async fn bearer(&self) -> Result<Secret> {
        self.tokens.bearer().await
    }

    pub async fn tile_cookies(&self) -> Result<TileCookies> {
        self.tokens.cookies().await
    }

    pub async fn navdata_packages(&self) -> Result<Vec<NavdataPackage>> {
        let bearer = self.bearer().await?;
        tracing::debug!(scopes = ?token_scopes(&bearer), "navdata: bearer scopes");
        let packages = self.client.navdata_packages(&bearer).await?;
        tracing::info!(count = packages.len(), "navdata: packages fetched");
        for p in &packages {
            tracing::debug!(
                cycle = %p.cycle,
                revision = %p.revision,
                format = %p.format,
                status = %p.package_status,
                description = %p.description,
                files = p.files.len(),
                "navdata: package"
            );
        }
        Ok(packages)
    }

    pub async fn charts_index(&self, icao: &str) -> Result<serde_json::Value> {
        let bearer = self.bearer().await?;
        self.client.charts_index(&bearer, icao).await
    }

    pub async fn chart_bytes(&self, icao: &str, filename: &str) -> Result<Vec<u8>> {
        let bearer = self.bearer().await?;
        self.client.chart_bytes(&bearer, icao, filename).await
    }

    pub async fn tile_bytes(&self, layer: &str, z: u8, x: u32, y: u32) -> Result<Vec<u8>> {
        let cookies = self.tile_cookies().await?;
        self.client.tile_bytes(&cookies, layer, z, x, y).await
    }
}
