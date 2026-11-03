//! A minimal HTTP/1.1 server for tests: one request per connection, routed
//! by a closure.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

#[derive(Debug, Clone)]
pub(crate) struct Request {
    pub(crate) method: String,
    /// Path and query, e.g. `/connect/token?x=1`.
    pub(crate) target: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

impl Request {
    pub(crate) fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("/")
    }

    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub(crate) fn form(&self) -> HashMap<String, String> {
        url::form_urlencoded::parse(&self.body)
            .into_owned()
            .collect()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    pub(crate) fn bytes(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    pub(crate) fn json(status: u16, value: serde_json::Value) -> Self {
        Self::bytes(status, value.to_string()).header("content-type", "application/json")
    }

    pub(crate) fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
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

    pub(crate) fn url(&self, path: &str) -> Url {
        self.base.join(path).unwrap()
    }

    pub(crate) fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    pub(crate) fn hits(&self, path: &str) -> usize {
        self.requests().iter().filter(|r| r.path() == path).count()
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
    let mut lines = head.lines();
    let mut start = lines.next().unwrap_or_default().split_whitespace();
    let method = start.next().unwrap_or_default().to_owned();
    let target = start.next().unwrap_or("/").to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            Some((k.trim().to_owned(), v.trim().to_owned()))
        })
        .collect();
    let len = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    if socket.read_exact(&mut body).await.is_err() {
        return;
    }
    let request = Request {
        method,
        target,
        headers,
        body,
    };
    log.lock().unwrap().push(request.clone());
    let response = handler(&request);

    let mut out = format!("HTTP/1.1 {} X\r\n", response.status);
    for (k, v) in &response.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str(&format!(
        "content-length: {}\r\nconnection: close\r\n\r\n",
        response.body.len()
    ));
    let _ = socket.write_all(out.as_bytes()).await;
    let _ = socket.write_all(&response.body).await;
    let _ = socket.flush().await;
}
