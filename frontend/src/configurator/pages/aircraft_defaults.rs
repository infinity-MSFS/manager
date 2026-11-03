//! Aircraft Defaults (v3 `AircraftDefaultsPage.tsx`, v1 T-38 `Defaults`):
//! the `MAIN` and `IFF` sections as EGI, IFF, alerts, navigation
//! frequencies and courses. Values are strings on the wire; VOR and ILS
//! frequencies are stored as `MHz x 100` (`"11750"` for 117.50 MHz).
//!
//! Also holds the page furniture the Weapons page shares.

use infinity_ui::prelude::*;
use serde_json::Value;
use wasm_bindgen::JsCast;

use super::PageCtx;
use crate::configurator::schema::{display_scalar, path_set, path_value};
use crate::configurator::state::Configurator;
use crate::editorial::EYEBROW;

// ---- Parsing with JS semantics.

/// `parseInt`: optional sign, then leading digits.
pub(super) fn parse_int(s: &str) -> Option<i64> {
    let s = s.trim_start();
    let (sign, rest) = match s.strip_prefix('-') {
        Some(r) => (-1, r),
        None => (1, s.strip_prefix('+').unwrap_or(s)),
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    // Longer than i64 is far outside every range checked here.
    Some(digits.parse::<i64>().map_or(i64::MAX, |n| sign * n))
}

/// `parseFloat`: the longest leading decimal number.
pub(super) fn parse_float(s: &str) -> Option<f64> {
    let s = s.trim_start();
    let mut end = 0;
    let mut seen_dot = false;
    for (i, c) in s.char_indices() {
        let ok =
            c.is_ascii_digit() || (c == '.' && !seen_dot) || (i == 0 && (c == '-' || c == '+'));
        if !ok {
            break;
        }
        seen_dot |= c == '.';
        end = i + c.len_utf8();
    }
    s.get(..end)?.parse::<f64>().ok().filter(|n| n.is_finite())
}

// ---- Validators (empty is valid: unset).

pub(super) fn in_range(v: &str, min: i64, max: i64) -> bool {
    v.is_empty() || parse_int(v).is_some_and(|n| (min..=max).contains(&n))
}

fn valid_transponder(v: &str) -> bool {
    v.is_empty() || (v.len() == 4 && v.chars().all(|c| ('0'..='7').contains(&c)))
}

fn valid_course(v: &str) -> bool {
    in_range(v, 1, 360)
}

fn valid_tacan(v: &str) -> bool {
    in_range(v, 1, 126)
}

fn valid_vor(v: &str) -> bool {
    v.is_empty() || parse_float(v).is_some_and(|n| (108.0..=117.95).contains(&n))
}

fn valid_ils(v: &str) -> bool {
    v.is_empty() || parse_float(v).is_some_and(|n| (108.1..=111.95).contains(&n))
}

// ---- Formatters.

fn format_transponder(v: &str) -> String {
    v.chars()
        .filter(|c| ('0'..='7').contains(c))
        .take(4)
        .collect()
}

pub(super) fn format_digits(v: &str) -> String {
    v.chars().filter(char::is_ascii_digit).collect()
}

/// Digits only, clamped to 1–360.
fn format_course(v: &str) -> String {
    let cleaned = format_digits(v);
    if cleaned.is_empty() {
        return cleaned;
    }
    match cleaned.parse::<u64>() {
        Ok(n) if n < 1 => "1".to_owned(),
        Ok(n) if n <= 360 => cleaned,
        _ => "360".to_owned(),
    }
}

/// Digits and one dot, at most two decimals.
fn format_freq_ui(v: &str) -> String {
    let cleaned: String = v
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut parts = cleaned.splitn(2, '.');
    let whole = parts.next().unwrap_or_default();
    match parts.next() {
        None => whole.to_owned(),
        Some(rest) => {
            let decimals: String = rest.chars().filter(|c| *c != '.').take(2).collect();
            format!("{whole}.{decimals}")
        }
    }
}

/// What a typed nav frequency means: `11750` and `117.5` are both
/// `117.50`, `118` is `118.00`.
fn auto_format_nav_freq(v: &str) -> String {
    let cleaned: String = v
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
        let mut d: String = decimal.chars().take(2).collect();
        while d.len() < 2 {
            d.push('0');
        }
        return format!("{whole}.{d}");
    }
    // ASCII digits only from here, so byte slicing is safe.
    let n = cleaned.len();
    if n >= 5 {
        format!("{}.{}", &cleaned[..n - 2], &cleaned[n - 2..])
    } else if n == 4 {
        format!("{}.{}0", &cleaned[..3], &cleaned[3..])
    } else {
        format!("{cleaned}.00")
    }
}

