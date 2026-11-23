//! Saved equipment, per bridge namespace: `<app_data>/equipment/<ns>.json`.
//!
//! The aircraft owns the live fit; this is what survives between flights
//! (see [`EquipmentProfile`]). [`observe`] is the whole persistence rule,
//! kept pure so it is testable without a sim: fed every `equip_state` the
//! aircraft pushes, it records the user's changes and says when to restore.

use std::path::{Path, PathBuf};

use infinity_ipc::dto::{EquipmentPreset, EquipmentProfile};
use infinity_ipc::{IpcError, IpcErrorCode, IpcResult};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::atomic::{read_json_or_default, write_json};

#[derive(Debug)]
pub struct EquipmentStore {
    dir: PathBuf,
    write: Mutex<()>,
}

impl EquipmentStore {
    pub fn new(app_data: &Path) -> Self {
        Self {
            dir: app_data.join("equipment"),
            write: Mutex::new(()),
        }
    }

    fn path(&self, namespace: &str) -> IpcResult<PathBuf> {
        let ok = !namespace.is_empty()
            && namespace
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !ok {
            return Err(IpcError::new(
                IpcErrorCode::ValidationFailed,
                format!("not a bridge namespace: {namespace:?}"),
            ));
        }
        Ok(self.dir.join(format!("{namespace}.json")))
    }

    pub async fn load(&self, namespace: &str) -> IpcResult<EquipmentProfile> {
        read_json_or_default(&self.path(namespace)?).await
    }

    /// Read-modify-write under the store's lock. `edit` returns whether it
    /// changed anything; the file is only written when it did. Returns the
    /// resulting profile and that flag.
    pub async fn update<F>(&self, namespace: &str, edit: F) -> IpcResult<(EquipmentProfile, bool)>
    where
        F: FnOnce(&mut EquipmentProfile) -> IpcResult<bool>,
    {
        let path = self.path(namespace)?;
        let _guard = self.write.lock().await;
        let mut profile: EquipmentProfile = read_json_or_default(&path).await?;
        let changed = edit(&mut profile)?;
        if changed {
            write_json(&path, &profile).await?;
        }
        Ok((profile, changed))
    }
}

/// The parts of an `equip_state` snapshot persistence reads.
#[derive(Debug, Clone, PartialEq)]
pub struct Observed {
    pub variant: String,
    pub professional: bool,
    /// `default`, `restore`, `select` or `entitlement`; empty from an
    /// aircraft too old to report it.
    pub origin: String,
    pub restored: bool,
    pub selection: Vec<u32>,
    pub categories: Value,
}

impl Observed {
    /// `None` unless the snapshot is from a resolved, swappable aircraft
    /// new enough to report its selection: nothing else can be persisted
    /// or restored.
    pub fn parse(snapshot: &Value) -> Option<Self> {
        let o = snapshot.as_object()?;
        let variant = o.get("variant")?.as_str()?.to_owned();
        if o.get("swappable").and_then(Value::as_bool) != Some(true) {
            return None;
        }
        let selection = o
            .get("selection")?
            .as_array()?
            .iter()
            .filter_map(Value::as_u64)
            .filter_map(|i| u32::try_from(i).ok())
            .collect();
        Some(Self {
            variant,
            professional: o.get("professional").and_then(Value::as_bool) == Some(true),
            origin: o
                .get("origin")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            restored: o.get("restored").and_then(Value::as_bool) == Some(true),
            selection,
            categories: o.get("categories").cloned().unwrap_or(Value::Null),
        })
    }
}

/// What to do about one snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Observation {
    /// The profile changed and must be saved.
    pub changed: bool,
    /// Send `equip_apply {items, restore: true}`. `Some(vec![])` when
    /// nothing is saved: the aircraft still has to hear that the restore
    /// happened, or it would be asked again on every push.
    pub restore: Option<Vec<u32>>,
}

