use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use infinity_ipc::dto::{SessionState, UserProfile};
use infinity_ipc::{IpcError, IpcErrorCode, IpcResult};
use infinity_providers::{AccessToken, IdentityProvider};
use tokio::sync::Mutex;

use crate::pkce::{Pkce, random_token};
use crate::wire::{self, Tokens};
use crate::{AuthConfig, TokenStore};

const PENDING_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Debug)]
pub struct PkceIdentity {
    cfg: AuthConfig,
    http: reqwest::Client,
    store: Arc<dyn TokenStore>,
    state: Mutex<State>,
    /// Held while a refresh is in flight, so concurrent callers wait for
    /// one refresh instead of each spending the refresh token. With
    /// rotation on, a second use of the same refresh token is a reuse and
    /// the identity provider revokes the whole session.
    refreshing: Mutex<()>,
}

#[derive(Debug, Default)]
struct State {
    pending: Option<Pending>,
    tokens: Option<Tokens>,
    profile: Option<UserProfile>,
    /// Bumped whenever the session is replaced or ended, so a refresh that
    /// started under an older session cannot write its tokens back.
    generation: u64,
}

#[derive(Debug)]
struct Pending {
    csrf: String,
    pkce: Pkce,
    started: tokio::time::Instant,
}

impl PkceIdentity {
    async fn fresh_token(&self) -> Option<AccessToken> {
        let state = self.state.lock().await;
        state
            .tokens
            .as_ref()
            .filter(|t| t.is_fresh())
            .map(|t| AccessToken::new(t.access_token.clone()))
    }

    pub fn new(cfg: AuthConfig, store: Arc<dyn TokenStore>) -> IpcResult<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| IpcError::internal(format!("http client: {e}")))?;
        Ok(Self {
            cfg,
            http,
            store,
            state: Mutex::new(State::default()),
            refreshing: Mutex::new(()),
        })
    }

    pub async fn restore(&self) -> SessionState {
        let refresh_token = match self.store.load() {
            Ok(Some(t)) => t,
            Ok(None) => return SessionState::SignedOut,
            Err(e) => {
                tracing::warn!(error = %e, "could not read stored session");
                return SessionState::SignedOut;
            }
        };
        match self
            .establish(wire::refresh(&self.http, &self.cfg, &refresh_token).await)
            .await
        {
            Ok(state) => state,
            Err(e) => {
                tracing::info!(code = %e.code, "stored session no longer valid");
                if let Err(e) = self.store.clear() {
                    tracing::warn!(error = %e, "could not clear stored session");
                }
                SessionState::SignedOut
            }
        }
    }

    async fn establish(&self, tokens: IpcResult<Tokens>) -> IpcResult<SessionState> {
        let tokens = tokens?;
        let profile = wire::userinfo(&self.http, &self.cfg, &tokens.access_token).await?;
        if let Some(rt) = &tokens.refresh_token {
            self.store.save(rt)?;
        }
        let mut state = self.state.lock().await;
        state.tokens = Some(tokens);
        state.profile = Some(profile.clone());
        state.generation += 1;
        Ok(SessionState::SignedIn { profile })
    }
}

#[async_trait]
impl IdentityProvider for PkceIdentity {
    fn is_callback(&self, url: &url::Url) -> bool {
        let expected = &self.cfg.redirect_uri;
        url.scheme() == expected.scheme()
            && url.host_str() == expected.host_str()
            && url.path() == expected.path()
    }

    async fn session(&self) -> SessionState {
        let state = self.state.lock().await;
        match (&state.profile, &state.pending) {
            (Some(profile), _) => SessionState::SignedIn {
                profile: profile.clone(),
            },
            (None, Some(p)) if p.started.elapsed() < PENDING_TTL => SessionState::Pending,
            _ => SessionState::SignedOut,
        }
    }