/// `"11750"` to `"117.50"`.
fn cfg_freq_to_ui(cfg: &str) -> String {
    let digits = format_digits(cfg);
    match digits.parse::<u64>() {
        #[allow(clippy::cast_precision_loss)]
        Ok(n) => format!("{:.2}", n as f64 / 100.0),
        Err(_) => String::new(),
    }
}

/// `"117.50"` to `"11750"`.
fn ui_freq_to_cfg(ui: &str) -> String {
    let cleaned = format_freq_ui(ui);
    match parse_float(&cleaned) {
        #[allow(clippy::cast_possible_truncation)]
        Some(n) => ((n * 100.0).round() as i64).to_string(),
        None => String::new(),
    }
}

// ---- Shared page furniture.

/// A page title, as the tab headings of field groups.
#[component]
pub(super) fn PageTitle(
    title: &'static str,
    #[prop(optional)] lead: Option<&'static str>,
) -> impl IntoView {
    view! {
        <header class="mb-12">
            <h2 class="text-3xl font-extralight tracking-tight">{title}</h2>
            {lead
                .map(|l| {
                    view! {
                        <p class="mt-3 max-w-xl border-l border-accent-400/40 pl-5 text-sm leading-relaxed font-light text-white/60">
                            {l}
                        </p>
                    }
                })}
        </header>
    }
}

/// A numbered section with a one-line explanation.
#[component]
pub(super) fn Section(
    num: &'static str,
    title: &'static str,
    subtitle: &'static str,
    children: Children,
) -> impl IntoView {
    view! {
        <section class="flex flex-col">
            <div class=cn!("mb-3 flex items-center gap-4", EYEBROW)>
                <span class="text-accent-400/80">{num}</span>
                <span>{title}</span>
                <span class="h-px flex-1 bg-white/10"></span>
            </div>
            <p class="mb-6 text-sm font-light text-white/50">{subtitle}</p>
            {children()}
        </section>
    }
}

/// A labelled cell in a section grid.
#[component]
pub(super) fn Cell(
    label: &'static str,
    #[prop(optional)] description: Option<&'static str>,
    children: Children,
) -> impl IntoView {
    view! {
        <div class="flex flex-col gap-1.5 border-t border-white/10 pt-4">
            <span class="text-xs font-medium tracking-[0.14em] text-white/50 uppercase">{label}</span>
            {description.map(|d| view! { <span class="text-xs font-light text-white/40">{d}</span> })}
            {children()}
        </div>
    }
}

pub(super) const GRID: &str = "grid grid-cols-1 gap-x-10 gap-y-6 md:grid-cols-2 xl:grid-cols-3";

/// A text input that rewrites what is typed through `format` and reports
/// the result. The element is never re-created, so focus and caret survive
/// edits; outside changes to `value` replace the text.
#[component]
pub(super) fn FormattedInput(
    #[prop(into)] value: Signal<String>,
    format: fn(&str) -> String,
    #[prop(into)] on_change: Callback<String>,
    #[prop(into, default = Signal::from(false))] invalid: Signal<bool>,
    #[prop(optional)] placeholder: Option<&'static str>,
    #[prop(optional)] max_len: Option<usize>,
    #[prop(optional)] mono: bool,
) -> impl IntoView {
    let draft = RwSignal::new(value.get_untracked());
    Effect::new(move |_| {
        let v = value.get();
        if draft.get_untracked() != v {
            draft.set(v);
        }
    });
    view! {
        <TextInput
            value=draft
            invalid=invalid
            placeholder=placeholder.unwrap_or_default()
            class=if mono { "font-mono tracking-wide" } else { "" }
            on_input=Callback::new(move |s: String| {
                let s: String = match max_len {
                    Some(n) => s.chars().take(n).collect(),
                    None => s,
                };
                let next = format(&s);
                // Always set, so a rejected keystroke is erased from the field.
                draft.set(next.clone());
                on_change.run(next);
            })
        />
    }
}

