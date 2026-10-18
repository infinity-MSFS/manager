use async_trait::async_trait;
use infinity_ipc::catalog::{Catalog, OwnershipPolicy, PackageSpec, PresignArtifact, Product};
use infinity_ipc::dto::{Channel, SessionState};
use infinity_ipc::{IpcError, IpcErrorCode, IpcResult};

use crate::{AccessToken, ContentProvider, DownloadUrl, IdentityProvider};

#[derive(Debug, Clone, Copy, Default)]
pub struct NoIdentity;

#[async_trait]
impl IdentityProvider for NoIdentity {
    fn available(&self) -> bool {
        false
    }

    async fn session(&self) -> SessionState {
        SessionState::SignedOut
    }

    async fn begin_login(&self) -> IpcResult<url::Url> {
        Err(no_accounts())
    }

    async fn complete_login(&self, _callback: &url::Url) -> IpcResult<SessionState> {
        Err(no_accounts())
    }

    async fn logout(&self) -> IpcResult<()> {
        Ok(())
    }

    async fn access_token(&self) -> IpcResult<AccessToken> {
        Err(no_accounts())
    }
}

fn no_accounts() -> IpcError {
    IpcError::unsupported("this build has no account provider")
}

#[derive(Debug, Clone)]
pub struct HttpContent {
    http: reqwest::Client,
    catalog_url: String,
    artifact_base: Option<url::Url>,
}

impl HttpContent {
    pub const CHANNEL_PLACEHOLDER: &'static str = "{channel}";

    pub fn new(catalog_url: impl Into<String>, artifact_base: Option<url::Url>) -> IpcResult<Self> {
        let catalog_url = catalog_url.into();
        let probe = catalog_url.replace(Self::CHANNEL_PLACEHOLDER, "stable");
        if !catalog_url.contains(Self::CHANNEL_PLACEHOLDER) || url::Url::parse(&probe).is_err() {
            return Err(IpcError::internal(format!(
                "catalog url {catalog_url:?} must be a URL containing {}",
                Self::CHANNEL_PLACEHOLDER
            )));
        }
        let http = reqwest::Client::builder()
            .user_agent(concat!("infinity-manager/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| IpcError::internal(format!("http client: {e}")))?;
        Ok(Self {
            http,
            catalog_url,
            artifact_base: artifact_base.map(with_trailing_slash),
        })
    }
}

#[async_trait]
impl ContentProvider for HttpContent {
    async fn catalog(&self, channel: Channel) -> IpcResult<Catalog> {
        let url = self
            .catalog_url
            .replace(Self::CHANNEL_PLACEHOLDER, channel.as_str());
        let resp = self.http.get(url).send().await.map_err(network)?;
        match resp.status() {
            s if s.is_success() => {}
            reqwest::StatusCode::NOT_FOUND => {
                return Err(IpcError::new(
                    IpcErrorCode::CdnNotFound,
                    format!("no catalog for channel {}", channel.as_str()),
                ));
            }
            s => {
                return Err(IpcError::new(
                    IpcErrorCode::NetworkUnreachable,
                    format!("catalog request failed with HTTP {s}"),
                ));
            }
        }
        resp.json::<Catalog>().await.map_err(|e| {
            IpcError::new(
                IpcErrorCode::CatalogInvalid,
                format!("catalog did not parse: {e}"),
            )
        })
    }

    async fn resolve_download(
        &self,
        product: &Product,
        artifact: PresignArtifact,
    ) -> IpcResult<DownloadUrl> {
        if !matches!(product.ownership, OwnershipPolicy::Free) {
            return Err(IpcError::new(
                IpcErrorCode::NotOwned,
                format!("{} requires the official build", product.name),
            ));
        }
        let key = artifact_key(&product.package, artifact).ok_or_else(|| {
            IpcError::new(
                IpcErrorCode::ManifestInvalid,
                format!("{} has no {artifact:?} artifact", product.id),
            )
        })?;
        let base = self
            .artifact_base
            .as_ref()
            .ok_or_else(|| IpcError::unsupported("this build has no download source configured"))?;
        Ok(DownloadUrl {
            url: join(base, key)?,
            expires_at: None,
        })
    }
}

fn artifact_key(package: &PackageSpec, artifact: PresignArtifact) -> Option<&str> {
    let (archive, manifest) = match package {
        PackageSpec::Msfs2024 {
            archive,
            manifest_key,
            ..
        }
        | PackageSpec::Msfs2020 {
            archive,
            manifest_key,
            ..
        } => (archive, manifest_key.as_deref()),
        PackageSpec::Generic { archive } => (archive, None),
    };
    match artifact {
        PresignArtifact::Archive => Some(archive.key.as_str()),
        PresignArtifact::Manifest => manifest,
    }
}

fn with_trailing_slash(mut url: url::Url) -> url::Url {
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    url
}

fn join(base: &url::Url, key: &str) -> IpcResult<url::Url> {
    let invalid = || {
        IpcError::new(
            IpcErrorCode::ManifestInvalid,
            format!("bad artifact key {key:?}"),
        )
    };
    if key.starts_with('/') || key.contains("..") || key.contains("://") {
        return Err(invalid());
    }
    let url = base.join(key).map_err(|_| invalid())?;
    if url.origin() != base.origin() || !url.path().starts_with(base.path()) {
        return Err(invalid());
    }
    Ok(url)
}

fn network(e: reqwest::Error) -> IpcError {
    let code = if e.is_timeout() {
        IpcErrorCode::CdnTimeout
    } else {
        IpcErrorCode::NetworkUnreachable
    };
    IpcError::new(code, e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> url::Url {
        url::Url::parse("https://cdn.example.com/free").unwrap()
    }

    #[test]
    fn join_stays_under_base() {
        let base = with_trailing_slash(base());
        assert_eq!(
            join(&base, "pkgs/a.zip").unwrap().as_str(),
            "https://cdn.example.com/free/pkgs/a.zip"
        );
        for bad in ["../x", "/etc", "https://evil.example/a", "//evil.example/a"] {
            assert!(join(&base, bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn catalog_url_needs_placeholder() {
        assert!(HttpContent::new("https://example.com/stable.json", None).is_err());
        assert!(HttpContent::new("https://example.com/{channel}.json", None).is_ok());
    }

    #[tokio::test]
    async fn no_identity_reports_unsupported() {
        let err = NoIdentity.begin_login().await.unwrap_err();
        assert_eq!(err.code, IpcErrorCode::Unsupported);
        assert!(!NoIdentity.available());
    }
}
