//! The saved side of the equipment page: presets, each variant's last fit
//! and what it starts the next flight with. The backend owns the file and
//! the restore (`infinity-app`'s `equipment` module); this follows its
//! profile and sends the user's edits.

use infinity_ipc::commands as cmd;
use infinity_ipc::dto::{
    EquipmentPreset, EquipmentPresetRef, EquipmentPresetSave, EquipmentProfile, EquipmentStartup,
};
use infinity_ipc::events::EquipmentProfileUpdated;
use infinity_ui::prelude::*;
use serde_json::Value;

use crate::configurator::widgets::describe;
use crate::ipc;

#[derive(Debug, Clone, Copy)]
pub(super) struct Presets {
    namespace: StoredValue<String>,
    pub profile: RwSignal<EquipmentProfile>,
    pub busy: RwSignal<bool>,
    pub error: RwSignal<Option<String>>,
}

impl Presets {
    pub(super) fn new(namespace: String) -> Self {
        let this = Self {
            namespace: StoredValue::new(namespace.clone()),
            profile: RwSignal::new(EquipmentProfile::default()),
            busy: RwSignal::new(false),
            error: RwSignal::new(None),
        };
        let ns = namespace.clone();
        ipc::listen::<EquipmentProfileUpdated>(move |change| {
            if change.namespace == ns {
                let _ = this.profile.try_set(change.profile);
            }
        });
        leptos::task::spawn_local(async move {
            match ipc::invoke::<cmd::EquipmentProfileGet>(&namespace).await {
                Ok(p) => {
                    let _ = this.profile.try_set(p);
                }
                Err(e) => {
                    let _ = this.error.try_set(Some(describe(&e)));
                }
            }
        });
        this
    }

    fn ns(self) -> String {
        self.namespace.get_value()
    }

    /// Runs one edit, taking the profile it answers with.
    fn run<F>(self, f: F)
    where
        F: std::future::Future<Output = infinity_ipc::IpcResult<EquipmentProfile>> + 'static,
    {
        if self.busy.get_untracked() {
            return;
        }
        self.busy.set(true);
        self.error.set(None);
        leptos::task::spawn_local(async move {
            match f.await {
                Ok(p) => {
                    let _ = self.profile.try_set(p);
                }
                Err(e) => {
                    let _ = self.error.try_set(Some(describe(&e)));
                }
            }
            let _ = self.busy.try_set(false);
        });
    }

    /// Saves `items` as a new preset, or over `id`'s.
    pub(super) fn save(self, id: Option<String>, name: String, variant: String, items: Vec<u32>) {
        let args = EquipmentPresetSave {
            namespace: self.ns(),
            preset: EquipmentPreset {
                id: id.unwrap_or_default(),
                name,
                variant,
                items,
                updated_at_ms: 0,
            },
        };
        self.run(async move { ipc::invoke::<cmd::EquipmentPresetSaveCmd>(&args).await });
    }

    pub(super) fn delete(self, id: String) {
        let args = EquipmentPresetRef {
            namespace: self.ns(),
            id,
        };
        self.run(async move { ipc::invoke::<cmd::EquipmentPresetDelete>(&args).await });
    }

    pub(super) fn set_startup(self, variant: String, preset_id: Option<String>) {
        let args = EquipmentStartup {
            namespace: self.ns(),
            variant,
            preset_id,
        };
        self.run(async move { ipc::invoke::<cmd::EquipmentStartupSet>(&args).await });
    }

    /// Loads a preset into the running aircraft; `on_snapshot` gets the
    /// fit it answers with.
    pub(super) fn apply(self, id: String, on_snapshot: impl Fn(&Value) + 'static) {
        if self.busy.get_untracked() {
            return;
        }
        self.busy.set(true);
        self.error.set(None);
        let args = EquipmentPresetRef {
            namespace: self.ns(),
            id,
        };
        leptos::task::spawn_local(async move {
            match ipc::invoke::<cmd::EquipmentPresetApply>(&args).await {
                Ok(raw) => on_snapshot(&raw),
                Err(e) => {
                    let _ = self.error.try_set(Some(describe(&e)));
                }
            }
            let _ = self.busy.try_set(false);
        });
    }
}

/// `items` with `idx` made the choice of its category, given the category
/// each option belongs to. A draft's radio press.
pub(super) fn select_in(items: &[u32], idx: u32, category_of: impl Fn(u32) -> Option<i64>) -> Vec<u32> {
    let cat = category_of(idx);
    let mut out: Vec<u32> = items
        .iter()
        .copied()
        .filter(|&i| i != idx && (cat.is_none() || category_of(i) != cat))
        .collect();
    out.push(idx);
    out.sort_unstable();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_draft_press_replaces_only_its_own_category() {
        // 1, 2 in category 0; 5, 6 in category 1.
        let cat = |i: u32| Some(if i < 5 { 0 } else { 1 });
        assert_eq!(select_in(&[1, 5], 2, cat), vec![2, 5]);
        assert_eq!(select_in(&[1, 5], 6, cat), vec![1, 6]);
        assert_eq!(select_in(&[], 6, cat), vec![6]);
    }
}