/// The input element behind an event, for focus handling.
fn input_of(ev: &leptos::ev::Event) -> Option<web_sys::HtmlInputElement> {
    ev.target()?.dyn_into().ok()
}

/// v1's `NavFrequencyInput`: free typing while focused, normalised and
/// validated on blur, Enter or Tab; an invalid entry reverts. Escape
/// reverts.
#[component]
fn NavFreqInput(
    #[prop(into)] value: Signal<String>,
    placeholder: &'static str,
    validate: fn(&str) -> bool,
    #[prop(into)] on_change: Callback<String>,
) -> impl IntoView {
    let local = RwSignal::new(String::new());
    let focused = RwSignal::new(false);
    let committed = StoredValue::new(false);
    let shown = Signal::derive(move || {
        if focused.get() {
            local.get()
        } else {
            value.get()
        }
    });
    let invalid =
        Signal::derive(move || !focused.get() && value.with(|v| !v.is_empty() && !validate(v)));
    let commit = move || {
        if committed.get_value() {
            return;
        }
        committed.set_value(true);
        let formatted = auto_format_nav_freq(&local.get_untracked());
        if !formatted.is_empty() && validate(&formatted) {
            on_change.run(formatted);
        } else {
            local.set(value.get_untracked());
        }
    };
    view! {
        <div
            on:focusin=move |ev| {
                local.set(value.get_untracked());
                focused.set(true);
                committed.set_value(false);
                if let Some(input) = input_of(&ev) {
                    input.select();
                }
            }
            on:focusout=move |_| {
                focused.set(false);
                commit();
            }
            on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                let input = ev.target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok());
                match ev.key().as_str() {
                    "Enter" => {
                        ev.prevent_default();
                        commit();
                        if let Some(i) = input {
                            let _ = i.blur();
                        }
                    }
                    "Tab" => commit(),
                    "Escape" => {
                        ev.prevent_default();
                        local.set(value.get_untracked());
                        committed.set_value(true);
                        if let Some(i) = input {
                            let _ = i.blur();
                        }
                    }
                    _ => {}
                }
            }
        >
            <TextInput
                value=shown
                invalid=invalid
                placeholder=placeholder
                class="font-mono tracking-wide"
                on_input=Callback::new(move |s: String| {
                    local.set(s.chars().take(10).collect());
                    committed.set_value(false);
                })
            />
        </div>
    }
}

// ---- The page.

const ALIGN_OPTIONS: [&str; 3] = ["Instant", "Stored", "Full"];
const TACAN_BANDS: [&str; 2] = ["X", "Y"];

/// A string field of `section`, tracked.
fn read(cfg: Configurator, path: &'static str) -> Signal<String> {
    let value = cfg.value;
    Memo::new(move |_| value.with(|v| display_scalar(&path_value(v, path)))).into()
}

fn write(cfg: Configurator, path: &'static str) -> Callback<String> {
    Callback::new(move |v: String| cfg.update(|root| path_set(root, path, Value::String(v))))
}

/// A plain formatted text field.
fn text_field(
    cfg: Configurator,
    label: &'static str,
    path: &'static str,
    format: fn(&str) -> String,
    validate: fn(&str) -> bool,
    placeholder: &'static str,
    max_len: usize,
) -> impl IntoView {
    let value = read(cfg, path);
    let invalid = Signal::derive(move || value.with(|v| !validate(v)));
    view! {
        <Cell label=label>
            <FormattedInput
                value=value
                format=format
                on_change=write(cfg, path)
                invalid=invalid
                placeholder=placeholder
                max_len=max_len
            />
        </Cell>
    }
}

