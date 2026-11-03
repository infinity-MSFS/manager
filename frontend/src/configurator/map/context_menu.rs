//! The map's right-click menu (v3 `components/map/MapContextMenu.tsx`):
//! nearby waypoints for flight plans and destinations, or nearby airspace
//! and military training routes for zones.

use infinity_ipc::nav::{MtrRouteWithDistance, RestrictiveAirspaceDataWithDistance, WaypointData};
use infinity_ui::prelude::*;
use infinity_ui::style::GLASS_RAISED;
use leptos::html::Div;

use super::super::zones::airspace_type_label;
use super::MapPointer;
use crate::editorial::EYEBROW;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuMode {
    Waypoints,
    /// Airspace and MTRs, in two tabs.
    Airspaces,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Airspaces,
    Mtrs,
}

const ROW: &str = "flex w-full items-center justify-between gap-3 border-t border-white/[0.06] px-4 py-2.5 text-left transition-colors first:border-t-0 hover:bg-white/[0.06]";

fn distance_badge(nm: f64) -> impl IntoView {
    view! { <span class="shrink-0 font-mono text-[11px] text-accent-300">{format!("{nm:.1} nm")}</span> }
}

fn mtr_detail(m: &MtrRouteWithDistance) -> String {
    let r = &m.route;
    let mut s = format!("{} · {} segments", r.family.prefix(), r.segment_count);
    if let (Some(lo), Some(hi)) = (r.min_lower_ft, r.max_upper_ft) {
        s.push_str(&format!(" · {}–{} ft", lo.round(), hi.round()));
    }
    s
}

