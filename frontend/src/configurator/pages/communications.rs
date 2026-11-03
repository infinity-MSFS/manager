//! Communications page (v3 `CommunicationsPage.tsx`): 40 VHF and 40 UHF
//! presets. `COM.VHF` and `COM.UHF` are arrays of MHz floats; older
//! encodings (kHz integers, MHz x 100) are normalised on read, as v1 did.
//! Inputs auto-format on commit (`1183` becomes `118.300`).

use infinity_ui::prelude::*;
use serde_json::Value;
use wasm_bindgen::JsCast;

use super::PageCtx;
use crate::configurator::schema::{number, path_get};
use crate::editorial::EYEBROW;

const PRESET_COUNT: usize = 40;
const ROWS_PER_COLUMN: usize = 20;
const VHF_BASE: f64 = 118.0;
const UHF_BASE: f64 = 225.0;

fn round3(n: f64) -> f64 {
    (n * 1000.0).round() / 1000.0
}

/// MHz from text, rounded to kHz; `None` when blank or not a number.
pub(crate) fn ui_to_mhz(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    t.parse::<f64>().ok().filter(|n| n.is_finite()).map(round3)
}

pub(crate) fn mhz_to_ui(n: f64) -> String {
    if n.is_finite() {
        format!("{n:.3}")
    } else {
        String::new()
    }
}

/// A stored preset in MHz: kHz integers (`118300`) and MHz x 100 integers
/// (`11830`) are converted; zero and garbage fall back to the band base.
pub(crate) fn normalize_stored(value: f64, fallback: f64) -> f64 {
    if !value.is_finite() || value == 0.0 {
        return fallback;
    }
    let abs = value.abs();
    let mhz = if abs >= 100_000.0 {
        value / 1_000.0
    } else if abs >= 1_000.0 {
        value / 100.0
    } else {
        value
    };
    round3(mhz)
}

/// Exactly `len` normalised presets, padded with `base`.
pub(crate) fn presets(stored: Option<&Value>, len: usize, base: f64) -> Vec<f64> {
    let mut out: Vec<f64> = stored
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .take(len)
                .map(|v| normalize_stored(v.as_f64().unwrap_or(f64::NAN), base))
                .collect()
        })
        .unwrap_or_default();
    out.resize(len, base);
    out
}

/// Digits and one decimal point, at most three decimals (while typing).
pub(crate) fn format_frequency(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut parts = cleaned.splitn(2, '.');
    let whole = parts.next().unwrap_or_default();
    match parts.next() {
        None => whole.to_owned(),
        Some(rest) => {
            let decimals: String = rest.chars().filter(|c| *c != '.').take(3).collect();
            format!("{whole}.{decimals}")
        }
    }
}

/// Completes a typed frequency on commit: `118.3` to `118.300`, `118300`
/// to `118.300`, `1183` to `118.300`, `118` to `118.000`.
pub(crate) fn auto_format_frequency(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if cleaned.is_empty() {
        return String::new();
    }
    if cleaned.contains('.') {
        let mut parts = cleaned.split('.');
        let whole = parts.next().unwrap_or_default();
        let decimal = parts.next().unwrap_or_default();
        let mut d: String = decimal.chars().take(3).collect();
        while d.len() < 3 {
            d.push('0');
        }
        return format!("{whole}.{d}");
    }
    let n = cleaned.len();
    if n >= 6 {
        return format!("{}.{}", &cleaned[..n - 3], &cleaned[n - 3..]);
    }
    if n >= 4 {
        let mut d = cleaned[3..].to_owned();
        while d.len() < 3 {
            d.push('0');
        }
        return format!("{}.{d}", &cleaned[..3]);
    }
    format!("{cleaned}.000")
}

fn in_range(s: &str, lo: f64, hi: f64) -> bool {
    if s.trim().is_empty() {
        return true;
    }
    s.trim()
        .parse::<f64>()
        .is_ok_and(|f| f.is_finite() && f >= lo && f <= hi)
}

pub(crate) fn validate_vhf(s: &str) -> bool {
    in_range(s, 118.0, 150.8)
}

