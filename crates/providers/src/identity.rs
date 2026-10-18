use async_trait::async_trait;
use infinity_ipc::IpcResult;
use infinity_ipc::dto::SessionState;

#[async_trait]
pub trait IdentityProvider: Send + Sync + std::fmt::Debug {
    fn available(&self) -> bool {
        true
    }

    fn is_callback(&self, _url: &url::Url) -> bool {
        false
    }
    async fn session(&self) -> SessionState;
    async fn begin_login(&self) -> IpcResult<url::Url>;
    async fn complete_login(&self, callback: &url::Url) -> IpcResult<SessionState>;
    async fn logout(&self) -> IpcResult<()>;
    async fn access_token(&self) -> IpcResult<AccessToken>;
}

#[derive(Clone)]
pub struct AccessToken(String);

impl AccessToken {
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccessToken(<redacted>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_the_token() {
        let token = AccessToken::new("eyJsecret");
        assert!(!format!("{token:?}").contains("eyJsecret"));
    }
}
