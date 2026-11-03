//! Weapon programs (v3 `WeaponsPage.tsx`, v1 T-38 `Weapons`): programs
//! A-F and GUN under `WPN.programs`, the `WPN.default_program`, and the
//! CCIP / CCRP reference images. Every known program reads with defaults
//! even when absent; the first edit to one writes it whole.

use infinity_ui::prelude::*;
use serde_json::{Map, Value};

use super::PageCtx;
use super::aircraft_defaults::{Cell, FormattedInput, PageTitle, format_digits, parse_int};
use crate::configurator::schema::{
    as_number, display_scalar, number, object_mut, path_get, path_set,
};
use crate::configurator::state::Configurator;
use crate::editorial::EYEBROW;

const PROGRAM_KEYS: [&str; 7] = ["A", "B", "C", "D", "E", "F", "GUN"];
const DELIVERY_MODES: [&str; 3] = ["CCIP", "CCRP", "MAN"];

fn program_title(k: &str) -> String {
    if k == "GUN" {
        "Gun Program".to_owned()
    } else {
        format!("Program {k}")
    }
}

/// The program fields, with their defaults and valid ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NumField {
    Mils,
    Dive,
    Cas,
    Alt,
}

impl NumField {
    const ALL: [Self; 4] = [Self::Mils, Self::Dive, Self::Cas, Self::Alt];