#[component]
pub(crate) fn MapContextMenu(
    at: MapPointer,
    mode: MenuMode,
    #[prop(into)] loading: Signal<bool>,
    #[prop(into, optional)] waypoints: Option<Signal<Vec<WaypointData>>>,
    #[prop(into, optional)] airspaces: Option<Signal<Vec<RestrictiveAirspaceDataWithDistance>>>,
    #[prop(into, optional)] mtrs: Option<Signal<Vec<MtrRouteWithDistance>>>,
    #[prop(into)] on_close: Callback<()>,
    #[prop(into, optional)] on_waypoint: Option<Callback<WaypointData>>,
    #[prop(into, optional)] on_airspace: Option<Callback<RestrictiveAirspaceDataWithDistance>>,
    #[prop(into, optional)] on_mtr: Option<Callback<MtrRouteWithDistance>>,
) -> impl IntoView {
    let waypoints = waypoints.unwrap_or_else(|| Signal::stored(Vec::new()));
    let airspaces = airspaces.unwrap_or_else(|| Signal::stored(Vec::new()));
    let mtrs = mtrs.unwrap_or_else(|| Signal::stored(Vec::new()));
    let node = NodeRef::<Div>::new();
    let tab = RwSignal::new(Tab::Airspaces);
    // Open on whichever tab has results once they arrive; airspace wins ties.
    Effect::new(move |_| {
        if !loading.get() && airspaces.with(Vec::is_empty) && mtrs.with(|m| !m.is_empty()) {
            tab.set(Tab::Mtrs);
        }
    });
    let position = RwSignal::new((at.x, at.y));
    // Flip to stay on screen once measured.
    Effect::new(move |_| {
        let Some(el) = node.get() else { return };
        let rect = el.get_bounding_client_rect();
        let (w, h) = web_sys::window()
            .map(|win| {
                (
                    win.inner_width()
                        .ok()
                        .and_then(|v| v.as_f64())
                        .unwrap_or(f64::MAX),
                    win.inner_height()
                        .ok()
                        .and_then(|v| v.as_f64())
                        .unwrap_or(f64::MAX),
                )
            })
            .unwrap_or((f64::MAX, f64::MAX));
        let x = if at.x + rect.width() > w - 10.0 {
            at.x - rect.width()
        } else {
            at.x
        };
        let y = if at.y + rect.height() > h - 10.0 {
            at.y - rect.height()
        } else {
            at.y
        };
        position.set((x.max(4.0), y.max(4.0)));
    });
    use_escape(move || on_close.run(()));
    use_click_outside(node, move || on_close.run(()));

    let title = move || match (mode, tab.get()) {
        (MenuMode::Waypoints, _) => "Nearby waypoints",
        (_, Tab::Mtrs) => "Nearby MTRs",
        _ => "Nearby airspace",
    };
    let hint = move || match (mode, tab.get()) {
        (MenuMode::Waypoints, _) => "Pick a waypoint to add it.",
        (_, Tab::Mtrs) => "Pick a route to add its corridor as a training zone.",
        _ => "Pick an airspace to add it as a training zone.",
    };
    let empty = move || match (mode, tab.get()) {
        (MenuMode::Waypoints, _) => "No waypoints found nearby.",
        (_, Tab::Mtrs) => "No MTRs within 50 nm.",
        _ => "No restricted airspace within 50 nm.",
    };
    let count = move || match (mode, tab.get()) {
        (MenuMode::Waypoints, _) => waypoints.with(Vec::len),
        (_, Tab::Mtrs) => mtrs.with(Vec::len),
        _ => airspaces.with(Vec::len),
    };
    let tab_button = move |which: Tab, label: &'static str| {
        view! {
            <button
                type="button"
                class=move || {
                    cn!(
                        "flex-1 border-b py-2 text-xs transition-colors",
                        if tab.get() == which { "border-accent-400 text-white" } else { "border-transparent text-white/50 hover:text-white" },
                    )
                }
                on:click=move |_| tab.set(which)
            >
                {move || {
                    let n = if which == Tab::Mtrs { mtrs.with(Vec::len) } else { airspaces.with(Vec::len) };
                    format!("{label} ({n})")
                }}
            </button>
        }
    };

    let body = move || {
        if loading.get() {
            return view! {
                <div class="flex flex-col items-center gap-3 px-4 py-10 text-xs text-white/50">
                    <Spinner size=Size::Sm />
                    "Searching…"
                </div>
            }
            .into_any();
        }
        if count() == 0 {
            return view! { <p class="px-4 py-10 text-center text-sm font-light text-white/50">{empty()}</p> }
                .into_any();
        }
        match (mode, tab.get()) {
            (MenuMode::Waypoints, _) => waypoints
                .get()
                .into_iter()
                .map(|wp| {
                    let pick = wp.clone();
                    let detail = {
                        let mut d = wp.waypoint_type.clone();
                        if let Some(n) = wp.name.as_deref().filter(|n| !n.is_empty()) {
                            d.push_str(" · ");
                            d.push_str(n);
                        }
                        d
                    };
                    view! {
                        <button
                            type="button"
                            class=ROW
                            on:click=move |_| {
                                if let Some(cb) = on_waypoint {
                                    cb.run(pick.clone());
                                }
                                on_close.run(());
                            }
                        >
                            <div class="min-w-0 flex-1">
                                <div class="truncate text-sm font-medium text-white">
                                    {wp.identifier.clone()}
                                    {wp.area_code.clone().filter(|a| !a.is_empty()).map(|a| view! { <span class="ml-1.5 font-mono text-xs text-white/40">{a}</span> })}
                                </div>
                                <div class="mt-0.5 truncate text-xs text-white/55">{detail}</div>
                            </div>
                        </button>
                    }
                })
                .collect_view()
                .into_any(),
            (_, Tab::Mtrs) => mtrs
                .get()
                .into_iter()
                .map(|m| {
                    let pick = m.clone();
                    let detail = mtr_detail(&m);
                    view! {
                        <button
                            type="button"
                            class=ROW
                            on:click=move |_| {
                                if let Some(cb) = on_mtr {
                                    cb.run(pick.clone());
                                }
                                on_close.run(());
                            }
                        >
                            <div class="min-w-0 flex-1">
                                <div class="truncate text-sm font-medium text-white">
                                    {m.route.ident_full.clone()}
                                    {m.route.is_low_level.then(|| view! { <span class="ml-1.5 font-mono text-[10px] tracking-wider text-amber-300 uppercase">"LL"</span> })}
                                </div>
                                <div class="mt-0.5 truncate text-xs text-white/55">{detail}</div>
                            </div>
                            {distance_badge(m.distance_nm)}
                        </button>
                    }
                })
                .collect_view()
                .into_any(),
            _ => airspaces
                .get()
                .into_iter()
                .map(|a| {
                    let pick = a.clone();
                    let detail = format!("{} · {}", airspace_type_label(&a.airspace_type), a.designation);
                    view! {
                        <button
                            type="button"
                            class=ROW
                            on:click=move |_| {
                                if let Some(cb) = on_airspace {
                                    cb.run(pick.clone());
                                }
                                on_close.run(());
                            }
                        >
                            <div class="min-w-0 flex-1">
                                <div class="truncate text-sm font-medium text-white">{a.name.clone()}</div>
                                <div class="mt-0.5 truncate text-xs text-white/55">{detail}</div>
                            </div>
                            {distance_badge(a.distance)}
                        </button>
                    }
                })
                .collect_view()
                .into_any(),
        }
    };

    view! {
        <div
            node_ref=node
            class=cn!(GLASS_RAISED, "fixed z-[10000] w-80 overflow-hidden rounded-2xl border")
            style=move || {
                let (x, y) = position.get();
                format!("left:{x}px;top:{y}px")
            }
        >
            <div class="flex items-center justify-between border-b border-white/10 px-4 py-3">
                <span class="text-sm font-medium tracking-tight text-white">{title}</span>
                <IconButton icon=icons::X label="Close" variant=Variant::Ghost size=Size::Sm on_click=on_close />
            </div>
            <p class=cn!("border-b border-white/10 px-4 py-2", EYEBROW, "tracking-[0.15em] normal-case")>
                {format!("{:.6}, {:.6}", at.lat, at.lon)}
            </p>
            {(mode == MenuMode::Airspaces)
                .then(|| {
                    view! {
                        <div class="flex border-b border-white/10">
                            {tab_button(Tab::Airspaces, "Airspace")}
                            {tab_button(Tab::Mtrs, "MTRs")}
                        </div>
                    }
                })}
            <div class="max-h-80 overflow-y-auto">{body}</div>
            {move || {
                (!loading.get() && count() > 0)
                    .then(|| view! { <p class="border-t border-white/10 px-4 py-2 text-[11px] text-white/45">{hint()}</p> })
            }}
        </div>
    }
}
