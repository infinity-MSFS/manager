//! One approach slot (v3 `ApproachCard`). The airport, runway and type
//! drive everything: changing any of them clears the nav-derived fields,
//! and once the selection settles the card fills itself from the nav
//! database (ILS / VOR: the published procedure; SCA: the runway record).
//! The only explicit action is Save.

use infinity_ipc::commands as cmd;
use infinity_ipc::nav::{NavAirport, NavApproach, NavRunway, NavSearch};
use infinity_ui::prelude::*;

use super::model::{
    Config, Draft, clamp_icao, configs_equal, course_from_bearing, derive_sca, header_title,
    missing, slot_key,
};
use crate::configurator::state::Configurator;
use crate::configurator::widgets::{SuggestionItem, Suggestions, debounce, describe};
use crate::editorial::EYEBROW;
use crate::ipc;

const SUGGEST_MS: u32 = 200;

/// lucide `save`.
const SAVE: IconData = IconData::new(
    r#"<path d="M15.2 3a2 2 0 0 1 1.4.6l3.8 3.8a2 2 0 0 1 .6 1.4V19a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z"/><path d="M17 21v-7a1 1 0 0 0-1-1H8a1 1 0 0 0-1 1v7"/><path d="M7 3v4a1 1 0 0 0 1 1h7"/>"#,
);

fn now_time() -> String {
    String::from(js_sys::Date::new_0().to_locale_time_string("default"))
}

/// The selection the card syncs from.
type SyncKey = (String, String, String);

