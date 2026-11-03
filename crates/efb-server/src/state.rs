//! The authoritative EFB state.
//!
//! Full `sim` and `ui` maps plus a sequence counter per channel. Every
//! mutation bumps its counter and broadcasts the delta; a late joiner gets a
//! snapshot cloned under one lock, so it never sees half an update.

use std::sync::Arc;

use serde_json::{Map, Value};
use tokio::sync::{RwLock, broadcast};

use crate::protocol::{Broadcast, ClientId, Outgoing, PROTOCOL_VERSION};

/// A client this far behind is resynced with a snapshot. Telemetry at 30 Hz
/// takes about 30 s to fill it.
const BROADCAST_CAPACITY: usize = 1024;

#[derive(Debug, Default)]
struct Inner {
    sim: Map<String, Value>,
    ui: Map<String, Value>,
    sim_seq: u64,
    ui_seq: u64,
}

/// Cheap to clone; usable before the server starts and kept across restarts.
#[derive(Debug, Clone)]
pub struct EfbState {
    inner: Arc<RwLock<Inner>>,
    tx: broadcast::Sender<Broadcast>,
}

impl Default for EfbState {
    fn default() -> Self {
        Self::new()
    }
}

impl EfbState {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            inner: Arc::new(RwLock::new(Inner::default())),
            tx,
        }
    }

    /// Live broadcasts after this call. The hydrating snapshot comes from
    /// [`Self::snapshot_frame`].
    pub fn subscribe(&self) -> broadcast::Receiver<Broadcast> {
        self.tx.subscribe()
    }

    pub async fn snapshot_frame(&self) -> Outgoing {
        let g = self.inner.read().await;
        Outgoing::Snapshot {
            proto: PROTOCOL_VERSION,
            sim_seq: g.sim_seq,
            ui_seq: g.ui_seq,
            sim: g.sim.clone(),
            ui: g.ui.clone(),
        }
    }

    /// Applies a sim delta (`null` deletes) and returns the new `sim_seq`.
    /// An empty delta changes nothing and broadcasts nothing.
    pub async fn apply_sim_delta(&self, data: Map<String, Value>) -> u64 {
        if data.is_empty() {
            return self.inner.read().await.sim_seq;
        }
        let seq = {
            let mut g = self.inner.write().await;
            g.sim_seq = g.sim_seq.wrapping_add(1);
            merge(&mut g.sim, &data);
            g.sim_seq
        };
        // No receiver just means no client; the next one gets a snapshot.
        let _ = self.tx.send(Broadcast {
            frame: Outgoing::Sim { seq, data },
            origin: None,
        });
        seq
    }

    /// Same as sim. UI deltas go to everyone, originator included.
    pub async fn apply_ui_delta(&self, data: Map<String, Value>, origin: Option<ClientId>) -> u64 {
        if data.is_empty() {
            return self.inner.read().await.ui_seq;
        }
        let seq = {
            let mut g = self.inner.write().await;
            g.ui_seq = g.ui_seq.wrapping_add(1);
            merge(&mut g.ui, &data);
            g.ui_seq
        };
        let _ = self.tx.send(Broadcast {
            frame: Outgoing::Ui { seq, data },
            origin,
        });
        seq
    }

    /// Sends a `cmd` to every client except `origin`. `None` reaches all:
    /// that is how the manager (or a provider) talks to clients.
    pub fn forward_cmd(&self, name: impl Into<String>, data: Value, origin: Option<ClientId>) {
        let _ = self.tx.send(Broadcast {
            frame: Outgoing::Cmd {
                name: name.into(),
                data,
            },
            origin,
        });
    }

    /// Replaces the whole sim map, broadcasting only what changed (removed
    /// keys as `null`), so a caller can push its full payload every tick.
    /// A non-object is ignored: sim state is key/value at the top level.
    pub async fn publish_sim_full(&self, value: Value) -> u64 {
        let Value::Object(new) = value else {
            return self.inner.read().await.sim_seq;
        };
        let delta = {
            let g = self.inner.read().await;
            let mut delta: Map<String, Value> = new
                .iter()
                .filter(|(k, v)| g.sim.get(*k) != Some(v))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            for k in g.sim.keys().filter(|k| !new.contains_key(*k)) {
                delta.insert(k.clone(), Value::Null);
            }
            delta
        };
        self.apply_sim_delta(delta).await
    }
}

fn merge(target: &mut Map<String, Value>, delta: &Map<String, Value>) {
    for (k, v) in delta {
        if v.is_null() {
            target.remove(k);
        } else {
            target.insert(k.clone(), v.clone());
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    fn map(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            _ => panic!("not an object"),
        }
    }

    #[tokio::test]
    async fn sim_delta_bumps_seq_and_broadcasts() {
        let s = EfbState::new();
        let mut rx = s.subscribe();
        assert_eq!(s.apply_sim_delta(map(json!({ "ALT_FT": 12345 }))).await, 1);
        let Outgoing::Sim { seq, data } = rx.recv().await.unwrap().frame else {
            panic!("expected Sim frame");
        };
        assert_eq!(seq, 1);
        assert_eq!(data.get("ALT_FT"), Some(&json!(12345)));
    }

    #[tokio::test]
    async fn ui_null_deletes_key() {
        let s = EfbState::new();
        s.apply_ui_delta(map(json!({ "page": "home" })), None).await;
        s.apply_ui_delta(map(json!({ "page": null })), None).await;
        let Outgoing::Snapshot { ui, ui_seq, .. } = s.snapshot_frame().await else {
            panic!("expected snapshot");
        };
        assert!(!ui.contains_key("page"));
        assert_eq!(ui_seq, 2);
    }

    #[tokio::test]
    async fn publish_sim_full_emits_diff_only() {
        let s = EfbState::new();
        let mut rx = s.subscribe();
        s.publish_sim_full(json!({ "ALT_FT": 100, "IAS_KT": 250 }))
            .await;
        let Outgoing::Sim { data, .. } = rx.recv().await.unwrap().frame else {
            panic!()
        };
        assert_eq!(data.len(), 2);

        s.publish_sim_full(json!({ "ALT_FT": 100, "IAS_KT": 250 }))
            .await;
        let r = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await;
        assert!(r.is_err(), "expected no broadcast on no-op publish");

        s.publish_sim_full(json!({ "ALT_FT": 200 })).await;
        let Outgoing::Sim { data, .. } = rx.recv().await.unwrap().frame else {
            panic!()
        };
        assert_eq!(data.get("ALT_FT"), Some(&json!(200)));
        assert_eq!(
            data.get("IAS_KT"),
            Some(&Value::Null),
            "removed keys go out as null"
        );
    }
}