pub(crate) fn validate_uhf(s: &str) -> bool {
    in_range(s, 225.0, 399.975)
}

#[derive(Debug, Clone, Copy)]
struct Band {
    key: &'static str,
    label: &'static str,
    title: &'static str,
    subtitle: &'static str,
    base: f64,
    validate: fn(&str) -> bool,
}

const VHF: Band = Band {
    key: "VHF",
    label: "VHF",
    title: "VHF presets",
    subtitle: "118.000 – 136.975 MHz",
    base: VHF_BASE,
    validate: validate_vhf,
};

const UHF: Band = Band {
    key: "UHF",
    label: "UHF",
    title: "UHF presets",
    subtitle: "225.000 – 399.975 MHz",
    base: UHF_BASE,
    validate: validate_uhf,
};

pub(super) fn page(ctx: PageCtx) -> AnyView {
    view! {
        <div class="flex flex-col">
            <header class="mb-10">
                <h2 class="text-3xl font-extralight tracking-tight">"Communications"</h2>
            </header>
            <div class="grid gap-12 xl:grid-cols-2">
                <PresetColumn ctx=ctx.clone() band=VHF />
                <PresetColumn ctx=ctx band=UHF />
            </div>
        </div>
    }
    .into_any()
}

#[component]
fn PresetColumn(ctx: PageCtx, band: Band) -> impl IntoView {
    let cfg = ctx.cfg;
    let path = format!("COM.{}", band.key);
    let read = path.clone();
    let values = Memo::new(move |_| {
        cfg.value
            .with(|root| presets(path_get(root, &read), PRESET_COUNT, band.base))
    });
    let set_at = move |i: usize, ui: String| {
        let mhz = ui_to_mhz(&format_frequency(&ui));
        let mut next = values.get_untracked();
        if let Some(slot) = next.get_mut(i) {
            *slot = mhz.filter(|m| *m != 0.0).unwrap_or(band.base);
        }
        cfg.set_at(&path, Value::Array(next.into_iter().map(number).collect()));
    };
    let set_at = StoredValue::new(set_at);
    view! {
        <section class="border-t border-white/10 pt-5">
            <div class="mb-6 flex items-baseline justify-between">
                <span class="text-lg font-light">{band.title}</span>
                <span class="font-mono text-[11px] text-accent-300">{band.subtitle}</span>
            </div>
            <div
                class="grid gap-x-6 gap-y-3"
                style=format!("grid-template-rows:repeat({ROWS_PER_COLUMN},minmax(0,1fr));grid-auto-flow:column")
            >
                {(0..PRESET_COUNT)
                    .map(|i| {
                        let value = Signal::derive(move || {
                            values.with(|v| v.get(i).copied().map(mhz_to_ui).unwrap_or_default())
                        });
                        view! {
                            <div class="flex items-center gap-3">
                                <span class=cn!("w-16 shrink-0", EYEBROW, "tracking-[0.15em] text-white/60")>
                                    {format!("{} {:02}", band.label, i + 1)}
                                </span>
                                <FrequencyInput
                                    value=value
                                    placeholder=mhz_to_ui(band.base)
                                    validate=band.validate
                                    on_commit=Callback::new(move |s: String| set_at.with_value(|f| f(i, s)))
                                />
                            </div>
                        }
                    })
                    .collect_view()}
            </div>
        </section>
    }
}

fn input_of(ev: &leptos::ev::Event) -> Option<web_sys::HtmlInputElement> {
    ev.target()?.dyn_into::<web_sys::HtmlInputElement>().ok()
}

