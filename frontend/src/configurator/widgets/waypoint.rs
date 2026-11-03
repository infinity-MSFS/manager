//! waypointPick (v3 `WaypointPickWidget`): search the navigation database
//! and store `{ ident, lat, lon }`, the flight-plan waypoint shape.

use infinity_ipc::commands as cmd;
use infinity_ipc::nav::{NavSearch, WaypointData};
use infinity_ui::prelude::*;
use serde_json::json;

use super::WidgetProps;
use super::common::{SuggestionItem, Suggestions, WidgetLabel, debounce};
use crate::configurator::schema::{num_at, str_at};
use crate::ipc;

const DEBOUNCE_MS: u32 = 250;

/// The secondary line of a waypoint search result.
pub(crate) fn waypoint_detail(wp: &WaypointData) -> String {
    let mut s = wp.waypoint_type.clone();
    if let Some(area) = wp.area_code.as_deref().filter(|a| !a.is_empty()) {
        s.push_str(" · ");
        s.push_str(area);
    }
    if let Some(name) = wp.name.as_deref().filter(|n| !n.is_empty()) {
        s.push_str(" · ");
        s.push_str(name);
    }
    s
}

pub(super) fn waypoint_widget(props: WidgetProps) -> AnyView {
    let value = props.value;
    let on_change = props.on_change;
    let cfg = props.ctx.cfg;
    let draft = RwSignal::new(value.with_untracked(|v| str_at(v, "ident")));
    let results = RwSignal::new(Vec::<WaypointData>::new());
    let open = RwSignal::new(false);

    debounce(
        move || draft.get(),
        DEBOUNCE_MS,
        move |query: String| {
            if query.chars().count() < 3 {
                results.set(Vec::new());
                return;
            }
            leptos::task::spawn_local(async move {
                let args = NavSearch {
                    product: cfg.product_ref(),
                    query,
                };
                let rows = ipc::invoke::<cmd::NavSearchWaypoints>(&args)
                    .await
                    .unwrap_or_default();
                let _ = results.try_set(rows.into_iter().take(15).collect());
            });
        },
    );

    let summary = move || {
        value.with(|v| {
            let ident = str_at(v, "ident");
            if ident.is_empty() {
                return None;
            }
            let lat = num_at(v, "lat").unwrap_or_default();
            let lon = num_at(v, "lon").unwrap_or_default();
            Some(format!("{ident}: {lat:.4}, {lon:.4}"))
        })
    };

    view! {
        <div class="flex flex-col gap-1.5">
            <WidgetLabel label=props.label() />
            <div
                class="relative w-64"
                on:focusin=move |_| open.set(true)
                on:focusout=move |_| open.set(false)
            >
                <TextInput
                    value=draft
                    placeholder="Ident"
                    class="uppercase"
                    on_input=Callback::new(move |s: String| {
                        draft.set(s);
                        open.set(true);
                    })
                />
                <Suggestions
                    open=Signal::derive(move || open.get() && results.with(|r| !r.is_empty()))
                    class="min-w-[18rem]"
                >
                    {move || {
                        results
                            .get()
                            .into_iter()
                            .map(|wp| {
                                let detail = waypoint_detail(&wp);
                                let ident = wp.identifier.clone();
                                view! {
                                    <SuggestionItem on_pick=Callback::new(move |()| {
                                        draft.set(wp.identifier.clone());
                                        open.set(false);
                                        on_change
                                            .run(json!({ "ident": wp.identifier, "lat": wp.latitude, "lon": wp.longitude }));
                                    })>
                                        <span class="font-mono font-medium">{ident}</span>
                                        <span class="ml-2 text-xs text-white/50">{detail}</span>
                                    </SuggestionItem>
                                }
                            })
                            .collect_view()
                    }}
                </Suggestions>
            </div>
            {move || {
                summary().map(|s| view! { <p class="font-mono text-[11px] text-white/40">{s}</p> })
            }}
        </div>
    }
    .into_any()
}
