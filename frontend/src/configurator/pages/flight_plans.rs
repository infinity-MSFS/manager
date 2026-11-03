//! Flight Plans page (v3 `FlightPlansPage.tsx`): `FLIGHT_PLANS` keyed
//! `00`, `01`, ... each `{ name, waypoints: [{ ident, lat, lon }] }`. Plan
//! list and waypoint editor on the left, every plan drawn on the map with
//! the active one highlighted. Waypoints come from the navigation
//! database, the group's `customWaypoints`, `DEST XX` entries of the `DST`
//! section, a pasted route, or a right-click on the map.

mod route;

use infinity_ipc::commands as cmd;
use infinity_ipc::nav::{NavNearest, NavSearch, NavWaypoint, WaypointData};
use infinity_ui::prelude::*;
use serde_json::{Map, Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;

use self::route::{
    CustomWaypoint, DestEntry, Waypoint, compile, is_dest_prefix, local_suggestions, merge_results,
    next_plan_key, read_custom_waypoints, read_destinations, resolve_local, tokens,
};
use super::PageCtx;
use crate::configurator::map::{
    InteractiveMap, MapContextMenu, MapLayers, MapPointer, MapRoute, MenuMode, US_CENTER,
};
use crate::configurator::schema::{num_at, object_mut, str_at, str_prop};
use crate::configurator::state::Configurator;
use crate::configurator::widgets::{
    SuggestionItem, Suggestions, WidgetLabel, debounce, waypoint_detail,
};
use crate::editorial::EYEBROW;
use crate::ipc;

/// An edit to a plan's waypoint list.
type WaypointEdit = Box<dyn FnOnce(&mut Vec<Value>)>;

const SEARCH_DEBOUNCE_MS: u32 = 250;
const ACTIVE_COLOR: &str = "#8b5cf6";
const INACTIVE_COLOR: &str = "#64748b";

/// lucide `copy`.
const COPY: IconData = IconData::new(
    r#"<rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/>"#,
);

fn plans_of(root: &Value) -> Map<String, Value> {
    root.get("FLIGHT_PLANS")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn edit_plans(cfg: Configurator, f: impl FnOnce(&mut Map<String, Value>)) {
    cfg.update(|root| {
        if let Some(r) = object_mut(root)
            && let Some(plans) = object_mut(r.entry("FLIGHT_PLANS").or_insert(Value::Null))
        {
            f(plans);
        }
    });
}

fn edit_plan(cfg: Configurator, key: &str, f: impl FnOnce(&mut Map<String, Value>)) {
    edit_plans(cfg, |plans| {
        if let Some(plan) = plans.get_mut(key).and_then(object_mut) {
            f(plan);
        }
    });
}

fn waypoints_of(plan: &Value) -> Vec<Value> {
    plan.get("waypoints")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn set_waypoints(cfg: Configurator, key: &str, waypoints: Vec<Value>) {
    edit_plan(cfg, key, |plan| {
        plan.insert("waypoints".to_owned(), Value::Array(waypoints));
    });
}

fn drawable(w: &Value) -> Option<(String, f64, f64)> {
    let ident = w.get("ident")?.as_str()?.to_owned();
    let lat = w.get("lat")?.as_f64().filter(|n| n.is_finite())?;
    let lon = w.get("lon")?.as_f64().filter(|n| n.is_finite())?;
    Some((ident, lat, lon))
}

fn routes(plans: &Map<String, Value>, active: Option<&str>) -> Vec<MapRoute> {
    plans
        .iter()
        .map(|(id, p)| MapRoute {
            id: id.clone(),
            name: str_prop(p, "name")
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Flight Plan {id}")),
            waypoints: waypoints_of(p).iter().filter_map(drawable).collect(),
            color: Some(
                if Some(id.as_str()) == active {
                    ACTIVE_COLOR
                } else {
                    INACTIVE_COLOR
                }
                .to_owned(),
            ),
        })
        .filter(|r| !r.waypoints.is_empty())
        .collect()
}

fn center(plans: &Map<String, Value>, active: Option<&str>) -> (f64, f64) {
    let first = |p: &Value| {
        waypoints_of(p)
            .iter()
            .find_map(drawable)
            .map(|(_, a, b)| (a, b))
    };
    active
        .and_then(|k| plans.get(k))
        .and_then(first)
        .or_else(|| plans.values().find_map(first))
        .unwrap_or(US_CENTER)
}

pub(super) fn page(ctx: PageCtx) -> AnyView {
    let cfg = ctx.cfg;
    let customs = StoredValue::new(read_custom_waypoints(&ctx.group));
    let title = str_prop(&ctx.group, "title")
        .unwrap_or("Flight Plans")
        .to_owned();
    let plans = Memo::new(move |_| cfg.value.with(plans_of));
    let keys = Memo::new(move |_| plans.with(|p| p.keys().cloned().collect::<Vec<_>>()));
    let dests = Memo::new(move |_| cfg.value.with(read_destinations));
    let active = RwSignal::new(keys.with_untracked(|k| k.first().cloned()));

    let add_plan = move |()| {
        let key = plans.with_untracked(next_plan_key);
        let blank = json!({ "name": format!("Flight Plan {key}"), "waypoints": [] });
        let k = key.clone();
        edit_plans(cfg, move |p| {
            p.insert(k, blank);
        });
        active.set(Some(key));
    };

    // Right-click: nearby waypoints for the active plan.
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
    let append_to_active = Callback::new(move |wp: WaypointData| {
        let Some(key) = active.get_untracked() else {
            return;
        };
        if !plans.with_untracked(|p| p.contains_key(&key)) {
            return;
        }
        let value = Waypoint::from(&wp).to_value();
        edit_plan(cfg, &key, move |plan| {
            let mut wps = waypoints_of(&Value::Object(plan.clone()));
            wps.push(value);
            plan.insert("waypoints".to_owned(), Value::Array(wps));
        });
    });

    let layers = Signal::derive(move || MapLayers {
        routes: plans.with(|p| routes(p, active.get().as_deref())),
        ..MapLayers::default()
    });
    let map_center = Signal::derive(move || plans.with(|p| center(p, active.get().as_deref())));

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
                                format!("{n:02} plan{}", if n == 1 { "" } else { "s" })
                            }}
                        </span>
                        <Button variant=Variant::Glass size=Size::Sm icon=icons::PLUS on_click=Callback::new(add_plan)>
                            "New plan"
                        </Button>
                    </div>
                    <div class="flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto py-3 pr-1">
                        {move || {
                            keys.with(Vec::is_empty)
                                .then(|| {
                                    view! {
                                        <p class="py-6 text-center text-sm font-light text-white/40">
                                            "No flight plans yet."
                                        </p>
                                    }
                                })
                        }}
                        <For each=move || keys.get() key=|k| k.clone() let:key>
                            <PlanRow cfg=cfg key=key plans=plans active=active customs=customs dests=dests />
                        </For>
                    </div>
                </aside>
                <div class="relative min-h-0 flex-1">
                    <InteractiveMap center=map_center zoom=8.0 layers=layers on_right_click=on_right_click />
                    <div class="pointer-events-none absolute bottom-4 left-4 z-[500] rounded-lg border border-white/10 bg-black/70 px-3 py-2 text-xs text-white/60">
                        "Right-click the map to add a nearby waypoint to the active plan"
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
                                        on_waypoint=append_to_active
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

#[component]
fn PlanRow(
    cfg: Configurator,
    key: String,
    plans: Memo<Map<String, Value>>,
    active: RwSignal<Option<String>>,
    customs: StoredValue<Vec<CustomWaypoint>>,
    dests: Memo<Vec<DestEntry>>,
) -> impl IntoView {
    let k = key.clone();
    let plan = Memo::new(move |_| plans.with(|p| p.get(&k).cloned().unwrap_or(Value::Null)));
    let k = key.clone();
    let selected = Signal::derive(move || active.get().as_deref() == Some(k.as_str()));
    let k = key.clone();
    let title = move || {
        plan.with(|p| {
            let n = str_at(p, "name");
            if n.is_empty() { format!("Plan {k}") } else { n }
        })
    };
    let count = move || {
        let n = plan.with(|p| waypoints_of(p).len());
        format!("{n} waypoint{}", if n == 1 { "" } else { "s" })
    };
    let select_key = key.clone();
    let remove_key = key.clone();
    let remove = move |()| {
        let k = remove_key.clone();
        edit_plans(cfg, |p| {
            p.remove(&k);
        });
        if active.get_untracked().as_deref() == Some(remove_key.as_str()) {
            active.set(plans.with_untracked(|p| p.keys().next().cloned()));
        }
    };
    let editor_key = StoredValue::new(key.clone());

    view! {
        <div
            class=move || {
                cn!(
                    "cursor-pointer border-l-2 py-3 pr-2 pl-3 transition-colors",
                    if selected.get() { "border-accent-400 bg-accent-500/[0.06]" } else { "border-white/10 hover:border-white/30 hover:bg-white/[0.03]" },
                )
            }
            on:click=move |_| active.set(Some(select_key.clone()))
        >
            <div class="flex items-start gap-3">
                <span class="mt-0.5 font-mono text-[11px] text-accent-400/80">{key.clone()}</span>
                <div class="min-w-0 flex-1">
                    <div class="truncate text-sm text-white">{title}</div>
                    <div class="text-xs text-white/50">{count}</div>
                </div>
                <IconButton
                    icon=icons::TRASH_2
                    label="Remove plan"
                    variant=Variant::Ghost
                    tone=Tone::Danger
                    size=Size::Xs
                    on_click=Callback::new(remove)
                />
            </div>
            <Show when=move || selected.get()>
                <div class="mt-3 flex flex-col gap-4" on:click=|ev| ev.stop_propagation()>
                    <PlanEditor cfg=cfg key=editor_key.get_value() plan=plan customs=customs dests=dests />
                    <CompiledPlan cfg=cfg key=editor_key.get_value() plan=plan customs=customs dests=dests />
                </div>
            </Show>
        </div>
    }
}

#[component]
fn PlanEditor(
    cfg: Configurator,
    key: String,
    plan: Memo<Value>,
    customs: StoredValue<Vec<CustomWaypoint>>,
    dests: Memo<Vec<DestEntry>>,
) -> impl IntoView {
    let key = StoredValue::new(key);
    let waypoints = Memo::new(move |_| plan.with(waypoints_of));
    let len = Memo::new(move |_| waypoints.with(Vec::len));
    let name = Signal::derive(move || plan.with(|p| str_at(p, "name")));
    let edit = move |f: WaypointEdit| {
        let mut wps = waypoints.get_untracked();
        f(&mut wps);
        set_waypoints(cfg, &key.get_value(), wps);
    };

    let search = RwSignal::new(String::new());
    let results = RwSignal::new(Vec::<WaypointData>::new());
    let open = RwSignal::new(false);
    let term = move || search.get().trim().to_uppercase();
    let dest_hint =
        Signal::derive(move || is_dest_prefix(&term()) && dests.with(|d| !d.is_empty()));

    // Custom waypoints and destinations match from the first character.
    Effect::new(move |_| {
        let t = term();
        if t.is_empty() {
            results.set(Vec::new());
            return;
        }
        let mut local =
            customs.with_value(|c| dests.with_untracked(|d| local_suggestions(&t, c, d)));
        local.truncate(12);
        results.set(local);
    });
    // The navigation database keeps a three-character minimum.
    debounce(term, SEARCH_DEBOUNCE_MS, move |t: String| {
        if t.chars().count() < 3 {
            return;
        }
        leptos::task::spawn_local(async move {
            let args = NavSearch {
                product: cfg.product_ref(),
                query: t.clone(),
            };
            let rows = ipc::invoke::<cmd::NavSearchWaypoints>(&args)
                .await
                .unwrap_or_default();
            let still = search.try_with_untracked(|s| s.trim().to_uppercase() == t);
            if still == Some(true) {
                let local =
                    customs.with_value(|c| dests.with_untracked(|d| local_suggestions(&t, c, d)));
                results.set(merge_results(local, rows));
            }
        });
    });

    let add = move |wp: WaypointData| {
        let v = Waypoint::from(&wp).to_value();
        edit(Box::new(move |all| all.push(v)));
        search.set(String::new());
        results.set(Vec::new());
        open.set(false);
    };

    view! {
        <Field label="Plan name">
            <TextInput
                value=name
                size=Size::Sm
                on_input=Callback::new(move |s: String| {
                    edit_plan(cfg, &key.get_value(), |p| {
                        p.insert("name".to_owned(), Value::String(s));
                    });
                })
            />
        </Field>
        <div class="flex flex-col gap-2">
            <WidgetLabel label="Waypoints" aside=Signal::derive(move || format!("{:02}", len.get())) />
            {move || {
                (len.get() == 0).then(|| view! { <p class="text-xs font-light text-white/40">"No waypoints yet."</p> })
            }}
            <For each=move || 0..len.get() key=|i| *i let:i>
                {
                    let wp = Signal::derive(move || waypoints.with(|w| w.get(i).cloned().unwrap_or(Value::Null)));
                    let coords = move || {
                        wp.with(|w| {
                            format!(
                                "{:.2}, {:.2}",
                                num_at(w, "lat").unwrap_or_default(),
                                num_at(w, "lon").unwrap_or_default()
                            )
                        })
                    };
                    view! {
                        <div class="flex items-center gap-2 border-t border-white/10 py-1">
                            <span class="w-6 text-center font-mono text-[10px] text-violet-300">{i + 1}</span>
                            <span class="min-w-0 flex-1 truncate font-mono text-sm text-white">
                                {move || wp.with(|w| str_at(w, "ident"))}
                            </span>
                            <span class="font-mono text-[10px] text-white/40">{coords}</span>
                            <IconButton
                                icon=icons::CHEVRON_UP
                                label="Move up"
                                variant=Variant::Ghost
                                size=Size::Xs
                                disabled=Signal::derive(move || i == 0)
                                on_click=Callback::new(move |()| {
                                    if i > 0 {
                                        edit(Box::new(move |all| all.swap(i - 1, i)));
                                    }
                                })
                            />
                            <IconButton
                                icon=icons::CHEVRON_DOWN
                                label="Move down"
                                variant=Variant::Ghost
                                size=Size::Xs
                                disabled=Signal::derive(move || i + 1 >= len.get())
                                on_click=Callback::new(move |()| {
                                    edit(Box::new(move |all| {
                                        if i + 1 < all.len() {
                                            all.swap(i, i + 1);
                                        }
                                    }));
                                })
                            />
                            <IconButton
                                icon=icons::TRASH_2
                                label="Remove waypoint"
                                variant=Variant::Ghost
                                tone=Tone::Danger
                                size=Size::Xs
                                on_click=Callback::new(move |()| {
                                    edit(Box::new(move |all| {
                                        if i < all.len() {
                                            all.remove(i);
                                        }
                                    }));
                                })
                            />
                        </div>
                    }
                }
            </For>
            <div
                class="relative"
                on:focusin=move |_| open.set(true)
                on:focusout=move |_| open.set(false)
            >
                <TextInput
                    value=search
                    size=Size::Sm
                    class="uppercase"
                    placeholder="Search ident or DEST XX…"
                    on_input=Callback::new(move |s: String| {
                        search.set(s);
                        open.set(true);
                    })
                />
                <Suggestions open=Signal::derive(move || {
                    open.get() && (results.with(|r| !r.is_empty()) || dest_hint.get())
                })>
                    {move || {
                        dest_hint
                            .get()
                            .then(|| {
                                let n = dests.with(Vec::len);
                                view! {
                                    <p class="px-3 py-1.5 text-xs text-white/50">
                                        <strong class="text-white/70">"DEST"</strong>
                                        {format!(" — type a number (e.g. DEST 01 or just 01) to add one of {n} destinations")}
                                    </p>
                                }
                            })
                    }}
                    {move || {
                        results
                            .get()
                            .into_iter()
                            .map(|wp| {
                                let detail = waypoint_detail(&wp);
                                let ident = wp.identifier.clone();
                                view! {
                                    <SuggestionItem on_pick=Callback::new(move |()| add(wp.clone()))>
                                        <span class="font-mono font-medium">{ident}</span>
                                        <span class="ml-2 text-xs text-white/50">{detail}</span>
                                    </SuggestionItem>
                                }
                            })
                            .collect_view()
                    }}
                </Suggestions>
            </div>
        </div>
    }
}

/// The text a paste event carries.
fn pasted_text(ev: &leptos::ev::ClipboardEvent) -> Option<String> {
    let ev: &JsValue = ev.as_ref();
    let data = js_sys::Reflect::get(ev, &JsValue::from_str("clipboardData")).ok()?;
    let get: js_sys::Function = js_sys::Reflect::get(&data, &JsValue::from_str("getData"))
        .ok()?
        .dyn_into()
        .ok()?;
    get.call1(&data, &JsValue::from_str("text"))
        .ok()?
        .as_string()
}

/// The plan as a route string (SimBrief / VATSIM style). Editing or
/// pasting a route resolves every token and replaces the waypoints.
#[component]
fn CompiledPlan(
    cfg: Configurator,
    key: String,
    plan: Memo<Value>,
    customs: StoredValue<Vec<CustomWaypoint>>,
    dests: Memo<Vec<DestEntry>>,
) -> impl IntoView {
    let key = StoredValue::new(key);
    let compiled = Memo::new(move |_| plan.with(|p| compile(&waypoints_of(p))));
    let draft = RwSignal::new(compiled.get_untracked());
    let editing = RwSignal::new(false);
    let resolving = RwSignal::new(false);
    let copy_msg = RwSignal::new(None::<&'static str>);

    // Follow the plan while the user is not typing.
    Effect::new(move |_| {
        let c = compiled.get();
        if !editing.get_untracked() {
            draft.set(c);
        }
    });

    let resolve = move |text: String| {
        let toks = tokens(&text);
        if toks.is_empty() {
            set_waypoints(cfg, &key.get_value(), Vec::new());
            editing.set(false);
            return;
        }
        resolving.set(true);
        leptos::task::spawn_local(async move {
            let customs = customs.try_get_value().unwrap_or_default();
            let dests = dests.try_get_untracked().unwrap_or_default();
            let mut resolved = Vec::with_capacity(toks.len());
            for tok in toks {
                if let Some(wp) = resolve_local(&tok, &customs, &dests) {
                    resolved.push(wp);
                    continue;
                }
                let args = NavWaypoint {
                    product: cfg.product_ref(),
                    identifier: tok.clone(),
                    area_code: None,
                };
                match ipc::invoke::<cmd::NavWaypointByIdentifier>(&args).await {
                    Ok(Some(wp)) => resolved.push(Waypoint::from(&wp)),
                    _ => resolved.push(Waypoint {
                        ident: tok,
                        lat: 0.0,
                        lon: 0.0,
                    }),
                }
            }
            if let Some(k) = key.try_get_value() {
                set_waypoints(cfg, &k, resolved.iter().map(Waypoint::to_value).collect());
            }
            let _ = resolving.try_set(false);
            let _ = editing.try_set(false);
        });
    };

    let copy = move |()| {
        let text = compiled.get_untracked();
        if text.is_empty() {
            return;
        }
        write_clipboard(&text);
        copy_msg.set(Some("Copied to clipboard"));
        leptos::task::spawn_local(async move {
            ipc::sleep(2000).await;
            let _ = copy_msg.try_set(None);
        });
    };

    view! {
        <div class="border-t border-white/10 pt-3">
            <div class="flex items-center justify-between gap-3">
                <span class=EYEBROW>"Compiled flight plan"</span>
                <Button
                    variant=Variant::Glass
                    size=Size::Xs
                    icon=COPY
                    disabled=Signal::derive(move || compiled.with(String::is_empty))
                    on_click=Callback::new(copy)
                >
                    "Copy"
                </Button>
            </div>
            {move || {
                resolving
                    .get()
                    .then(|| {
                        view! {
                            <div class="mt-2 flex items-center gap-2 text-xs text-accent-300">
                                <Spinner size=Size::Xs />
                                "Resolving waypoints…"
                            </div>
                        }
                    })
            }}
            <div
                class="mt-2"
                on:focusout=move |_| {
                    let text = draft.get_untracked().trim().to_owned();
                    if text == compiled.get_untracked() {
                        editing.set(false);
                    } else {
                        resolve(text);
                    }
                }
                on:paste=move |ev: leptos::ev::ClipboardEvent| {
                    ev.prevent_default();
                    if let Some(text) = pasted_text(&ev).filter(|t| !t.trim().is_empty()) {
                        editing.set(true);
                        draft.set(text.clone());
                        resolve(text);
                    }
                }
            >
                <TextArea
                    value=draft
                    rows=3
                    size=Size::Sm
                    class="resize-none font-mono"
                    placeholder="Paste or type a flight plan (e.g. KSPS BULME ELECT)"
                    on_input=Callback::new(move |s: String| {
                        editing.set(true);
                        draft.set(s);
                    })
                />
            </div>
            <p class="mt-1 text-[11px] text-white/50">
                "Paste route formats like \u{201c}KSPS BULME ELECT\u{201d}"
                {move || copy_msg.get().map(|m| view! { <span class="ml-2 text-accent-300">{format!("· {m}")}</span> })}
            </p>
        </div>
    }
}