    async fn begin_login(&self) -> IpcResult<url::Url> {
        let pkce = Pkce::generate();
        let csrf = random_token();

        let mut url = self.cfg.endpoints.authorize.clone();
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("response_type", "code")
                .append_pair("client_id", &self.cfg.client_id)
                .append_pair("redirect_uri", self.cfg.redirect_uri.as_str())
                .append_pair("scope", &self.cfg.scope)
                .append_pair("state", &csrf)
                .append_pair("code_challenge", &pkce.challenge)
                .append_pair("code_challenge_method", "S256");
            if let Some(aud) = &self.cfg.audience {
                q.append_pair("audience", aud);
            }
        }

        self.state.lock().await.pending = Some(Pending {
            csrf,
            pkce,
            started: tokio::time::Instant::now(),
        });
        Ok(url)
    }

    async fn complete_login(&self, callback: &url::Url) -> IpcResult<SessionState> {
        let params = Callback::parse(callback)?;
        let pending =
            self.state.lock().await.pending.take().ok_or_else(|| {
                IpcError::new(IpcErrorCode::AuthRequired, "no sign-in in progress")
            })?;
        if pending.started.elapsed() >= PENDING_TTL {
            return Err(IpcError::new(
                IpcErrorCode::AuthExpired,
                "sign-in timed out",
            ));
        }
        if params.state != pending.csrf {
            return Err(IpcError::new(
                IpcErrorCode::AuthForbidden,
                "sign-in state mismatch",
            ));
        }
        let tokens =
            wire::exchange_code(&self.http, &self.cfg, &params.code, &pending.pkce.verifier).await;
        self.establish(tokens).await
    }

    async fn logout(&self) -> IpcResult<()> {
        let refresh_token = {
            let mut state = self.state.lock().await;
            let rt = state.tokens.take().and_then(|t| t.refresh_token);
            *state = State {
                generation: state.generation + 1,
                ..State::default()
            };
            rt
        };
        self.store.clear()?;
        if let Some(rt) = refresh_token
            && let Err(e) = wire::revoke(&self.http, &self.cfg, &rt).await
        {
            tracing::warn!(code = %e.code, "token revocation failed");
        }
        Ok(())
    }

    async fn access_token(&self) -> IpcResult<AccessToken> {
        if let Some(token) = self.fresh_token().await {
            return Ok(token);
        }
        let _refreshing = self.refreshing.lock().await;
        // Whoever held the gate before us may have refreshed already.
        let (generation, refresh_token) = {
            let state = self.state.lock().await;
            match &state.tokens {
                Some(t) if t.is_fresh() => return Ok(AccessToken::new(t.access_token.clone())),
                Some(t) => (state.generation, t.refresh_token.clone()),
                None => (state.generation, None),
            }
        };
        let refresh_token = refresh_token
            .ok_or_else(|| IpcError::new(IpcErrorCode::AuthRequired, "not signed in"))?;

        let tokens = wire::refresh(&self.http, &self.cfg, &refresh_token).await?;
        let mut state = self.state.lock().await;
        if state.generation != generation {
            // Signed out (or in as someone else) while refreshing: these
            // tokens belong to a session that no longer exists.
            return Err(IpcError::new(IpcErrorCode::AuthRequired, "not signed in"));
        }
        if let Some(rt) = &tokens.refresh_token {
            self.store.save(rt)?;
        }
        let access = AccessToken::new(tokens.access_token.clone());
        state.tokens = Some(tokens);
        Ok(access)
    }
}

#[derive(Debug)]
struct Callback {
    code: String,
    state: String,
}

