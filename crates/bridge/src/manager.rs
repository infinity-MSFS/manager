use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use infinity_bridge_host::{BridgeServer, ServerConfig};
use infinity_ipc::catalog::BridgeSpec;
use serde_json::Value;
use tokio::sync::{Mutex, broadcast};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::error::BridgeError;
use crate::shutdown::CancellableListener;

/// Per-attempt command timeout.
///
/// Must stay above the relay's own `requestTimeoutMs` (6 s in
/// `coherent/bridge/Main.res`). The relay knows *why* a command went
/// unanswered (module absent, CommBus unbound, handler refused) and says so
/// in its ack; a host that gives up first throws that away and every cause
/// collapses into one `TIMEOUT`.
pub const DEFAULT_CMD_TIMEOUT: Duration = Duration::from_millis(8_000);

/// One retry covers a socket that dropped between the connection check and
/// the send; three covers a gauge that is still coming up.
pub const DEFAULT_CMD_ATTEMPTS: u32 = 3;

/// Tuning shared by every bridge a manager starts.
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    /// Loopback by default: gauges run on this machine, and nothing else
    /// should be able to drive aircraft systems.
    pub bind_ip: IpAddr,
    pub event_capacity: usize,
    /// Keepalive cadence. The reaper is what notices a half-open socket.
    /// Ten seconds between pings with a thirty second silence budget survives
    /// a loading-screen stall (the sim blocks the relay's thread during
    /// loads) while still clearing a dead connection before a user notices.
    pub ping_interval: Duration,
    pub ping_timeout: Duration,
    /// Delay before the first command retry; doubles per attempt.
    pub retry_backoff: Duration,
    /// How long [`BridgeManager::stop`] waits for the server to wind down.
    pub shutdown_timeout: Duration,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            bind_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            event_capacity: 65_536,
            ping_interval: Duration::from_secs(10),
            ping_timeout: Duration::from_secs(30),
            retry_backoff: Duration::from_millis(250),
            shutdown_timeout: Duration::from_secs(5),
        }
    }
}

/// One event from a gauge, tagged with the bridge it arrived on.
#[derive(Debug, Clone, PartialEq)]
pub struct BridgeEvent {
    pub namespace: String,
    pub name: String,
    pub data: Value,
}

impl BridgeEvent {
    /// The UI channel this event is forwarded on, `bridge:<namespace>:<name>`.
    ///
    /// `None` for names a Tauri event cannot carry (anything but
    /// alphanumerics, `-`, `/`, `:`, `_`). Those are the dotted internal
    /// streams (`terrain.*`, `flir.*`) that run at up to 15 Hz and that the UI
    /// never consumes; forwarding them only produced warning spam.
    pub fn channel(&self) -> Option<String> {
        let valid = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '/' | ':' | '_'))
        };
        (valid(&self.namespace) && valid(&self.name))
            .then(|| format!("bridge:{}:{}", self.namespace, self.name))
    }
}

pub use infinity_ipc::dto::{BridgeStatus, ConnectionChange};

struct Entry {
    spec: BridgeSpec,
    server: BridgeServer,
    addr: SocketAddr,
    token: CancellationToken,
    serve: JoinHandle<()>,
    pumps: [JoinHandle<()>; 2],
}

impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entry")
            .field("spec", &self.spec)
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct Inner {
    config: BridgeConfig,
    bridges: Mutex<HashMap<String, Entry>>,
    events: broadcast::Sender<BridgeEvent>,
    status: broadcast::Sender<ConnectionChange>,
}

impl Drop for Inner {
    /// The last handle going away takes the servers with it rather than
    /// leaving them bound with nobody to stop them.
    fn drop(&mut self) {
        for entry in self.bridges.get_mut().values() {
            entry.token.cancel();
        }
    }
}

/// Owns every running bridge, keyed by namespace. Cheap to clone.
#[derive(Debug, Clone)]
pub struct BridgeManager {
    inner: Arc<Inner>,
}

impl Default for BridgeManager {
    fn default() -> Self {
        Self::new(BridgeConfig::default())
    }
}

impl BridgeManager {
    pub fn new(config: BridgeConfig) -> Self {
        let (events, _) = broadcast::channel(config.event_capacity.max(1));
        let (status, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(Inner {
                config,
                bridges: Mutex::new(HashMap::new()),
                events,
                status,
            }),
        }
    }

