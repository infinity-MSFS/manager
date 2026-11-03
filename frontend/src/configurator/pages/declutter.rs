//! Declutter page (v3 `DeclutterPage.tsx`): show / hide toggles for the
//! HUD and MFD readouts beside reference images. Values are `"1"` (show)
//! and `"0"` (hide) strings under `DECLUTTER.*` and `MFD_DECLUTTER.*`; the
//! output's `invertBoolStrings` transform flips them for the gauge.

use infinity_ui::prelude::*;
use serde_json::Value;

use super::PageCtx;
use crate::configurator::schema::{display_scalar, path_get};

const HUD_ROWS: [(&str, &str); 13] = [
    ("dcl_currnt_g", "Current G"),
    ("dcl_x_g", "Max G"),
    ("dcl_mach_no", "Mach Number"),
    ("dcl_tot_fuel", "Total Fuel"),
    ("dcl_bingo_fuel", "Bingo Fuel"),
    ("dcl_ralt", "Rad Alt"),
    ("dcl_alt_alert", "Altitude Alert"),
    ("dcl_safearm", "Master Arm State"),
    ("dcl_td_box", "TD Box"),
    ("dcl_egi_data", "EGI Data"),
    ("dcl_aoa", "Angle of Attack"),
    ("dcl_fpm", "FPM Indicator"),
    ("dcl_bulls", "Bullseye Information"),
];

const MFD_ROWS: [(&str, &str); 11] = [
    ("dcl_m_alt_alert", "Alert Altitude"),
    ("dcl_m_ralt", "Rad Alt"),
    ("dcl_m_gs", "Ground Speed"),
    ("dcl_m_wind", "Wind"),
    ("dcl_m_tod", "Time Of Day"),
    ("dcl_m_navdata", "Navigation Data"),
    ("dcl_m_commdata", "Communication Data"),
    ("dcl_m_egisteer", "EGI Steer Information"),
    ("dcl_m_ttg", "Time To Go"),
    ("dcl_m_bulls", "Bullseye Information"),
    ("dcl_m_mapsym", "Map Symbols"),
];

/// Whether a stored value means shown; anything unrecognised hides.
pub(crate) fn is_shown(v: Option<&Value>) -> bool {
    let s = v.map(display_scalar).unwrap_or_default();
    matches!(
        s.trim().to_lowercase().as_str(),
        "1" | "true" | "on" | "yes" | "show"
    )
}

pub(crate) fn to_config(show: bool) -> Value {
    Value::String(if show { "1" } else { "0" }.to_owned())
}

pub(super) fn page(ctx: PageCtx) -> AnyView {
    let cfg = ctx.cfg;
    view! {
        <div class="flex flex-col">
            <header class="mb-10">
                <h2 class="text-3xl font-extralight tracking-tight">"Declutter"</h2>
            </header>
            <div class="grid gap-12 xl:grid-cols-2">
                <Section
                    cfg=cfg
                    title="MFD declutter"
                    subtitle="Hide non-essential overlays for a cleaner tape."
                    image="/configurator/MFD.png"
                    section="MFD_DECLUTTER"
                    rows=&MFD_ROWS
                />
                <Section
                    cfg=cfg
                    title="HUD declutter"
                    subtitle="Tune the HUD readouts to match your workflow."
                    image="/configurator/HUD.png"
                    section="DECLUTTER"
                    rows=&HUD_ROWS
                />
            </div>
        </div>
    }
    .into_any()
}

#[component]
fn Section(
    cfg: crate::configurator::state::Configurator,
    title: &'static str,
    subtitle: &'static str,
    image: &'static str,
    section: &'static str,
    rows: &'static [(&'static str, &'static str)],
) -> impl IntoView {
    view! {
        <section class="flex flex-col border-t border-white/10 pt-5">
            <div class="mb-6">
                <span class="text-lg font-light">{title}</span>
                <p class="mt-1 text-sm font-light text-white/55">{subtitle}</p>
            </div>
            <div class="mb-6 flex h-[420px] items-center justify-center overflow-hidden rounded-sm border border-white/10 bg-black/30">
                <img src=image alt=format!("{title} reference") class="h-full w-full object-contain" />
            </div>
            <div class="flex flex-col">
                {rows
                    .iter()
                    .map(|&(key, label)| {
                        let path = format!("{section}.{key}");
                        let read = path.clone();
                        let shown = Signal::derive(move || cfg.value.with(|root| is_shown(path_get(root, &read))));
                        let set = Callback::new(move |show: bool| cfg.set_at(&path, to_config(show)));
                        view! { <ShowHide label=label shown=shown on_change=set /> }
                    })
                    .collect_view()}
            </div>
        </section>
    }
}

/// A row with a two-way Show / Hide choice.
#[component]
fn ShowHide(
    label: &'static str,
    #[prop(into)] shown: Signal<bool>,
    #[prop(into)] on_change: Callback<bool>,
) -> impl IntoView {
    let option = move |value: bool, text: &'static str, icon: IconData| {
        let selected = Signal::derive(move || shown.get() == value);
        view! {
            <button
                type="button"
                aria-pressed=move || selected.get().to_string()
                class=move || {
                    cn!(
                        "inline-flex items-center gap-2 border-b px-3 py-1.5 text-xs tracking-[0.15em] uppercase transition-colors",
                        if selected.get() { "border-accent-400 text-white" } else { "border-transparent text-white/45 hover:text-white" },
                    )
                }
                on:click=move |_| on_change.run(value)
            >
                <Icon icon=icon class="h-3.5 w-3.5" />
                {text}
            </button>
        }
    };
    view! {
        <div class="flex items-center justify-between gap-4 border-t border-white/10 py-2.5">
            <span class="text-sm font-light text-white/85">{label}</span>
            <div class="inline-flex shrink-0 gap-1">
                {option(true, "Show", icons::EYE)}
                {option(false, "Hide", icons::EYE_OFF)}
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn stored_strings_read_like_v1() {
        assert!(is_shown(Some(&json!("1"))));
        assert!(is_shown(Some(&json!(" Show "))));
        assert!(is_shown(Some(&json!(true))));
        assert!(is_shown(Some(&json!(1))));
        assert!(!is_shown(Some(&json!("0"))));
        assert!(!is_shown(None));
        assert_eq!(to_config(true), json!("1"));
        assert_eq!(to_config(false), json!("0"));
    }
}
