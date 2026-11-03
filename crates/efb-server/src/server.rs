use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::{Sink, SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::{Mutex, RwLock, broadcast, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tower_http::services::{ServeDir, ServeFile};
use tracing::{debug, info, warn};

use crate::dto::{EfbConnectedClient, EfbStatus};
use crate::error::EfbError;
use crate::pairing::{
    PAIRING_CODE_TTL_SECS, PairingCodes, PairingSecret, now_unix, sign_pairing_token,
    verify_pairing_token,
};
use crate::protocol::{ClientId, Incoming, Outgoing, close_code};
use crate::providers::Providers;
use crate::state::EfbState;
use crate::{cmd, http};

/// v1 used 6970 and early v3 9012, which collides with ASUS Armoury Crate's
/// loopback server: it binds `127.0.0.1:9012`, out-races a `0.0.0.0` bind for
/// loopback traffic and drops the connection. 28012 is clear of the usual
/// desktop-utility squatters.
pub const DEFAULT_PORT: u16 = 28012;

/// Loopback only. Exposing the EFB to the LAN is the user's explicit choice
/// (`0.0.0.0:28012`).
pub fn default_bind() -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, DEFAULT_PORT))
}

#[derive(Debug)]
pub struct EfbConfig {
    pub bind: SocketAddr,
    pub pairing_secret: PairingSecret,
    /// The mirror SPA. Missing or `None` leaves the server WS-only.
    pub static_dir: Option<PathBuf>,
    /// Let loopback peers (the in-sim panel) in without a token.
    pub loopback_bypass: bool,
    pub providers: Providers,
    /// A client that sends nothing, pings included, for this long is closed
    /// with 4408. Clients ping every 20 s.
    pub idle_timeout: Duration,
    /// Bounds [`EfbServer::stop`] so a wedged provider cannot hold up the
    /// manager's exit.
    pub shutdown_timeout: Duration,
}

impl EfbConfig {
    pub fn new(pairing_secret: PairingSecret) -> Self {
        Self {
            bind: default_bind(),
            pairing_secret,
            static_dir: None,
            loopback_bypass: true,
            providers: Providers::default(),
            idle_timeout: Duration::from_secs(60),
            shutdown_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Debug)]
struct ClientEntry {
    device: String,
    addr: SocketAddr,
    connected_at: Instant,
    loopback: bool,
    kick: Option<oneshot::Sender<()>>,
}

/// State that outlives a single start/stop, shared with request handlers.
#[derive(Debug)]
pub(crate) struct Shared {
    pub(crate) state: EfbState,
    secret: RwLock<Option<Arc<PairingSecret>>>,
    clients: Mutex<HashMap<ClientId, ClientEntry>>,
    pub(crate) codes: Mutex<PairingCodes>,
    next_client: AtomicU64,
    changes: watch::Sender<u64>,
}

impl Shared {
    fn changed(&self) {
        self.changes.send_modify(|n| *n = n.wrapping_add(1));
    }
}

/// What one `start` spun up.
#[derive(Debug)]
struct Running {
    bind: SocketAddr,
    token: CancellationToken,
    tasks: TaskTracker,
    serve: JoinHandle<()>,
    providers: Providers,
    shutdown_timeout: Duration,
}

/// Request-handler context.
#[derive(Debug, Clone)]
pub(crate) struct Ctx {
    pub(crate) shared: Arc<Shared>,
    pub(crate) providers: Providers,
    loopback_bypass: bool,
    idle_timeout: Duration,
    token: CancellationToken,
    tasks: TaskTracker,
}

impl Ctx {
    /// Tracked, so `stop` waits for in-flight provider work.
    pub(crate) fn spawn<F>(&self, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.tasks.spawn(fut);
    }

