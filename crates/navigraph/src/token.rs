//! Keeps a live Navigraph access token (and tile cookies), refreshing and
//! persisting the rotated refresh token as needed.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::client::{AppCredentials, NavTokens, NavigraphClient, TileCookies};
use crate::error::{NavigraphError, Result};
use crate::secret::Secret;
use crate::store::{CredentialsSource, RefreshTokenStore};

/// Turns a refresh token into fresh tokens. [`NavigraphClient`] is the live
/// implementation; tests substitute their own.
#[async_trait]
pub trait TokenExchanger: Send + Sync + std::fmt::Debug {
    async fn exchange(&self, creds: &AppCredentials, refresh_token: &Secret) -> Result<NavTokens>;
}

#[async_trait]
impl TokenExchanger for NavigraphClient {
    async fn exchange(&self, creds: &AppCredentials, refresh_token: &Secret) -> Result<NavTokens> {
        self.refresh(creds, refresh_token).await
    }
}

#[derive(Debug)]
struct Session {
    access_token: Secret,
    cookies: TileCookies,
    fresh_until: Instant,
    refresh_token: Secret,
    /// The rotated refresh token has not reached the store yet.
    needs_putback: bool,
}

#[derive(Debug, Default)]
struct State {
    session: Option<Session>,
    relink_required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenStatus {
    pub session_live: bool,
    pub relink_required: bool,
}

/// CloudFront tile cookies live about an hour; refresh before they lapse
/// even if the access token would last longer.
const COOKIE_LIFE: Duration = Duration::from_secs(55 * 60);
const SKEW: Duration = Duration::from_secs(60);
/// Wait before re-reading the store after a rejected refresh token, in case
/// another instance rotated it concurrently.
const RETRY_DELAY: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct TokenManager {
    store: Arc<dyn RefreshTokenStore>,
    exchanger: Arc<dyn TokenExchanger>,
    credentials: Arc<dyn CredentialsSource>,
    creds_cache: Mutex<Option<AppCredentials>>,
    inner: Mutex<State>,
    retry_delay: Duration,
}

impl TokenManager {
    pub fn new(
        store: Arc<dyn RefreshTokenStore>,
        exchanger: Arc<dyn TokenExchanger>,
        credentials: Arc<dyn CredentialsSource>,
    ) -> Self {
        Self {
            store,
            exchanger,
            credentials,
            creds_cache: Mutex::new(None),
            inner: Mutex::new(State::default()),
            retry_delay: RETRY_DELAY,
        }
    }

    #[must_use]
    pub fn with_retry_delay(mut self, delay: Duration) -> Self {
        self.retry_delay = delay;
        self
    }

    pub fn store(&self) -> &Arc<dyn RefreshTokenStore> {
        &self.store
    }

    /// The app credentials, fetched from the source on first use and cached.
    /// A failed fetch is not cached.
    pub async fn credentials(&self) -> Result<AppCredentials> {
        let mut cache = self.creds_cache.lock().await;
        if let Some(c) = cache.as_ref() {
            return Ok(c.clone());
        }
        let c = self
            .credentials
            .fetch()
            .await
            .map_err(NavigraphError::Credentials)?;
        *cache = Some(c.clone());
        Ok(c)
    }

    /// Installs tokens from a fresh link whose refresh token is persisted.
    pub async fn prime(&self, tokens: NavTokens) {
        self.install(tokens, false).await;
    }

    /// Installs tokens whose refresh token could not be persisted; the next
    /// [`bearer`](Self::bearer) retries the write.
    pub async fn prime_unpersisted(&self, tokens: NavTokens) {
        self.install(tokens, true).await;
    }

    async fn install(&self, tokens: NavTokens, needs_putback: bool) {
        let mut st = self.inner.lock().await;
        st.relink_required = false;
        st.session = Some(session_from(tokens, needs_putback));
    }

    /// Forgets the in-memory session. Does not touch the store.
    pub async fn clear(&self) {
        let mut st = self.inner.lock().await;
        st.session = None;
        st.relink_required = false;
    }

    pub async fn status(&self) -> TokenStatus {
        let st = self.inner.lock().await;
        TokenStatus {
            session_live: st
                .session
                .as_ref()
                .is_some_and(|s| s.fresh_until > Instant::now()),
            relink_required: st.relink_required,
        }
    }

    pub async fn bearer(&self) -> Result<Secret> {
        self.ensure_fresh().await.map(|(b, _)| b)
    }

    pub async fn cookies(&self) -> Result<TileCookies> {
        self.ensure_fresh().await.map(|(_, c)| c)
    }

