//! toggleList (v3 `ToggleListWidget`): a multi-select stored as the array
//! of selected item values, in schema order.

use infinity_ui::prelude::*;
use serde_json::Value;

use super::WidgetProps;
use super::common::WidgetLabel;
use crate::configurator::schema::{display_scalar, str_prop};

/// The new selection after turning `item` on or off, in `order`.
pub(crate) fn toggle(order: &[Value], selected: &[Value], item: &Value, on: bool) -> Vec<Value> {
    order
        .iter()
        .filter(|v| if *v == item { on } else { selected.contains(v) })
        .cloned()
        .collect()
}

pub(super) fn toggle_list_widget(props: WidgetProps) -> AnyView {
    let items: Vec<(Value, String)> = props
        .field
        .get("items")
        .and_then(Value::as_array)
        .map(|all| {
            all.iter()
                .filter_map(|it| {
                    let v = it.get("value")?.clone();
                    let label = str_prop(it, "label")
                        .map(str::to_owned)
                        .unwrap_or_else(|| display_scalar(&v));
                    Some((v, label))
                })
                .collect()
        })
        .unwrap_or_default();
    let order: Vec<Value> = items.iter().map(|(v, _)| v.clone()).collect();
    let order = StoredValue::new(order);
    let value = props.value;
    let on_change = props.on_change;
    let selected = Memo::new(move |_| value.with(|v| v.as_array().cloned().unwrap_or_default()));

    view! {
        <fieldset class="flex flex-col gap-3">
            <legend class="mb-3">
                <WidgetLabel label=props.label() />
            </legend>
            <div class="grid grid-cols-[repeat(auto-fill,minmax(180px,1fr))] gap-x-6 gap-y-2 border-t border-white/10 pt-3">
                {items
                    .into_iter()
                    .map(|(item, label)| {
                        let probe = item.clone();
                        let checked = Signal::derive(move || selected.with(|s| s.contains(&probe)));
                        view! {
                            <Checkbox
                                checked=checked
                                label=label
                                on_change=Callback::new(move |on: bool| {
                                    let next = order
                                        .with_value(|o| toggle(o, &selected.get_untracked(), &item, on));
                                    on_change.run(Value::Array(next));
                                })
                            />
                        }
                    })
                    .collect_view()}
            </div>
        </fieldset>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn toggling_keeps_schema_order_and_drops_unknowns() {
        let order = [json!("a"), json!(2), json!("c")];
        let sel = [json!("c"), json!("zzz")];
        assert_eq!(
            toggle(&order, &sel, &json!("a"), true),
            vec![json!("a"), json!("c")]
        );
        assert_eq!(
            toggle(&order, &sel, &json!("c"), false),
            Vec::<Value>::new()
        );
        assert_eq!(
            toggle(&order, &[json!(2)], &json!("c"), true),
            vec![json!(2), json!("c")]
        );
    }
}