fn select(
    cfg: Configurator,
    path: &'static str,
    default: &'static str,
    options: &[&'static str],
) -> impl IntoView {
    let value = read(cfg, path);
    let names: Vec<&'static str> = options.to_vec();
    let current = Signal::derive(move || option_name(&value.get(), default, &names));
    view! {
        <Select
            value=current
            groups=Signal::stored(SelectGroup::flat(options.iter().copied()))
            on_change=write(cfg, path)
        />
    }
}

/// The option a stored value names. Configs imported from v1 store the
/// option's index (`"2"`) where v3/v4 store its name (`"Full"`); both
/// read as the same option.
fn option_name(value: &str, default: &str, options: &[&str]) -> String {
    if value.is_empty() {
        return default.to_owned();
    }
    match value.parse::<usize>().ok().and_then(|i| options.get(i)) {
        Some(name) if !options.contains(&value) => (*name).to_owned(),
        _ => value.to_owned(),
    }
}

fn tacan_field(
    cfg: Configurator,
    label: &'static str,
    freq: &'static str,
    mode: &'static str,
) -> impl IntoView {
    let channel = read(cfg, freq);
    let invalid = Signal::derive(move || channel.with(|v| !valid_tacan(v)));
    view! {
        <Cell label=label description="Channel 1–126 + band (X/Y).">
            <div class="flex items-center gap-2">
                <div class="min-w-0 flex-1">
                    <FormattedInput
                        value=channel
                        format=format_digits
                        on_change=write(cfg, freq)
                        invalid=invalid
                        placeholder="1"
                        max_len=3
                    />
                </div>
                <div class="w-24">{select(cfg, mode, "Y", &TACAN_BANDS)}</div>
            </div>
        </Cell>
    }
}

fn nav_freq_field(
    cfg: Configurator,
    label: &'static str,
    description: &'static str,
    path: &'static str,
    placeholder: &'static str,
    validate: fn(&str) -> bool,
) -> impl IntoView {
    let stored = read(cfg, path);
    let ui = Signal::derive(move || stored.with(|s| cfg_freq_to_ui(s)));
    let save = write(cfg, path);
    view! {
        <Cell label=label description=description>
            <NavFreqInput
                value=ui
                placeholder=placeholder
                validate=validate
                on_change=Callback::new(move |v: String| save.run(ui_freq_to_cfg(&v)))
            />
        </Cell>
    }
}

pub(super) fn page(ctx: PageCtx) -> AnyView {
    let cfg = ctx.cfg;
    view! {
        <div class="flex flex-col">
            <PageTitle title="Aircraft defaults" />
            <div class="flex flex-col gap-14">
                <Section num="01" title="EGI" subtitle="Default alignment behavior and navigation setup.">
                    <div class=GRID>
                        <Cell label="Align type" description="Instant, Stored, or Full alignment.">
                            {select(cfg, "MAIN.align_type", "Instant", &ALIGN_OPTIONS)}
                        </Cell>
                    </div>
                </Section>
                <Section num="02" title="IFF" subtitle="Default transponder code (octal).">
                    <div class=GRID>
                        {text_field(cfg, "Transponder code (0000–7777)", "IFF.transponder_code", format_transponder, valid_transponder, "1200", 4)}
                    </div>
                </Section>
                <Section num="03" title="Aural / visual alerts" subtitle="Fuel and altitude thresholds for alerts.">
                    <div class=GRID>
                        {text_field(cfg, "Bingo fuel (0–9999 lbs)", "MAIN.bingo_fuel", format_digits, |v| in_range(v, 0, 9999), "2000", 4)}
                        {text_field(cfg, "MSL alert altitude (0–40000 ft)", "MAIN.alt_alert_msl", format_digits, |v| in_range(v, 0, 40000), "10000", 5)}
                        {text_field(cfg, "AGL alert altitude (0–7500 ft)", "MAIN.alt_alert_agl", format_digits, |v| in_range(v, 0, 7500), "500", 4)}
                    </div>
                </Section>
                <Section num="04" title="Navigation frequencies" subtitle="TACAN and VOR / ILS default frequencies.">
                    <div class=GRID>
                        {tacan_field(cfg, "Air-to-ground TACAN", "MAIN.ag_tacan_freq", "MAIN.ag_tacan_mode")}
                        {tacan_field(cfg, "Air-to-air TACAN", "MAIN.aa_tacan_freq", "MAIN.aa_tacan_mode")}
                        {nav_freq_field(cfg, "VOR frequency", "108.00–117.95 MHz", "MAIN.vor_freq", "108.00", valid_vor)}
                        {nav_freq_field(cfg, "ILS frequency", "108.10–111.95 MHz", "MAIN.ils_freq", "108.10", valid_ils)}
                    </div>
                </Section>
                <Section num="05" title="Navigation courses" subtitle="Default courses (1–360°) for common nav sources.">
                    <div class=GRID>
                        {text_field(cfg, "Heading bug (1–360°)", "MAIN.heading_bug", format_course, valid_course, "360", 3)}
                        {text_field(cfg, "EGI course (1–360°)", "MAIN.egi_crs", format_course, valid_course, "360", 3)}
                        {text_field(cfg, "VOR course (1–360°)", "MAIN.vor_crs", format_course, valid_course, "360", 3)}
                        {text_field(cfg, "ILS course (1–360°)", "MAIN.ils_crs", format_course, valid_course, "360", 3)}
                        {text_field(cfg, "TAC course (1–360°)", "MAIN.tac_crs", format_course, valid_course, "360", 3)}
                    </div>
                </Section>
            </div>
        </div>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_indices_read_as_option_names() {
        let opts = ["Instant", "Stored", "Full"];
        assert_eq!(option_name("2", "Instant", &opts), "Full");
        assert_eq!(option_name("Stored", "Instant", &opts), "Stored");
        assert_eq!(option_name("", "Instant", &opts), "Instant");
        assert_eq!(option_name("9", "Instant", &opts), "9");
    }

    #[test]
    fn js_number_parsing() {
        assert_eq!(parse_int("12ab"), Some(12));
        assert_eq!(parse_int("-4"), Some(-4));
        assert_eq!(parse_int("x"), None);
        assert_eq!(parse_float("117.5x"), Some(117.5));
        assert_eq!(parse_float("1.2.3"), Some(1.2));
        assert_eq!(parse_float(""), None);
    }

    #[test]
    fn validators() {
        assert!(valid_transponder(""));
        assert!(valid_transponder("1277"));
        assert!(!valid_transponder("1287"));
        assert!(!valid_transponder("127"));
        assert!(valid_course("360") && !valid_course("0") && !valid_course("361"));
        assert!(valid_tacan("126") && !valid_tacan("127"));
        assert!(valid_vor("117.95") && !valid_vor("118.00"));
        assert!(valid_ils("108.10") && !valid_ils("108.00"));
        assert!(in_range("", 0, 1));
    }

    #[test]
    fn formatters() {
        assert_eq!(format_transponder("12a89773"), "1277");
        assert_eq!(format_course("400"), "360");
        assert_eq!(format_course("0"), "1");
        assert_eq!(format_course("9x5"), "95");
        assert_eq!(format_course("99999999999999999999999"), "360");
        assert_eq!(format_freq_ui("11a7.5.06"), "117.50");
        assert_eq!(format_freq_ui("117"), "117");
    }

    #[test]
    fn nav_frequency_normalisation() {
        assert_eq!(auto_format_nav_freq("11750"), "117.50");
        assert_eq!(auto_format_nav_freq("1175"), "117.50");
        assert_eq!(auto_format_nav_freq("118"), "118.00");
        assert_eq!(auto_format_nav_freq("117.5"), "117.50");
        assert_eq!(auto_format_nav_freq("117.555"), "117.55");
        assert_eq!(auto_format_nav_freq(""), "");
    }

    #[test]
    fn frequency_storage_is_mhz_times_100() {
        assert_eq!(cfg_freq_to_ui("11750"), "117.50");
        assert_eq!(cfg_freq_to_ui(""), "");
        assert_eq!(ui_freq_to_cfg("117.50"), "11750");
        assert_eq!(ui_freq_to_cfg("108.1"), "10810");
        assert_eq!(ui_freq_to_cfg(""), "");
    }
}