    /// Holds the state lock across the refresh so concurrent callers share
    /// one exchange instead of racing to spend the same refresh token.
    async fn ensure_fresh(&self) -> Result<(Secret, TileCookies)> {
        let mut st = self.inner.lock().await;
        if let Some(s) = st.session.as_mut()
            && s.fresh_until > Instant::now()
        {
            if s.needs_putback {
                if self.store.save(&s.refresh_token).await.is_ok() {
                    s.needs_putback = false;
                } else {
                    tracing::warn!("navigraph: token write-back still failing (will retry)");
                }
            }
            return Ok((s.access_token.clone(), s.cookies.clone()));
        }

        let (token, from_store) = match st.session.as_ref() {
            Some(s) => (s.refresh_token.clone(), false),
            None => match self.store.load().await {
                Ok(Some(t)) => (t, true),
                Ok(None) => return Err(NavigraphError::NotLinked),
                Err(e) => return Err(NavigraphError::Store(e)),
            },
        };
        let creds = self.credentials().await?;

        match self.exchanger.exchange(&creds, &token).await {
            Ok(tokens) => Ok(self.adopt(&mut st, tokens).await),
            Err(NavigraphError::RelinkRequired) if from_store => {
                // Another instance may have rotated the token under us:
                // re-read once before declaring the link dead.
                drop(st);
                tokio::time::sleep(self.retry_delay).await;
                let mut st = self.inner.lock().await;
                let retried = self.store.load().await.ok().flatten();
                if let Some(t2) = retried.filter(|t2| *t2 != token)
                    && let Ok(tokens) = self.exchanger.exchange(&creds, &t2).await
                {
                    return Ok(self.adopt(&mut st, tokens).await);
                }
                self.drop_link(&mut st).await;
                Err(NavigraphError::RelinkRequired)
            }
            Err(NavigraphError::RelinkRequired) => {
                self.drop_link(&mut st).await;
                Err(NavigraphError::RelinkRequired)
            }
            Err(e) => Err(e),
        }
    }

    async fn adopt(&self, st: &mut State, tokens: NavTokens) -> (Secret, TileCookies) {
        let mut session = session_from(tokens, true);
        if self.store.save(&session.refresh_token).await.is_ok() {
            session.needs_putback = false;
        } else {
            tracing::warn!("navigraph: token write-back failed; token held in memory");
        }
        let out = (session.access_token.clone(), session.cookies.clone());
        st.session = Some(session);
        st.relink_required = false;
        out
    }