    /// The device a request speaks for, or `None` to refuse it. A valid
    /// token names the device; a loopback peer under the bypass may instead
    /// pass a plain label (anything without `|`), or nothing.
    pub(crate) async fn authorize(&self, peer: SocketAddr, token: Option<&str>) -> Option<String> {
        if let Some(token) = token {
            let secret = self.shared.secret.read().await.clone();
            if let Some(device) = secret.and_then(|s| verify_pairing_token(&s, token, now_unix())) {
                return Some(device);
            }
        }
        if !(self.loopback_bypass && peer.ip().to_canonical().is_loopback()) {
            return None;
        }
        // Never echo a rejected token into the client list.
        let label = token.filter(|t| !t.is_empty() && !t.contains('|') && t.len() <= 64);
        Some(label.unwrap_or("loopback").to_owned())
    }
}

/// The EFB server. One per app; holds the state and pairing across restarts.
#[derive(Debug)]
pub struct EfbServer {
    shared: Arc<Shared>,
    running: Mutex<Option<Running>>,
}

impl Default for EfbServer {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for EfbServer {
    fn drop(&mut self) {
        if let Some(running) = self.running.get_mut().take() {
            running.token.cancel();
        }
    }
}

impl EfbServer {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                state: EfbState::new(),
                secret: RwLock::new(None),
                clients: Mutex::new(HashMap::new()),
                codes: Mutex::new(PairingCodes::default()),
                next_client: AtomicU64::new(1),
                changes: watch::Sender::new(0),
            }),
            running: Mutex::new(None),
        }
    }

    /// Valid before `start`, so providers that push can be built over it.
    pub fn state(&self) -> EfbState {
        self.shared.state.clone()
    }

    /// Ticks on start, stop, connect and disconnect; the UI refetches
    /// [`Self::status`].
    pub fn subscribe_changes(&self) -> watch::Receiver<u64> {
        self.shared.changes.subscribe()
    }

    pub async fn is_running(&self) -> bool {
        self.running.lock().await.is_some()
    }

    pub async fn bind_addr(&self) -> Option<SocketAddr> {
        self.running.lock().await.as_ref().map(|r| r.bind)
    }

    pub async fn status(&self) -> EfbStatus {
        let bind_addr = self.bind_addr().await;
        EfbStatus {
            running: bind_addr.is_some(),
            bind_addr: bind_addr.map(|a| a.to_string()),
            clients: self.connected_clients().await,
        }
    }

    /// Lets codes be minted before the server starts. Tokens already issued
    /// under a different secret stop verifying.
    pub async fn set_secret(&self, secret: PairingSecret) {
        *self.shared.secret.write().await = Some(Arc::new(secret));
    }

    /// "Forget all devices": switch to `secret`, drop unredeemed codes and
    /// close every non-loopback socket. Persist `secret` first, or the
    /// rotation is undone by the next launch.
    pub async fn rotate_secret(&self, secret: PairingSecret) {
        self.set_secret(secret).await;
        self.shared.codes.lock().await.clear();
        let mut kicked = 0usize;
        for entry in self.shared.clients.lock().await.values_mut() {
            if !entry.loopback
                && let Some(kick) = entry.kick.take()
            {
                let _ = kick.send(());
                kicked += 1;
            }
        }
        info!(kicked, "efb-server: rotated pairing secret");
    }

    pub async fn mint_token(&self, device_id: &str, ttl_secs: u64) -> Result<String, EfbError> {
        let secret = self
            .shared
            .secret
            .read()
            .await
            .clone()
            .ok_or(EfbError::NoSecret)?;
        sign_pairing_token(&secret, device_id, now_unix().saturating_add(ttl_secs))
    }

    /// A short code redeemable once at `GET /pair?code=` for a token valid
    /// `token_ttl_secs`. The code itself expires after
    /// [`PAIRING_CODE_TTL_SECS`].
    pub async fn mint_pairing_code(
        &self,
        device_id: &str,
        token_ttl_secs: u64,
    ) -> Result<String, EfbError> {
        let token = self.mint_token(device_id, token_ttl_secs).await?;
        Ok(self
            .shared
            .codes
            .lock()
            .await
            .insert(token, Duration::from_secs(PAIRING_CODE_TTL_SECS)))
    }

    /// Closes one client (4001). `false` if it was already gone.
    pub async fn disconnect_client(&self, client_id: ClientId) -> bool {
        self.shared
            .clients
            .lock()
            .await
            .get_mut(&client_id)
            .and_then(|e| e.kick.take())
            .is_some_and(|kick| kick.send(()).is_ok())
    }

    pub async fn connected_clients(&self) -> Vec<EfbConnectedClient> {
        let now = Instant::now();
        let mut out: Vec<EfbConnectedClient> = self
            .shared
            .clients
            .lock()
            .await
            .iter()
            .map(|(id, e)| EfbConnectedClient {
                client_id: *id,
                device: e.device.clone(),
                addr: e.addr.to_string(),
                age_secs: now.saturating_duration_since(e.connected_at).as_secs(),
                loopback: e.loopback,
            })
            .collect();
        out.sort_by_key(|c| c.client_id);
        out
    }

    /// Binds and serves. Returns the bound address (useful with port 0).
    pub async fn start(&self, config: EfbConfig) -> Result<SocketAddr, EfbError> {
        let mut running = self.running.lock().await;
        if running.is_some() {
            return Err(EfbError::AlreadyRunning);
        }
        let listener = tokio::net::TcpListener::bind(config.bind)
            .await
            .map_err(|source| EfbError::Bind {
                addr: config.bind,
                source,
            })?;
        let bind = listener.local_addr().map_err(|source| EfbError::Bind {
            addr: config.bind,
            source,
        })?;
        self.set_secret(config.pairing_secret).await;

        let token = CancellationToken::new();
        let tasks = TaskTracker::new();
        let ctx = Ctx {
            shared: self.shared.clone(),
            providers: config.providers.clone(),
            loopback_bypass: config.loopback_bypass,
            idle_timeout: config.idle_timeout,
            token: token.clone(),
            tasks: tasks.clone(),
        };
        let app = router(ctx, config.static_dir.as_deref());

        let serve = {
            let stop = token.clone().cancelled_owned();
            tokio::spawn(async move {
                let app = app.into_make_service_with_connect_info::<SocketAddr>();
                if let Err(e) = axum::serve(listener, app)
                    .with_graceful_shutdown(stop)
                    .await
                {
                    warn!(error = %e, "efb-server: serve loop ended");
                }
            })
        };
        info!(%bind, "efb-server: listening");
        *running = Some(Running {
            bind,
            token,
            tasks,
            serve,
            providers: config.providers,
            shutdown_timeout: config.shutdown_timeout,
        });
        drop(running);
        self.shared.changed();
        Ok(bind)
    }

    /// Closes every socket (1001), waits for in-flight work, then lets the
    /// browser provider release its engines. State and pairing survive.
    pub async fn stop(&self) -> Result<(), EfbError> {
        let r = self
            .running
            .lock()
            .await
            .take()
            .ok_or(EfbError::NotRunning)?;
        r.token.cancel();
        r.tasks.close();
        let drained = tokio::time::timeout(r.shutdown_timeout, async {
            let _ = r.serve.await;
            r.tasks.wait().await;
        })
        .await;
        if drained.is_err() {
            warn!("efb-server: shutdown timed out; abandoning in-flight work");
        }
        // After the socket is down, so nothing opens a session in the gap.
        if let Some(browser) = r.providers.browser {
            let _ = tokio::time::timeout(r.shutdown_timeout, browser.shutdown()).await;
        }
        self.shared.changed();
        info!("efb-server: stopped");
        Ok(())
    }
}

