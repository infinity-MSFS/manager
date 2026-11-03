//! freq (v3 `FreqWidget`): a radio frequency in decimal MHz, snapped to
//! its band's channel step. The export pipeline converts units if the
//! output declares a transform.

use infinity_ui::prelude::*;
use serde_json::Value;

use super::WidgetProps;
use super::common::{WidgetLabel, use_draft};
use crate::configurator::schema::{display_scalar, number, str_prop};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Band {
    pub min: f64,
    pub max: f64,
    pub step: f64,
}

/// Limits for a schema `band`: `uhf`, `vhf-nav`, or VHF COM for anything
/// else (`vhf`, `vhf-com`, unknown).
pub(crate) fn band(name: &str) -> Band {
    match name {
        "uhf" => Band {
            min: 225.0,
            max: 399.975,
            step: 0.025,
        },
        "vhf-nav" => Band {
            min: 108.0,
            max: 117.95,
            step: 0.05,
        },
        _ => Band {
            min: 118.0,
            max: 136.99,
            step: 0.025,
        },
    }
}

/// Snaps `n` to the nearest multiple of `step`, trimmed of float noise
/// (`118.07500000000002` becomes `118.075`).
pub(crate) fn round_to_step(n: f64, step: f64) -> f64 {
    if step <= 0.0 {
        return n;
    }
    let snapped = (n / step).round() * step;
    (snapped * 1e6).round() / 1e6
}

impl Band {
    pub(crate) fn contains(self, mhz: f64) -> bool {
        mhz >= self.min - 1e-9 && mhz <= self.max + 1e-9
    }
}

fn show(v: &Value) -> String {
    match v {
        Value::Number(_) => display_scalar(v),
        _ => String::new(),
    }
}

fn parse(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|n| n.is_finite())
}

pub(super) fn freq_widget(props: WidgetProps) -> AnyView {
    let name = str_prop(&props.field, "band").unwrap_or("vhf").to_owned();
    let limits = band(&name);
    // The draft keeps what was typed; the stored value is the snapped one,
    // so compare snapped to snapped.
    let draft = use_draft(props.value, show, move |s| {
        parse(s).map(|n| number(round_to_step(n, limits.step)))
    });
    let on_change = props.on_change;
    let invalid = Signal::derive(move || {
        draft.with(|d| parse(d).is_some_and(|n| !limits.contains(round_to_step(n, limits.step))))
    });
    view! {
        <label class="flex flex-col gap-1.5">
            <WidgetLabel label=props.label() aside=format!("{} MHz", name.to_uppercase()) />
            <TextInput
                value=draft
                input_type="number"
                invalid=invalid
                placeholder=format!("{:.3}", limits.min)
                on_input=Callback::new(move |s: String| {
                    draft.set(s.clone());
                    if let Some(n) = parse(&s) {
                        on_change.run(number(round_to_step(n, limits.step)));
                    }
                })
            />
        </label>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_match_v3() {
        assert_eq!(band("uhf").min, 225.0);
        assert_eq!(band("vhf-nav").step, 0.05);
        assert_eq!(band("vhf"), band("vhf-com"));
        assert_eq!(band("nonsense"), band("vhf"));
        assert!(band("vhf").contains(136.99));
        assert!(!band("vhf").contains(117.0));
    }

    #[test]
    fn snapping_rounds_to_the_channel_and_drops_noise() {
        assert_eq!(round_to_step(118.07, 0.025), 118.075);
        assert_eq!(round_to_step(118.06, 0.025), 118.05);
        assert_eq!(round_to_step(110.33, 0.05), 110.35);
        assert_eq!(round_to_step(243.0, 0.025), 243.0);
        assert_eq!(round_to_step(5.0, 0.0), 5.0);
    }
}
