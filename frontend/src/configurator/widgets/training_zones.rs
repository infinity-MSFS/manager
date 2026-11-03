//! trainingZones (v3 `TrainingZonesWidget`): search and list on the left,
//! map on the right. A zone comes from restrictive airspace (its rings) or
//! from a military training route (its corridor, `type: "MTR"`).

use infinity_ipc::commands as cmd;
use infinity_ipc::nav::{MtrRouteSummary, NavSearch, RestrictiveAirspaceData};
use infinity_ui::prelude::*;

use super::WidgetProps;
use super::common::{WidgetLabel, debounce, describe};
use super::zone_ui::{
    ColorSwatches, ZoneDot, selection_after_remove, training_areas, training_center,
};
use crate::configurator::map::{InteractiveMap, MapLayers};
use crate::configurator::zones::{
    TrainingZone, ZoneColor, airspace_type_label, airspace_zone, mtr_zone, read_list, write_list,
};
use crate::ipc;

const DEBOUNCE_MS: u32 = 250;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AddMode {
    Airspace,
    Mtr,
}

pub(crate) fn mtr_summary_line(r: &MtrRouteSummary) -> String {
    let mut s = format!("{} · {} segments", r.family.prefix(), r.segment_count);
    if let (Some(lo), Some(hi)) = (r.min_lower_ft, r.max_upper_ft) {
        s.push_str(&format!(" · {}–{} ft", lo.round(), hi.round()));
    }
    s
}

#[component]
pub(crate) fn ModePill(
    mode: RwSignal<AddMode>,
    #[prop(into)] on_change: Callback<AddMode>,
) -> impl IntoView {
    view! {
        <Tabs
            value=Signal::derive(move || if mode.get() == AddMode::Airspace { "airspace".to_owned() } else { "mtr".to_owned() })
            items=Signal::stored(vec![TabItem::new("airspace", "Airspace"), TabItem::new("mtr", "MTR")])
            full_width=true
            on_change=Callback::new(move |v: String| {
                on_change.run(if v == "mtr" { AddMode::Mtr } else { AddMode::Airspace });
            })
        />
    }
}