fn router(ctx: Ctx, static_dir: Option<&std::path::Path>) -> Router {
    let app = Router::new()
        .route("/health", get(http::health))
        .route("/ws", get(ws_upgrade))
        .route("/pair", get(http::pair_redeem))
        .route("/efb/doc/{id}/page/{page}", get(http::doc_page))
        .route("/efb/nav/tile/{layer}/{z}/{x}/{y}", get(http::nav_tile))
        .route("/efb/nav/chart/{icao}/{file}", get(http::nav_chart))
        .route("/efb/browser/{session}/frame.jpg", get(http::browser_frame))
        .with_state(ctx);
    match static_dir {
        Some(dir) if dir.is_dir() => {
            info!(dir = %dir.display(), "efb-server: serving static SPA");
            // Unknown paths get index.html: the SPA routes client-side.
            let spa = ServeDir::new(dir).fallback(ServeFile::new(dir.join("index.html")));
            app.fallback_service(spa)
        }
        Some(dir) => {
            warn!(dir = %dir.display(), "efb-server: static_dir missing; WS-only");
            app
        }
        None => app,
    }
}

#[derive(Deserialize)]
struct WsParams {
    token: Option<String>,
}

/// Refused before the upgrade with a plain 401, as v3 did, so a client sees
/// it as a failed connect rather than a close code.
async fn ws_upgrade(
    ws: WebSocketUpgrade,
    Query(params): Query<WsParams>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(ctx): State<Ctx>,
) -> Response {
    let Some(device) = ctx.authorize(peer, params.token.as_deref()).await else {
        let why = if params.token.is_some() {
            "invalid token"
        } else {
            "missing token"
        };
        return (StatusCode::UNAUTHORIZED, why).into_response();
    };
    let id = ctx.shared.next_client.fetch_add(1, Ordering::Relaxed);
    let (kick, kicked) = oneshot::channel();
    let entry = ClientEntry {
        device: device.clone(),
        addr: peer,
        connected_at: Instant::now(),
        loopback: peer.ip().to_canonical().is_loopback(),
        kick: Some(kick),
    };
    debug!(id, device, %peer, "efb-server: ws upgrade accepted");
    let tasks = ctx.tasks.clone();
    ws.on_upgrade(move |socket| {
        tasks.track_future(async move {
            ctx.shared.clients.lock().await.insert(id, entry);
            ctx.shared.changed();
            client_loop(&ctx, socket, id, kicked).await;
            ctx.shared.clients.lock().await.remove(&id);
            ctx.shared.changed();
            debug!(id, device, "efb-server: client gone");
        })
    })
}