    /// Gauge events from every bridge, merged.
    pub fn subscribe(&self) -> broadcast::Receiver<BridgeEvent> {
        self.inner.events.subscribe()
    }

    /// Connection transitions from every bridge, merged.
    pub fn subscribe_status(&self) -> broadcast::Receiver<ConnectionChange> {
        self.inner.status.subscribe()
    }

    /// Starts serving `spec` on `<bind_ip>:<port>/<namespace>` and returns the
    /// bound address (useful with port 0).
    pub async fn start(&self, spec: BridgeSpec) -> Result<SocketAddr, BridgeError> {
        let mut bridges = self.inner.bridges.lock().await;
        if bridges.contains_key(&spec.namespace) {
            return Err(BridgeError::AlreadyRunning(spec.namespace));
        }
        let cfg = &self.inner.config;
        let wanted = SocketAddr::new(cfg.bind_ip, spec.port);
        let bind_err = |source| BridgeError::Bind {
            namespace: spec.namespace.clone(),
            addr: wanted,
            source,
        };
        let listener = tokio::net::TcpListener::bind(wanted)
            .await
            .map_err(bind_err)?;
        let addr = listener.local_addr().map_err(bind_err)?;

        let path = format!("/{}", spec.namespace);
        let mut host_cfg = ServerConfig::new(addr.to_string(), path.as_str());
        host_cfg.event_capacity = cfg.event_capacity.max(1);
        host_cfg.ping_interval = cfg.ping_interval;
        host_cfg.ping_timeout = cfg.ping_timeout;
        let (server, router) = BridgeServer::router(host_cfg);

        let token = CancellationToken::new();
        let serve = {
            let listener = CancellableListener::new(listener, token.clone());
            let stop = token.clone().cancelled_owned();
            let namespace = spec.namespace.clone();
            tokio::spawn(async move {
                if let Err(e) = axum::serve(listener, router)
                    .with_graceful_shutdown(stop)
                    .await
                {
                    warn!(namespace, error = %e, "bridge: serve loop ended");
                }
            })
        };
        let pumps = [
            self.spawn_event_pump(&spec.namespace, &server, token.clone()),
            self.spawn_status_pump(&spec.namespace, &server, token.clone()),
        ];

        info!(namespace = %spec.namespace, %addr, path, "bridge: started");
        bridges.insert(
            spec.namespace.clone(),
            Entry {
                spec,
                server,
                addr,
                token,
                serve,
                pumps,
            },
        );
        Ok(addr)
    }

