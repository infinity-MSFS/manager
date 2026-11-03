//! UI shared by the zone widgets and the Zones page.

use infinity_ui::prelude::*;

use crate::configurator::map::{MapArea, MapCircle, US_CENTER};
use crate::configurator::zones::{NoFlyZone, TrainingZone, ZoneColor};

/// A row of colour swatches.
#[component]
pub(crate) fn ColorSwatches(
    #[prop(into)] selected: Signal<ZoneColor>,
    #[prop(into)] on_pick: Callback<ZoneColor>,
) -> impl IntoView {
    view! {
        <div class="flex gap-1.5">
            {ZoneColor::ALL
                .into_iter()
                .map(|c| {
                    view! {
                        <button
                            type="button"
                            aria-label=c.name()
                            aria-pressed=move || (selected.get() == c).to_string()
                            class=move || {
                                cn!(
                                    "h-6 w-6 rounded-full border-2 transition-colors",
                                    if selected.get() == c { "border-white" } else { "border-white/20 hover:border-white/50" },
                                )
                            }
                            style=format!("background:{}", c.css())
                            on:click=move |ev| {
                                ev.stop_propagation();
                                on_pick.run(c);
                            }
                        ></button>
                    }
                })
                .collect_view()}
        </div>
    }
}

/// A coloured dot.
#[component]
pub(crate) fn ZoneDot(#[prop(into)] color: Signal<ZoneColor>) -> impl IntoView {
    view! {
        <span
            aria-hidden="true"
            class="mt-1 h-3 w-3 shrink-0 rounded-full"
            style=move || format!("background:{}", color.get().css())
        ></span>
    }
}

pub(crate) fn training_areas(zones: &[TrainingZone]) -> Vec<MapArea> {
    zones
        .iter()
        .enumerate()
        .filter_map(|(i, z)| {
            let rings: Vec<_> = z
                .boundary
                .iter()
                .filter(|r| r.len() >= 3)
                .cloned()
                .collect();
            (!rings.is_empty()).then(|| MapArea {
                id: format!("tz-{i}"),
                name: z.title(i),
                boundary: rings,
                color: Some(z.color.css().to_owned()),
            })
        })
        .collect()
}

pub(crate) fn no_fly_circles(zones: &[NoFlyZone]) -> Vec<MapCircle> {
    zones
        .iter()
        .enumerate()
        .filter(|(_, z)| z.lat.is_finite() && z.lon.is_finite() && z.radius.is_finite())
        .map(|(i, z)| MapCircle {
            id: format!("nfz-{i}"),
            name: if z.name.is_empty() {
                format!("No-Fly Zone {}", i + 1)
            } else {
                z.name.clone()
            },
            lat: z.lat,
            lon: z.lon,
            radius_nm: z.radius,
            color: Some(z.color.css().to_owned()),
        })
        .collect()
}

/// Where a training-zone map looks: the selected zone's first point, else
/// the first zone's, else the middle of the US.
pub(crate) fn training_center(zones: &[TrainingZone], selected: Option<usize>) -> (f64, f64) {
    let first = |z: &TrainingZone| {
        z.boundary
            .first()
            .and_then(|r| r.first())
            .map(|p| (p.lat, p.lon))
    };
    selected
        .and_then(|i| zones.get(i))
        .and_then(first)
        .or_else(|| zones.first().and_then(first))
        .unwrap_or(US_CENTER)
}

pub(crate) fn no_fly_center(zones: &[NoFlyZone], selected: Option<usize>) -> (f64, f64) {
    let ok = |z: &NoFlyZone| (z.lat.is_finite() && z.lon.is_finite()).then_some((z.lat, z.lon));
    selected
        .and_then(|i| zones.get(i))
        .and_then(ok)
        .or_else(|| zones.iter().find_map(ok))
        .unwrap_or(US_CENTER)
}

/// The selection after removing `removed`.
pub(crate) fn selection_after_remove(
    selected: Option<usize>,
    removed: usize,
    remaining: usize,
) -> Option<usize> {
    match selected {
        Some(s) if s == removed => (remaining > 0).then(|| removed.min(remaining - 1)),
        Some(s) if removed < s => Some(s - 1),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removing_shifts_or_clears_the_selection() {
        assert_eq!(selection_after_remove(Some(2), 2, 2), Some(1));
        assert_eq!(selection_after_remove(Some(0), 0, 0), None);
        assert_eq!(selection_after_remove(Some(3), 1, 4), Some(2));
        assert_eq!(selection_after_remove(Some(0), 1, 4), Some(0));
        assert_eq!(selection_after_remove(None, 1, 4), None);
    }

    #[test]
    fn centers_fall_back_to_the_us() {
        assert_eq!(training_center(&[], None), US_CENTER);
        assert_eq!(no_fly_center(&[NoFlyZone::blank(0)], Some(0)), (0.0, 0.0));
    }
}