/// Edits freely while focused; on blur, Enter or Tab the text is
/// auto-formatted and committed if valid, otherwise reverted. Escape
/// reverts.
#[component]
fn FrequencyInput(
    #[prop(into)] value: Signal<String>,
    #[prop(into)] placeholder: String,
    validate: fn(&str) -> bool,
    #[prop(into)] on_commit: Callback<String>,
) -> impl IntoView {
    let local = RwSignal::new(value.get_untracked());
    let focused = RwSignal::new(false);
    // Enter commits, then blurs; the blur must not commit again.
    let committed = RwSignal::new(false);
    let shown = Signal::derive(move || {
        if focused.get() {
            local.get()
        } else {
            value.get()
        }
    });
    let invalid =
        Signal::derive(move || !focused.get() && value.with(|v| !v.is_empty() && !validate(v)));
    let commit = {
        move || {
            if committed.get_untracked() {
                return;
            }
            committed.set(true);
            let formatted = auto_format_frequency(&local.get_untracked());
            if !formatted.is_empty() && validate(&formatted) {
                on_commit.run(formatted);
            } else {
                local.set(value.get_untracked());
            }
        }
    };
    let on_blur = commit;
    let on_key = commit;
    view! {
        <div
            class="min-w-0 flex-1"
            on:focusin=move |ev| {
                local.set(value.get_untracked());
                focused.set(true);
                committed.set(false);
                if let Some(input) = input_of(&ev) {
                    input.select();
                }
            }
            on:focusout=move |_| {
                focused.set(false);
                on_blur();
            }
            on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                match ev.key().as_str() {
                    "Enter" => {
                        ev.prevent_default();
                        on_key();
                        if let Some(input) = input_of(&ev) {
                            let _ = input.blur();
                        }
                    }
                    "Tab" => on_key(),
                    "Escape" => {
                        ev.prevent_default();
                        local.set(value.get_untracked());
                        committed.set(true);
                        if let Some(input) = input_of(&ev) {
                            let _ = input.blur();
                        }
                    }
                    _ => {}
                }
            }
        >
            <TextInput
                value=shown
                placeholder=placeholder
                invalid=invalid
                size=Size::Sm
                class="font-mono tracking-wide"
                on_input=Callback::new(move |s: String| {
                    local.set(s.chars().take(10).collect());
                    committed.set(false);
                })
            />
        </div>
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn stored_encodings_normalise() {
        assert_eq!(normalize_stored(118_300.0, VHF_BASE), 118.3);
        assert_eq!(normalize_stored(11_830.0, VHF_BASE), 118.3);
        assert_eq!(normalize_stored(243.0, UHF_BASE), 243.0);
        assert_eq!(normalize_stored(0.0, UHF_BASE), UHF_BASE);
        assert_eq!(normalize_stored(f64::NAN, VHF_BASE), VHF_BASE);
    }

    #[test]
    fn presets_pad_and_truncate() {
        let p = presets(Some(&json!([118300, "x", 121.5])), 4, VHF_BASE);
        assert_eq!(p, vec![118.3, VHF_BASE, 121.5, VHF_BASE]);
        assert_eq!(presets(None, 2, UHF_BASE), vec![UHF_BASE, UHF_BASE]);
        let many: Vec<Value> = (0..50).map(|_| json!(120.0)).collect();
        assert_eq!(
            presets(Some(&Value::Array(many)), PRESET_COUNT, VHF_BASE).len(),
            40
        );
    }

    #[test]
    fn auto_format_matches_v1() {
        assert_eq!(auto_format_frequency("118.3"), "118.300");
        assert_eq!(auto_format_frequency("118300"), "118.300");
        assert_eq!(auto_format_frequency("1183"), "118.300");
        assert_eq!(auto_format_frequency("118"), "118.000");
        assert_eq!(auto_format_frequency("12"), "12.000");
        assert_eq!(auto_format_frequency("1.2.3"), "1.200");
        assert_eq!(auto_format_frequency("abc"), "");
        assert_eq!(auto_format_frequency("243.0000"), "243.000");
    }

    #[test]
    fn typing_format_and_validation() {
        assert_eq!(format_frequency("12a3.45.67"), "123.456");
        assert_eq!(format_frequency("118"), "118");
        assert!(validate_vhf("118.000"));
        assert!(!validate_vhf("117.9"));
        assert!(validate_vhf(""));
        assert!(validate_uhf("399.975"));
        assert!(!validate_uhf("400"));
        assert_eq!(ui_to_mhz("118.3004"), Some(118.3));
        assert_eq!(ui_to_mhz(" "), None);
        assert_eq!(mhz_to_ui(118.3), "118.300");
    }
}