#[component]
pub(super) fn ApproachCard(
    cfg: Configurator,
    /// The slot number, 500 to 509. (`slot` is reserved by `view!`.)
    number: u32,
    draft: RwSignal<Draft>,
    #[prop(into)] committed: Signal<Option<Config>>,
    #[prop(into)] on_save: Callback<Config>,
    #[prop(into)] on_delete: Callback<()>,
) -> impl IntoView {
    let slot = number;
    let d = draft;
    let icao = Memo::new(move |_| d.with(|x| x.c.icao.clone()));
    let runway = Memo::new(move |_| d.with(|x| x.c.runway.clone()));
    let ty = Memo::new(move |_| d.with(|x| x.c.approach_type.clone()));
    let expanded = Memo::new(move |_| d.with(|x| x.expanded));

    // VOR carries no glideslope; default 3 degrees so the draft can complete.
    Effect::new(move |_| {
        let (t, gs) = d.with(|x| (x.c.approach_type.clone(), x.c.app_gs));
        if t == "VOR" && !(gs.is_finite() && gs > 0.0) {
            d.update(|x| x.c.app_gs = 3.0);
        }
    });
    // SCA: keep FIP / FAF on the reciprocal course.
    Effect::new(move |_| {
        if let Some(next) = d.with(|x| derive_sca(&x.c)) {
            d.update(|x| x.c = next);
        }
    });

    // ---- ICAO validation, suggestions, runway and type discovery.
    let valid = RwSignal::new(None::<bool>);
    let checking = RwSignal::new(false);
    let suggestions = RwSignal::new(Vec::<String>::new());
    let suggest_loading = RwSignal::new(false);
    let suggest_open = RwSignal::new(false);
    let runways = RwSignal::new(Vec::<String>::new());
    let runways_loading = RwSignal::new(false);
    let types = RwSignal::new(None::<(bool, bool)>);
    let types_loading = RwSignal::new(false);
    let nav_loading = RwSignal::new(false);
    let nav_error = RwSignal::new(None::<String>);

    Effect::new(move |_| {
        if icao.get().is_empty() {
            suggestions.set(Vec::new());
            suggest_loading.set(false);
        } else {
            suggest_loading.set(true);
        }
    });
    debounce(
        move || icao.get(),
        SUGGEST_MS,
        move |query: String| {
            if query.is_empty() {
                return;
            }
            leptos::task::spawn_local(async move {
                let args = NavSearch {
                    product: cfg.product_ref(),
                    query: query.clone(),
                };
                let rows = ipc::invoke::<cmd::NavAirportsByName>(&args)
                    .await
                    .unwrap_or_default();
                if icao.try_get_untracked().as_deref() == Some(query.as_str()) {
                    suggestions.set(rows.into_iter().take(8).collect());
                    suggest_loading.set(false);
                }
            });
        },
    );

    Effect::new(move |_| {
        let i = icao.get();
        valid.set(None);
        if i.chars().count() != 4 {
            runways.set(Vec::new());
            types.set(None);
            checking.set(false);
            return;
        }
        checking.set(true);
        leptos::task::spawn_local(async move {
            let args = NavAirport {
                product: cfg.product_ref(),
                icao: i.clone(),
            };
            let ok = ipc::invoke::<cmd::NavIsValidIcao>(&args)
                .await
                .unwrap_or(false);
            if icao.try_get_untracked().as_deref() == Some(i.as_str()) {
                valid.set(Some(ok));
                checking.set(false);
            }
        });
    });

    Effect::new(move |_| {
        let (v, i) = (valid.get(), icao.get());
        if v != Some(true) {
            return;
        }
        runways_loading.set(true);
        leptos::task::spawn_local(async move {
            let args = NavAirport {
                product: cfg.product_ref(),
                icao: i.clone(),
            };
            let rows: Vec<String> = ipc::invoke::<cmd::NavRunways>(&args)
                .await
                .map(|r| r.into_iter().map(|r| r.runway_identifier).collect())
                .unwrap_or_default();
            if icao.try_get_untracked().as_deref() != Some(i.as_str()) {
                return;
            }
            let first = rows.first().cloned();
            runways.set(rows);
            runways_loading.set(false);
            if let Some(first) = first
                && d.with_untracked(|x| x.c.runway.is_empty())
            {
                d.update(|x| {
                    x.c.runway = first;
                    x.c.approach_type = "SCA".to_owned();
                });
            }
        });
    });

    Effect::new(move |_| {
        let (v, i, r) = (valid.get(), icao.get(), runway.get());
        if v != Some(true) {
            return;
        }
        if r.is_empty() {
            types.set(None);
            return;
        }
        types_loading.set(true);
        leptos::task::spawn_local(async move {
            let args = NavRunway {
                product: cfg.product_ref(),
                icao: i.clone(),
                runway: r.clone(),
            };
            let found = ipc::invoke::<cmd::NavApproachTypes>(&args)
                .await
                .ok()
                .map(|t| (t.has_ils, t.has_vor));
            let current = (icao.try_get_untracked(), runway.try_get_untracked());
            if current == (Some(i), Some(r)) {
                types.set(found);
                types_loading.set(false);
            }
        });
    });
    // A type the runway does not offer falls back to SCA.
    Effect::new(move |_| {
        let (Some((ils, vor)), t) = (types.get(), ty.get()) else {
            return;
        };
        let ok = t == "SCA" || (t == "ILS" && ils) || (t == "VOR" && vor);
        if !ok {
            d.update(|x| x.c.approach_type = "SCA".to_owned());
        }
    });

    // ---- Auto-sync from the nav database when the selection settles.
    let sync_key = Memo::new(move |_| {
        (valid.get() == Some(true) && !runway.get().is_empty())
            .then(|| (icao.get(), runway.get(), ty.get()))
    });
    // A card hydrated from saved config counts its selection as synced, so
    // opening it cannot overwrite saved values.
    let last_key = StoredValue::new(
        committed
            .get_untracked()
            .map(|c| (c.icao, c.runway, c.approach_type)),
    );
    let nonce = RwSignal::new(0u32);
    let last_nonce = StoredValue::new(0u32);
    let generation = StoredValue::new(0u64);

    // Forget the synced selection once cleared, so re-entering it refills.
    Effect::new(move |_| {
        if icao.get().is_empty() {
            last_key.set_value(None);
        }
    });

    Effect::new(move |_| {
        let key = sync_key.get();
        let n = nonce.get();
        let Some(key) = key else { return };
        let forced = n != last_nonce.get_value();
        last_nonce.set_value(n);
        if !forced && last_key.with_value(|k| k.as_ref() == Some(&key)) {
            return;
        }
        last_key.set_value(Some(key.clone()));
        let gen_now = generation.try_update_value(|g| {
            *g += 1;
            *g
        });
        nav_loading.set(true);
        nav_error.set(None);
        leptos::task::spawn_local(sync(
            cfg,
            d,
            key,
            gen_now,
            generation,
            nav_loading,
            nav_error,
        ));
    });

    // ---- Derived display.
    let draft_cfg = Memo::new(move |_| d.with(Draft::to_config));
    let reasons = Memo::new(move |_| d.with(|x| missing(&x.c)));
    let complete = Signal::derive(move || reasons.with(Vec::is_empty));
    let saved = Signal::derive(move || {
        complete.get() && committed.with(|c| draft_cfg.with(|dc| configs_equal(dc, c.as_ref())))
    });
    let title = move || {
        let c = committed.get();
        d.with(|x| header_title(slot, &x.c, c.as_ref()))
    };
    let badge = move || {
        let (text, tone, icon) = match (committed.with(Option::is_some), saved.get()) {
            (true, true) => ("Saved", Tone::Accent, icons::CIRCLE_CHECK),
            (true, false) => ("Unsaved changes", Tone::Warning, icons::CIRCLE_ALERT),
            _ => ("Not saved", Tone::Warning, icons::CIRCLE_ALERT),
        };
        view! { <Badge tone=tone icon=icon size=Size::Sm>{text}</Badge> }
    };

    let set_icao = move |v: String| {
        d.update(|x| {
            x.clear_nav();
            x.c.icao = clamp_icao(&v);
            x.c.runway = String::new();
            x.c.approach_type = "SCA".to_owned();
        });
    };
    let dot = move || {
        if icao.with(String::is_empty) {
            "bg-white/30"
        } else {
            match valid.get() {
                None => "bg-white/40",
                Some(true) => "bg-accent-400",
                Some(false) => "bg-red-400",
            }
        }
    };
    let icao_hint = move || {
        if checking.get() {
            "Checking ICAO…"
        } else {
            match valid.get() {
                Some(true) => "Valid ICAO",
                Some(false) => "Invalid ICAO",
                None => "Enter 4-letter ICAO",
            }
        }
    };

    let runway_select = move || {
        let placeholder = if valid.get() != Some(true) {
            "ICAO must be valid"
        } else if runways_loading.get() {
            "Loading…"
        } else if runways.with(Vec::is_empty) {
            "No runways"
        } else {
            "Select runway…"
        };
        view! {
            <Select
                value=runway
                groups=Signal::derive(move || SelectGroup::flat(runways.get()))
                placeholder=placeholder
                disabled=Signal::derive(move || {
                    valid.get() != Some(true) || runways_loading.get() || runways.with(Vec::is_empty)
                })
                on_change=Callback::new(move |v: String| {
                    d.update(|x| {
                        x.clear_nav();
                        x.c.runway = v;
                        x.c.approach_type = "SCA".to_owned();
                    });
                })
            />
        }
    };
    let type_select = move || {
        let placeholder = if types_loading.get() {
            "Loading…"
        } else {
            "Select type…"
        };
        view! {
            <Select
                value=ty
                groups=Signal::derive(move || {
                    let mut opts = vec![SelectOption::new("SCA", "SCA (Manual)")];
                    if let Some((ils, vor)) = types.get() {
                        if ils {
                            opts.push(SelectOption::plain("ILS"));
                        }
                        if vor {
                            opts.push(SelectOption::plain("VOR"));
                        }
                    }
                    vec![SelectGroup::new("", opts)]
                })
                placeholder=placeholder
                disabled=Signal::derive(move || {
                    valid.get() != Some(true) || runway.with(String::is_empty) || types_loading.get()
                })
                on_change=Callback::new(move |v: String| {
                    // Nothing nav-derived survives a type switch; the sync refills.
                    d.update(|x| {
                        x.clear_nav();
                        x.c.approach_type = v;
                    });
                })
            />
        }
    };

    let sync_status = move || {
        if nav_loading.get() {
            view! {
                <span class="inline-flex items-center gap-1.5 text-white/60">
                    <Spinner size=Size::Xs />
                    "Syncing from Nav DB…"
                </span>
            }
            .into_any()
        } else if nav_error.with(Option::is_some) {
            view! { <span class="text-red-300">"Nav DB sync failed"</span> }.into_any()
        } else if let Some(t) = d.with(|x| x.last_sync.clone()) {
            view! { <span class="text-white/60">{format!("Auto-filled from Nav DB {t}")}</span> }
                .into_any()
        } else if committed.with(Option::is_some) {
            view! { <span class="text-white/60">"Loaded from saved config"</span> }.into_any()
        } else {
            view! { <span class="text-white/40">"Select an airport and runway to auto-fill"</span> }
                .into_any()
        }
    };

    let num = move |get: fn(&Config) -> f64, set: fn(&mut Config, f64)| {
        (
            Signal::derive(move || d.with(|x| Some(get(&x.c)))),
            Callback::new(move |v: Option<f64>| d.update(|x| set(&mut x.c, v.unwrap_or(0.0)))),
        )
    };
    let opt = move |get: fn(&Config) -> Option<f64>, set: fn(&mut Config, Option<f64>)| {
        (
            Signal::derive(move || d.with(|x| get(&x.c))),
            Callback::new(move |v: Option<f64>| d.update(|x| set(&mut x.c, v))),
        )
    };
    let ro = move |get: fn(&Config) -> Option<f64>| Signal::derive(move || d.with(|x| get(&x.c)));

    let details = move || {
        if ty.get() == "SCA" {
            let (freq, set_freq) = num(|c| c.navaid_freq, |c, v| c.navaid_freq = v);
            let (fip, set_fip) = opt(|c| c.fip_distance, |c, v| c.fip_distance = v);
            let (faf, set_faf) = opt(|c| c.faf_distance, |c, v| c.faf_distance = v);
            let (crs, set_crs) = num(|c| c.navaid_course, |c, v| c.navaid_course = v);
            let (gs, set_gs) = num(|c| c.app_gs, |c, v| c.app_gs = v);
            let (lat, set_lat) = num(|c| c.rwy_lat, |c, v| c.rwy_lat = v);
            let (lon, set_lon) = num(|c| c.rwy_lon, |c, v| c.rwy_lon = v);
            let (elev, set_elev) = num(|c| c.rwy_elev, |c, v| c.rwy_elev = v);
            view! {
                <div class="grid grid-cols-2 gap-3">
                    <NumInput label="Navaid freq" value=freq on_change=set_freq zero_blank=true />
                    <div></div>
                    <NumInput label="FIP dist (nm)" value=fip on_change=set_fip />
                    <NumInput label="FAP dist (nm)" value=faf on_change=set_faf />
                    <NumInput label="True course (°)" value=crs on_change=set_crs zero_blank=true />
                    <NumInput label="GS (°)" value=gs on_change=set_gs zero_blank=true />
                    <NumInput label="RWY lat" value=lat on_change=set_lat zero_blank=true />
                    <NumInput label="RWY lon" value=lon on_change=set_lon zero_blank=true />
                    <NumInput label="RWY elev (ft)" value=elev on_change=set_elev zero_blank=true />
                    <div></div>
                    <ReadOnly label="FIP lat" value=ro(|c| c.fip_lat) />
                    <ReadOnly label="FIP lon" value=ro(|c| c.fip_lon) />
                    <ReadOnly label="FAP lat" value=ro(|c| c.faf_lat) />
                    <ReadOnly label="FAP lon" value=ro(|c| c.faf_lon) />
                </div>
            }
            .into_any()
        } else {
            let row = move |label: &'static str, value: Signal<String>| {
                view! {
                    <div class="flex items-baseline justify-between gap-3 border-t border-white/[0.06] py-1.5 first:border-t-0">
                        <span class="text-white/55">{label}</span>
                        <span data-selectable class="font-mono text-white/90">{move || value.get()}</span>
                    </div>
                }
            };
            let show = move |f: fn(&Config) -> String| Signal::derive(move || d.with(|x| f(&x.c)));
            view! {
                <div class="text-xs">
                    {row("Navaid type", show(|c| if c.navaid_type.is_empty() { "—".into() } else { c.navaid_type.clone() }))}
                    {row("Navaid freq", show(|c| if c.navaid_freq != 0.0 { c.navaid_freq.to_string() } else { "—".into() }))}
                    {row("True course", show(|c| if c.navaid_course != 0.0 { format!("{}°", c.navaid_course) } else { "—".into() }))}
                    {row("RWY lat/lon", show(|c| if c.rwy_lat != 0.0 && c.rwy_lon != 0.0 { format!("{:.4}, {:.4}", c.rwy_lat, c.rwy_lon) } else { "—".into() }))}
                    {row("RWY elev", show(|c| if c.rwy_elev != 0.0 { format!("{} ft", c.rwy_elev) } else { "—".into() }))}
                    {row("Glideslope", show(|c| if c.app_gs != 0.0 { format!("{}°", c.app_gs) } else { "—".into() }))}
                </div>
            }
            .into_any()
        }
    };

    let readiness = move || {
        let r = reasons.get();
        if r.is_empty() {
            let text = if saved.get() {
                "Saved to config"
            } else {
                "Ready to save"
            };
            view! { <p class="border-l border-accent-400/60 py-1 pl-4 text-sm text-accent-200">{text}</p> }.into_any()
        } else {
            view! {
                <div class="border-l border-amber-400/50 py-1 pl-4">
                    <p class="mb-1 text-sm text-amber-100">"Missing required fields"</p>
                    <ul class="list-disc space-y-1 pl-4 text-xs text-amber-100/75">
                        {r.into_iter().take(4).map(|m| view! { <li>{m}</li> }).collect_view()}
                    </ul>
                </div>
            }
            .into_any()
        }
    };

    view! {
        <article
            id=card_dom_id(slot)
            class="flex w-[340px] shrink-0 flex-col overflow-hidden rounded-sm border border-white/10 bg-white/[0.02] transition-colors hover:border-white/20"
        >
            <header class="border-b border-white/10 px-4 py-4">
                <div class="flex items-start justify-between gap-3">
                    <div class="min-w-0">
                        <h3 class="truncate text-[15px] font-medium tracking-tight">{title}</h3>
                        <p class="mt-0.5 font-mono text-[11px] text-white/40">{slot_key(slot)}</p>
                    </div>
                    {badge}
                </div>
                <button
                    type="button"
                    class="mt-3 inline-flex items-center gap-2 text-sm text-white/65 transition-colors hover:text-white"
                    aria-expanded=move || expanded.get().to_string()
                    on:click=move |_| d.update(|x| x.expanded = !x.expanded)
                >
                    <span class=move || cn!("inline-flex transition-transform", expanded.get().then_some("rotate-90"))>
                        <Icon icon=icons::CHEVRON_RIGHT class="h-4 w-4" />
                    </span>
                    {move || if expanded.get() { "Collapse" } else { "Expand" }}
                </button>
            </header>
            <Show
                when=move || expanded.get()
                fallback=move || {
                    view! {
                        <p class="p-4 text-sm font-light text-white/55">
                            {move || if committed.with(Option::is_some) { "This slot is configured." } else { "Empty slot. Expand to configure." }}
                        </p>
                    }
                }
            >
                <div data-yscroll class="flex max-h-[640px] flex-col gap-4 overflow-y-auto p-4">
                    <Field label="Airport ICAO">
                        <div class="flex items-center gap-3">
                            <span aria-hidden="true" class=move || cn!("h-2 w-2 shrink-0 rounded-full", dot())></span>
                            <div
                                class="relative flex-1"
                                on:focusin=move |_| suggest_open.set(true)
                                on:focusout=move |_| suggest_open.set(false)
                            >
                                <TextInput
                                    value=icao
                                    placeholder="KPHX"
                                    class="uppercase"
                                    invalid=Signal::derive(move || valid.get() == Some(false))
                                    on_input=Callback::new(move |v: String| {
                                        set_icao(v);
                                        suggest_open.set(true);
                                    })
                                />
                                <Suggestions open=Signal::derive(move || {
                                    suggest_open.get() && (suggest_loading.get() || suggestions.with(|s| !s.is_empty()))
                                })>
                                    {move || suggest_loading.get().then(|| view! { <div class="px-3 py-1.5"><Spinner size=Size::Xs /></div> })}
                                    {move || {
                                        suggestions
                                            .get()
                                            .into_iter()
                                            .map(|s| {
                                                let pick = s.clone();
                                                view! {
                                                    <SuggestionItem on_pick=Callback::new(move |()| {
                                                        set_icao(pick.clone());
                                                        suggest_open.set(false);
                                                    })>
                                                        <span class="font-mono">{s}</span>
                                                    </SuggestionItem>
                                                }
                                            })
                                            .collect_view()
                                    }}
                                </Suggestions>
                            </div>
                        </div>
                        <p class="text-xs text-white/50">{icao_hint}</p>
                    </Field>
                    <Field label="Runway">{runway_select}</Field>
                    <Field label="Approach type">
                        {type_select}
                        {move || runway.with(String::is_empty).then(|| view! { <p class="text-xs text-white/40">"Select runway first."</p> })}
                    </Field>
                    <div class="flex min-h-6 items-center justify-between gap-3 text-xs">
                        {sync_status}
                        {move || {
                            (!nav_loading.get() && sync_key.with(Option::is_some))
                                .then(|| {
                                    view! {
                                        <Button variant=Variant::Link size=Size::Xs on_click=Callback::new(move |()| nonce.update(|n| *n += 1))>
                                            "Re-sync"
                                        </Button>
                                    }
                                })
                        }}
                    </div>
                    {move || nav_error.get().map(|e| view! { <p data-selectable class="text-xs text-red-300">{e}</p> })}
                    <section class="border-t border-white/10 pt-3">
                        <p class=cn!("mb-3", EYEBROW)>"Details"</p>
                        {details}
                    </section>
                    {readiness}
                    <div class="flex gap-2">
                        <div class="flex-1">
                            {move || {
                                let is_saved = saved.get();
                                view! {
                                    <Button
                                        variant=if is_saved { Variant::Glass } else { Variant::Solid }
                                        icon=if is_saved { icons::CIRCLE_CHECK } else { SAVE }
                                        full_width=true
                                        disabled=Signal::derive(move || !complete.get() || saved.get())
                                        on_click=Callback::new(move |()| {
                                            if complete.get_untracked() && !saved.get_untracked() {
                                                on_save.run(draft_cfg.get_untracked());
                                            }
                                        })
                                    >
                                        {if is_saved { "Saved" } else { "Save" }}
                                    </Button>
                                }
                            }}
                        </div>
                        <Button variant=Variant::Glass tone=Tone::Danger icon=icons::TRASH_2 on_click=on_delete>
                            "Delete"
                        </Button>
                    </div>
                </div>
            </Show>
        </article>
    }
}

