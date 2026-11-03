//! list and keyedList (v3 `ListWidget`, `KeyedListWidget`).
//!
//! A `list` is an array whose items are either objects rendered from
//! `itemSchema.fields` or scalars rendered by one `widget`. A `keyedList`
//! is an object keyed `keyPrefix + n` (`APP_500`, `APP_501`, ...); adding
//! takes the lowest free number from `keyStart`, so existing keys never
//! shift (the gauges address them by key).

use std::sync::Arc;

use infinity_ui::prelude::*;
use serde_json::{Map, Value};

use super::common::{SchemaNote, WidgetLabel};
use super::{WidgetCtx, WidgetProps, render};
use crate::configurator::schema::{num_prop, object_mut, str_prop};

/// An edit to a container, applied to a copy and written back.
type Edit<T> = Box<dyn FnOnce(&mut T)>;

/// What one item is made of.
#[derive(Debug, Clone)]
enum ItemShape {
    Fields(Arc<Vec<Value>>),
    Widget(Arc<Value>),
    Missing,
}

fn shape_of(field: &Value) -> ItemShape {
    if let Some(fields) = field
        .get("itemSchema")
        .and_then(|s| s.get("fields"))
        .and_then(Value::as_array)
    {
        return ItemShape::Fields(Arc::new(fields.clone()));
    }
    match field.get("widget") {
        Some(w) if w.is_object() => ItemShape::Widget(Arc::new(w.clone())),
        _ => ItemShape::Missing,
    }
}

fn blank(shape: &ItemShape) -> Value {
    match shape {
        ItemShape::Fields(_) => Value::Object(Map::new()),
        ItemShape::Widget(_) => Value::String(String::new()),
        ItemShape::Missing => Value::Null,
    }
}

/// Swaps items `i` and `j` when both exist.
pub(crate) fn swap(items: &mut [Value], i: usize, j: usize) {
    if i < items.len() && j < items.len() {
        items.swap(i, j);
    }
}

/// The lowest free key `prefix + n` with `n >= start`.
pub(crate) fn next_key(existing: &Map<String, Value>, prefix: &str, start: i64) -> String {
    let mut n = start;
    loop {
        let key = format!("{prefix}{n}");
        if !existing.contains_key(&key) {
            return key;
        }
        n += 1;
    }
}

/// Renders one item: its fields, or its single widget. `item` and `set`
/// address the item inside its container.
fn item_editor(
    shape: &ItemShape,
    item: Signal<Value>,
    set: Callback<Value>,
    ctx: &WidgetCtx,
) -> AnyView {
    match shape {
        ItemShape::Fields(fields) => fields
            .iter()
            .map(|f| {
                let id = str_prop(f, "id").unwrap_or_default().to_owned();
                let read = id.clone();
                let value = Signal::derive(move || {
                    item.with(|v| v.get(&read).cloned().unwrap_or(Value::Null))
                });
                let on_change = Callback::new(move |next: Value| {
                    let mut obj = item.get_untracked();
                    if let Some(map) = object_mut(&mut obj) {
                        map.insert(id.clone(), next);
                    }
                    set.run(obj);
                });
                render(WidgetProps {
                    field: Arc::new(f.clone()),
                    value,
                    on_change,
                    ctx: ctx.clone(),
                })
            })
            .collect_view()
            .into_any(),
        ItemShape::Widget(widget) => render(WidgetProps {
            field: widget.clone(),
            value: item,
            on_change: set,
            ctx: ctx.clone(),
        }),
        ItemShape::Missing => view! {
            <SchemaNote message="A list field needs an `itemSchema` with `fields`, or a `widget`." />
        }
        .into_any(),
    }
}

pub(super) fn list_widget(props: WidgetProps) -> AnyView {
    let shape = shape_of(&props.field);
    let value = props.value;
    let on_change = props.on_change;
    let items = Memo::new(move |_| value.with(|v| v.as_array().cloned().unwrap_or_default()));
    let len = Memo::new(move |_| items.with(Vec::len));
    let edit = move |f: Edit<Vec<Value>>| {
        let mut next = items.get_untracked();
        f(&mut next);
        on_change.run(Value::Array(next));
    };
    let ctx = props.ctx.clone();
    let add_shape = shape.clone();

    view! {
        <fieldset class="flex flex-col gap-3">
            <legend class="mb-3">
                <WidgetLabel label=props.label() aside=Signal::derive(move || format!("{:02}", len.get())) />
            </legend>
            {move || {
                (len.get() == 0)
                    .then(|| {
                        view! {
                            <p class="border-t border-white/10 py-3 text-sm font-light text-white/40">
                                "Empty."
                            </p>
                        }
                    })
            }}
            <For each=move || 0..len.get() key=|i| *i let:i>
                {
                    let item = Signal::derive(move || {
                        items.with(|all| all.get(i).cloned().unwrap_or(Value::Null))
                    });
                    let set = Callback::new(move |v: Value| {
                        edit(Box::new(move |all| {
                            if let Some(slot) = all.get_mut(i) {
                                *slot = v;
                            }
                        }));
                    });
                    let editor = item_editor(&shape, item, set, &ctx);
                    view! {
                        <div class="flex items-start gap-4 border-t border-white/10 pt-3">
                            <span class="w-6 pt-2 font-mono text-[11px] text-accent-400/80">
                                {format!("{:02}", i + 1)}
                            </span>
                            <div class="flex min-w-0 flex-1 flex-col gap-3">{editor}</div>
                            <div class="flex shrink-0 gap-1">
                                <IconButton
                                    icon=icons::CHEVRON_UP
                                    label="Move up"
                                    variant=Variant::Ghost
                                    size=Size::Sm
                                    disabled=Signal::derive(move || i == 0)
                                    on_click=Callback::new(move |()| {
                                        if i > 0 {
                                            edit(Box::new(move |all| swap(all, i - 1, i)));
                                        }
                                    })
                                />
                                <IconButton
                                    icon=icons::CHEVRON_DOWN
                                    label="Move down"
                                    variant=Variant::Ghost
                                    size=Size::Sm
                                    disabled=Signal::derive(move || i + 1 >= len.get())
                                    on_click=Callback::new(move |()| {
                                        edit(Box::new(move |all| swap(all, i, i + 1)));
                                    })
                                />
                                <IconButton
                                    icon=icons::TRASH_2
                                    label="Remove"
                                    variant=Variant::Ghost
                                    tone=Tone::Danger
                                    size=Size::Sm
                                    on_click=Callback::new(move |()| {
                                        edit(Box::new(move |all| {
                                            if i < all.len() {
                                                all.remove(i);
                                            }
                                        }));
                                    })
                                />
                            </div>
                        </div>
                    }
                }
            </For>
            <div>
                <Button
                    variant=Variant::Glass
                    size=Size::Sm
                    icon=icons::PLUS
                    on_click=Callback::new(move |()| {
                        let item = blank(&add_shape);
                        edit(Box::new(move |all| all.push(item)));
                    })
                >
                    "Add"
                </Button>
            </div>
        </fieldset>
    }
    .into_any()
}

