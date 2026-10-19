use infinity_ipc::{IpcError, IpcResult};

#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub endpoints: Endpoints,
    pub client_id: String,
    pub audience: Option<String>,
    pub redirect_uri: url::Url,
    pub scope: String,
}

impl AuthConfig {
    pub const DEFAULT_SCOPE: &'static str = "openid profile email offline_access";
}

#[derive(Debug, Clone)]
pub struct Endpoints {
    pub authorize: url::Url,
    pub token: url::Url,
    pub userinfo: url::Url,
    pub revoke: Option<url::Url>,
}

impl Endpoints {
    pub fn auth0(domain: &str) -> IpcResult<Self> {
        let base = url::Url::parse(&format!("https://{domain}/"))
            .map_err(|e| IpcError::internal(format!("bad auth domain {domain:?}: {e}")))?;
        let at = |path: &str| {
            base.join(path)
                .map_err(|e| IpcError::internal(format!("bad auth endpoint {path}: {e}")))
        };
        Ok(Self {
            authorize: at("authorize")?,
            token: at("oauth/token")?,
            userinfo: at("userinfo")?,
            revoke: Some(at("oauth/revoke")?),
        })
    }
}
