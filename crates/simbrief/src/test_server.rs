//! A minimal HTTP/1.1 server for tests: one request per connection, routed
//! by a closure.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

#[derive(Debug, Clone)]
pub(crate) struct Request {
    pub(crate) method: String,
    /// Path and query, e.g. `/api/xml.fetcher.php?userid=1&json=1`.
    pub(crate) target: String,
}

#[derive(Debug, Clone)]
pub(crate) struct Response {
    status: u16,
    content_type: Option<&'static str>,
    body: Vec<u8>,
}

impl Response {
    pub(crate) fn bytes(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            content_type: None,
            body: body.into(),
        }
    }

    pub(crate) fn json(status: u16, value: serde_json::Value) -> Self {
        Self {
            content_type: Some("application/json"),
            ..Self::bytes(status, value.to_string())
        }
    }
}

type Handler = dyn Fn(&Request) -> Response + Send + Sync;

pub(crate) struct TestServer {
    base: Url,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl TestServer {
    pub(crate) async fn start(
        handler: impl Fn(&Request) -> Response + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<Handler> = Arc::new(handler);
        let log = requests.clone();
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let (handler, log) = (handler.clone(), log.clone());
                tokio::spawn(async move { serve(socket, &*handler, &log).await });
            }
        });
        Self { base, requests }
    }

    pub(crate) fn base(&self) -> Url {
        self.base.clone()
    }

    pub(crate) fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

async fn serve(mut socket: tokio::net::TcpStream, handler: &Handler, log: &Mutex<Vec<Request>>) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if socket.read(&mut byte).await.unwrap_or(0) == 0 {
            return;
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let mut start = head.lines().next().unwrap_or_default().split_whitespace();
    let request = Request {
        method: start.next().unwrap_or_default().to_owned(),
        target: start.next().unwrap_or("/").to_owned(),
    };
    log.lock().unwrap().push(request.clone());
    let response = handler(&request);

    let mut out = format!("HTTP/1.1 {} X\r\n", response.status);
    if let Some(ct) = response.content_type {
        out.push_str(&format!("content-type: {ct}\r\n"));
    }
    out.push_str(&format!(
        "content-length: {}\r\nconnection: close\r\n\r\n",
        response.body.len()
    ));
    let _ = socket.write_all(out.as_bytes()).await;
    let _ = socket.write_all(&response.body).await;
    let _ = socket.flush().await;
}