/// The persistence rule, applied to every snapshot the aircraft pushes.
///
/// * A fresh aircraft (`restored: false`) is restored to the variant's
///   startup preset or its last fit. Nothing it reports is recorded until
///   that has landed: its boot defaults would overwrite the saved fit.
/// * A USER change (`origin: select`, from the manager or the EFB) is
///   recorded as the variant's last fit. A restore, the boot defaults and an
///   entitlement repair are not choices, and recording the last would save
///   freeware fallbacks over Professional picks the gate puts back.
/// * The layout (`categories[]`) is cached for building presets offline.
///   A freeware layout never replaces a Professional one: it offers less.
pub fn observe(profile: &mut EquipmentProfile, seen: &Observed) -> Observation {
    let mut out = Observation::default();
    let has_layout = seen.categories.as_array().is_some_and(|a| !a.is_empty());
    let entry = profile.variant_mut(&seen.variant);
    let differs = entry.layout.as_ref() != Some(&seen.categories)
        || entry.layout_professional != seen.professional;
    if has_layout && (seen.professional || !entry.layout_professional) && differs {
        entry.layout = Some(seen.categories.clone());
        entry.layout_professional = seen.professional;
        out.changed = true;
    }
    if !seen.restored {
        out.restore = Some(profile.startup_items(&seen.variant).unwrap_or_default());
        return out;
    }
    if seen.origin == "select" {
        let entry = profile.variant_mut(&seen.variant);
        if entry.last_fit.as_ref() != Some(&seen.selection) {
            entry.last_fit = Some(seen.selection.clone());
            out.changed = true;
        }
    }
    out
}

/// Inserts or replaces `preset` (matched by id; an empty id gets a new one
/// from `now_ms`). Returns the stored id.
pub fn upsert_preset(profile: &mut EquipmentProfile, mut preset: EquipmentPreset, now_ms: u64) -> String {
    preset.updated_at_ms = now_ms;
    if preset.id.is_empty() {
        let mut n = 0u32;
        loop {
            let id = format!("p{now_ms:x}{n}");
            if profile.preset(&id).is_none() {
                preset.id = id;
                break;
            }
            n += 1;
        }
    }
    let id = preset.id.clone();
    match profile.presets.iter_mut().find(|p| p.id == id) {
        Some(slot) => *slot = preset,
        None => profile.presets.push(preset),
    }
    id
}

