//! restrictiveAirspacePick (v3 `RestrictiveAirspacePickWidget`): search
//! airspace by name and store it as one training zone, every ring of its
//! boundary expanded.

use infinity_ipc::commands as cmd;
use infinity_ipc::nav::{NavSearch, RestrictiveAirspaceData};
use infinity_ui::prelude::*;
use serde_json::{Map, Value};

use super::WidgetProps;
use super::common::{SuggestionItem, Suggestions, WidgetLabel, debounce, describe};
use crate::configurator::zones::{TrainingZone, ZoneColor, airspace_rings, airspace_type_label};
use crate::ipc;

const DEBOUNCE_MS: u32 = 250;

pub(super) fn airspace_widget(props: WidgetProps) -> AnyView {
    let value = props.value;
    let on_change = props.on_change;
    let cfg = props.ctx.cfg;
    let zone =
        Memo::new(move |_| value.with(|v| serde_json::from_value::<TrainingZone>(v.clone()).ok()));
    let draft = RwSignal::new(String::new());
    let results = RwSignal::new(Vec::<RestrictiveAirspaceData>::new());
    let open = RwSignal::new(false);
    let loading = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);

    debounce(
        move || draft.get(),
        DEBOUNCE_MS,
        move |query: String| {
            if query.chars().count() < 2 {
                results.set(Vec::new());
                return;
            }
            leptos::task::spawn_local(async move {
                let args = NavSearch {
                    product: cfg.product_ref(),
                    query,
                };
                let rows = ipc::invoke::<cmd::NavSearchAirspace>(&args)
                    .await
                    .unwrap_or_default();
                let _ = results.try_set(rows.into_iter().take(30).collect());
            });
        },
    );

    let pick = move |row: RestrictiveAirspaceData| {
        loading.set(true);
        error.set(None);
        open.set(false);
        leptos::task::spawn_local(async move {
            match airspace_rings(cfg, &row.designation).await {
                Ok(boundary) => {
                    let color = zone
                        .get_untracked()
                        .map(|z| z.color)
                        .unwrap_or(ZoneColor::Yellow);
                    let next = TrainingZone {
                        name: row.name,
                        designation: row.designation,
                        zone_type: row.airspace_type,
                        color,
                        boundary,
                        extra: Map::new(),
                    };
                    if let Ok(v) = serde_json::to_value(next) {
                        on_change.run(v);
                    }
                    let _ = draft.try_set(String::new());
                }
                Err(e) => {
                    let _ = error.try_set(Some(describe(&e)));
                }
            }
            let _ = loading.try_set(false);
        });
    };

    let clear = move |()| {
        on_change.run(serde_json::json!({
            "name": "", "designation": "", "type": "", "boundary": []
        }));
    };

    view! {
        <div class="flex flex-col gap-1.5">
            <WidgetLabel label=props
                .field
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or("Training zone")
                .to_owned() />
            {move || {
                zone.get()
                    .filter(|z| !z.designation.is_empty())
                    .map(|z| {
                        let detail = format!(
                            "{}{}{}",
                            z.designation,
                            if z.zone_type.is_empty() { String::new() } else { format!(" · {}", airspace_type_label(&z.zone_type)) },
                            z.ring_summary().map(|s| format!(" · {s}")).unwrap_or_default(),
                        );
                        view! {
                            <div class="flex items-center justify-between gap-4 border-t border-white/10 py-2">
                                <div class="min-w-0">
                                    <span class="text-sm font-medium text-white">{z.name.clone()}</span>
                                    <span class="ml-2 text-xs text-white/50">{detail}</span>
                                </div>
                                <Button variant=Variant::Ghost size=Size::Sm on_click=Callback::new(clear)>
                                    "Clear"
                                </Button>
                            </div>
                        }
                    })
            }}
            <div
                class="relative w-80"
                on:focusin=move |_| open.set(true)
                on:focusout=move |_| open.set(false)
            >
                <TextInput
                    value=draft
                    icon=icons::SEARCH
                    placeholder="Search by name (e.g. WESTOVER)"
                    disabled=loading
                    on_input=Callback::new(move |s: String| {
                        draft.set(s);
                        open.set(true);
                    })
                />
                <Suggestions open=Signal::derive(move || open.get() && results.with(|r| !r.is_empty()))>
                    {move || {
                        results
                            .get()
                            .into_iter()
                            .map(|row| {
                                let detail = format!("{} · {}", row.designation, airspace_type_label(&row.airspace_type));
                                let name = row.name.clone();
                                view! {
                                    <SuggestionItem on_pick=Callback::new(move |()| pick(row.clone()))>
                                        <span class="font-medium">{name}</span>
                                        <span class="ml-2 text-xs text-white/50">{detail}</span>
                                    </SuggestionItem>
                                }
                            })
                            .collect_view()
                    }}
                </Suggestions>
            </div>
            {move || error.get().map(|e| view! { <p data-selectable class="text-xs text-red-300">{e}</p> })}
        </div>
    }
    .into_any()
}