    async fn drop_link(&self, st: &mut State) {
        if let Err(e) = self.store.clear().await {
            tracing::warn!(error = %e, "navigraph: clearing dead token failed");
        }
        st.session = None;
        st.relink_required = true;
    }
}

fn session_from(tokens: NavTokens, needs_putback: bool) -> Session {
    let access_life = Duration::from_secs(u64::from(tokens.expires_in));
    let life = access_life.min(COOKIE_LIFE).saturating_sub(SKEW);
    Session {
        access_token: tokens.access_token,
        cookies: tokens.cookies,
        fresh_until: Instant::now() + life,
        refresh_token: tokens.refresh_token,
        needs_putback,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use infinity_ipc::{IpcError, IpcResult};

    use super::*;
    use crate::store::StaticCredentials;

    #[derive(Debug)]
    struct FakeStore {
        token: StdMutex<Option<String>>,
        fail_puts: AtomicBool,
        puts: AtomicU32,
    }

    impl FakeStore {
        fn with(token: Option<&str>) -> Arc<Self> {
            Arc::new(Self {
                token: StdMutex::new(token.map(str::to_string)),
                fail_puts: AtomicBool::new(false),
                puts: AtomicU32::new(0),
            })
        }

        fn current(&self) -> Option<String> {
            self.token.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl RefreshTokenStore for FakeStore {
        async fn load(&self) -> IpcResult<Option<Secret>> {
            Ok(self.current().map(Secret::new))
        }
        async fn save(&self, t: &Secret) -> IpcResult<()> {
            if self.fail_puts.load(Ordering::SeqCst) {
                return Err(IpcError::internal("down"));
            }
            self.puts.fetch_add(1, Ordering::SeqCst);
            *self.token.lock().unwrap() = Some(t.expose().to_string());
            Ok(())
        }
        async fn clear(&self) -> IpcResult<()> {
            *self.token.lock().unwrap() = None;
            Ok(())
        }
    }

    /// Accepts only the latest refresh token and rotates it by appending `+`.
    #[derive(Debug)]
    struct FakeExchanger {
        expected: StdMutex<String>,
    }

    impl FakeExchanger {
        fn expecting(t: &str) -> Arc<Self> {
            Arc::new(Self {
                expected: StdMutex::new(t.into()),
            })
        }
    }

    #[async_trait]
    impl TokenExchanger for FakeExchanger {
        async fn exchange(
            &self,
            _creds: &AppCredentials,
            refresh_token: &Secret,
        ) -> Result<NavTokens> {
            let mut cur = self.expected.lock().unwrap();
            if refresh_token.expose() != *cur {
                return Err(NavigraphError::RelinkRequired);
            }
            let next = format!("{cur}+");
            *cur = next.clone();
            Ok(NavTokens {
                access_token: Secret::new(format!("access-for-{}", refresh_token.expose())),
                expires_in: 0, // instantly stale
                refresh_token: Secret::new(next),
                cookies: cookies(),
            })
        }
    }

    fn cookies() -> TileCookies {
        TileCookies {
            policy: "p".into(),
            signature: "s".into(),
            key_pair_id: "k".into(),
        }
    }

    fn manager(store: Arc<FakeStore>, exchanger: Arc<FakeExchanger>) -> TokenManager {
        TokenManager::new(
            store,
            exchanger,
            Arc::new(StaticCredentials(AppCredentials::new("i", "s"))),
        )
        .with_retry_delay(Duration::from_millis(1))
    }

    #[tokio::test]
    async fn rotation_persists_before_reuse() {
        let store = FakeStore::with(Some("t0"));
        let m = manager(store.clone(), FakeExchanger::expecting("t0"));

        assert_eq!(m.bearer().await.unwrap().expose(), "access-for-t0");
        assert_eq!(store.current().as_deref(), Some("t0+"));

        assert_eq!(m.bearer().await.unwrap().expose(), "access-for-t0+");
        assert_eq!(store.current().as_deref(), Some("t0++"));
        assert_eq!(store.puts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn putback_failure_keeps_serving_and_heals() {
        let store = FakeStore::with(Some("t0"));
        store.fail_puts.store(true, Ordering::SeqCst);
        let m = manager(store.clone(), FakeExchanger::expecting("t0"));

        assert_eq!(m.bearer().await.unwrap().expose(), "access-for-t0");
        assert_eq!(store.current().as_deref(), Some("t0")); // stale

        store.fail_puts.store(false, Ordering::SeqCst);
        assert_eq!(m.bearer().await.unwrap().expose(), "access-for-t0+");
        assert_eq!(store.current().as_deref(), Some("t0++"));
    }

    #[tokio::test]
    async fn dead_stored_token_requires_relink() {
        let store = FakeStore::with(Some("WRONG"));
        let m = manager(store.clone(), FakeExchanger::expecting("t0"));

        let err = m.bearer().await.unwrap_err();
        assert!(matches!(err, NavigraphError::RelinkRequired));
        assert!(store.current().is_none());
        assert!(m.status().await.relink_required);
    }

    #[tokio::test]
    async fn concurrent_rotation_is_picked_up_on_retry() {
        // The stored token was rotated by another instance between our read
        // and our exchange: the retry finds the new one and succeeds.
        #[derive(Debug)]
        struct RacingStore {
            reads: AtomicU32,
        }
        #[async_trait]
        impl RefreshTokenStore for RacingStore {
            async fn load(&self) -> IpcResult<Option<Secret>> {
                let n = self.reads.fetch_add(1, Ordering::SeqCst);
                Ok(Some(Secret::new(if n == 0 { "stale" } else { "t0" })))
            }
            async fn save(&self, _: &Secret) -> IpcResult<()> {
                Ok(())
            }
            async fn clear(&self) -> IpcResult<()> {
                Ok(())
            }
        }
        let m = TokenManager::new(
            Arc::new(RacingStore {
                reads: AtomicU32::new(0),
            }),
            FakeExchanger::expecting("t0"),
            Arc::new(StaticCredentials(AppCredentials::new("i", "s"))),
        )
        .with_retry_delay(Duration::from_millis(1));
        assert_eq!(m.bearer().await.unwrap().expose(), "access-for-t0");
        assert!(!m.status().await.relink_required);
    }

    #[tokio::test]
    async fn not_linked_when_store_empty() {
        let m = manager(FakeStore::with(None), FakeExchanger::expecting("t0"));
        assert!(matches!(
            m.bearer().await.unwrap_err(),
            NavigraphError::NotLinked
        ));
    }

    #[tokio::test]
    async fn prime_clears_relink_and_serves() {
        let m = manager(FakeStore::with(None), FakeExchanger::expecting("x"));
        m.prime(NavTokens {
            access_token: "primed".into(),
            expires_in: 3600,
            refresh_token: "rt".into(),
            cookies: cookies(),
        })
        .await;
        assert_eq!(m.bearer().await.unwrap().expose(), "primed");
        assert_eq!(m.cookies().await.unwrap(), cookies());
        assert!(m.status().await.session_live);
    }

    #[tokio::test]
    async fn unpersisted_prime_writes_back_on_next_use() {
        let store = FakeStore::with(None);
        let m = manager(store.clone(), FakeExchanger::expecting("x"));
        m.prime_unpersisted(NavTokens {
            access_token: "primed".into(),
            expires_in: 3600,
            refresh_token: "rt".into(),
            cookies: cookies(),
        })
        .await;
        assert!(store.current().is_none());
        m.bearer().await.unwrap();
        assert_eq!(store.current().as_deref(), Some("rt"));
    }

    #[tokio::test]
    async fn credentials_fetched_once() {
        #[derive(Debug, Default)]
        struct Counting(AtomicU32);
        #[async_trait]
        impl CredentialsSource for Counting {
            async fn fetch(&self) -> IpcResult<AppCredentials> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(AppCredentials::new("i", "s"))
            }
        }
        let source = Arc::new(Counting::default());
        let m = TokenManager::new(
            FakeStore::with(Some("t0")),
            FakeExchanger::expecting("t0"),
            source.clone(),
        );
        m.bearer().await.unwrap();
        m.bearer().await.unwrap();
        m.credentials().await.unwrap();
        assert_eq!(source.0.load(Ordering::SeqCst), 1);
    }
}