pub(super) fn training_zones_widget(props: WidgetProps) -> AnyView {
    let value = props.value;
    let on_change = props.on_change;
    let cfg = props.ctx.cfg;
    let zones = Memo::new(move |_| value.with(read_list::<TrainingZone>));
    let selected = RwSignal::new(if zones.with_untracked(Vec::is_empty) {
        None
    } else {
        Some(0)
    });
    let mode = RwSignal::new(AddMode::Airspace);
    let search = RwSignal::new(String::new());
    let airspace_hits = RwSignal::new(Vec::<RestrictiveAirspaceData>::new());
    let mtr_hits = RwSignal::new(Vec::<MtrRouteSummary>::new());
    let searching = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);

    debounce(
        move || (search.get(), mode.get()),
        DEBOUNCE_MS,
        move |(query, m): (String, AddMode)| {
            if m == AddMode::Airspace && query.chars().count() < 2 {
                airspace_hits.set(Vec::new());
                return;
            }
            searching.set(true);
            leptos::task::spawn_local(async move {
                match m {
                    AddMode::Airspace => {
                        let args = NavSearch {
                            product: cfg.product_ref(),
                            query,
                        };
                        let rows = ipc::invoke::<cmd::NavSearchAirspace>(&args)
                            .await
                            .unwrap_or_default();
                        let _ = airspace_hits.try_set(rows.into_iter().take(30).collect());
                    }
                    AddMode::Mtr => {
                        let rows = ipc::invoke::<cmd::MtrSearch>(&query)
                            .await
                            .unwrap_or_default();
                        let _ = mtr_hits.try_set(rows.into_iter().take(60).collect());
                    }
                }
                let _ = searching.try_set(false);
            });
        },
    );

    let write = move |next: Vec<TrainingZone>| on_change.run(write_list(&next));
    let push = move |zone: TrainingZone| {
        let mut next = zones.get_untracked();
        next.push(zone);
        let last = next.len() - 1;
        write(next);
        selected.set(Some(last));
        search.set(String::new());
        airspace_hits.set(Vec::new());
        mtr_hits.set(Vec::new());
    };
    let add_airspace = move |row: RestrictiveAirspaceData| {
        error.set(None);
        leptos::task::spawn_local(async move {
            match airspace_zone(cfg, row.name, row.designation, row.airspace_type).await {
                Ok(z) => push(z),
                Err(e) => {
                    let _ = error.try_set(Some(describe(&e)));
                }
            }
        });
    };
    let add_mtr = move |row: MtrRouteSummary| {
        error.set(None);
        leptos::task::spawn_local(async move {
            let ident = row.ident_full.clone();
            match mtr_zone(row.ident_full, row.family).await {
                Ok(Some(z)) => push(z),
                Ok(None) => {
                    let _ = error.try_set(Some(format!(
                        "{ident} has no segments with a non-zero corridor width; nothing to draw."
                    )));
                }
                Err(e) => {
                    let _ = error.try_set(Some(describe(&e)));
                }
            }
        });
    };
    let remove = move |i: usize| {
        let mut next = zones.get_untracked();
        if i < next.len() {
            next.remove(i);
        }
        let remaining = next.len();
        write(next);
        selected.update(|s| *s = selection_after_remove(*s, i, remaining));
    };
    let patch = move |i: usize, f: Box<dyn FnOnce(&mut TrainingZone)>| {
        let mut next = zones.get_untracked();
        if let Some(z) = next.get_mut(i) {
            f(z);
        }
        write(next);
    };

    let layers = Signal::derive(move || MapLayers {
        areas: zones.with(|z| training_areas(z)),
        ..MapLayers::default()
    });
    let center = Signal::derive(move || zones.with(|z| training_center(z, selected.get())));
    let zoom = Signal::derive(move || {
        if selected.get().is_some() || zones.with(|z| !z.is_empty()) {
            8.0
        } else {
            5.0
        }
    });

    view! {
        <div class="flex h-[600px] gap-6">
            <div class="flex min-h-0 w-80 flex-col gap-3">
                <WidgetLabel label=props.field.get("label").and_then(|l| l.as_str()).unwrap_or("Training zones").to_owned() />
                <ModePill
                    mode=mode
                    on_change=Callback::new(move |m| {
                        mode.set(m);
                        search.set(String::new());
                        airspace_hits.set(Vec::new());
                        mtr_hits.set(Vec::new());
                    })
                />
                <div class="relative">
                    {move || {
                        let placeholder = if mode.get() == AddMode::Airspace {
                            "Search by name (WESTOVER…)"
                        } else {
                            "Search MTR (VR1064, IR, 002…)"
                        };
                        view! {
                            <TextInput
                                value=search
                                icon=icons::SEARCH
                                placeholder=placeholder
                                on_input=Callback::new(move |s| search.set(s))
                            />
                        }
                    }}
                    {move || searching.get().then(|| view! { <div class="absolute top-2.5 right-3"><Spinner size=Size::Xs /></div> })}
                </div>
                {move || error.get().map(|e| view! { <p data-selectable class="text-xs text-red-300">{e}</p> })}
                {move || {
                    let rows = airspace_hits.get();
                    (mode.get() == AddMode::Airspace && !rows.is_empty())
                        .then(|| {
                            view! {
                                <div class="max-h-56 overflow-y-auto border-y border-white/10">
                                    {rows
                                        .into_iter()
                                        .map(|row| {
                                            let detail = format!("{} · {}", row.designation, airspace_type_label(&row.airspace_type));
                                            let name = row.name.clone();
                                            view! {
                                                <ResultRow title=name detail=detail on_pick=Callback::new(move |()| add_airspace(row.clone())) />
                                            }
                                        })
                                        .collect_view()}
                                </div>
                            }
                        })
                }}
                {move || {
                    let rows = mtr_hits.get();
                    (mode.get() == AddMode::Mtr && !rows.is_empty())
                        .then(|| {
                            view! {
                                <div class="max-h-56 overflow-y-auto border-y border-white/10">
                                    {rows
                                        .into_iter()
                                        .map(|row| {
                                            let title = if row.is_low_level { format!("{} · LL", row.ident_full) } else { row.ident_full.clone() };
                                            let detail = mtr_summary_line(&row);
                                            view! {
                                                <ResultRow title=title detail=detail on_pick=Callback::new(move |()| add_mtr(row.clone())) />
                                            }
                                        })
                                        .collect_view()}
                                </div>
                            }
                        })
                }}
                <div class="flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto">
                    {move || {
                        zones
                            .with(Vec::is_empty)
                            .then(|| {
                                view! {
                                    <p class="border-t border-white/10 py-6 text-center text-sm font-light text-white/40">
                                        "No training zones yet. Search above to add one."
                                    </p>
                                }
                            })
                    }}
                    <For each=move || 0..zones.with(Vec::len) key=|i| *i let:i>
                        <TrainingZoneRow
                            index=i
                            zone=Signal::derive(move || zones.with(|z| z.get(i).cloned()))
                            selected=Signal::derive(move || selected.get() == Some(i))
                            on_select=Callback::new(move |()| selected.set(Some(i)))
                            on_remove=Callback::new(move |()| remove(i))
                            on_name=Callback::new(move |s: String| patch(i, Box::new(move |z| z.name = s)))
                            on_designation=Callback::new(move |s: String| patch(i, Box::new(move |z| z.designation = s)))
                            on_color=Callback::new(move |c| patch(i, Box::new(move |z| z.color = c)))
                        />
                    </For>
                </div>
            </div>
            <div class="min-h-0 flex-1">
                <InteractiveMap center=center zoom=zoom layers=layers />
            </div>
        </div>
    }
    .into_any()
}