pub(super) fn card_dom_id(slot: u32) -> String {
    format!("approach-card-{}", slot_key(slot))
}

/// Fetches nav data for `key` and patches the draft, unless a newer sync
/// started meanwhile.
async fn sync(
    cfg: Configurator,
    d: RwSignal<Draft>,
    key: SyncKey,
    generation_now: Option<u64>,
    generation: StoredValue<u64>,
    loading: RwSignal<bool>,
    error: RwSignal<Option<String>>,
) {
    let (icao, runway, ty) = key;
    let current = move || generation.try_get_value() == generation_now;
    let result = if ty == "ILS" || ty == "VOR" {
        let args = NavApproach {
            product: cfg.product_ref(),
            icao,
            runway,
            approach_type: ty,
        };
        ipc::invoke::<cmd::NavApproachData>(&args)
            .await
            .map(|data| {
                if !current() {
                    return;
                }
                let nz = |v: f64| (v != 0.0).then_some(v);
                let gs = if data.approch_type == "VOR"
                    && !(data.app_gs.is_finite() && data.app_gs > 0.0)
                {
                    3.0
                } else {
                    data.app_gs
                };
                let stamp = now_time();
                let _ = d.try_update(|x| {
                    let c = &mut x.c;
                    c.icao = data.airport_icao.clone();
                    c.approach_type = data.approch_type.clone();
                    c.navaid_type = data.navaid_type.clone();
                    c.navaid_freq = data.navaid_freq;
                    c.navaid_course = f64::from(data.navaid_course);
                    c.rwy_lat = data.rwy_lat;
                    c.rwy_lon = data.rwy_lon;
                    c.rwy_elev = f64::from(data.rwy_elev);
                    c.fip_distance = Some(data.fip_distance);
                    c.faf_distance = Some(data.faf_distance);
                    c.fip_lat = nz(data.fip_lat);
                    c.fip_lon = nz(data.fip_lon);
                    c.faf_lat = nz(data.faf_lat);
                    c.faf_lon = nz(data.faf_lon);
                    c.app_gs = gs;
                    x.last_sync = Some(stamp);
                });
            })
    } else {
        // SCA: seed position, elevation and course from the runway record;
        // frequency and FIP / FAP distances stay manual.
        let args = NavRunway {
            product: cfg.product_ref(),
            icao,
            runway,
        };
        ipc::invoke::<cmd::NavRunwayDetails>(&args).await.map(|rd| {
            if !current() {
                return;
            }
            let stamp = now_time();
            let _ = d.try_update(|x| {
                let c = &mut x.c;
                c.rwy_lat = rd.latitude;
                c.rwy_lon = rd.longitude;
                c.rwy_elev = f64::from(rd.elevation_ft);
                if let Some(b) = rd.true_bearing.filter(|b| b.is_finite()) {
                    c.navaid_course = course_from_bearing(b);
                }
                if !(c.app_gs.is_finite() && c.app_gs > 0.0) {
                    c.app_gs = 3.0;
                }
                x.last_sync = Some(stamp);
            });
        })
    };
    if !current() {
        return;
    }
    if let Err(e) = result {
        let _ = error.try_set(Some(describe(&e)));
    }
    let _ = loading.try_set(false);
}