pub(super) fn keyed_list_widget(props: WidgetProps) -> AnyView {
    let shape = match shape_of(&props.field) {
        // A keyed item is always an object.
        ItemShape::Widget(_) => ItemShape::Missing,
        s => s,
    };
    let prefix = str_prop(&props.field, "keyPrefix")
        .unwrap_or_default()
        .to_owned();
    #[allow(clippy::cast_possible_truncation)]
    let start = num_prop(&props.field, "keyStart").unwrap_or(0.0) as i64;
    let value = props.value;
    let on_change = props.on_change;
    let entries = Memo::new(move |_| value.with(|v| v.as_object().cloned().unwrap_or_default()));
    let keys = Memo::new(move |_| entries.with(|m| m.keys().cloned().collect::<Vec<_>>()));
    let edit = move |f: Edit<Map<String, Value>>| {
        let mut next = entries.get_untracked();
        f(&mut next);
        on_change.run(Value::Object(next));
    };
    let ctx = props.ctx.clone();
    let add_shape = shape.clone();

    view! {
        <fieldset class="flex flex-col gap-3">
            <legend class="mb-3">
                <WidgetLabel label=props.label() aside=Signal::derive(move || format!("{:02}", keys.with(Vec::len))) />
            </legend>
            {move || {
                keys.with(Vec::is_empty)
                    .then(|| {
                        view! {
                            <p class="border-t border-white/10 py-3 text-sm font-light text-white/40">
                                "Empty."
                            </p>
                        }
                    })
            }}
            <For each=move || keys.get() key=|k| k.clone() let:key>
                {
                    let read = key.clone();
                    let item = Signal::derive(move || {
                        entries.with(|m| m.get(&read).cloned().unwrap_or(Value::Null))
                    });
                    let write = key.clone();
                    let set = Callback::new(move |v: Value| {
                        let k = write.clone();
                        edit(Box::new(move |m| {
                            m.insert(k, v);
                        }));
                    });
                    let remove = key.clone();
                    let editor = item_editor(&shape, item, set, &ctx);
                    view! {
                        <div class="flex items-start gap-4 border-t border-white/10 pt-3">
                            <div class="flex min-w-0 flex-1 flex-col gap-3">
                                <span class="font-mono text-[11px] tracking-[0.2em] text-accent-400/80 uppercase">
                                    {key.clone()}
                                </span>
                                {editor}
                            </div>
                            <IconButton
                                icon=icons::TRASH_2
                                label="Remove"
                                variant=Variant::Ghost
                                tone=Tone::Danger
                                size=Size::Sm
                                on_click=Callback::new(move |()| {
                                    let k = remove.clone();
                                    edit(Box::new(move |m| {
                                        m.remove(&k);
                                    }));
                                })
                            />
                        </div>
                    }
                }
            </For>
            <div>
                <Button
                    variant=Variant::Glass
                    size=Size::Sm
                    icon=icons::PLUS
                    on_click=Callback::new(move |()| {
                        let item = blank(&add_shape);
                        let prefix = prefix.clone();
                        edit(Box::new(move |m| {
                            let k = next_key(m, &prefix, start);
                            m.insert(k, item);
                        }));
                    })
                >
                    "Add"
                </Button>
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
    fn next_key_fills_the_lowest_gap() {
        let m = json!({ "APP_500": {}, "APP_502": {} });
        let m = m.as_object().cloned().unwrap_or_default();
        assert_eq!(next_key(&m, "APP_", 500), "APP_501");
        assert_eq!(next_key(&Map::new(), "", 3), "3");
    }

    #[test]
    fn shapes_and_blanks() {
        let f = json!({ "itemSchema": { "fields": [ { "id": "a" } ] } });
        assert!(matches!(shape_of(&f), ItemShape::Fields(_)));
        assert_eq!(blank(&shape_of(&f)), json!({}));
        let w = json!({ "widget": { "type": "string" } });
        assert_eq!(blank(&shape_of(&w)), json!(""));
        assert_eq!(blank(&shape_of(&json!({}))), Value::Null);
        let mut v = vec![json!(1), json!(2)];
        swap(&mut v, 0, 1);
        assert_eq!(v, vec![json!(2), json!(1)]);
        swap(&mut v, 1, 5);
        assert_eq!(v, vec![json!(2), json!(1)]);
    }
}