/// Removes a preset and every startup choice naming it. Whether it existed.
pub fn delete_preset(profile: &mut EquipmentProfile, id: &str) -> bool {
    let before = profile.presets.len();
    profile.presets.retain(|p| p.id != id);
    for v in &mut profile.variants {
        if v.startup.as_deref() == Some(id) {
            v.startup = None;
        }
    }
    profile.presets.len() != before
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn seen(origin: &str, restored: bool, selection: &[u32]) -> Observed {
        Observed {
            variant: "DC30L".into(),
            professional: true,
            origin: origin.into(),
            restored,
            selection: selection.to_vec(),
            categories: json!([{ "categoryIdx": 1 }]),
        }
    }

    fn preset(name: &str, items: &[u32]) -> EquipmentPreset {
        EquipmentPreset {
            id: String::new(),
            name: name.into(),
            variant: "DC30L".into(),
            items: items.to_vec(),
            updated_at_ms: 0,
        }
    }

    #[test]
    fn a_fresh_aircraft_is_restored_and_its_defaults_are_not_recorded() {
        let mut p = EquipmentProfile::default();
        p.variant_mut("DC30L").last_fit = Some(vec![4, 7]);
        let o = observe(&mut p, &seen("default", false, &[1, 2]));
        assert_eq!(o.restore, Some(vec![4, 7]));
        assert_eq!(p.variant("DC30L").and_then(|v| v.last_fit.clone()), Some(vec![4, 7]));
    }

    #[test]
    fn with_nothing_saved_the_restore_is_still_sent_empty() {
        let mut p = EquipmentProfile::default();
        let o = observe(&mut p, &seen("default", false, &[1]));
        assert_eq!(o.restore, Some(vec![]));
    }

    #[test]
    fn a_startup_preset_wins_over_the_last_fit_until_it_is_deleted() {
        let mut p = EquipmentProfile::default();
        p.variant_mut("DC30L").last_fit = Some(vec![4]);
        let id = upsert_preset(&mut p, preset("Long haul", &[9]), 10);
        p.variant_mut("DC30L").startup = Some(id.clone());
        assert_eq!(observe(&mut p, &seen("default", false, &[])).restore, Some(vec![9]));
        assert!(delete_preset(&mut p, &id));
        assert_eq!(p.variant("DC30L").and_then(|v| v.startup.clone()), None);
        assert_eq!(observe(&mut p, &seen("default", false, &[])).restore, Some(vec![4]));
    }

    #[test]
    fn only_user_choices_are_recorded() {
        let mut p = EquipmentProfile::default();
        for origin in ["restore", "entitlement", "default", ""] {
            let o = observe(&mut p, &seen(origin, true, &[1, 2]));
            assert_eq!(o.restore, None, "{origin}");
            assert_eq!(p.variant("DC30L").and_then(|v| v.last_fit.clone()), None, "{origin}");
        }
        let o = observe(&mut p, &seen("select", true, &[3, 5]));
        assert!(o.changed);
        assert_eq!(p.variant("DC30L").and_then(|v| v.last_fit.clone()), Some(vec![3, 5]));
        // The 2 s re-push of the same fit writes nothing.
        assert!(!observe(&mut p, &seen("select", true, &[3, 5])).changed);
    }

    #[test]
    fn a_freeware_layout_never_replaces_a_professional_one() {
        let mut p = EquipmentProfile::default();
        observe(&mut p, &seen("select", true, &[]));
        let mut free = seen("select", true, &[]);
        free.professional = false;
        free.categories = json!([{ "categoryIdx": 2 }]);
        observe(&mut p, &free);
        let v = p.variant("DC30L").cloned().unwrap_or_default();
        assert!(v.layout_professional);
        assert_eq!(v.layout, Some(json!([{ "categoryIdx": 1 }])));
    }

    #[test]
    fn parse_needs_a_swappable_resolved_aircraft_with_a_selection() {
        let ok = json!({ "variant": "DC30L", "swappable": true, "professional": true,
            "origin": "select", "restored": true, "selection": [1, 2], "categories": [] });
        let o = Observed::parse(&ok);
        assert_eq!(o.map(|o| o.selection), Some(vec![1, 2]));
        for bad in [
            json!({ "variant": null, "swappable": true, "selection": [] }),
            json!({ "variant": "KC10A", "swappable": false, "selection": [] }),
            json!({ "variant": "DC30L", "swappable": true }),
        ] {
            assert_eq!(Observed::parse(&bad), None, "{bad}");
        }
    }

    #[test]
    fn upsert_replaces_by_id_and_assigns_unique_ids() {
        let mut p = EquipmentProfile::default();
        let a = upsert_preset(&mut p, preset("A", &[1]), 5);
        let b = upsert_preset(&mut p, preset("B", &[2]), 5);
        assert_ne!(a, b);
        let mut edited = preset("A2", &[3]);
        edited.id = a.clone();
        upsert_preset(&mut p, edited, 6);
        assert_eq!(p.presets.len(), 2);
        assert_eq!(p.preset(&a).map(|x| x.name.as_str()), Some("A2"));
    }

    #[tokio::test]
    async fn the_store_round_trips_and_rejects_bad_namespaces() {
        let dir = tempfile::tempdir().unwrap();
        let store = EquipmentStore::new(dir.path());
        assert_eq!(store.load("dc10").await.unwrap(), EquipmentProfile::default());
        let (_, changed) = store
            .update("dc10", |p| {
                upsert_preset(p, preset("A", &[1]), 1);
                Ok(true)
            })
            .await
            .unwrap();
        assert!(changed);
        assert_eq!(store.load("dc10").await.unwrap().presets.len(), 1);
        assert!(store.load("../x").await.is_err());
    }
}
