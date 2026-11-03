//! Widget registry (v3 `widgets/index.tsx`). A widget renders one field
//! from its schema node, its current value and a change callback. Adding a
//! widget is one module and one arm in [`render`].

mod airport;
mod approach;
mod basic;
mod common;
mod enum_select;
pub(crate) mod freq;
mod list;
mod no_fly_zones;
mod restrictive_airspace;
mod runway;
mod toggle_list;
mod training_zones;
mod waypoint;
pub(crate) mod zone_ui;

use std::sync::Arc;

use infinity_ui::prelude::*;
use serde_json::Value;

// Shared with the custom pages.
pub(crate) use common::{SchemaNote, SuggestionItem, Suggestions, WidgetLabel, debounce, describe};
pub(crate) use no_fly_zones::{NoFlyEdit, NoFlyZoneRow, NumberField};
pub(crate) use training_zones::{AddMode, ModePill, ResultRow, TrainingZoneRow, mtr_summary_line};
pub(crate) use waypoint::waypoint_detail;

use super::schema::str_prop;
use super::state::Configurator;

/// What every widget may reach beyond its own value: the configurator (for
/// nav lookups by product and whole-tree writes) and the dotted path of the
/// group it sits in, for sibling reads.
#[derive(Debug, Clone)]
pub(crate) struct WidgetCtx {
    pub cfg: Configurator,
    pub group_path: Arc<str>,
}

#[derive(Debug, Clone)]
pub(crate) struct WidgetProps {
    pub field: Arc<Value>,
    pub value: Signal<Value>,
    pub on_change: Callback<Value>,
    pub ctx: WidgetCtx,
}

impl WidgetProps {
    pub(crate) fn label(&self) -> String {
        super::schema::label_of(&self.field)
    }

    pub(crate) fn prop_str(&self, key: &str) -> Option<String> {
        str_prop(&self.field, key).map(str::to_owned)
    }
}

const SUPPORTED: &[&str] = &[
    "bool",
    "int",
    "float",
    "string",
    "enum",
    "freq",
    "toggleList",
    "list",
    "keyedList",
    "airportPick",
    "runwayPick",
    "approachPick",
    "waypointPick",
    "restrictiveAirspacePick",
    "trainingZones",
    "noFlyZones",
];

pub(crate) fn is_supported(widget_type: &str) -> bool {
    SUPPORTED.contains(&widget_type)
}

/// Renders a field by its `type`; unknown types get an inline note.
pub(crate) fn render(props: WidgetProps) -> AnyView {
    let ty = str_prop(&props.field, "type")
        .unwrap_or_default()
        .to_owned();
    match ty.as_str() {
        "bool" => basic::bool_widget(props),
        "int" => basic::number_widget(props, basic::Numeric::Int),
        "float" => basic::number_widget(props, basic::Numeric::Float),
        "string" => basic::string_widget(props),
        "enum" => enum_select::enum_widget(props),
        "freq" => freq::freq_widget(props),
        "toggleList" => toggle_list::toggle_list_widget(props),
        "list" => list::list_widget(props),
        "keyedList" => list::keyed_list_widget(props),
        "airportPick" => airport::airport_widget(props),
        "runwayPick" => runway::runway_widget(props),
        "approachPick" => approach::approach_widget(props),
        "waypointPick" => waypoint::waypoint_widget(props),
        "restrictiveAirspacePick" => restrictive_airspace::airspace_widget(props),
        "trainingZones" => training_zones::training_zones_widget(props),
        "noFlyZones" => no_fly_zones::no_fly_zones_widget(props),
        _ => {
            let id = str_prop(&props.field, "id").unwrap_or_default().to_owned();
            let message = if ty.is_empty() {
                format!("Field \u{201c}{id}\u{201d} has no widget type.")
            } else {
                format!("The \u{201c}{ty}\u{201d} widget is not supported yet (field {id}).")
            };
            view! { <SchemaNote message=message /> }.into_any()
        }
    }
}