    fn key(self) -> &'static str {
        match self {
            Self::Mils => "reticle_depression_mils",
            Self::Dive => "dive_angle_deg",
            Self::Cas => "release_cas",
            Self::Alt => "release_altitude_msl",
        }
    }

    fn default(self) -> f64 {
        match self {
            Self::Mils | Self::Dive => 0.0,
            Self::Cas => 250.0,
            Self::Alt => 5000.0,
        }
    }

    fn range(self) -> (i64, i64) {
        match self {
            Self::Mils => (0, 300),
            Self::Dive => (0, 70),
            Self::Cas => (150, 500),
            Self::Alt => (0, 20000),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Mils => "Mils depression (0–300)",
            Self::Dive => "Dive angle (0–70°)",
            Self::Cas => "Release CAS (150–500 kt)",
            Self::Alt => "Release altitude (0–20000 ft)",
        }
    }

    fn max_len(self) -> usize {
        match self {
            Self::Mils | Self::Cas => 3,
            Self::Dive => 2,
            Self::Alt => 5,
        }
    }

    fn valid(self, s: &str) -> bool {
        let (lo, hi) = self.range();
        s.is_empty() || parse_int(s).is_some_and(|n| (lo..=hi).contains(&n))
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Program {
    delivery_mode: String,
    /// Shown as JS would print the stored number.
    numbers: [f64; 4],
}

impl Program {
    fn read(v: Option<&Value>) -> Self {
        let field = |k: &str| v.and_then(|p| p.get(k));
        Self {
            delivery_mode: field("delivery_mode")
                .and_then(Value::as_str)
                .unwrap_or("CCIP")
                .to_owned(),
            numbers: NumField::ALL
                .map(|f| field(f.key()).and_then(as_number).unwrap_or(f.default())),
        }
    }

    fn get(&self, f: NumField) -> f64 {
        self.numbers[f as usize]
    }

    fn text(&self, f: NumField) -> String {
        display_scalar(&number(self.get(f)))
    }

    fn any_invalid(&self) -> bool {
        NumField::ALL.into_iter().any(|f| !f.valid(&self.text(f)))
    }
}

/// v3's `toIntOr`: an empty or unparseable entry keeps the old value.
fn to_int_or(prev: f64, s: &str) -> f64 {
    if s.trim().is_empty() {
        return prev;
    }
    #[allow(clippy::cast_precision_loss)]
    parse_int(s).map_or(prev, |n| n as f64)
}

/// Writes one field of a program, filling any missing field with its
/// default so the gauge always sees a whole program. Unknown fields stay.
fn patch_program(root: &mut Value, key: &str, field: &str, value: Value) {
    let path = format!("WPN.programs.{key}");
    let mut program = path_get(root, &path)
        .cloned()
        .unwrap_or(Value::Object(Map::new()));
    let defaults = Program::read(Some(&program));
    if let Some(map) = object_mut(&mut program) {
        map.entry("delivery_mode")
            .or_insert_with(|| Value::String(defaults.delivery_mode.clone()));
        for f in NumField::ALL {
            map.entry(f.key())
                .or_insert_with(|| number(defaults.get(f)));
        }
        map.insert(field.to_owned(), value);
    }
    path_set(root, &path, program);
}

fn program_signal(cfg: Configurator, key: &'static str) -> Memo<Program> {
    let value = cfg.value;
    Memo::new(move |_| value.with(|v| Program::read(path_get(v, &format!("WPN.programs.{key}")))))
}

const CHEVRON: IconData = icons::CHEVRON_RIGHT;

#[component]
fn ProgramCard(
    cfg: Configurator,
    key: &'static str,
    #[prop(into)] default_program: Signal<String>,
) -> impl IntoView {
    let program = program_signal(cfg, key);
    let open = RwSignal::new(key == "A");
    let is_default = Signal::derive(move || default_program.get() == key);
    let summary = move || {
        program.with(|p| {
            format!(
                "Mode {} · {} KTAS · {} ft",
                p.delivery_mode,
                p.text(NumField::Cas),
                p.text(NumField::Alt)
            )
        })
    };
    let set_default = move || cfg.set_at("WPN.default_program", Value::String(key.to_owned()));

    let number_cell = move |f: NumField| {
        let text = Signal::derive(move || program.with(|p| p.text(f)));
        let invalid = Signal::derive(move || text.with(|t| !f.valid(t)));
        view! {
            <Cell label=f.label()>
                <FormattedInput
                    value=text
                    format=format_digits
                    invalid=invalid
                    max_len=f.max_len()
                    on_change=Callback::new(move |s: String| {
                        let prev = program.get_untracked().get(f);
                        let next = to_int_or(prev, &s);
                        cfg.update(|root| patch_program(root, key, f.key(), number(next)));
                    })
                />
            </Cell>
        }
    };

    view! {
        <article class="group relative border-t border-white/10">
            <div
                role="button"
                tabindex="0"
                aria-expanded=move || open.get().to_string()
                class="flex w-full cursor-pointer items-center justify-between gap-4 py-4 text-left"
                on:click=move |_| open.update(|o| *o = !*o)
                on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                    if ev.key() == "Enter" || ev.key() == " " {
                        ev.prevent_default();
                        open.update(|o| *o = !*o);
                    }
                }
            >
                <div class="flex min-w-0 items-baseline gap-4">
                    <span class="w-10 font-mono text-[11px] text-accent-400/80">{key}</span>
                    <div class="min-w-0">
                        <div class="flex items-center gap-2">
                            <span class="truncate text-lg font-light">{program_title(key)}</span>
                            {move || is_default.get().then(|| view! { <Badge tone=Tone::Accent size=Size::Sm>"Default"</Badge> })}
                        </div>
                        <p class="mt-0.5 truncate font-mono text-[11px] text-white/45">{summary}</p>
                    </div>
                </div>
                <div class="flex shrink-0 items-center gap-3" on:click=|ev| ev.stop_propagation()>
                    {move || {
                        let on = is_default.get();
                        view! {
                            <Button
                                variant=if on { Variant::Soft } else { Variant::Glass }
                                size=Size::Sm
                                disabled=on
                                on_click=Callback::new(move |()| set_default())
                            >
                                "Set default"
                            </Button>
                        }
                    }}
                    <span class=move || cn!("inline-flex text-white/50 transition-transform", open.get().then_some("rotate-90"))>
                        <Icon icon=CHEVRON class="h-5 w-5" />
                    </span>
                </div>
            </div>
            <Show when=move || open.get()>
                <div class="pb-8 pl-14">
                    <div class="grid grid-cols-1 gap-x-8 gap-y-6 md:grid-cols-2 xl:grid-cols-5">
                        <Cell label="Delivery mode">
                            <Select
                                value=Signal::derive(move || program.with(|p| p.delivery_mode.clone()))
                                groups=Signal::stored(SelectGroup::flat(DELIVERY_MODES))
                                on_change=Callback::new(move |v: String| {
                                    cfg.update(|root| patch_program(root, key, "delivery_mode", Value::String(v)));
                                })
                            />
                        </Cell>
                        {NumField::ALL.map(number_cell).into_iter().collect_view()}
                    </div>
                    {move || {
                        program
                            .with(Program::any_invalid)
                            .then(|| {
                                view! {
                                    <div class="mt-6 border-l border-red-400/50 py-1 pl-5">
                                        <p class="text-sm font-light text-red-200/90">
                                            "One or more fields are out of range. Values still save, but the aircraft may clamp or reject them."
                                        </p>
                                    </div>
                                }
                            })
                    }}
                </div>
            </Show>
        </article>
    }
}