/// Highest seq per channel already delivered, so deltas that raced the
/// snapshot are not replayed after it.
#[derive(Debug, Default)]
struct Seen {
    sim: u64,
    ui: u64,
}

impl Seen {
    fn from_snapshot(frame: &Outgoing) -> Self {
        match frame {
            Outgoing::Snapshot {
                sim_seq, ui_seq, ..
            } => Self {
                sim: *sim_seq,
                ui: *ui_seq,
            },
            _ => Self::default(),
        }
    }

    fn is_stale(&self, frame: &Outgoing) -> bool {
        match frame {
            Outgoing::Sim { seq, .. } => *seq <= self.sim,
            Outgoing::Ui { seq, .. } => *seq <= self.ui,
            _ => false,
        }
    }
}

async fn client_loop(
    ctx: &Ctx,
    socket: WebSocket,
    id: ClientId,
    mut kicked: oneshot::Receiver<()>,
) {
    let state = &ctx.shared.state;
    let (mut sink, mut stream) = socket.split();
    // Subscribe before the snapshot so nothing falls in between.
    let mut bus = state.subscribe();
    let snap = state.snapshot_frame().await;
    let mut seen = Seen::from_snapshot(&snap);
    if !send(&mut sink, &snap).await {
        return;
    }

    let idle = tokio::time::sleep(ctx.idle_timeout);
    tokio::pin!(idle);

    let close = loop {
        tokio::select! {
            msg = stream.next() => {
                let Some(Ok(msg)) = msg else { break None };
                idle.as_mut().reset(tokio::time::Instant::now() + ctx.idle_timeout);
                match msg {
                    Message::Text(text) => {
                        let Ok(frame) = serde_json::from_str::<Incoming>(&text) else {
                            debug!(id, "efb-server: bad frame");
                            continue;
                        };
                        let reply = match frame {
                            Incoming::Sim { data } => { state.apply_sim_delta(data).await; None }
                            Incoming::Ui { data } => { state.apply_ui_delta(data, Some(id)).await; None }
                            Incoming::Cmd { name, data } => { cmd::handle(ctx, id, name, data); None }
                            Incoming::Resync => {
                                let snap = state.snapshot_frame().await;
                                seen = Seen::from_snapshot(&snap);
                                Some(snap)
                            }
                            Incoming::Ping => Some(Outgoing::Pong),
                        };
                        if let Some(reply) = reply && !send(&mut sink, &reply).await {
                            break None;
                        }
                    }
                    Message::Ping(p) => {
                        let _ = sink.send(Message::Pong(p)).await;
                    }
                    Message::Close(_) => break None,
                    Message::Pong(_) | Message::Binary(_) => {}
                }
            }
            recv = bus.recv() => match recv {
                Ok(b) if b.is_for(id) && !seen.is_stale(&b.frame) => {
                    if !send(&mut sink, &b.frame).await {
                        break None;
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(id, lagged = n, "efb-server: client lagged; resyncing");
                    let snap = state.snapshot_frame().await;
                    seen = Seen::from_snapshot(&snap);
                    if !send(&mut sink, &snap).await {
                        break None;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break None,
            },
            () = &mut idle => break Some((close_code::IDLE, "idle")),
            _ = &mut kicked => break Some((close_code::FORGOTTEN, "forgotten")),
            () = ctx.token.cancelled() => break Some((close_code::GOING_AWAY, "server stopping")),
        }
    };
    if let Some((code, reason)) = close {
        let frame = CloseFrame {
            code,
            reason: Utf8Bytes::from_static(reason),
        };
        let _ = sink.send(Message::Close(Some(frame))).await;
    }
}

async fn send<S>(sink: &mut S, frame: &Outgoing) -> bool
where
    S: Sink<Message> + Unpin,
{
    match serde_json::to_string(frame) {
        Ok(text) => sink.send(Message::Text(text.into())).await.is_ok(),
        Err(e) => {
            warn!(error = %e, "efb-server: frame serialization failed");
            true
        }
    }
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
