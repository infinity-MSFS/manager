//! approachPick (v3 `ApproachPickWidget`): ILS / VOR for the sibling
//! airport and runway. Picking one resolves the approach and writes its
//! data into sibling fields in one edit; `targetMap` renames targets.

use std::collections::BTreeMap;

use infinity_ipc::commands as cmd;
use infinity_ipc::nav::{ApproachData, NavApproach, NavRunway};
use infinity_ui::prelude::*;
use serde_json::{self, Value};

use super::WidgetProps;
use super::common::{WidgetLabel, describe};
use crate::configurator::schema::{display_scalar, read_sibling, write_siblings};
use crate::ipc;

const DEFAULT_DEPENDS_ON: [&str; 2] = ["Airport_icao", "runway_name"];

/// `ApproachData` field to sibling field, as the T-38's `ApproachConfig`
/// names them.
const DEFAULT_TARGET_MAP: [(&str, &str); 15] = [
    ("Approch_type", "Approch_type"),
    ("Airport_icao", "Airport_icao"),
    ("navaid_type", "navaid_type"),
    ("navaid_freq", "navaid_freq"),
    ("navaid_course", "navaid_course"),
    ("rwy_lon", "rwy_lon"),
    ("rwy_lat", "rwy_lat"),
    ("rwy_elev", "rwy_elev"),
    ("fip_distance", "FIP_distance"),
    ("faf_distance", "FAF_distance"),
    ("app_gs", "app_gs"),
    ("fip_lat", "FIP_lat"),
    ("fip_lon", "FIP_lon"),
    ("faf_lat", "FAF_lat"),
    ("faf_lon", "FAF_lon"),
];

/// The default map with the schema's overrides applied.
pub(crate) fn target_map(overrides: Option<&Value>) -> BTreeMap<String, String> {
    let mut map: BTreeMap<String, String> = DEFAULT_TARGET_MAP
        .iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect();
    if let Some(Value::Object(o)) = overrides {
        for (k, v) in o {
            map.insert(k.clone(), v.as_str().unwrap_or_default().to_owned());
        }
    }
    map
}

/// Sibling writes for a resolved approach. `Approch_type` is left to the
/// widget's own value; an empty target skips the field.
pub(crate) fn sibling_updates(
    data: &ApproachData,
    targets: &BTreeMap<String, String>,
) -> Vec<(String, Value)> {
    let wire = serde_json::to_value(data).unwrap_or(Value::Null);
    targets
        .iter()
        .filter(|(nav, sibling)| nav.as_str() != "Approch_type" && !sibling.is_empty())
        .map(|(nav, sibling)| {
            (
                sibling.clone(),
                wire.get(nav).cloned().unwrap_or(Value::Null),
            )
        })
        .collect()
}

