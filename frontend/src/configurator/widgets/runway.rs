//! runwayPick (v3 `RunwayPickWidget`): the runways of a sibling airport
//! field (`dependsOn`, default `Airport_icao`). A runway the new airport
//! lacks is cleared.

use infinity_ipc::commands as cmd;
use infinity_ipc::nav::NavAirport;
use infinity_ui::prelude::*;
use serde_json::Value;

use super::WidgetProps;
use super::common::WidgetLabel;
use crate::configurator::schema::{display_scalar, read_sibling};
use crate::ipc;

const DEFAULT_DEPENDS_ON: &str = "Airport_icao";

pub(super) fn runway_widget(props: WidgetProps) -> AnyView {
    let depends_on = props
        .prop_str("dependsOn")
        .unwrap_or_else(|| DEFAULT_DEPENDS_ON.to_owned());
    let cfg = props.ctx.cfg;
    let group_path = props.ctx.group_path.clone();
    let airport = Memo::new(move |_| {
        cfg.value.with(|root| {
            read_sibling(root, &group_path, &depends_on)
                .map(display_scalar)
                .unwrap_or_default()
        })
    });
    let value = props.value;
    let on_change = props.on_change;
    let current = Memo::new(move |_| value.with(display_scalar));

    let runways = LocalResource::new(move || {
        let icao = airport.get();
        async move {
            if icao.is_empty() {
                return Vec::new();
            }
            let args = NavAirport {
                product: cfg.product_ref(),
                icao,
            };
            ipc::invoke::<cmd::NavRunways>(&args)
                .await
                .map(|rows| rows.into_iter().map(|r| r.runway_identifier).collect())
                .unwrap_or_default()
        }
    });
    let list = Signal::derive(move || runways.get().unwrap_or_default());
    let loading = Signal::derive(move || !airport.get().is_empty() && runways.get().is_none());

    // Drop a selection the new airport does not offer.
    Effect::new(move |_| {
        let offered = list.get();
        let cur = current.get_untracked();
        if !cur.is_empty() && !offered.is_empty() && !offered.contains(&cur) {
            on_change.run(Value::String(String::new()));
        }
    });

    let placeholder = move || {
        if airport.get().is_empty() {
            "Select airport first"
        } else if loading.get() {
            "Loading…"
        } else if list.with(Vec::is_empty) {
            "No runways"
        } else {
            "Select runway"
        }
    };

    view! {
        <div class="flex flex-col gap-1.5">
            <WidgetLabel label=props.label() />
            {move || {
                view! {
                    <Select
                        value=current
                        groups=Signal::derive(move || SelectGroup::flat(list.get()))
                        placeholder=placeholder()
                        disabled=Signal::derive(move || {
                            airport.get().is_empty() || loading.get() || list.with(Vec::is_empty)
                        })
                        on_change=Callback::new(move |s: String| on_change.run(Value::String(s)))
                    />
                }
            }}
        </div>
    }
    .into_any()
}
