#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::pairing::sign_pairing_token;
use crate::protocol::PROTOCOL_VERSION;
use crate::providers::{MAX_VIEWPORT, MIN_VIEWPORT, ProviderError, TileSource, WxProvider};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::{self, protocol::frame::coding::CloseCode};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

fn test_secret() -> PairingSecret {
    PairingSecret::new(b"sufficiently-long-test-secret-bytes".to_vec()).unwrap()
}

fn config(loopback_bypass: bool, providers: Providers) -> EfbConfig {
    EfbConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        loopback_bypass,
        providers,
        ..EfbConfig::new(test_secret())
    }
}

async fn start_with(cfg: EfbConfig) -> (EfbServer, SocketAddr) {
    let srv = EfbServer::new();
    let bind = srv.start(cfg).await.unwrap();
    (srv, bind)
}

async fn start_server() -> (EfbServer, SocketAddr, String) {
    let (srv, bind) = start_with(config(false, Providers::default())).await;
    let token = sign_pairing_token(&test_secret(), "test", 2_000_000_000).unwrap();
    (srv, bind, token)
}

async fn open_ws(bind: SocketAddr, token: &str) -> Ws {
    let (ws, _) = connect_async(format!("ws://{bind}/ws?token={token}"))
        .await
        .unwrap();
    ws
}

async fn next_json(ws: &mut Ws) -> Value {
    loop {
        match tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("frame in time")
            .unwrap()
            .unwrap()
        {
            tungstenite::Message::Text(t) => return serde_json::from_str(&t).unwrap(),
            tungstenite::Message::Close(c) => panic!("closed: {c:?}"),
            _ => {}
        }
    }
}

async fn send_text(ws: &mut Ws, text: &str) {
    ws.send(tungstenite::Message::text(text)).await.unwrap();
}

async fn close_code_of(ws: &mut Ws) -> Option<CloseCode> {
    loop {
        match tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("close in time")
        {
            Some(Ok(tungstenite::Message::Close(c))) => return c.map(|c| c.code),
            Some(Ok(_)) => {}
            None | Some(Err(_)) => return None,
        }
    }
}

#[tokio::test]
async fn health_open() {
    let (_srv, bind, _) = start_server().await;
    let r = reqwest::get(format!("http://{bind}/health")).await.unwrap();
    assert!(r.status().is_success());
    assert_eq!(r.json::<Value>().await.unwrap(), json!({ "status": "ok" }));
}

