//! Live equipment over a catalog-declared bridge (v3
//! `useBridgeEquipment.ts`). Nothing is persisted: the aircraft owns the
//! fit. The gauge answers `equip_query` with a snapshot, applies
//! `equip_cmd`, and pushes `equip_state` whenever the fit changes, so the
//! page queries once and then follows the pushes. A cheap status poll only
//! drives the badge and re-queries when the gauge comes up.

use infinity_ipc::commands as cmd;
use infinity_ipc::dto::BridgeCommand;
use infinity_ipc::events::{BridgeConnectionChanged, BridgeEventReceived};
use infinity_ui::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::configurator::widgets::describe;
use crate::ipc;

const QUERY: &str = "equip_query";
const APPLY: &str = "equip_cmd";
const STATE_EVENT: &str = "equip_state";
/// `equip_cmd` opcode. Every category is a radio group, so select is the
/// only verb.
const CMD_SELECT: i64 = 2;
const CONNECTION_POLL_MS: u32 = 3000;
/// First retry delay after a failed query; doubles up to the cap. Covers
/// the sim sitting on a loading screen with the module not up yet.
const RETRY_BASE_MS: u32 = 1000;
const RETRY_MAX_MS: u32 = 15000;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct EquipmentOption {
    pub idx: i64,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub active: bool,
    /// `freeware`, `professional` or `deluxe`, when reported.
    #[serde(default)]
    pub tier: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct EquipmentCategory {
    pub category_idx: i64,
    #[serde(default)]
    pub category: String,
    /// False for groups the variant forces.
    #[serde(default)]
    pub selectable: bool,
    #[serde(default)]
    pub options: Vec<EquipmentOption>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub(super) struct Snapshot {
    /// ATC MODEL of the detected airframe, once resolved.
    pub variant: Option<String>,
    pub swappable: bool,
    pub professional: bool,
    pub categories: Vec<EquipmentCategory>,
}

/// A snapshot from the wire; `None` when it is not an object. Malformed
/// categories are skipped rather than failing the snapshot.
pub(super) fn parse_snapshot(raw: &Value) -> Option<Snapshot> {
    let o = raw.as_object()?;
    let categories = o
        .get("categories")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|c| serde_json::from_value(c.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    Some(Snapshot {
        variant: o.get("variant").and_then(Value::as_str).map(str::to_owned),
        swappable: o.get("swappable").and_then(Value::as_bool) == Some(true),
        professional: o.get("professional").and_then(Value::as_bool) == Some(true),
        categories,
    })
}

#[derive(Debug, Default)]
struct Control {
    /// Bumped by every newer snapshot so a late answer cannot overwrite it.
    generation: u64,
    in_flight: bool,
    retry_delay: u32,
    /// Bumped to cancel a scheduled retry.
    retry_token: u64,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Equipment {
    namespace: StoredValue<String>,
    pub snapshot: RwSignal<Option<Snapshot>>,
    pub connected: RwSignal<bool>,
    pub loading: RwSignal<bool>,
    pub error: RwSignal<Option<String>>,
    pub pending: RwSignal<Option<i64>>,
    control: StoredValue<Control>,
}

impl Equipment {
    pub(super) fn new(namespace: String) -> Self {
        let this = Self {
            namespace: StoredValue::new(namespace.clone()),
            snapshot: RwSignal::new(None),
            connected: RwSignal::new(false),
            loading: RwSignal::new(false),
            error: RwSignal::new(None),
            pending: RwSignal::new(None),
            control: StoredValue::new(Control {
                retry_delay: RETRY_BASE_MS,
                ..Control::default()
            }),
        };

        // Follow pushes. Attached before the first query so a change during
        // the round trip is not lost.
        let ns = namespace.clone();
        ipc::listen::<BridgeEventReceived>(move |msg| {
            if msg.namespace != ns || msg.name != STATE_EVENT {
                return;
            }
            if let Some(s) = parse_snapshot(&msg.data) {
                // A push is newer than any query in flight and proves the
                // gauge is answering: stand any retry down.
                let _ = this.control.try_update_value(|c| {
                    c.generation += 1;
                    c.retry_token += 1;
                    c.retry_delay = RETRY_BASE_MS;
                });
                this.snapshot.set(Some(s));
                this.loading.set(false);
                this.error.set(None);
            }
        });
        let ns = namespace;
        ipc::listen::<BridgeConnectionChanged>(move |change| {
            if change.namespace == ns {
                this.on_status(change.connected, false);
            }
        });
        leptos::task::spawn_local(this.poll());
        this
    }

    fn alive(self) -> bool {
        self.control.try_with_value(|_| ()).is_some()
    }

    /// Badge state, and a re-query on a rising edge of either signal:
    /// `connected` is the relay socket (up as soon as the sim panel loads),
    /// `ready` the aircraft module behind it.
    fn on_status(self, connected: bool, ready_edge: bool) {
        let Some(was) = self.connected.try_get_untracked() else {
            return;
        };
        self.connected.set(connected);
        if (connected && !was) || ready_edge {
            leptos::task::spawn_local(self.refresh());
        }
    }

    async fn poll(self) {
        let mut last_ready = false;
        while self.alive() {
            let Some(ns) = self.namespace.try_get_value() else {
                return;
            };
            let (connected, ready) = match ipc::invoke::<cmd::BridgeStatusList>(&()).await {
                Ok(all) => all
                    .iter()
                    .find(|b| b.namespace == ns)
                    .map_or((false, false), |b| (b.connected, b.connected && b.ready)),
                Err(_) => (false, false),
            };
            if !self.alive() {
                return;
            }
            self.on_status(connected, ready && !last_ready);
            last_ready = ready;
            ipc::sleep(CONNECTION_POLL_MS).await;
        }
    }

    /// Queries the full snapshot; on failure retries with backoff.
    pub(super) async fn refresh(self) {
        let started = self.control.try_update_value(|c| {
            c.retry_token += 1;
            if c.in_flight {
                return None;
            }
            c.in_flight = true;
            c.generation += 1;
            Some(c.generation)
        });
        let Some(Some(generation)) = started else {
            return;
        };
        let Some(namespace) = self.namespace.try_get_value() else {
            return;
        };
        self.loading.set(true);
        self.error.set(None);
        let args = BridgeCommand {
            namespace,
            command: QUERY.to_owned(),
            payload: Value::Null,
        };
        let result = ipc::invoke::<cmd::BridgeCommandSend>(&args).await;
        let Some(current) = self.control.try_update_value(|c| {
            c.in_flight = false;
            c.generation
        }) else {
            return;
        };
        if current != generation {
            return;
        }
        match result {
            Ok(raw) => {
                if let Some(s) = parse_snapshot(&raw) {
                    self.snapshot.set(Some(s));
                }
                let _ = self
                    .control
                    .try_update_value(|c| c.retry_delay = RETRY_BASE_MS);
            }
            Err(e) => {
                self.error.set(Some(describe(&e)));
                self.schedule_retry();
            }
        }
        self.loading.set(false);
    }

    fn schedule_retry(self) {
        let Some((delay, token)) = self.control.try_update_value(|c| {
            let delay = c.retry_delay;
            c.retry_delay = (delay * 2).min(RETRY_MAX_MS);
            c.retry_token += 1;
            (delay, c.retry_token)
        }) else {
            return;
        };
        leptos::task::spawn_local(async move {
            ipc::sleep(delay).await;
            if self.control.try_with_value(|c| c.retry_token) == Some(token) {
                self.refresh().await;
            }
        });
    }

    /// Selects option `idx` within its category.
    pub(super) fn select(self, idx: i64) {
        if self.pending.get_untracked().is_some() {
            return;
        }
        let Some(namespace) = self.namespace.try_get_value() else {
            return;
        };
        self.pending.set(Some(idx));
        self.error.set(None);
        leptos::task::spawn_local(async move {
            let args = BridgeCommand {
                namespace,
                command: APPLY.to_owned(),
                payload: json!({ "equipIdx": idx, "cmd": CMD_SELECT }),
            };
            match ipc::invoke::<cmd::BridgeCommandSend>(&args).await {
                Ok(raw) => {
                    if let Some(s) = parse_snapshot(&raw) {
                        let _ = self.control.try_update_value(|c| c.generation += 1);
                        let _ = self.snapshot.try_set(Some(s));
                    }
                }
                Err(e) => {
                    let _ = self.error.try_set(Some(describe(&e)));
                }
            }
            let _ = self.pending.try_set(None);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_parse_leniently() {
        let s = parse_snapshot(&json!({
            "variant": "DC10E",
            "swappable": true,
            "professional": false,
            "categories": [
                { "categoryIdx": 0, "category": "Radios", "selectable": true,
                  "options": [ { "idx": 4, "label": "A", "available": true, "active": true, "tier": "professional" } ] },
                { "nonsense": 1 }
            ]
        }))
        .unwrap_or_default();
        assert_eq!(s.variant.as_deref(), Some("DC10E"));
        assert!(s.swappable && !s.professional);
        assert_eq!(s.categories.len(), 1);
        assert_eq!(
            s.categories[0].options[0].tier.as_deref(),
            Some("professional")
        );
        assert_eq!(parse_snapshot(&json!(null)), None);
        assert_eq!(
            parse_snapshot(&json!({})).map(|s| s.categories.len()),
            Some(0)
        );
    }
}
