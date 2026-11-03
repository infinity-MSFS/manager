//! Loopback WebSocket that lets an in-sim panel open a link in the user's
//! browser.
//!
//! MSFS panels run in a sandboxed web view that cannot launch the system
//! browser, yet some need to send the user to a web page (a device sign-in,
//! a store page). They connect to `ws://127.0.0.1:6969/open-url` and send
//!
//! ```json
//! { "t": "open_url", "url": "https://…" }
//! ```
//!
//! Same port, path and message as v3. Bound to loopback only, and only
//! http(s) URLs are handed on, so a local message cannot launch another
//! scheme or a file.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use serde::Deserialize;

/// Loopback port panels connect to. Shared with v3.
pub const PORT: u16 = 6969;
/// WebSocket path. Shared with v3.
pub const PATH: &str = "/open-url";

/// Opens a URL that already passed [`accepted_url`].
pub type Opener = Arc<dyn Fn(String) + Send + Sync>;

/// Binds `127.0.0.1:`[`PORT`] and serves until the process exits. Errors
/// only when the port cannot be bound.
pub async fn run(open: Opener) -> std::io::Result<()> {
    serve(SocketAddr::from(([127, 0, 0, 1], PORT)), open).await
}

/// [`run`] on any address; tests use port 0.
pub async fn serve(addr: SocketAddr, open: Opener) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(addr = %listener.local_addr()?, path = PATH, "link server listening");
    serve_on(listener, open).await
}

/// Serves on an already bound listener.
pub async fn serve_on(listener: tokio::net::TcpListener, open: Opener) -> std::io::Result<()> {
    let router = Router::new().route(PATH, get(upgrade)).with_state(open);
    axum::serve(listener, router).await
}

async fn upgrade(ws: WebSocketUpgrade, State(open): State<Opener>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| session(socket, open))
}

async fn session(mut socket: WebSocket, open: Opener) {
    while let Some(Ok(msg)) = socket.recv().await {
        // Binary, ping and pong frames are not part of the protocol.
        if let Message::Text(text) = msg
            && let Some(url) = parse(text.as_str())
        {
            tracing::info!(%url, "opening a link for an in-sim panel");
            open(url);
        }
    }
}

#[derive(Deserialize)]
struct Incoming {
    t: String,
    #[serde(default)]
    url: Option<String>,
}

/// The URL to open from one text frame, if it is an `open_url` message
/// with an acceptable URL.
fn parse(text: &str) -> Option<String> {
    let msg: Incoming = match serde_json::from_str(text) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "link server: malformed message");
            return None;
        }
    };
    if msg.t != "open_url" {
        return None;
    }
    let Some(url) = msg.url else {
        tracing::warn!("link server: open_url without a url");
        return None;
    };
    let url = accepted_url(&url);
    if url.is_none() {
        tracing::warn!("link server: refusing a non-http(s) url");
    }
    url
}

/// `url` trimmed, when it is http or https.
pub fn accepted_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let lower = trimmed.get(..8).unwrap_or(trimmed).to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://")).then(|| trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures_util::SinkExt;
    use tokio::sync::mpsc;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    use super::*;

    #[test]
    fn only_http_and_https_are_accepted() {
        assert_eq!(
            accepted_url("  https://example.com/a "),
            Some("https://example.com/a".to_owned())
        );
        assert!(accepted_url("HTTP://example.com").is_some());
        assert_eq!(accepted_url("file:///C:/Windows/System32/calc.exe"), None);
        assert_eq!(accepted_url("javascript:alert(1)"), None);
        assert_eq!(accepted_url("ms-settings:"), None);
        assert_eq!(accepted_url(""), None);
    }

    #[test]
    fn messages_parse_as_v3() {
        assert_eq!(
            parse(r#"{"t":"open_url","url":"https://x.test"}"#),
            Some("https://x.test".to_owned())
        );
        assert_eq!(parse(r#"{"t":"other","url":"https://x.test"}"#), None);
        assert_eq!(parse(r#"{"t":"open_url"}"#), None);
        assert_eq!(parse("not json"), None);
    }

    #[tokio::test]
    async fn opens_urls_sent_over_the_socket() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let open: Opener = Arc::new(move |url| {
            let _ = tx.send(url);
        });
        tokio::spawn(serve_on(listener, open));

        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}{PATH}"))
            .await
            .unwrap();
        for text in [
            r#"{"t":"open_url","url":"file:///etc/passwd"}"#,
            r#"{"t":"open_url","url":"https://infinity.test/login"}"#,
        ] {
            ws.send(WsMessage::text(text)).await.unwrap();
        }
        let got = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap();
        assert_eq!(got.as_deref(), Some("https://infinity.test/login"));
        assert!(rx.try_recv().is_err());
    }
}