#[tokio::test]
async fn ws_rejects_missing_or_bad_token() {
    let (_srv, bind, _) = start_server().await;
    assert!(connect_async(format!("ws://{bind}/ws")).await.is_err());
    assert!(
        connect_async(format!("ws://{bind}/ws?token=x|1|00"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn ws_sends_snapshot_on_connect() {
    let (srv, bind, token) = start_server().await;
    srv.state()
        .apply_sim_delta(serde_json::from_str(r#"{"ALT_FT":12345}"#).unwrap())
        .await;
    let mut ws = open_ws(bind, &token).await;
    let v = next_json(&mut ws).await;
    assert_eq!(v["t"], "snapshot");
    assert_eq!(v["proto"], PROTOCOL_VERSION);
    assert_eq!(v["sim"]["ALT_FT"], 12345);
    let clients = srv.connected_clients().await;
    assert_eq!(clients.len(), 1);
    assert_eq!(clients[0].device, "test");
}

#[tokio::test]
async fn ui_writes_round_trip() {
    let (_srv, bind, token) = start_server().await;
    let mut a = open_ws(bind, &token).await;
    let mut b = open_ws(bind, &token).await;
    next_json(&mut a).await;
    next_json(&mut b).await;

    send_text(&mut a, r#"{"t":"ui","data":{"page":"home"}}"#).await;
    // Both, originator included: clients commit on the rebroadcast.
    for ws in [&mut a, &mut b] {
        let v = next_json(ws).await;
        assert_eq!(v["t"], "ui");
        assert_eq!(v["seq"], 1);
        assert_eq!(v["data"]["page"], "home");
    }
}

#[tokio::test]
async fn cmd_does_not_echo_to_originator() {
    let (_srv, bind, token) = start_server().await;
    let mut a = open_ws(bind, &token).await;
    let mut b = open_ws(bind, &token).await;
    next_json(&mut a).await;
    next_json(&mut b).await;

    send_text(&mut a, r#"{"t":"cmd","name":"refresh","data":null}"#).await;
    let v = next_json(&mut b).await;
    assert_eq!(v["t"], "cmd");
    assert_eq!(v["name"], "refresh");
    let r = tokio::time::timeout(Duration::from_millis(100), a.next()).await;
    assert!(r.is_err(), "originator should not receive its own cmd");
}

#[tokio::test]
async fn ping_pong_and_resync() {
    let (srv, bind, token) = start_server().await;
    let mut ws = open_ws(bind, &token).await;
    next_json(&mut ws).await;
    send_text(&mut ws, r#"{"t":"ping"}"#).await;
    assert_eq!(next_json(&mut ws).await["t"], "pong");

    srv.state()
        .apply_ui_delta(serde_json::from_str(r#"{"k":1}"#).unwrap(), None)
        .await;
    assert_eq!(next_json(&mut ws).await["t"], "ui");
    send_text(&mut ws, r#"{"t":"resync"}"#).await;
    let snap = next_json(&mut ws).await;
    assert_eq!(snap["t"], "snapshot");
    assert_eq!(snap["ui"]["k"], 1);
    assert_eq!(snap["ui_seq"], 1);
}

#[tokio::test]
async fn loopback_bypass_accepts_tokenless_localhost() {
    let (srv, bind) = start_with(config(true, Providers::default())).await;
    let (mut ws, _) = connect_async(format!("ws://{bind}/ws")).await.unwrap();
    assert_eq!(next_json(&mut ws).await["t"], "snapshot");
    let clients = srv.connected_clients().await;
    assert_eq!(clients.len(), 1);
    assert!(clients[0].loopback);
    assert_eq!(clients[0].device, "loopback");

    // A rejected token is never shown as the device name.
    let (mut ws, _) = connect_async(format!("ws://{bind}/ws?token=dev|1|abcd"))
        .await
        .unwrap();
    next_json(&mut ws).await;
    assert!(
        srv.connected_clients()
            .await
            .iter()
            .all(|c| c.device == "loopback")
    );
}

#[tokio::test]
async fn mint_token_round_trip() {
    let srv = EfbServer::new();
    srv.set_secret(test_secret()).await;
    let token = srv.mint_token("tablet-001", 86_400).await.unwrap();
    let parts: Vec<&str> = token.split('|').collect();
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0], "tablet-001");
}

#[tokio::test]
async fn mint_token_needs_a_secret() {
    let srv = EfbServer::new();
    assert!(matches!(
        srv.mint_token("tablet", 60).await,
        Err(EfbError::NoSecret)
    ));
}

#[tokio::test]
async fn pairing_code_redeems_once_for_a_working_token() {
    let (srv, bind, _) = start_server().await;
    let code = srv.mint_pairing_code("ipad", 3600).await.unwrap();
    let url = format!("http://{bind}/pair?code={}", code.to_lowercase());
    let r = reqwest::get(&url).await.unwrap();
    assert!(r.status().is_success());
    let token = r.json::<Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(reqwest::get(&url).await.unwrap().status(), 404);
    assert_eq!(
        reqwest::get(format!("http://{bind}/pair"))
            .await
            .unwrap()
            .status(),
        400
    );

    let mut ws = open_ws(bind, &token).await;
    next_json(&mut ws).await;
    assert_eq!(srv.connected_clients().await[0].device, "ipad");
}

#[tokio::test]
async fn rotating_the_secret_invalidates_old_tokens_on_a_running_server() {
    let (srv, bind, token) = start_server().await;
    let mut ws = open_ws(bind, &token).await;
    next_json(&mut ws).await;
    srv.rotate_secret(PairingSecret::generate()).await;
    assert!(
        connect_async(format!("ws://{bind}/ws?token={token}"))
            .await
            .is_err()
    );
    // Loopback sockets are left alone.
    send_text(&mut ws, r#"{"t":"ping"}"#).await;
    assert_eq!(next_json(&mut ws).await["t"], "pong");
    let fresh = srv.mint_token("tablet", 60).await.unwrap();
    let mut ws = open_ws(bind, &fresh).await;
    next_json(&mut ws).await;
}

#[tokio::test]
async fn disconnect_client_closes_with_forgotten() {
    let (srv, bind, token) = start_server().await;
    let mut ws = open_ws(bind, &token).await;
    next_json(&mut ws).await;
    let id = srv.connected_clients().await[0].client_id;
    assert!(srv.disconnect_client(id).await);
    assert_eq!(close_code_of(&mut ws).await, Some(CloseCode::from(4001)));
    assert!(!srv.disconnect_client(999).await);
}

#[tokio::test]
async fn idle_clients_are_closed() {
    let (_srv, bind) = start_with(EfbConfig {
        idle_timeout: Duration::from_millis(100),
        ..config(true, Providers::default())
    })
    .await;
    let (mut ws, _) = connect_async(format!("ws://{bind}/ws")).await.unwrap();
    next_json(&mut ws).await;
    assert_eq!(close_code_of(&mut ws).await, Some(CloseCode::from(4408)));
}

#[tokio::test]
async fn stop_closes_clients_and_frees_the_port() {
    let (srv, bind, token) = start_server().await;
    let changes = srv.subscribe_changes();
    let mut ws = open_ws(bind, &token).await;
    next_json(&mut ws).await;
    srv.stop().await.unwrap();
    assert_eq!(close_code_of(&mut ws).await, Some(CloseCode::Away));
    assert!(changes.has_changed().unwrap());
    assert!(!srv.is_running().await);
    assert!(matches!(srv.stop().await, Err(EfbError::NotRunning)));

    let again = srv
        .start(EfbConfig {
            bind,
            ..config(false, Providers::default())
        })
        .await
        .unwrap();
    assert_eq!(again, bind);
    assert!(matches!(
        srv.start(config(false, Providers::default())).await,
        Err(EfbError::AlreadyRunning)
    ));
}

struct FakeWx;

#[async_trait]
impl WxProvider for FakeWx {
    async fn fetch(&self, ids: Vec<String>) -> Result<Value, ProviderError> {
        if ids == ["FAIL"] {
            return Err(ProviderError::new("network", "down"));
        }
        Ok(json!(
            ids.iter()
                .map(|id| json!({ "icaoId": id }))
                .collect::<Vec<_>>()
        ))
    }
}

#[tokio::test]
async fn wx_fetch_publishes_wx_data_and_reports() {
    let providers = Providers {
        wx: Some(Arc::new(FakeWx)),
        ..Providers::default()
    };
    let (_srv, bind) = start_with(config(true, providers)).await;
    let (mut ws, _) = connect_async(format!("ws://{bind}/ws")).await.unwrap();
    next_json(&mut ws).await;

    send_text(
        &mut ws,
        r#"{"t":"cmd","name":"wx.fetch","data":{"ids":[" kedw "]}}"#,
    )
    .await;
    let ui = next_json(&mut ws).await;
    assert_eq!(ui["t"], "ui");
    assert_eq!(ui["data"]["wxData"]["ids"], json!(["KEDW"]));
    assert_eq!(ui["data"]["wxData"]["stations"][0]["icaoId"], "KEDW");
    assert!(ui["data"]["wxData"]["fetched_at"].is_u64());
    let result = next_json(&mut ws).await;
    assert_eq!(result["name"], "wx.result");
    assert_eq!(result["data"], json!({ "ok": true, "error": null }));

    send_text(
        &mut ws,
        r#"{"t":"cmd","name":"wx.fetch","data":{"ids":["FAIL"]}}"#,
    )
    .await;
    let result = next_json(&mut ws).await;
    assert_eq!(result["data"]["error"]["code"], "network");

    send_text(
        &mut ws,
        r#"{"t":"cmd","name":"wx.fetch","data":{"ids":[]}}"#,
    )
    .await;
    let result = next_json(&mut ws).await;
    assert_eq!(result["data"]["error"]["code"], "no_stations");
}

struct FakeTiles;

#[async_trait]
impl TileSource for FakeTiles {
    async fn tile(&self, layer: &str, z: u8, x: u32, y: u32) -> Result<Vec<u8>, ProviderError> {
        Ok(format!("{layer}/{z}/{x}/{y}").into_bytes())
    }
}

#[tokio::test]
async fn image_routes_need_a_token_off_loopback_bypass() {
    let providers = Providers {
        tiles: Some(Arc::new(FakeTiles)),
        ..Providers::default()
    };
    let (_srv, bind) = start_with(config(false, providers)).await;
    let token = sign_pairing_token(&test_secret(), "t", 2_000_000_000).unwrap();
    let base = format!("http://{bind}/efb/nav/tile");

    let r = reqwest::get(format!("{base}/dark/3/1/2.png"))
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let r = reqwest::get(format!("{base}/dark/3/1/2.png?token={token}"))
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "image/png");
    assert_eq!(r.bytes().await.unwrap().as_ref(), b"dark/3/1/2");

    for bad in ["nope/3/1/2.png", "dark/19/1/2.png", "dark/x/1/2.png"] {
        let r = reqwest::get(format!("{base}/{bad}?token={token}"))
            .await
            .unwrap();
        assert_eq!(r.status(), 400, "{bad}");
    }
    let r = reqwest::get(format!("http://{bind}/efb/doc/a/page/1.jpg?token={token}"))
        .await
        .unwrap();
    assert_eq!(r.status(), 404, "no doc provider");
}

/// Records the calls a browser provider receives.
#[derive(Default)]
struct FakeBrowser {
    calls: std::sync::Mutex<Vec<String>>,
}

impl FakeBrowser {
    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl crate::providers::BrowserProvider for FakeBrowser {
    async fn open(&self, session: &str, url: &str, width: u32, height: u32) {
        self.record(format!("open {session} {url} {width}x{height}"));
    }
    async fn navigate(&self, session: &str, url: &str) {
        self.record(format!("nav {session} {url}"));
    }
    async fn action(&self, session: &str, action: &str) {
        self.record(format!("action {session} {action}"));
    }
    fn input(&self, session: &str, event: Value) {
        self.record(format!("input {session} {}", event["n"]));
    }
    async fn close(&self, session: &str) {
        self.record(format!("close {session}"));
    }
    async fn frame(&self, session: &str, seq: u64) -> Option<Vec<u8>> {
        (session == "s1").then(|| format!("jpeg {seq}").into_bytes())
    }
    async fn shutdown(&self) {
        self.record("shutdown".into());
    }
}

async fn wait_for_calls(fake: &FakeBrowser, n: usize) -> Vec<String> {
    for _ in 0..100 {
        let calls = fake.calls();
        if calls.len() >= n {
            return calls;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    fake.calls()
}

#[tokio::test]
async fn browser_cmds_reach_the_provider_with_input_in_order() {
    let fake = Arc::new(FakeBrowser::default());
    let providers = Providers {
        browser: Some(fake.clone()),
        ..Providers::default()
    };
    let (srv, bind) = start_with(config(true, providers)).await;
    let (mut ws, _) = connect_async(format!("ws://{bind}/ws")).await.unwrap();
    next_json(&mut ws).await;

    send_text(
        &mut ws,
        r#"{"t":"cmd","name":"browser.open","data":{"session":"s1","url":"https://e.com","w":10,"h":9000}}"#,
    )
    .await;
    assert_eq!(
        wait_for_calls(&fake, 1).await,
        [format!(
            "open s1 https://e.com {MIN_VIEWPORT}x{MAX_VIEWPORT}"
        )]
    );

    // Inline, never spawned: a burst arrives exactly in the order sent.
    for n in 0..50 {
        let frame =
            json!({ "t": "cmd", "name": "browser.input", "data": { "session": "s1", "n": n } });
        send_text(&mut ws, &frame.to_string()).await;
    }
    // No session, so dropped rather than guessed at.
    send_text(
        &mut ws,
        r#"{"t":"cmd","name":"browser.input","data":{"n":99}}"#,
    )
    .await;
    send_text(
        &mut ws,
        r#"{"t":"cmd","name":"browser.nav","data":{"session":"s1","url":"https://f.com","action":"back"}}"#,
    )
    .await;
    let calls = wait_for_calls(&fake, 52).await;
    let inputs: Vec<String> = (0..50).map(|n| format!("input s1 {n}")).collect();
    assert_eq!(calls[1..51], inputs[..]);
    assert_eq!(calls[51], "nav s1 https://f.com", "url wins over action");
    assert_eq!(calls.len(), 52);

    let r = reqwest::get(format!("http://{bind}/efb/browser/s1/frame.jpg?seq=7"))
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "image/jpeg");
    assert_eq!(r.headers()["cache-control"], "no-store");
    assert_eq!(r.bytes().await.unwrap().as_ref(), b"jpeg 7");
    let r = reqwest::get(format!("http://{bind}/efb/browser/s1/frame.jpg"))
        .await
        .unwrap();
    assert_eq!(r.status(), 400, "seq is required");
    let r = reqwest::get(format!("http://{bind}/efb/browser/s2/frame.jpg?seq=1"))
        .await
        .unwrap();
    assert_eq!(r.status(), 404);

    srv.stop().await.unwrap();
    assert_eq!(fake.calls().last().map(String::as_str), Some("shutdown"));
}

#[tokio::test]
async fn unwired_provider_cmds_are_relayed() {
    let (_srv, bind, token) = start_server().await;
    let mut a = open_ws(bind, &token).await;
    let mut b = open_ws(bind, &token).await;
    next_json(&mut a).await;
    next_json(&mut b).await;
    send_text(&mut a, r#"{"t":"cmd","name":"fpl.fetch","data":{}}"#).await;
    assert_eq!(next_json(&mut b).await["name"], "fpl.fetch");
}

#[tokio::test]
async fn static_spa_falls_back_to_index() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.html"), "<h1>efb</h1>").unwrap();
    let (_srv, bind) = start_with(EfbConfig {
        static_dir: Some(dir.path().to_path_buf()),
        ..config(false, Providers::default())
    })
    .await;
    let body = reqwest::get(format!("http://{bind}/some/client/route"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(body, "<h1>efb</h1>");
}