fn show_num(v: Option<f64>, zero_blank: bool) -> String {
    match v {
        Some(n) if n.is_finite() && !(zero_blank && n == 0.0) => n.to_string(),
        _ => String::new(),
    }
}

fn parse_num(s: &str) -> Option<Option<f64>> {
    let s = s.trim();
    if s.is_empty() {
        return Some(None);
    }
    s.parse::<f64>().ok().filter(|n| n.is_finite()).map(Some)
}

/// A number box. Empty means `None`; with `zero_blank`, 0 shows empty (v3's
/// `NumField`). Keeps what was typed while it parses to the same value.
#[component]
fn NumInput(
    label: &'static str,
    #[prop(into)] value: Signal<Option<f64>>,
    #[prop(into)] on_change: Callback<Option<f64>>,
    #[prop(optional)] zero_blank: bool,
) -> impl IntoView {
    let same = move |a: Option<f64>, b: Option<f64>| {
        let norm = |v: Option<f64>| {
            if zero_blank {
                v.filter(|n| *n != 0.0)
            } else {
                v
            }
        };
        norm(a) == norm(b)
    };
    let draft = RwSignal::new(show_num(value.get_untracked(), zero_blank));
    Effect::new(move |_| {
        let v = value.get();
        let typed = parse_num(&draft.get_untracked());
        if typed.is_none_or(|t| !same(t, v)) {
            draft.set(show_num(v, zero_blank));
        }
    });
    view! {
        <label class="flex flex-col gap-1">
            <span class="text-[11px] tracking-[0.12em] text-white/50 uppercase">{label}</span>
            <TextInput
                value=draft
                input_type="number"
                size=Size::Sm
                on_input=Callback::new(move |s: String| {
                    draft.set(s.clone());
                    if let Some(v) = parse_num(&s) {
                        on_change.run(v);
                    }
                })
            />
        </label>
    }
}

#[component]
fn ReadOnly(label: &'static str, #[prop(into)] value: Signal<Option<f64>>) -> impl IntoView {
    let text = Signal::derive(move || {
        value
            .get()
            .filter(|v| v.is_finite())
            .map(|v| format!("{v:.6}"))
            .unwrap_or_default()
    });
    view! {
        <label class="flex flex-col gap-1">
            <span class="text-[11px] tracking-[0.12em] text-white/50 uppercase">{label}</span>
            <TextInput value=text size=Size::Sm disabled=true on_input=Callback::new(|_: String| {}) />
        </label>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_boxes_blank_zero_and_empty() {
        assert_eq!(show_num(Some(0.0), true), "");
        assert_eq!(show_num(Some(0.0), false), "0");
        assert_eq!(show_num(None, false), "");
        assert_eq!(show_num(Some(2.5), true), "2.5");
        assert_eq!(parse_num(""), Some(None));
        assert_eq!(parse_num("3.5"), Some(Some(3.5)));
        assert_eq!(parse_num("-"), None);
    }
}