impl Callback {
    fn parse(url: &url::Url) -> IpcResult<Self> {
        let (mut code, mut state, mut error, mut description) = (None, None, None, None);
        for (k, v) in url.query_pairs() {
            match k.as_ref() {
                "code" => code = Some(v.into_owned()),
                "state" => state = Some(v.into_owned()),
                "error" => error = Some(v.into_owned()),
                "error_description" => description = Some(v.into_owned()),
                _ => {}
            }
        }
        if let Some(error) = error {
            return Err(IpcError::new(
                IpcErrorCode::AuthForbidden,
                description.unwrap_or_else(|| error.clone()),
            )
            .with_details(serde_json::json!({ "error": error })));
        }
        match (code, state) {
            (Some(code), Some(state)) => Ok(Self { code, state }),
            _ => Err(IpcError::new(
                IpcErrorCode::AuthRequired,
                "callback is missing code or state",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Endpoints, MemoryStore};

    fn provider() -> PkceIdentity {
        let cfg = AuthConfig {
            endpoints: Endpoints::auth0("example.auth0.com").unwrap(),
            client_id: "client".into(),
            audience: Some("https://api.example.com".into()),
            redirect_uri: url::Url::parse("infinity-manager://auth/callback").unwrap(),
            scope: AuthConfig::DEFAULT_SCOPE.into(),
        };
        PkceIdentity::new(cfg, Arc::new(MemoryStore::default())).unwrap()
    }

    #[tokio::test]
    async fn begin_login_builds_pkce_url_and_goes_pending() {
        let p = provider();
        let url = p.begin_login().await.unwrap();
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(url.host_str(), Some("example.auth0.com"));
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["client_id"], "client");
        assert!(q.contains_key("state"));
        assert_eq!(p.session().await, SessionState::Pending);
    }

    #[tokio::test]
    async fn mismatched_state_is_rejected_and_consumes_pending() {
        let p = provider();
        p.begin_login().await.unwrap();
        let cb = url::Url::parse("infinity-manager://auth/callback?code=c&state=forged").unwrap();
        let err = p.complete_login(&cb).await.unwrap_err();
        assert_eq!(err.code, IpcErrorCode::AuthForbidden);
        assert_eq!(p.session().await, SessionState::SignedOut);
    }

    /// A token endpoint that counts refreshes and answers each with a new
    /// rotated refresh token, after a delay so callers overlap.
    fn token_server() -> (url::Url, Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{Read, Write};
        use std::sync::atomic::Ordering;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let url = url::Url::parse(&format!("http://{addr}/oauth/token")).unwrap();
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                std::thread::sleep(Duration::from_millis(150));
                let body = format!(
                    r#"{{"access_token":"access-{n}","refresh_token":"refresh-{n}","expires_in":3600}}"#
                );
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (url, hits)
    }

    fn expired(refresh: &str) -> Tokens {
        Tokens {
            access_token: "stale".into(),
            refresh_token: Some(refresh.into()),
            expires_at: 0,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_callers_share_one_refresh() {
        let (token_url, hits) = token_server();
        let mut p = provider();
        p.cfg.endpoints.token = token_url;
        p.state.lock().await.tokens = Some(expired("refresh-0"));
        let p = Arc::new(p);

        let calls: Vec<_> = (0..5)
            .map(|_| {
                let p = p.clone();
                tokio::spawn(async move { p.access_token().await })
            })
            .collect();
        for call in calls {
            assert_eq!(call.await.unwrap().unwrap().expose(), "access-1");
        }
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(p.store.load().unwrap().as_deref(), Some("refresh-1"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refresh_finishing_after_logout_is_discarded() {
        let (token_url, _hits) = token_server();
        let mut p = provider();
        p.cfg.endpoints.token = token_url;
        p.cfg.endpoints.revoke = None;
        p.state.lock().await.tokens = Some(expired("refresh-0"));
        let p = Arc::new(p);

        let refreshing = tokio::spawn({
            let p = p.clone();
            async move { p.access_token().await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        p.logout().await.unwrap();

        let err = refreshing.await.unwrap().unwrap_err();
        assert_eq!(err.code, IpcErrorCode::AuthRequired);
        assert!(p.state.lock().await.tokens.is_none());
        assert_eq!(p.store.load().unwrap(), None, "no token written back");
    }

    #[test]
    fn server_error_in_callback_is_forbidden() {
        let cb = url::Url::parse(
            "infinity-manager://auth/callback?error=access_denied&error_description=nope",
        )
        .unwrap();
        let err = Callback::parse(&cb).unwrap_err();
        assert_eq!(err.code, IpcErrorCode::AuthForbidden);
        assert_eq!(err.message, "nope");
    }
}
