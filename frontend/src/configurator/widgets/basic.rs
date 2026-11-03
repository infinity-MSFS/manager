//! bool, int, float and string (v3 `BoolWidget`, `IntWidget`,
//! `FloatWidget`, `StringWidget`).

use infinity_ui::prelude::*;
use serde_json::Value;

use super::WidgetProps;
use super::common::{WidgetLabel, use_draft};
use crate::configurator::schema::{display_scalar, num_prop, number};

pub(super) fn bool_widget(props: WidgetProps) -> AnyView {
    let value = props.value;
    let on_change = props.on_change;
    let checked = Signal::derive(move || value.with(|v| v.as_bool() == Some(true)));
    view! {
        <div class="py-1">
            <Switch
                checked=checked
                on_change=Callback::new(move |on: bool| on_change.run(Value::Bool(on)))
                label=props.label()
            />
        </div>
    }
    .into_any()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Numeric {
    Int,
    Float,
}

fn show_number(v: &Value) -> String {
    match v {
        Value::Number(_) => display_scalar(v),
        _ => String::new(),
    }
}

fn parse_int(s: &str) -> Option<Value> {
    // `parseInt` semantics: leading integer part, so `12.7` is 12.
    let s = s.trim();
    let end = s
        .char_indices()
        .find(|&(i, c)| !(c.is_ascii_digit() || (i == 0 && (c == '-' || c == '+'))))
        .map_or(s.len(), |(i, _)| i);
    s.get(..end)?.parse::<i64>().ok().map(Value::from)
}

fn parse_float(s: &str) -> Option<Value> {
    s.trim()
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
        .map(number)
}

pub(super) fn number_widget(props: WidgetProps, kind: Numeric) -> AnyView {
    let parse = match kind {
        Numeric::Int => parse_int,
        Numeric::Float => parse_float,
    };
    let draft = use_draft(props.value, show_number, parse);
    let on_change = props.on_change;
    let min = num_prop(&props.field, "min");
    let max = num_prop(&props.field, "max");
    let invalid = Signal::derive(move || {
        let n = draft.with(|d| parse(d)).and_then(|v| v.as_f64());
        n.is_some_and(|n| min.is_some_and(|m| n < m) || max.is_some_and(|m| n > m))
    });
    let range = match (min, max) {
        (Some(a), Some(b)) => Some(format!("{a} – {b}")),
        (Some(a), None) => Some(format!("≥ {a}")),
        (None, Some(b)) => Some(format!("≤ {b}")),
        (None, None) => None,
    };
    view! {
        <label class="flex flex-col gap-1.5">
            <WidgetLabel label=props.label() aside=range.unwrap_or_default() />
            <TextInput
                value=draft
                input_type="number"
                invalid=invalid
                on_input=Callback::new(move |s: String| {
                    draft.set(s.clone());
                    if let Some(v) = parse(&s) {
                        on_change.run(v);
                    }
                })
            />
        </label>
    }
    .into_any()
}

pub(super) fn string_widget(props: WidgetProps) -> AnyView {
    let value = props.value;
    let on_change = props.on_change;
    let max_len = num_prop(&props.field, "maxLength")
        .filter(|n| *n >= 0.0)
        .map(|n| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let n = n as usize;
            n
        });
    let text = Signal::derive(move || {
        value.with(|v| match v {
            Value::String(s) => s.clone(),
            _ => String::new(),
        })
    });
    view! {
        <label class="flex flex-col gap-1.5">
            <WidgetLabel label=props.label() />
            <TextInput
                value=text
                on_input=Callback::new(move |s: String| {
                    let s = match max_len {
                        Some(n) => s.chars().take(n).collect(),
                        None => s,
                    };
                    on_change.run(Value::String(s));
                })
            />
        </label>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn int_parses_like_parse_int() {
        assert_eq!(parse_int("12"), Some(json!(12)));
        assert_eq!(parse_int("12.7"), Some(json!(12)));
        assert_eq!(parse_int("-3"), Some(json!(-3)));
        assert_eq!(parse_int(""), None);
        assert_eq!(parse_int("-"), None);
    }

    #[test]
    fn float_parses_and_rejects_garbage() {
        assert_eq!(parse_float("1.5"), Some(json!(1.5)));
        assert_eq!(parse_float("2"), Some(json!(2)));
        assert_eq!(parse_float("x"), None);
        assert_eq!(show_number(&json!(2.25)), "2.25");
        assert_eq!(show_number(&json!("a")), "");
    }
}
