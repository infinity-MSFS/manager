//! noFlyZones (v3 `NoFlyZonesWidget`): circles of name, centre, radius
//! (nm) and colour, listed beside the map.

use infinity_ui::prelude::*;

use super::WidgetProps;
use super::common::WidgetLabel;
use super::zone_ui::{
    ColorSwatches, ZoneDot, no_fly_center, no_fly_circles, selection_after_remove,
};
use crate::configurator::map::{InteractiveMap, MapLayers};
use crate::configurator::zones::{NoFlyZone, ZoneColor, read_list, write_list};

/// A number the user typed, `0` when it does not parse (v3's `Number(x) || 0`).
pub(crate) fn number_or_zero(s: &str) -> f64 {
    s.trim()
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
        .unwrap_or(0.0)
}

pub(super) fn no_fly_zones_widget(props: WidgetProps) -> AnyView {
    let value = props.value;
    let on_change = props.on_change;
    let zones = Memo::new(move |_| value.with(read_list::<NoFlyZone>));
    let selected = RwSignal::new(if zones.with_untracked(Vec::is_empty) {
        None
    } else {
        Some(0)
    });

    let write = move |next: Vec<NoFlyZone>| on_change.run(write_list(&next));
    let add = move |()| {
        let mut next = zones.get_untracked();
        next.push(NoFlyZone::blank(next.len()));
        let last = next.len() - 1;
        write(next);
        selected.set(Some(last));
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
    let patch = move |i: usize, f: Box<dyn FnOnce(&mut NoFlyZone)>| {
        let mut next = zones.get_untracked();
        if let Some(z) = next.get_mut(i) {
            f(z);
        }
        write(next);
    };

    let layers = Signal::derive(move || MapLayers {
        circles: zones.with(|z| no_fly_circles(z)),
        ..MapLayers::default()
    });
    let center = Signal::derive(move || zones.with(|z| no_fly_center(z, selected.get())));
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
                <div class="flex items-center justify-between">
                    <WidgetLabel label=props.field.get("label").and_then(|l| l.as_str()).unwrap_or("No-fly zones").to_owned() />
                    <Button variant=Variant::Glass size=Size::Sm icon=icons::PLUS on_click=Callback::new(add)>
                        "Add"
                    </Button>
                </div>
                <div class="flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto">
                    {move || {
                        zones
                            .with(Vec::is_empty)
                            .then(|| {
                                view! {
                                    <p class="border-t border-white/10 py-6 text-center text-sm font-light text-white/40">
                                        "No no-fly zones yet."
                                    </p>
                                }
                            })
                    }}
                    <For each=move || 0..zones.with(Vec::len) key=|i| *i let:i>
                        <NoFlyZoneRow
                            zone=Signal::derive(move || zones.with(|z| z.get(i).cloned()))
                            selected=Signal::derive(move || selected.get() == Some(i))
                            on_select=Callback::new(move |()| selected.set(Some(i)))
                            on_remove=Callback::new(move |()| remove(i))
                            on_edit=Callback::new(move |edit: NoFlyEdit| patch(i, Box::new(move |z| edit.apply(z))))
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

/// One change to a no-fly zone.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NoFlyEdit {
    Name(String),
    Lat(f64),
    Lon(f64),
    Radius(f64),
    Color(ZoneColor),
}

impl NoFlyEdit {
    pub(crate) fn apply(self, z: &mut NoFlyZone) {
        match self {
            Self::Name(s) => z.name = s,
            Self::Lat(v) => z.lat = v,
            Self::Lon(v) => z.lon = v,
            Self::Radius(v) => z.radius = v,
            Self::Color(c) => z.color = c,
        }
    }
}

fn fmt_coord(v: f64) -> String {
    if v.is_finite() {
        format!("{v:.4}")
    } else {
        "—".to_owned()
    }
}

/// A number input whose text survives keystrokes that do not change the
/// parsed value (`1.`, `-`).
#[component]
pub(crate) fn NumberField(
    #[prop(into)] label: String,
    #[prop(into)] value: Signal<f64>,
    #[prop(into)] on_change: Callback<f64>,
) -> impl IntoView {
    let draft = RwSignal::new(value.get_untracked().to_string());
    Effect::new(move |_| {
        let v = value.get();
        if number_or_zero(&draft.get_untracked()) != v {
            draft.set(v.to_string());
        }
    });
    view! {
        <Field label=label>
            <TextInput
                value=draft
                input_type="number"
                size=Size::Sm
                on_input=Callback::new(move |s: String| {
                    draft.set(s.clone());
                    on_change.run(number_or_zero(&s));
                })
            />
        </Field>
    }
}

#[component]
pub(crate) fn NoFlyZoneRow(
    #[prop(into)] zone: Signal<Option<NoFlyZone>>,
    #[prop(into)] selected: Signal<bool>,
    #[prop(into)] on_select: Callback<()>,
    #[prop(into)] on_remove: Callback<()>,
    #[prop(into)] on_edit: Callback<NoFlyEdit>,
) -> impl IntoView {
    let get = move |f: fn(&NoFlyZone) -> f64| {
        Signal::derive(move || zone.with(|z| z.as_ref().map(f).unwrap_or_default()))
    };
    let name = Signal::derive(move || {
        zone.with(|z| z.as_ref().map(|z| z.name.clone()).unwrap_or_default())
    });
    let color = Signal::derive(move || {
        zone.with(|z| z.as_ref().map(|z| z.color).unwrap_or(ZoneColor::Magenta))
    });
    let title = move || {
        let n = name.get();
        if n.is_empty() {
            "Unnamed".to_owned()
        } else {
            n
        }
    };
    let detail = move || {
        zone.with(|z| {
            z.as_ref().map_or_else(String::new, |z| {
                let r = if z.radius.is_finite() {
                    format!("{} nm", z.radius)
                } else {
                    "—".to_owned()
                };
                format!("{}, {} · {r}", fmt_coord(z.lat), fmt_coord(z.lon))
            })
        })
    };
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
                    <div class="font-mono text-[11px] text-white/50">{detail}</div>
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
                <div class="mt-3 grid grid-cols-2 gap-3" on:click=|ev| ev.stop_propagation()>
                    <div class="col-span-2">
                        <Field label="Name">
                            <TextInput value=name size=Size::Sm on_input=Callback::new(move |s| on_edit.run(NoFlyEdit::Name(s))) />
                        </Field>
                    </div>
                    <NumberField label="Latitude" value=get(|z| z.lat) on_change=Callback::new(move |v| on_edit.run(NoFlyEdit::Lat(v))) />
                    <NumberField label="Longitude" value=get(|z| z.lon) on_change=Callback::new(move |v| on_edit.run(NoFlyEdit::Lon(v))) />
                    <div class="col-span-2">
                        <NumberField label="Radius (nm)" value=get(|z| z.radius) on_change=Callback::new(move |v| on_edit.run(NoFlyEdit::Radius(v))) />
                    </div>
                    <div class="col-span-2">
                        <Field label="Colour">
                            <ColorSwatches selected=color on_pick=Callback::new(move |c| on_edit.run(NoFlyEdit::Color(c))) />
                        </Field>
                    </div>
                </div>
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_apply_and_bad_numbers_are_zero() {
        let mut z = NoFlyZone::blank(0);
        NoFlyEdit::Radius(7.5).apply(&mut z);
        NoFlyEdit::Color(ZoneColor::Red).apply(&mut z);
        assert_eq!(z.radius, 7.5);
        assert_eq!(z.color, ZoneColor::Red);
        assert_eq!(number_or_zero("abc"), 0.0);
        assert_eq!(number_or_zero(" -3.25 "), -3.25);
    }
}
