//! A minimal Chrome DevTools Protocol client.
//!
//! Hand-rolled rather than `chromiumoxide`: the browser needs about a dozen
//! methods and a handful of events, and the alternative is a large
//! dependency with its own process management duplicating what is here. The
//! protocol is JSON requests answered by `id`, plus unsolicited events.
//!
//! Each connection owns two tasks: a reader that resolves pending calls and
//! forwards events, and a writer draining an unbounded queue. The writer ends
//! when the last [`Cdp`] handle drops; the reader when the socket closes or
//! nobody is listening for events any more.
//!
//! Every event goes to one channel without filtering on `sessionId`: a
//! browser process here serves exactly one page target, so everything on the
//! socket belongs to it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tracing::debug;

use super::error::CdpError;
use super::lock;

/// Generous, because `Page.navigate` on a cold engine can be slow; the point
/// is only that nothing waits forever on a wedged browser.
const CALL_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Event {
    pub(crate) method: String,
    pub(crate) params: Value,
}

type Reply = Result<Value, String>;
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>>;

#[derive(Debug)]
pub(crate) struct Cdp {
    out: mpsc::UnboundedSender<Message>,
    pending: Pending,
    next_id: AtomicU64,
}

impl Cdp {
    /// Connects and starts pumping; events arrive on `events`.
    pub(crate) async fn connect(
        ws_url: &str,
        events: mpsc::UnboundedSender<Event>,
    ) -> Result<Arc<Self>, CdpError> {
        let (stream, _) = tokio_tungstenite::connect_async(ws_url)
            .await
            .map_err(|e| CdpError::Connect(Box::new(e)))?;
        let (mut sink, mut source) = stream.split();
        let (out, mut out_rx) = mpsc::unbounded_channel::<Message>();
        let pending: Pending = Arc::default();

        tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                if sink.send(msg).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });

        let reader_pending = pending.clone();
        tokio::spawn(async move {
            while let Some(Ok(msg)) = source.next().await {
                let Message::Text(text) = msg else { continue };
                match classify(&text) {
                    Some(Incoming::Reply { id, reply }) => {
                        if let Some(tx) = lock(&reader_pending).remove(&id) {
                            let _ = tx.send(reply);
                        }
                    }
                    Some(Incoming::Event(event)) => {
                        if events.send(event).is_err() {
                            break;
                        }
                    }
                    None => {}
                }
            }
            // Dropping the senders fails every outstanding call at once
            // rather than leaving each to time out.
            lock(&reader_pending).clear();
        });

        Ok(Arc::new(Self {
            out,
            pending,
            next_id: AtomicU64::new(1),
        }))
    }

    /// One call. `session` is `None` for browser-level methods and the
    /// attached page's session id for anything acting on the page.
    pub(crate) async fn call(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, CdpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        lock(&self.pending).insert(id, tx);
        let text = request(id, session, method, params);
        if self.out.send(Message::text(text)).is_err() {
            lock(&self.pending).remove(&id);
            return Err(CdpError::Closed);
        }
        match tokio::time::timeout(CALL_TIMEOUT, rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(message))) => Err(CdpError::Remote {
                method: method.to_owned(),
                message,
            }),
            Ok(Err(_)) => Err(CdpError::Closed),
            Err(_) => {
                lock(&self.pending).remove(&id);
                Err(CdpError::Timeout {
                    method: method.to_owned(),
                })
            }
        }
    }

    /// Fire and forget, for acks and shutdown where the reply carries
    /// nothing and waiting would serialise the frame loop behind it.
    pub(crate) fn notify(&self, session: Option<&str>, method: &str, params: Value) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if self
            .out
            .send(Message::text(request(id, session, method, params)))
            .is_err()
        {
            debug!(method, "efb-browser: notify dropped, connection closed");
        }
    }
}

fn request(id: u64, session: Option<&str>, method: &str, params: Value) -> String {
    let mut frame = json!({ "id": id, "method": method, "params": params });
    if let Some(session) = session {
        frame["sessionId"] = json!(session);
    }
    frame.to_string()
}

#[derive(Debug, PartialEq)]
enum Incoming {
    Reply { id: u64, reply: Reply },
    Event(Event),
}

/// A message with an `id` answers a call; one with a `method` is an event.
/// An `error` member is a refusal, surfaced by message so "navigation
/// refused" does not look like a silent no-op.
fn classify(text: &str) -> Option<Incoming> {
    let value: Value = serde_json::from_str(text).ok()?;
    if let Some(id) = value.get("id").and_then(Value::as_u64) {
        let reply = match value.get("error") {
            Some(err) => Err(err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("cdp error")
                .to_owned()),
            None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
        };
        return Some(Incoming::Reply { id, reply });
    }
    let method = value.get("method").and_then(Value::as_str)?;
    Some(Incoming::Event(Event {
        method: method.to_owned(),
        params: value.get("params").cloned().unwrap_or(Value::Null),
    }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn requests_carry_the_page_session_only_when_given() {
        let browser: Value = serde_json::from_str(&request(1, None, "A.b", json!({}))).unwrap();
        assert_eq!(browser, json!({ "id": 1, "method": "A.b", "params": {} }));
        let page: Value =
            serde_json::from_str(&request(2, Some("S"), "A.b", json!({ "x": 1 }))).unwrap();
        assert_eq!(page["sessionId"], "S");
        assert_eq!(page["params"]["x"], 1);
    }

    #[test]
    fn replies_errors_and_events_are_told_apart() {
        assert_eq!(
            classify(r#"{"id":3,"result":{"ok":true}}"#),
            Some(Incoming::Reply {
                id: 3,
                reply: Ok(json!({ "ok": true }))
            })
        );
        assert_eq!(
            classify(r#"{"id":4,"error":{"code":-32000,"message":"nope"}}"#),
            Some(Incoming::Reply {
                id: 4,
                reply: Err("nope".into())
            })
        );
        assert_eq!(
            classify(r#"{"id":5}"#),
            Some(Incoming::Reply {
                id: 5,
                reply: Ok(Value::Null)
            })
        );
        assert_eq!(
            classify(r#"{"method":"Page.loadEventFired","sessionId":"S"}"#),
            Some(Incoming::Event(Event {
                method: "Page.loadEventFired".into(),
                params: Value::Null
            }))
        );
        assert_eq!(classify("not json"), None);
        assert_eq!(classify(r#"{"neither":1}"#), None);
    }
}