pub(super) fn approach_widget(props: WidgetProps) -> AnyView {
    let depends: Vec<String> = props
        .field
        .get("dependsOn")
        .and_then(Value::as_array)
        .filter(|a| a.len() >= 2)
        .map(|a| a.iter().map(display_scalar).collect())
        .unwrap_or_else(|| DEFAULT_DEPENDS_ON.iter().map(|s| (*s).to_owned()).collect());
    let airport_key = depends.first().cloned().unwrap_or_default();
    let runway_key = depends.get(1).cloned().unwrap_or_default();
    let targets = StoredValue::new(target_map(props.field.get("targetMap")));
    let cfg = props.ctx.cfg;
    let group_path = props.ctx.group_path.clone();
    let gp = group_path.clone();
    let siblings = Memo::new(move |_| {
        cfg.value.with(|root| {
            let read = |k: &str| {
                read_sibling(root, &gp, k)
                    .map(display_scalar)
                    .unwrap_or_default()
            };
            (read(&airport_key), read(&runway_key))
        })
    });
    let value = props.value;
    let on_change = props.on_change;
    let current = Memo::new(move |_| value.with(display_scalar).to_uppercase());
    let loading = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);

    let available = LocalResource::new(move || {
        let (icao, runway) = siblings.get();
        async move {
            if icao.is_empty() || runway.is_empty() {
                return (false, false);
            }
            let args = NavRunway {
                product: cfg.product_ref(),
                icao,
                runway,
            };
            ipc::invoke::<cmd::NavApproachTypes>(&args)
                .await
                .map(|r| (r.has_ils, r.has_vor))
                .unwrap_or((false, false))
        }
    });
    let has = move |kind: &str| {
        available
            .get()
            .is_some_and(|(ils, vor)| if kind == "ILS" { ils } else { vor })
    };
    let ready = Signal::derive(move || {
        let (a, r) = siblings.get();
        !a.is_empty() && !r.is_empty()
    });

    let pick = move |kind: &'static str| {
        let (icao, runway) = siblings.get_untracked();
        if icao.is_empty() || runway.is_empty() {
            return;
        }
        let group_path = group_path.clone();
        loading.set(true);
        error.set(None);
        leptos::task::spawn_local(async move {
            let args = NavApproach {
                product: cfg.product_ref(),
                icao,
                runway,
                approach_type: kind.to_owned(),
            };
            match ipc::invoke::<cmd::NavApproachData>(&args).await {
                Ok(data) => {
                    let updates = targets.with_value(|t| sibling_updates(&data, t));
                    // One write for the batch; the widget's own value goes
                    // through its callback so it stays in sync.
                    cfg.value
                        .update(|root| write_siblings(root, &group_path, updates));
                    on_change.run(Value::String(kind.to_owned()));
                }
                Err(e) => {
                    let _ = error.try_set(Some(describe(&e)));
                }
            }
            let _ = loading.try_set(false);
        });
    };
    let pick = StoredValue::new(pick);

    let button = move |kind: &'static str| {
        let enabled = Signal::derive(move || ready.get() && has(kind) && !loading.get());
        move || {
            let active = current.get() == kind;
            view! {
                <Button
                    size=Size::Sm
                    variant=if active { Variant::Solid } else { Variant::Glass }
                    disabled=Signal::derive(move || !enabled.get())
                    on_click=Callback::new(move |()| pick.with_value(|p| p(kind)))
                >
                    {kind}
                </Button>
            }
        }
    };

    view! {
        <div class="flex flex-col gap-1.5">
            <WidgetLabel label=props.field.get("label").map(display_scalar).unwrap_or_else(|| "Approach".to_owned()) />
            <div class="flex items-center gap-2">
                {button("ILS")}
                {button("VOR")}
                {move || loading.get().then(|| view! { <Spinner size=Size::Sm /> })}
            </div>
            {move || {
                let note = if !ready.get() {
                    Some("Select airport and runway first.")
                } else if available.get().is_some_and(|(i, v)| !i && !v) && !loading.get() {
                    Some("No published ILS or VOR approach for this runway.")
                } else {
                    None
                };
                note.map(|n| view! { <p class="text-xs font-light text-white/40">{n}</p> })
            }}
            {move || error.get().map(|e| view! { <p data-selectable class="text-xs text-red-300">{e}</p> })}
        </div>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn data() -> ApproachData {
        ApproachData {
            approch_type: "ILS".into(),
            airport_icao: "KRND".into(),
            navaid_type: "Localizer".into(),
            navaid_freq: 110.3,
            navaid_course: 151,
            rwy_lon: -98.0,
            rwy_lat: 29.5,
            rwy_elev: 700,
            fip_distance: 10.0,
            faf_distance: 5.0,
            app_gs: 3.0,
            fip_lat: 1.0,
            fip_lon: 2.0,
            faf_lat: 3.0,
            faf_lon: 4.0,
        }
    }

    #[test]
    fn default_map_writes_t38_field_names() {
        let updates: BTreeMap<_, _> = sibling_updates(&data(), &target_map(None))
            .into_iter()
            .collect();
        assert_eq!(updates.get("FIP_distance"), Some(&json!(10.0)));
        assert_eq!(updates.get("navaid_course"), Some(&json!(151)));
        assert_eq!(updates.get("Airport_icao"), Some(&json!("KRND")));
        assert!(!updates.contains_key("Approch_type"));
    }

    #[test]
    fn overrides_rename_or_skip_targets() {
        let t = target_map(Some(&json!({ "rwy_lat": "lat", "app_gs": "" })));
        let updates: BTreeMap<_, _> = sibling_updates(&data(), &t).into_iter().collect();
        assert_eq!(updates.get("lat"), Some(&json!(29.5)));
        assert!(!updates.contains_key("rwy_lat"));
        assert!(!updates.contains_key("app_gs"));
    }
}
