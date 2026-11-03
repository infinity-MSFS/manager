//! The loaded configuration and its autosave (v3 `useConfigurator.ts`).
//!
//! One `RwSignal<Value>` holds the whole value tree; widgets read their
//! slice through memos so an edit re-renders only what changed. Every edit
//! schedules a save 500 ms later; edits during an in-flight save are
//! written straight after it rather than dropped. `liveBridge` products
//! keep their state in the aircraft, so nothing here touches persistence.

use infinity_ipc::IpcError;
use infinity_ipc::catalog::{ConfiguratorTransport, Product};
use infinity_ipc::commands as cmd;
use infinity_ipc::dto::{ConfigEnvelope, ConfiguratorSave, ProductRef};
use infinity_ui::prelude::*;
use serde_json::{Map, Value};

use super::schema::{path_set, path_value};
use super::toast::Toasts;
use crate::ipc;

const SAVE_DEBOUNCE_MS: u32 = 500;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SaveState {
    /// Nothing changed since load.
    Clean,
    /// An edit is waiting out the debounce.
    Pending,
    Saving,
    Saved,
    Failed(IpcError),
}

#[derive(Debug, Default)]
struct Queue {
    /// Bumped on every edit.
    edits: u64,
    /// The edit count the last successful save covered.
    saved: u64,
    in_flight: bool,
}

/// The configurator for one product, shared by the chrome, renderer,
/// widgets and pages through context.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Configurator {
    pub product: StoredValue<Product>,
    pub product_ref: StoredValue<ProductRef>,
    /// The whole value tree.
    pub value: RwSignal<Value>,
    schema_version: StoredValue<u32>,
    pub migrated: RwSignal<bool>,
    pub save_state: RwSignal<SaveState>,
    pub toasts: Toasts,
    /// Values live in the aircraft (`transport: live_bridge`).
    pub live: bool,
    queue: StoredValue<Queue>,
}

impl Configurator {
    pub(crate) fn new(product: Product, product_ref: ProductRef, toasts: Toasts) -> Self {
        let live = product
            .configurator
            .as_ref()
            .and_then(|c| c.transport)
            .is_some_and(|t| t == ConfiguratorTransport::LiveBridge);
        Self {
            product: StoredValue::new(product),
            product_ref: StoredValue::new(product_ref),
            value: RwSignal::new(Value::Object(Map::new())),
            schema_version: StoredValue::new(1),
            migrated: RwSignal::new(false),
            save_state: RwSignal::new(SaveState::Clean),
            toasts,
            live,
            queue: StoredValue::new(Queue::default()),
        }
    }

    pub(crate) fn product_ref(self) -> ProductRef {
        self.product_ref.get_value()
    }

    /// Loads (and migrates) the saved configuration, replacing local state.
    pub(crate) async fn load(self) -> Result<(), IpcError> {
        if self.live {
            self.value.set(Value::Object(Map::new()));
            return Ok(());
        }
        let loaded = ipc::invoke::<cmd::ConfiguratorLoad>(&self.product_ref()).await?;
        self.replace(loaded.envelope);
        let _ = self.migrated.try_set(loaded.migrated);
        Ok(())
    }

    /// Mirrors an envelope the backend already holds. Cancels any pending
    /// autosave: the file is newer than the edit it would write.
    pub(crate) fn replace(self, envelope: ConfigEnvelope) {
        let _ = self.queue.try_update_value(|q| q.saved = q.edits);
        let _ = self.schema_version.try_set_value(envelope.schema_version);
        let _ = self.value.try_set(envelope.value);
        let _ = self.save_state.try_set(SaveState::Clean);
    }

    /// The value at `path`, tracked.
    pub(crate) fn at(self, path: String) -> Memo<Value> {
        let value = self.value;
        Memo::new(move |_| value.with(|v| path_value(v, &path)))
    }

    /// Edits the value tree in place and schedules a save.
    pub(crate) fn update(self, edit: impl FnOnce(&mut Value)) {
        self.value.update(edit);
        self.touch();
    }

    /// Sets one path and schedules a save.
    pub(crate) fn set_at(self, path: &str, next: Value) {
        self.update(|root| path_set(root, path, next));
    }

    fn touch(self) {
        if self.live {
            return;
        }
        let Some(edit) = self.queue.try_update_value(|q| {
            q.edits += 1;
            q.edits
        }) else {
            return;
        };
        self.save_state.set(SaveState::Pending);
        leptos::task::spawn_local(async move {
            ipc::sleep(SAVE_DEBOUNCE_MS).await;
            if self.queue.try_with_value(|q| q.edits) == Some(edit) {
                self.save().await;
            }
        });
    }

    fn dirty(self) -> bool {
        self.queue
            .try_with_value(|q| q.edits > q.saved)
            .unwrap_or(false)
    }

    /// Writes any unsaved edit now. Outputs and shares read the saved file,
    /// so actions call this first. `Err` when the save failed.
    pub(crate) async fn flush(self) -> Result<(), IpcError> {
        if self.live {
            return Ok(());
        }
        // Wait out a save already running; it may not cover the latest edit.
        while self.queue.try_with_value(|q| q.in_flight) == Some(true) {
            ipc::sleep(50).await;
        }
        if self.dirty() {
            self.save().await;
        }
        match self.save_state.try_get_untracked() {
            Some(SaveState::Failed(e)) => Err(e),
            _ => Ok(()),
        }
    }

    async fn save(self) {
        let started = self.queue.try_update_value(|q| {
            if q.in_flight {
                false
            } else {
                q.in_flight = true;
                true
            }
        });
        if started != Some(true) {
            // The running save loops until the queue is clean.
            return;
        }
        loop {
            let Some(edit) = self.queue.try_with_value(|q| q.edits) else {
                return;
            };
            let Some(envelope) = self.envelope() else {
                return;
            };
            let _ = self.save_state.try_set(SaveState::Saving);
            let args = ConfiguratorSave {
                product: self.product_ref.get_value(),
                envelope,
            };
            match ipc::invoke::<cmd::ConfiguratorSave>(&args).await {
                Ok(()) => {
                    let _ = self.queue.try_update_value(|q| q.saved = q.saved.max(edit));
                    if !self.dirty() {
                        let _ = self.save_state.try_set(SaveState::Saved);
                        break;
                    }
                }
                Err(e) => {
                    let _ = self.save_state.try_set(SaveState::Failed(e));
                    break;
                }
            }
        }
        let _ = self.queue.try_update_value(|q| q.in_flight = false);
    }

    fn envelope(self) -> Option<ConfigEnvelope> {
        Some(ConfigEnvelope {
            schema_version: self.schema_version.try_get_value()?,
            product_id: self.product_ref.try_with_value(|p| p.product_id.clone())?,
            value: self.value.try_get_untracked()?,
        })
    }

    /// Saves an unsaved edit when the page goes away, so leaving within the
    /// debounce keeps it. The save outlives the page's reactive owner.
    pub(crate) fn save_on_cleanup(self) {
        on_cleanup(move || {
            if self.live || !self.dirty() {
                return;
            }
            let (Some(envelope), Some(product)) =
                (self.envelope(), self.product_ref.try_get_value())
            else {
                return;
            };
            leptos::task::spawn_local(async move {
                let args = ConfiguratorSave { product, envelope };
                if let Err(e) = ipc::invoke::<cmd::ConfiguratorSave>(&args).await {
                    ipc::warn(&format!("configurator: final save failed: {e}"));
                }
            });
        });
    }
}
