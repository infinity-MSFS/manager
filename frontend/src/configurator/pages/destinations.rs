//! Destinations page (v3 `DestinationsPage.tsx`): `DST` keyed by number
//! (`201` ... `499`), each `{ lat, lon, elevation }`. List on the left, red
//! markers on the map. Clicking the map drops a destination; right-click
//! offers nearby waypoints.

use infinity_ipc::commands as cmd;
use infinity_ipc::nav::{NavNearest, WaypointData};
use infinity_ui::prelude::*;
use serde_json::{Map, Value, json};

use super::PageCtx;
use crate::configurator::map::{
    InteractiveMap, MapContextMenu, MapLayers, MapPoint, MapPointer, MenuMode, PointKind, US_CENTER,
};
use crate::configurator::schema::{num_at, number, object_mut, str_prop};
use crate::configurator::state::Configurator;
use crate::configurator::widgets::NumberField;
use crate::editorial::EYEBROW;
use crate::ipc;

const KEY_MIN: u32 = 201;
const KEY_MAX: u32 = 499;

/// The lowest free number in `201..=499`, else `201`.
pub(crate) fn next_key(existing: &Map<String, Value>) -> String {
    (KEY_MIN..=KEY_MAX)
        .map(|i| i.to_string())
        .find(|k| !existing.contains_key(k))
        .unwrap_or_else(|| KEY_MIN.to_string())
}

/// Numeric keys in numeric order, the rest after them alphabetically.
pub(crate) fn sorted_keys(dst: &Map<String, Value>) -> Vec<String> {
    let mut keys: Vec<String> = dst.keys().cloned().collect();
    keys.sort_by(|a, b| match (a.parse::<f64>(), b.parse::<f64>()) {
        (Ok(x), Ok(y)) => x.total_cmp(&y),
        _ => a.cmp(b),
    });
    keys
}

/// Whether `old` may become `new`: a non-empty, unused key.
pub(crate) fn can_rename(dst: &Map<String, Value>, old: &str, new: &str) -> bool {
    let new = new.trim();
    !new.is_empty() && new != old && !dst.contains_key(new) && dst.contains_key(old)
}

fn finite(d: &Value, key: &str) -> Option<f64> {
    num_at(d, key).filter(|n| n.is_finite())
}