#[component]
fn ImageCard(
    title: &'static str,
    subtitle: &'static str,
    src: &'static str,
    alt: &'static str,
) -> impl IntoView {
    view! {
        <figure class="flex flex-col border-t border-white/10 pt-4">
            <p class=cn!("mb-1", EYEBROW)>{title}</p>
            <figcaption class="mb-4 text-sm font-light text-white/50">{subtitle}</figcaption>
            <div class="h-[420px] overflow-hidden rounded-sm border border-white/10 bg-black/20">
                <img src=src alt=alt class="h-full w-full object-contain" />
            </div>
        </figure>
    }
}

pub(super) fn page(ctx: PageCtx) -> AnyView {
    let cfg = ctx.cfg;
    let value = cfg.value;
    let default_program = Signal::derive(move || {
        value.with(|v| {
            path_get(v, "WPN.default_program")
                .map(display_scalar)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "A".to_owned())
        })
    });
    let options = vec![SelectGroup::new(
        "",
        PROGRAM_KEYS
            .iter()
            .map(|k| SelectOption::new(*k, program_title(k)))
            .collect(),
    )];

    view! {
        <div class="flex flex-col">
            <PageTitle title="Weapon programs" />
            <div class="grid gap-12 xl:grid-cols-[minmax(0,1fr)_420px]">
                <div class="flex flex-col">
                    <div class="mb-10 flex flex-wrap items-end justify-between gap-6 border-t border-white/10 pt-4">
                        <div>
                            <p class=EYEBROW>"Default program"</p>
                            <p class="mt-2 text-sm font-light text-white/55">
                                "Used when the aircraft initializes weapon delivery settings."
                            </p>
                        </div>
                        <div class="w-full sm:w-72">
                            <Select
                                value=default_program
                                groups=Signal::stored(options)
                                on_change=Callback::new(move |k: String| cfg.set_at("WPN.default_program", Value::String(k)))
                            />
                        </div>
                    </div>
                    <div class="flex flex-col">
                        {PROGRAM_KEYS
                            .into_iter()
                            .map(|k| view! { <ProgramCard cfg=cfg key=k default_program=default_program /> })
                            .collect_view()}
                    </div>
                </div>
                <div class="flex flex-col gap-10">
                    <ImageCard
                        title="CCIP reference"
                        subtitle="Quick visual reminder of CCIP delivery."
                        src="/configurator/CCIP.png"
                        alt="CCIP weapon delivery"
                    />
                    <ImageCard
                        title="CCRP reference"
                        subtitle="Quick visual reminder of CCRP delivery."
                        src="/configurator/CCRP.png"
                        alt="CCRP weapon delivery"
                    />
                </div>
            </div>
        </div>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn missing_programs_read_with_defaults() {
        let p = Program::read(None);
        assert_eq!(p.delivery_mode, "CCIP");
        assert_eq!(p.text(NumField::Cas), "250");
        assert_eq!(p.text(NumField::Alt), "5000");
        assert!(!p.any_invalid());
        let bad = Program::read(Some(&json!({ "release_cas": 100 })));
        assert!(bad.any_invalid());
    }

    #[test]
    fn empty_entries_keep_the_old_value() {
        assert_eq!(to_int_or(250.0, ""), 250.0);
        assert_eq!(to_int_or(250.0, "300"), 300.0);
    }

    #[test]
    fn a_patch_materialises_the_whole_program() {
        let mut root = json!({ "WPN": { "programs": { "B": { "extra": 1 } } } });
        patch_program(&mut root, "B", "release_cas", json!(300));
        let b = &root["WPN"]["programs"]["B"];
        assert_eq!(b["release_cas"], json!(300));
        assert_eq!(b["release_altitude_msl"], json!(5000));
        assert_eq!(b["delivery_mode"], json!("CCIP"));
        assert_eq!(b["extra"], json!(1));
        patch_program(&mut root, "GUN", "delivery_mode", json!("MAN"));
        assert_eq!(root["WPN"]["programs"]["GUN"]["dive_angle_deg"], json!(0));
    }
}