/// A search hit with a plus.
#[component]
pub(crate) fn ResultRow(
    #[prop(into)] title: String,
    #[prop(into)] detail: String,
    #[prop(into)] on_pick: Callback<()>,
) -> impl IntoView {
    view! {
        <button
            type="button"
            class="flex w-full items-center gap-2 border-t border-white/[0.06] px-2 py-2 text-left transition-colors first:border-t-0 hover:bg-white/5"
            on:click=move |_| on_pick.run(())
        >
            <Icon icon=icons::PLUS class="h-3.5 w-3.5 text-accent-400" />
            <div class="min-w-0 flex-1">
                <div class="truncate text-sm text-white">{title}</div>
                <div class="text-xs text-white/50">{detail}</div>
            </div>
        </button>
    }
}

/// One training zone, editable when selected. `zone` is `None` for the
/// instant a removed row lingers.
#[component]
pub(crate) fn TrainingZoneRow(
    index: usize,
    #[prop(into)] zone: Signal<Option<TrainingZone>>,
    #[prop(into)] selected: Signal<bool>,
    #[prop(into)] on_select: Callback<()>,
    #[prop(into)] on_remove: Callback<()>,
    #[prop(into)] on_name: Callback<String>,
    #[prop(into)] on_designation: Callback<String>,
    #[prop(into)] on_color: Callback<ZoneColor>,
) -> impl IntoView {
    let detail = move || {
        zone.with(|z| {
            let Some(zone) = z else { return String::new() };
            let mut d = zone.designation.clone();
            if !zone.zone_type.is_empty() {
                d.push_str(&format!(" · {}", airspace_type_label(&zone.zone_type)));
            }
            if let Some(s) = zone.ring_summary() {
                d.push_str(&format!(" · {s}"));
            }
            d
        })
    };
    let title = move || {
        zone.with(|z| match z {
            Some(z) if z.name.is_empty() && z.designation.is_empty() => "Unnamed".to_owned(),
            Some(z) => z.title(index),
            None => String::new(),
        })
    };
    let name = Signal::derive(move || {
        zone.with(|z| z.as_ref().map(|z| z.name.clone()).unwrap_or_default())
    });
    let designation = Signal::derive(move || {
        zone.with(|z| {
            z.as_ref()
                .map(|z| z.designation.clone())
                .unwrap_or_default()
        })
    });
    let color =
        Signal::derive(move || zone.with(|z| z.as_ref().map(|z| z.color).unwrap_or_default()));
    view! {
        <div
            class=move || {
                cn!(
                    "cursor-pointer border-l-2 py-3 pr-2 pl-3 transition-colors",
                    if selected.get() { "border-accent-400 bg-accent-500/[0.06]" } else { "border-white/10 hover:border-white/30 hover:bg-white/[0.03]" },
                )
            }
            on:click=move |_| on_select.run(())
        >
            <div class="flex items-start gap-2">
                <ZoneDot color=color />
                <div class="min-w-0 flex-1">
                    <div class="truncate text-sm text-white">{title}</div>
                    <div class="text-xs text-white/50">{detail}</div>
                </div>
                <IconButton
                    icon=icons::TRASH_2
                    label="Remove"
                    variant=Variant::Ghost
                    tone=Tone::Danger
                    size=Size::Xs
                    on_click=on_remove
                />
            </div>
            <Show when=move || selected.get()>
                <div class="mt-3 flex flex-col gap-3" on:click=|ev| ev.stop_propagation()>
                    <Field label="Name">
                        <TextInput value=name on_input=on_name size=Size::Sm />
                    </Field>
                    <Field label="Designation">
                        <TextInput value=designation on_input=on_designation size=Size::Sm />
                    </Field>
                    <Field label="Colour">
                        <ColorSwatches selected=color on_pick=on_color />
                    </Field>
                </div>
            </Show>
        </div>
    }
}