fn dst_of(root: &Value) -> Map<String, Value> {
    root.get("DST")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn edit_dst(cfg: Configurator, f: impl FnOnce(&mut Map<String, Value>)) {
    cfg.update(|root| {
        if let Some(r) = object_mut(root)
            && let Some(dst) = object_mut(r.entry("DST").or_insert(Value::Null))
        {
            f(dst);
        }
    });
}

pub(super) fn page(ctx: PageCtx) -> AnyView {
    let cfg = ctx.cfg;
    let title = str_prop(&ctx.group, "title")
        .unwrap_or("Destinations")
        .to_owned();
    let dst = Memo::new(move |_| cfg.value.with(dst_of));
    let keys = Memo::new(move |_| dst.with(sorted_keys));
    let active = RwSignal::new(keys.with_untracked(|k| k.first().cloned()));

    let add_at = move |lat: f64, lon: f64, elevation: f64| {
        let key = dst.with_untracked(next_key);
        let value = json!({ "lat": lat, "lon": lon, "elevation": number(elevation) });
        let k = key.clone();
        edit_dst(cfg, move |m| {
            m.insert(k, value);
        });
        active.set(Some(key));
    };

    let menu = RwSignal::new(None::<MapPointer>);
    let menu_loading = RwSignal::new(false);
    let nearby = RwSignal::new(Vec::<WaypointData>::new());
    let on_right_click = Callback::new(move |at: MapPointer| {
        menu.set(Some(at));
        menu_loading.set(true);
        nearby.set(Vec::new());
        leptos::task::spawn_local(async move {
            let args = NavNearest {
                product: cfg.product_ref(),
                lat: at.lat,
                lon: at.lon,
                limit: Some(12),
            };
            let rows = ipc::invoke::<cmd::NavNearestWaypoints>(&args)
                .await
                .unwrap_or_default();
            if menu.try_get_untracked().flatten() == Some(at) {
                nearby.set(rows);
                menu_loading.set(false);
            }
        });
    });

    let layers = Signal::derive(move || {
        let points = dst.with(|m| {
            sorted_keys(m)
                .into_iter()
                .filter_map(|k| {
                    let d = m.get(&k)?;
                    Some(MapPoint {
                        id: format!("dst-{k}"),
                        lat: finite(d, "lat")?,
                        lon: finite(d, "lon")?,
                        label: Some(format!("DEST {k}")),
                        color: Some("#ef4444".to_owned()),
                        kind: PointKind::Destination,
                    })
                })
                .collect()
        });
        MapLayers {
            points,
            ..MapLayers::default()
        }
    });
    let center = Signal::derive(move || {
        dst.with(|m| {
            let at = |d: &Value| Some((finite(d, "lat")?, finite(d, "lon")?));
            active
                .get()
                .and_then(|k| m.get(&k))
                .and_then(at)
                .or_else(|| sorted_keys(m).iter().filter_map(|k| m.get(k)).find_map(at))
                .unwrap_or(US_CENTER)
        })
    });

    view! {
        <div class="flex flex-col">
            <header class="mb-8">
                <h2 class="text-3xl font-extralight tracking-tight">{title}</h2>
            </header>
            <div class="flex h-[680px] gap-6">
                <aside class="flex min-h-0 w-96 flex-col">
                    <div class="flex items-center justify-between border-b border-white/10 pb-3">
                        <span class=EYEBROW>
                            {move || {
                                let n = keys.with(Vec::len);
                                format!("{n:02} destination{}", if n == 1 { "" } else { "s" })
                            }}
                        </span>
                        <Button
                            variant=Variant::Glass
                            size=Size::Sm
                            icon=icons::PLUS
                            on_click=Callback::new(move |()| add_at(0.0, 0.0, 0.0))
                        >
                            "Add"
                        </Button>
                    </div>
                    <div class="flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto py-3 pr-1">
                        {move || {
                            keys.with(Vec::is_empty)
                                .then(|| {
                                    view! {
                                        <p class="py-6 text-center text-sm font-light text-white/40">
                                            "No destinations yet."
                                        </p>
                                    }
                                })
                        }}
                        <For each=move || keys.get() key=|k| k.clone() let:key>
                            <DestinationRow cfg=cfg key=key dst=dst keys=keys active=active />
                        </For>
                    </div>
                </aside>
                <div class="relative min-h-0 flex-1">
                    <InteractiveMap
                        center=center
                        zoom=8.0
                        layers=layers
                        on_click=Callback::new(move |(lat, lon)| add_at(lat, lon, 0.0))
                        on_right_click=on_right_click
                    />
                    <div class="pointer-events-none absolute bottom-4 left-4 z-[500] rounded-lg border border-white/10 bg-black/70 px-3 py-2 text-xs text-white/60">
                        "Click the map to drop a destination · Right-click for nearby waypoints"
                    </div>
                    {move || {
                        menu.get()
                            .map(|at| {
                                view! {
                                    <MapContextMenu
                                        at=at
                                        mode=MenuMode::Waypoints
                                        loading=menu_loading
                                        waypoints=nearby
                                        on_close=Callback::new(move |()| menu.set(None))
                                        on_waypoint=Callback::new(move |wp: WaypointData| {
                                            add_at(wp.latitude, wp.longitude, 0.0);
                                        })
                                    />
                                }
                            })
                    }}
                </div>
            </div>
        </div>
    }
    .into_any()
}

fn fmt(v: Option<f64>, digits: usize) -> String {
    v.map_or_else(|| "—".to_owned(), |n| format!("{n:.digits$}"))
}

#[component]
fn DestinationRow(
    cfg: Configurator,
    key: String,
    dst: Memo<Map<String, Value>>,
    keys: Memo<Vec<String>>,
    active: RwSignal<Option<String>>,
) -> impl IntoView {
    let key = StoredValue::new(key);
    let d =
        Memo::new(move |_| dst.with(|m| m.get(&key.get_value()).cloned().unwrap_or(Value::Null)));
    let selected = Signal::derive(move || active.get() == Some(key.get_value()));
    let detail = move || {
        d.with(|d| {
            let elev = finite(d, "elevation").map_or_else(|| "—".to_owned(), |e| format!("{e} ft"));
            format!(
                "{}, {} · {elev}",
                fmt(finite(d, "lat"), 4),
                fmt(finite(d, "lon"), 4)
            )
        })
    };
    let patch = move |field: &'static str, v: Value| {
        let k = key.get_value();
        edit_dst(cfg, move |m| {
            if let Some(entry) = m.get_mut(&k).and_then(object_mut) {
                entry.insert(field.to_owned(), v);
            }
        });
    };
    let remove = move |()| {
        let k = key.get_value();
        let was_active = active.get_untracked().as_deref() == Some(k.as_str());
        let gone = k.clone();
        edit_dst(cfg, move |m| {
            m.remove(&gone);
        });
        if was_active {
            active.set(keys.with_untracked(|ks| ks.first().cloned()));
        }
    };
    // The number commits on blur or Enter: renaming re-keys the row.
    let number_draft = RwSignal::new(key.get_value());
    let rename = move || {
        let old = key.get_value();
        let new = number_draft.get_untracked().trim().to_owned();
        if !dst.with_untracked(|m| can_rename(m, &old, &new)) {
            number_draft.set(old);
            return;
        }
        let (o, n) = (old.clone(), new.clone());
        edit_dst(cfg, move |m| {
            if let Some(v) = m.remove(&o) {
                m.insert(n, v);
            }
        });
        if active.get_untracked().as_deref() == Some(old.as_str()) {
            active.set(Some(new));
        }
    };
    let get = move |field: &'static str| {
        Signal::derive(move || d.with(|d| finite(d, field).unwrap_or_default()))
    };

    view! {
        <div
            class=move || {
                cn!(
                    "cursor-pointer border-l-2 py-3 pr-2 pl-3 transition-colors",
                    if selected.get() { "border-accent-400 bg-accent-500/[0.06]" } else { "border-white/10 hover:border-white/30 hover:bg-white/[0.03]" },
                )
            }
            on:click=move |_| active.set(Some(key.get_value()))
        >
            <div class="flex items-start gap-3">
                <span class="mt-0.5 font-mono text-[11px] text-white/40">"DEST"</span>
                <div class="min-w-0 flex-1">
                    <div class="truncate font-mono text-sm text-white">{key.get_value()}</div>
                    <div class="font-mono text-[11px] text-white/50">{detail}</div>
                </div>
                <IconButton
                    icon=icons::TRASH_2
                    label="Remove destination"
                    variant=Variant::Ghost
                    tone=Tone::Danger
                    size=Size::Xs
                    on_click=Callback::new(remove)
                />
            </div>
            <Show when=move || selected.get()>
                <div class="mt-3 grid grid-cols-2 gap-3" on:click=|ev| ev.stop_propagation()>
                    <div
                        class="col-span-2"
                        on:focusout=move |_| rename()
                        on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                            if ev.key() == "Enter" {
                                ev.prevent_default();
                                rename();
                            }
                        }
                    >
                        <Field label="Number">
                            <TextInput value=number_draft size=Size::Sm class="font-mono" on_input=Callback::new(move |s| number_draft.set(s)) />
                        </Field>
                    </div>
                    <NumberField label="Latitude" value=get("lat") on_change=Callback::new(move |v| patch("lat", number(v))) />
                    <NumberField label="Longitude" value=get("lon") on_change=Callback::new(move |v| patch("lon", number(v))) />
                    <div class="col-span-2">
                        <NumberField
                            label="Elevation (ft)"
                            value=get("elevation")
                            on_change=Callback::new(move |v: f64| patch("elevation", number(v.trunc())))
                        />
                    </div>
                </div>
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn keys_allocate_and_sort_numerically() {
        let m = map(json!({ "201": {}, "1000": {}, "203": {}, "abc": {} }));
        assert_eq!(next_key(&m), "202");
        assert_eq!(sorted_keys(&m), vec!["201", "203", "1000", "abc"]);
        assert_eq!(next_key(&Map::new()), "201");
    }

    #[test]
    fn renames_need_a_free_non_empty_key() {
        let m = map(json!({ "201": {}, "202": {} }));
        assert!(can_rename(&m, "201", "250"));
        assert!(!can_rename(&m, "201", "202"));
        assert!(!can_rename(&m, "201", " "));
        assert!(!can_rename(&m, "201", "201"));
        assert!(!can_rename(&m, "999", "250"));
    }
}
