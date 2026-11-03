//! enum (v3 `EnumWidget`). Options keep their JSON type: a numeric option
//! writes a number back, not its string form.

use infinity_ui::prelude::*;
use serde_json::Value;

use super::WidgetProps;
use super::common::WidgetLabel;
use crate::configurator::schema::{display_scalar, str_prop};

pub(super) fn enum_widget(props: WidgetProps) -> AnyView {
    let raw: Vec<(Value, String)> = props
        .field
        .get("options")
        .and_then(Value::as_array)
        .map(|opts| {
            opts.iter()
                .filter_map(|o| {
                    let value = o.get("value")?.clone();
                    let label = str_prop(o, "label")
                        .map(str::to_owned)
                        .unwrap_or_else(|| display_scalar(&value));
                    Some((value, label))
                })
                .collect()
        })
        .unwrap_or_default();
    let groups = vec![SelectGroup::new(
        "",
        raw.iter()
            .map(|(v, l)| SelectOption::new(display_scalar(v), l.clone()))
            .collect(),
    )];
    let raw = StoredValue::new(raw);
    let value = props.value;
    let on_change = props.on_change;
    let current = Signal::derive(move || {
        value.with(|v| {
            if v.is_null() {
                String::new()
            } else {
                display_scalar(v)
            }
        })
    });
    view! {
        <div class="flex flex-col gap-1.5">
            <WidgetLabel label=props.label() />
            <Select
                value=current
                groups=Signal::stored(groups)
                placeholder="—"
                on_change=Callback::new(move |s: String| {
                    let hit = raw.with_value(|r| r.iter().find(|(v, _)| display_scalar(v) == s).map(|(v, _)| v.clone()));
                    if let Some(v) = hit {
                        on_change.run(v);
                    }
                })
            />
        </div>
    }
    .into_any()
}