    /// [`Self::start`], treating an already running namespace as success.
    pub async fn ensure_started(&self, spec: BridgeSpec) -> Result<(), BridgeError> {
        match self.start(spec).await {
            Ok(_) | Err(BridgeError::AlreadyRunning(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn spawn_event_pump(
        &self,
        namespace: &str,
        server: &BridgeServer,
        token: CancellationToken,
    ) -> JoinHandle<()> {
        let mut rx = server.subscribe_events();
        let tx = self.inner.events.clone();
        let namespace = namespace.to_owned();
        tokio::spawn(async move {
            loop {
                let evt = tokio::select! {
                    () = token.cancelled() => break,
                    evt = rx.recv() => evt,
                };
                match evt {
                    Ok(evt) => {
                        // No subscriber is not an error.
                        let _ = tx.send(BridgeEvent {
                            namespace: namespace.clone(),
                            name: evt.name,
                            data: evt.data,
                        });
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(namespace, dropped = n, "bridge: event pump lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    }

    fn spawn_status_pump(
        &self,
        namespace: &str,
        server: &BridgeServer,
        token: CancellationToken,
    ) -> JoinHandle<()> {
        let mut rx = server.connection_status();
        let tx = self.inner.status.clone();
        let namespace = namespace.to_owned();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = token.cancelled() => break,
                    changed = rx.changed() => if changed.is_err() { break },
                }
                let connected = *rx.borrow_and_update();
                let _ = tx.send(ConnectionChange {
                    namespace: namespace.clone(),
                    connected,
                });
            }
        })
    }

    /// Stops one bridge: closes the listener and every gauge socket on it, so
    /// the gauge's reconnect timer finds the next server on that port.
    pub async fn stop(&self, namespace: &str) -> Result<(), BridgeError> {
        let entry = self
            .inner
            .bridges
            .lock()
            .await
            .remove(namespace)
            .ok_or_else(|| BridgeError::NotFound(namespace.to_owned()))?;
        self.shutdown(entry).await;
        Ok(())
    }

    pub async fn stop_all(&self) {
        let entries: Vec<Entry> = self
            .inner
            .bridges
            .lock()
            .await
            .drain()
            .map(|e| e.1)
            .collect();
        for entry in entries {
            self.shutdown(entry).await;
        }
    }

    async fn shutdown(&self, entry: Entry) {
        let was_connected = entry.server.is_connected().await;
        entry.token.cancel();
        if tokio::time::timeout(self.inner.config.shutdown_timeout, entry.serve)
            .await
            .is_err()
        {
            warn!(namespace = %entry.spec.namespace, "bridge: server did not stop in time");
        }
        for pump in entry.pumps {
            pump.abort();
        }
        if was_connected {
            let _ = self.inner.status.send(ConnectionChange {
                namespace: entry.spec.namespace.clone(),
                connected: false,
            });
        }
        info!(namespace = %entry.spec.namespace, "bridge: stopped");
    }

    async fn server(&self, namespace: &str) -> Result<BridgeServer, BridgeError> {
        self.inner
            .bridges
            .lock()
            .await
            .get(namespace)
            .map(|e| e.server.clone())
            .ok_or_else(|| BridgeError::NotFound(namespace.to_owned()))
    }

    pub async fn is_connected(&self, namespace: &str) -> Result<bool, BridgeError> {
        Ok(self.server(namespace).await?.is_connected().await)
    }

    /// Whether a connected relay has reported the module behind it reachable.
    ///
    /// [`Self::is_connected`] only proves the relay's socket is open, which
    /// it is from the moment the panel loads, long before the aircraft's WASM
    /// exists. A UI that gates on connection alone fires its first query into
    /// that gap and shows a timeout for a bridge that is merely not ready yet.
    pub async fn is_ready(&self, namespace: &str) -> Result<bool, BridgeError> {
        Ok(self.server(namespace).await?.is_ready().await)
    }

    pub async fn local_addr(&self, namespace: &str) -> Result<SocketAddr, BridgeError> {
        self.inner
            .bridges
            .lock()
            .await
            .get(namespace)
            .map(|e| e.addr)
            .ok_or_else(|| BridgeError::NotFound(namespace.to_owned()))
    }

    pub async fn list(&self) -> Vec<BridgeSpec> {
        let bridges = self.inner.bridges.lock().await;
        let mut specs: Vec<BridgeSpec> = bridges.values().map(|e| e.spec.clone()).collect();
        specs.sort_by(|a, b| a.namespace.cmp(&b.namespace));
        specs
    }

    pub async fn statuses(&self) -> Vec<BridgeStatus> {
        let entries: Vec<(BridgeSpec, BridgeServer)> = {
            let bridges = self.inner.bridges.lock().await;
            bridges
                .values()
                .map(|e| (e.spec.clone(), e.server.clone()))
                .collect()
        };
        let mut out = Vec::with_capacity(entries.len());
        for (spec, server) in entries {
            out.push(BridgeStatus {
                id: spec.id,
                namespace: spec.namespace,
                port: spec.port,
                connected: server.is_connected().await,
                ready: server.is_ready().await,
            });
        }
        out.sort_by(|a, b| a.namespace.cmp(&b.namespace));
        out
    }

    /// Fire-and-forget event to every gauge on the bridge.
    pub async fn emit(&self, namespace: &str, name: &str, data: Value) -> Result<(), BridgeError> {
        self.server(namespace)
            .await?
            .emit(name, data)
            .await
            .map_err(|e| BridgeError::from_wire(&e))
    }

    /// Request/response command with [`DEFAULT_CMD_ATTEMPTS`].
    pub async fn command(
        &self,
        namespace: &str,
        name: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<Value, BridgeError> {
        self.command_with_attempts(namespace, name, payload, timeout, DEFAULT_CMD_ATTEMPTS)
            .await
    }

    /// Sends `name` and waits for the first ack, replaying only outcomes
    /// [`BridgeError::is_retryable`] accepts; a refusal comes back as-is.
    ///
    /// Retries assume the command is idempotent, which the DC-10 equipment
    /// protocol's are (`equip_query` reads; `equip_cmd` selects within a
    /// radio group). Anything else must pass `attempts = 1`. Clamped to >= 1.
    pub async fn command_with_attempts(
        &self,
        namespace: &str,
        name: &str,
        payload: Value,
        timeout: Duration,
        attempts: u32,
    ) -> Result<Value, BridgeError> {
        let attempts = attempts.max(1);
        let mut backoff = self.inner.config.retry_backoff;
        let mut attempt = 1;
        loop {
            // Re-resolved every attempt: a bridge restarted in between would
            // otherwise be addressed through a server nothing is connected to.
            let server = self.server(namespace).await?;
            let result = if server.is_connected().await {
                server
                    .command(name, payload.clone(), timeout)
                    .await
                    .map_err(|e| BridgeError::from_wire(&e))
            } else {
                Err(BridgeError::NotConnected(namespace.to_owned()))
            };
            let err = match result {
                Ok(v) => return Ok(v),
                Err(e) => e,
            };
            if !err.is_retryable() || attempt >= attempts {
                return Err(err);
            }
            warn!(namespace, command = name, attempt, of = attempts, error = %err, "bridge: command failed, retrying");
            tokio::time::sleep(backoff).await;
            backoff = backoff.saturating_mul(2);
            attempt += 1;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use infinity_ipc::catalog::BridgeKind;
    use serde_json::json;
    use tokio_tungstenite::tungstenite::Message;

    type Ws = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    fn spec(namespace: &str, port: u16) -> BridgeSpec {
        BridgeSpec {
            id: format!("{namespace}-bridge"),
            kind: BridgeKind::TauriWsBridge,
            port,
            namespace: namespace.into(),
        }
    }

    fn manager() -> BridgeManager {
        BridgeManager::new(BridgeConfig {
            retry_backoff: Duration::from_millis(10),
            ..BridgeConfig::default()
        })
    }

    async fn gauge(addr: SocketAddr, namespace: &str) -> Ws {
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/{namespace}"))
            .await
            .unwrap();
        ws
    }

    async fn wait_for(mut check: impl AsyncFnMut() -> bool) {
        for _ in 0..200 {
            if check().await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("condition never held");
    }

    async fn next_json(ws: &mut Ws) -> Value {
        loop {
            match ws.next().await.unwrap().unwrap() {
                Message::Text(t) => return serde_json::from_str(&t).unwrap(),
                Message::Close(_) => panic!("closed"),
                _ => {}
            }
        }
    }

    #[test]
    fn channel_names_follow_v3_and_skip_dotted_streams() {
        let evt = |name: &str| BridgeEvent {
            namespace: "dc10".into(),
            name: name.into(),
            data: Value::Null,
        };
        assert_eq!(
            evt("equip_state").channel().as_deref(),
            Some("bridge:dc10:equip_state")
        );
        assert_eq!(evt("terrain.request").channel(), None);
        assert_eq!(evt("flir.state").channel(), None);
    }

    #[tokio::test]
    async fn serves_the_gauge_at_its_namespace_and_merges_events() {
        let mgr = manager();
        let mut events = mgr.subscribe();
        let mut status = mgr.subscribe_status();
        let addr = mgr.start(spec("dc10", 0)).await.unwrap();
        assert!(!mgr.is_connected("dc10").await.unwrap());

        let mut ws = gauge(addr, "dc10").await;
        ws.send(Message::text(r#"{"t":"hello","client":"test"}"#))
            .await
            .unwrap();
        ws.send(Message::text(
            r#"{"t":"event","name":"equip_state","data":{"idx":3}}"#,
        ))
        .await
        .unwrap();

        let evt = events.recv().await.unwrap();
        assert_eq!(evt.namespace, "dc10");
        assert_eq!(evt.name, "equip_state");
        assert_eq!(evt.data, json!({ "idx": 3 }));
        assert_eq!(
            status.recv().await.unwrap(),
            ConnectionChange {
                namespace: "dc10".into(),
                connected: true
            }
        );
        assert!(mgr.is_connected("dc10").await.unwrap());
        let st = mgr.statuses().await;
        assert_eq!(st.len(), 1);
        assert!(st[0].connected);
    }

    #[tokio::test]
    async fn command_round_trips_through_the_gauge() {
        let mgr = manager();
        let addr = mgr.start(spec("dc10", 0)).await.unwrap();
        let mut ws = gauge(addr, "dc10").await;
        let m = mgr.clone();
        wait_for(async || m.is_connected("dc10").await.unwrap()).await;

        let call = tokio::spawn({
            let mgr = mgr.clone();
            async move {
                mgr.command("dc10", "equip_query", json!({}), Duration::from_secs(2))
                    .await
            }
        });
        let cmd = loop {
            let v = next_json(&mut ws).await;
            if v["t"] == "cmd" {
                break v;
            }
        };
        assert_eq!(cmd["name"], "equip_query");
        let ack = json!({ "t": "ack", "id": cmd["id"], "ok": true, "response": { "v": 1 } });
        ws.send(Message::text(ack.to_string())).await.unwrap();
        assert_eq!(call.await.unwrap().unwrap(), json!({ "v": 1 }));
    }

    #[tokio::test]
    async fn a_refusal_is_returned_without_retrying() {
        let mgr = manager();
        let addr = mgr.start(spec("dc10", 0)).await.unwrap();
        let mut ws = gauge(addr, "dc10").await;
        let m = mgr.clone();
        wait_for(async || m.is_connected("dc10").await.unwrap()).await;

        let call = tokio::spawn({
            let mgr = mgr.clone();
            async move {
                mgr.command("dc10", "equip_cmd", json!({}), Duration::from_secs(2))
                    .await
            }
        });
        let cmd = loop {
            let v = next_json(&mut ws).await;
            if v["t"] == "cmd" {
                break v;
            }
        };
        let ack = json!({ "t": "ack", "id": cmd["id"], "ok": false, "error": "INVALID_EQUIP_IDX" });
        ws.send(Message::text(ack.to_string())).await.unwrap();
        let err = call.await.unwrap().unwrap_err();
        assert!(matches!(err, BridgeError::Command { .. }), "{err}");
        // A second cmd would mean the refusal was replayed.
        let more = tokio::time::timeout(Duration::from_millis(100), async {
            loop {
                if next_json(&mut ws).await["t"] == "cmd" {
                    break;
                }
            }
        })
        .await;
        assert!(more.is_err());
    }

    #[tokio::test]
    async fn commands_without_a_gauge_report_not_connected() {
        let mgr = manager();
        mgr.start(spec("dc10", 0)).await.unwrap();
        let err = mgr
            .command_with_attempts("dc10", "equip_query", json!({}), Duration::from_secs(1), 2)
            .await
            .unwrap_err();
        assert!(matches!(err, BridgeError::NotConnected(_)));
        assert!(matches!(
            mgr.command("nope", "x", json!({}), Duration::from_secs(1))
                .await,
            Err(BridgeError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn starting_a_namespace_twice_is_refused() {
        let mgr = manager();
        mgr.start(spec("dc10", 0)).await.unwrap();
        assert!(matches!(
            mgr.start(spec("dc10", 0)).await,
            Err(BridgeError::AlreadyRunning(_))
        ));
        mgr.ensure_started(spec("dc10", 0)).await.unwrap();
        assert_eq!(mgr.list().await.len(), 1);
    }

    #[tokio::test]
    async fn stop_closes_gauge_sockets_and_frees_the_port() {
        let mgr = manager();
        let mut status = mgr.subscribe_status();
        let addr = mgr.start(spec("dc10", 0)).await.unwrap();
        let mut ws = gauge(addr, "dc10").await;
        let m = mgr.clone();
        wait_for(async || m.is_connected("dc10").await.unwrap()).await;
        assert!(status.recv().await.unwrap().connected);

        mgr.stop("dc10").await.unwrap();
        // The gauge must see its socket end, or it never reconnects.
        let ended = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match ws.next().await {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                    Some(Ok(_)) => {}
                }
            }
        })
        .await;
        assert!(ended.is_ok(), "gauge socket stayed open after stop");
        assert!(!status.recv().await.unwrap().connected);
        assert!(matches!(
            mgr.is_connected("dc10").await,
            Err(BridgeError::NotFound(_))
        ));

        // Same port again: the old listener is gone.
        let again = mgr.start(spec("dc10", addr.port())).await.unwrap();
        assert_eq!(again, addr);
        let mut ws = gauge(again, "dc10").await;
        ws.send(Message::text(r#"{"t":"hello"}"#)).await.unwrap();
        wait_for(async || m.is_connected("dc10").await.unwrap()).await;
        mgr.stop_all().await;
        assert!(mgr.list().await.is_empty());
    }

    #[tokio::test]
    async fn a_taken_port_is_a_bind_error() {
        let squatter = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = squatter.local_addr().unwrap().port();
        let err = manager().start(spec("dc10", port)).await.unwrap_err();
        assert!(matches!(err, BridgeError::Bind { .. }), "{err}");
    }
}
