//! Equipment page (v3 `EquipmentPage.tsx`), reached from the product's
//! library entry (Configure). Two modes over one set of cards:
//!
//! * **Live**: the running aircraft's fit, rendered from what the gauge
//!   reports. A press applies at once, exactly as the EFB's Configuration
//!   app does, and either one's change is remembered for the next flight.
//! * **Offline** (no aircraft): a draft over the layout the aircraft last
//!   reported for a variant, to build presets and pick what the next flight
//!   starts with before loading in.
//!
//! Every category is a radio group; unowned options are filtered out by the
//! gauge. Presets and the restore are the backend's (`infinity-app`'s
//! `equipment` module); [`presets`] follows them.

mod bridge;
mod presets;

use infinity_ipc::dto::{EquipmentPreset, EquipmentProfile};
use infinity_ui::prelude::*;

use self::bridge::{Equipment, EquipmentCategory, EquipmentOption, Snapshot, parse_categories};
use self::presets::{Presets, select_in};
use super::PageCtx;
use crate::editorial::EYEBROW;

/// The bridge namespace: the group's own `bridgeRef`, else the
/// configurator's, looked up in the product's `bridges`.
fn namespace(ctx: &PageCtx) -> Option<String> {
    let group_ref = ctx
        .group
        .get("bridgeRef")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    ctx.cfg.product.with_value(|p| {
        let r = group_ref.or_else(|| p.configurator.as_ref().and_then(|c| c.bridge_ref.clone()))?;
        p.bridges
            .iter()
            .find(|b| b.id == r)
            .map(|b| b.namespace.clone())
    })
}

const LOCK: IconData = IconData::new(
    r#"<rect width="18" height="11" x="3" y="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/>"#,
);

/// The `Select` value meaning "the last fit flown".
const LAST_FIT: &str = "";

#[component]
fn Notice(
    #[prop(into)] title: String,
    #[prop(into, optional)] body: Option<String>,
    #[prop(optional)] spinning: bool,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    view! {
        <div class="flex flex-col items-start gap-3 border-t border-white/10 py-10">
            <div class="flex items-center gap-3">
                {spinning.then(|| view! { <Spinner size=Size::Sm /> })}
                <p class="text-lg font-light text-white/80">{title}</p>
            </div>
            {body.map(|b| view! { <p class="max-w-xl text-sm font-light text-white/50">{b}</p> })}
            {children.map(|c| c())}
        </div>
    }
}

#[component]
fn ErrorLine(#[prop(into)] title: String, error: Signal<Option<String>>) -> impl IntoView {
    move || {
        let title = title.clone();
        error.get().map(|e| {
            view! {
                <div class="mb-8 border-l border-red-400/50 py-1 pl-5">
                    <p class=cn!("mb-1", EYEBROW, "text-white/40")>{title}</p>
                    <p data-selectable class="text-sm font-light text-white/70">{e}</p>
                </div>
            }
        })
    }
}

/// A cached layout with a draft's choices drawn as the active options.
/// A category the draft says nothing about keeps the layout's own choice.
fn draw_draft(layout: &[EquipmentCategory], draft: &[u32]) -> Vec<EquipmentCategory> {
    layout
        .iter()
        .cloned()
        .map(|mut c| {
            let chosen = c
                .options
                .iter()
                .any(|o| u32::try_from(o.idx).is_ok_and(|i| draft.contains(&i)));
            if chosen && c.selectable {
                for o in &mut c.options {
                    o.active = u32::try_from(o.idx).is_ok_and(|i| draft.contains(&i));
                }
            }
            c
        })
        .collect()
}

/// The layout's active choices, the draft a variant with nothing saved
/// starts from.
fn layout_choices(layout: &[EquipmentCategory]) -> Vec<u32> {
    layout
        .iter()
        .filter(|c| c.selectable)
        .flat_map(|c| c.options.iter().filter(|o| o.active))
        .filter_map(|o| u32::try_from(o.idx).ok())
        .collect()
}

fn layout_of(profile: &EquipmentProfile, variant: &str) -> Vec<EquipmentCategory> {
    profile
        .variant(variant)
        .and_then(|v| v.layout.as_ref())
        .map(parse_categories)
        .unwrap_or_default()
}

pub(super) fn page(ctx: PageCtx) -> AnyView {
    let Some(ns) = namespace(&ctx) else {
        let id = ctx.cfg.product.with_value(|p| p.id.clone());
        return view! {
            <Notice
                title="Configurator misconfigured"
                body=format!(
                    "No bridge is declared for \u{201c}{id}\u{201d}. The catalog entry needs a bridges[] spec and a configurator.bridgeRef pointing at it.",
                )
            />
        }
        .into_any();
    };
    let eq = Equipment::new(ns.clone());
    let presets = Presets::new(ns);
    let refresh = move |()| leptos::task::spawn_local(eq.refresh());

    // The variant flying now, when it can swap.
    let live = Memo::new(move |_| {
        eq.snapshot.with(|s| {
            s.as_ref()
                .filter(|s| s.swappable && eq.connected.get())
                .and_then(|s| s.variant.clone())
        })
    });
    // Offline: the variant whose draft is open, and the draft.
    let offline_variant = RwSignal::new(None::<String>);
    let draft = RwSignal::new(Vec::<u32>::new());
    let offline_variants = Memo::new(move |_| {
        presets.profile.with(|p| {
            p.variants
                .iter()
                .filter(|v| v.layout.is_some())
                .map(|v| v.variant.clone())
                .collect::<Vec<_>>()
        })
    });
    // Open the first variant with a layout, and re-seed the draft from
    // what that variant would start with whenever the variant changes.
    Effect::new(move |_| {
        let known = offline_variants.get();
        if offline_variant
            .with_untracked(|v| v.as_ref().is_none_or(|v| !known.contains(v)))
        {
            offline_variant.set(known.first().cloned());
        }
    });
    Effect::new(move |_| {
        let Some(v) = offline_variant.get() else {
            return;
        };
        let seed = presets.profile.with_untracked(|p| {
            p.startup_items(&v)
                .unwrap_or_else(|| layout_choices(&layout_of(p, &v)))
        });
        draft.set(seed);
    });

    let variant = Memo::new(move |_| live.get().or_else(|| offline_variant.get()));
    let categories = Memo::new(move |_| {
        if live.get().is_some() {
            return eq
                .snapshot
                .with(|s| s.as_ref().map(|s| s.categories.clone()).unwrap_or_default());
        }
        let Some(v) = offline_variant.get() else {
            return Vec::new();
        };
        let layout = presets.profile.with(|p| layout_of(p, &v));
        draft.with(|d| draw_draft(&layout, d))
    });
    // What "Save as preset" stores.
    let current_items = Signal::derive(move || {
        if live.get().is_some() {
            eq.snapshot
                .with(|s| s.as_ref().map(|s| s.selection.clone()).unwrap_or_default())
        } else {
            draft.get()
        }
    });

    let on_select = Callback::new(move |idx: i64| {
        if live.get_untracked().is_some() {
            eq.select(idx);
            return;
        }
        let Ok(idx) = u32::try_from(idx) else {
            return;
        };
        let cats = categories.get_untracked();
        let category_of = |i: u32| {
            cats.iter()
                .find(|c| c.options.iter().any(|o| o.idx == i64::from(i)))
                .map(|c| c.category_idx)
        };
        draft.update(|d| *d = select_in(d, idx, category_of));
    });
    let pending = Signal::derive(move || eq.pending.get());

    view! {
        <div class="flex flex-col">
            <header class="mb-10 flex flex-col gap-6 md:flex-row md:items-end md:justify-between">
                <div>
                    <h2 class="text-3xl font-extralight tracking-tight">"Equipment"</h2>
                    <p class="mt-3 max-w-xl border-l border-accent-400/40 pl-5 text-sm leading-relaxed font-light text-white/60">
                        {move || {
                            if live.get().is_some() {
                                "Changes apply to the running aircraft immediately, from here or from the EFB, and are remembered: the next flight starts with the fit you last flew, or with the preset you choose below."
                            } else {
                                "Build presets and choose what your next flight starts with. They load as soon as the aircraft connects; once in flight, swap from here or from the EFB."
                            }
                        }}
                    </p>
                </div>
                <div class="flex shrink-0 items-center gap-4">
                    {move || {
                        if eq.connected.get() {
                            view! { <StatusDot tone=Tone::Accent label="Aircraft connected" /> }.into_any()
                        } else {
                            view! { <StatusDot tone=Tone::Neutral label="No aircraft" /> }.into_any()
                        }
                    }}
                    <Button
                        variant=Variant::Glass
                        size=Size::Sm
                        icon=icons::REFRESH_CW
                        loading=eq.loading
                        on_click=Callback::new(refresh)
                    >
                        "Refresh"
                    </Button>
                </div>
            </header>

            <ErrorLine title="Equipment change failed" error=eq.error.into() />
            <ErrorLine title="Preset change failed" error=presets.error.into() />

            {move || eq.snapshot.get().filter(|_| live.get().is_some()).map(|s| view! { <StatusChips snapshot=s /> })}

            {move || {
                if live.get().is_some() {
                    return ().into_any();
                }
                let loading = eq.loading.get();
                let connected = eq.connected.get();
                let snap = eq.snapshot.get();
                match snap {
                    Some(s) if connected && s.variant.is_none() => {
                        return view! {
                            <Notice
                                title="Waiting for the aircraft to identify itself"
                                body="The gauge is connected but hasn\u{2019}t resolved which airframe it\u{2019}s in yet. This usually clears within a few seconds of the aircraft loading."
                                spinning=true
                            />
                        }
                            .into_any();
                    }
                    Some(Snapshot { variant: Some(v), swappable: false, .. }) if connected => {
                        return view! {
                            <Notice
                                title=format!("{v} has a fixed flight deck")
                                body="This airframe ships one configuration; there is no equipment to swap."
                            />
                        }
                            .into_any();
                    }
                    _ => {}
                }
                if loading && offline_variants.with(Vec::is_empty) {
                    return view! { <Notice title="Querying the aircraft…" spinning=true /> }.into_any();
                }
                if offline_variants.with(Vec::is_empty) {
                    return view! {
                        <Notice
                            title="No aircraft connected"
                            body="Load into the aircraft once and this page learns its equipment; after that you can build presets here with the sim closed."
                        >
                            <Button variant=Variant::Glass size=Size::Sm icon=icons::REFRESH_CW on_click=Callback::new(refresh)>
                                "Try again"
                            </Button>
                        </Notice>
                    }
                        .into_any();
                }
                view! { <OfflineBar variants=offline_variants.into() selected=offline_variant /> }.into_any()
            }}

            {move || {
                variant
                    .get()
                    .map(|v| {
                        view! {
                            <PresetPanel
                                presets=presets
                                variant=v
                                live=Signal::derive(move || live.get().is_some())
                                current=current_items
                                on_load=Callback::new(move |p: EquipmentPreset| {
                                    if live.get_untracked().is_some() {
                                        presets.apply(p.id, move |raw| eq.accept(raw));
                                    } else {
                                        draft.set(p.items);
                                    }
                                })
                            />
                        }
                    })
            }}

            <div class="grid grid-cols-1 gap-x-10 gap-y-10 md:grid-cols-2 xl:grid-cols-3">
                // Every push replaces the whole fit; nothing here holds focus.
                {move || {
                    let lock = live.get().is_none() && offline_variant.get().is_none();
                    categories
                        .get()
                        .into_iter()
                        .map(|category| {
                            view! {
                                <CategoryCard
                                    category=category
                                    on_select=on_select
                                    pending=pending
                                    locked=lock
                                />
                            }
                        })
                        .collect_view()
                }}
            </div>
        </div>
    }
    .into_any()
}

/// Offline: which variant's draft is open.
#[component]
fn OfflineBar(variants: Signal<Vec<String>>, selected: RwSignal<Option<String>>) -> impl IntoView {
    let groups = Signal::derive(move || {
        vec![SelectGroup::new(
            "",
            variants
                .get()
                .into_iter()
                .map(SelectOption::plain)
                .collect(),
        )]
    });
    view! {
        <div class="mb-8 flex flex-wrap items-center gap-4 border-t border-white/10 pt-6">
            <Badge tone=Tone::Neutral>"Offline"</Badge>
            <p class="text-sm font-light text-white/55">"Editing the equipment of"</p>
            <div class="w-48">
                <Select
                    value=Signal::derive(move || selected.get().unwrap_or_default())
                    groups=groups
                    size=Size::Sm
                    on_change=Callback::new(move |v: String| selected.set(Some(v)))
                />
            </div>
        </div>
    }
}

#[component]
fn PresetPanel(
    presets: Presets,
    variant: String,
    live: Signal<bool>,
    current: Signal<Vec<u32>>,
    on_load: Callback<EquipmentPreset>,
) -> impl IntoView {
    let name = RwSignal::new(String::new());
    let variant = StoredValue::new(variant);
    let startup = Signal::derive(move || {
        presets.profile.with(|p| {
            p.variant(&variant.get_value())
                .and_then(|v| v.startup.clone())
                .unwrap_or_default()
        })
    });
    let startup_groups = Signal::derive(move || {
        let mut options = vec![SelectOption::new(LAST_FIT, "The fit I last flew")];
        presets.profile.with(|p| {
            options.extend(
                p.presets
                    .iter()
                    .map(|x| SelectOption::new(x.id.clone(), x.name.clone())),
            );
        });
        vec![SelectGroup::new("", options)]
    });
    // This variant's presets first, then the rest by name.
    let list = Signal::derive(move || {
        let v = variant.get_value();
        let mut all = presets.profile.with(|p| p.presets.clone());
        all.sort_by(|a, b| (a.variant != v).cmp(&(b.variant != v)).then(a.name.cmp(&b.name)));
        all
    });
    let save = move |()| {
        let n = name.get_untracked();
        if n.trim().is_empty() {
            return;
        }
        presets.save(None, n, variant.get_value(), current.get_untracked());
        name.set(String::new());
    };

    view! {
        <section class="mb-12 grid gap-8 border-t border-white/10 pt-6 lg:grid-cols-[minmax(0,1fr)_minmax(0,1.4fr)]">
            <div class="flex flex-col gap-6">
                <div>
                    <p class=cn!("mb-3", EYEBROW)>{move || format!("Next {} flight starts with", variant.get_value())}</p>
                    <Select
                        value=startup
                        groups=startup_groups
                        disabled=presets.busy
                        on_change=Callback::new(move |id: String| {
                            let id = (id != LAST_FIT).then_some(id);
                            presets.set_startup(variant.get_value(), id);
                        })
                    />
                </div>
                <div>
                    <p class=cn!("mb-3", EYEBROW)>
                        {move || if live.get() { "Save the fit flying now" } else { "Save this draft" }}
                    </p>
                    <div class="flex items-center gap-2">
                        <div class="min-w-0 flex-1">
                            <TextInput
                                value=name
                                placeholder="Preset name"
                                size=Size::Sm
                                on_input=Callback::new(move |s: String| name.set(s))
                            />
                        </div>
                        <Button
                            variant=Variant::Glass
                            size=Size::Sm
                            icon=icons::DOWNLOAD
                            disabled=Signal::derive(move || {
                                presets.busy.get() || name.with(|n| n.trim().is_empty()) || current.with(Vec::is_empty)
                            })
                            on_click=Callback::new(save)
                        >
                            "Save preset"
                        </Button>
                    </div>
                </div>
            </div>
            <div>
                <p class=cn!("mb-3", EYEBROW)>"Presets"</p>
                {move || {
                    let rows = list.get();
                    if rows.is_empty() {
                        return view! {
                            <p class="border-t border-white/10 py-4 text-sm font-light text-white/40">
                                "No presets yet. Set up the equipment below and save it here."
                            </p>
                        }
                            .into_any();
                    }
                    view! {
                        <div class="flex flex-col border-t border-white/10">
                            {rows
                                .into_iter()
                                .map(|p| {
                                    view! {
                                        <PresetRow
                                            preset=p
                                            presets=presets
                                            variant=variant.get_value()
                                            live=live
                                            current=current
                                            on_load=on_load
                                        />
                                    }
                                })
                                .collect_view()}
                        </div>
                    }
                        .into_any()
                }}
            </div>
        </section>
    }
}

#[component]
fn PresetRow(
    preset: EquipmentPreset,
    presets: Presets,
    variant: String,
    live: Signal<bool>,
    current: Signal<Vec<u32>>,
    on_load: Callback<EquipmentPreset>,
) -> impl IntoView {
    let foreign = preset.variant != variant;
    let p = StoredValue::new(preset.clone());
    let matches = Signal::derive(move || {
        let mut now = current.get();
        now.sort_unstable();
        let mut mine = p.with_value(|p| p.items.clone());
        mine.sort_unstable();
        now == mine
    });
    let confirm = RwSignal::new(false);
    view! {
        <div class="flex flex-wrap items-center gap-3 border-b border-white/10 py-3">
            <div class="min-w-0 flex-1">
                <p class="truncate text-sm text-white/85">{preset.name.clone()}</p>
                <p class="text-xs font-light text-white/40">
                    {format!("{} items", preset.items.len())}
                    {foreign.then(|| format!(" \u{b7} saved on the {}", preset.variant))}
                </p>
            </div>
            {move || matches.get().then(|| view! { <Badge tone=Tone::Accent size=Size::Xs>"Current"</Badge> })}
            <Button
                variant=Variant::Glass
                size=Size::Sm
                disabled=presets.busy
                on_click=Callback::new(move |()| on_load.run(p.get_value()))
            >
                {move || if live.get() { "Load into aircraft" } else { "Open" }}
            </Button>
            <Button
                variant=Variant::Ghost
                size=Size::Sm
                disabled=Signal::derive(move || presets.busy.get() || matches.get() || current.with(Vec::is_empty))
                on_click=Callback::new(move |()| {
                    let x = p.get_value();
                    presets.save(Some(x.id), x.name, x.variant, current.get_untracked());
                })
            >
                "Overwrite"
            </Button>
            <Button
                variant=Variant::Ghost
                tone=Tone::Danger
                size=Size::Sm
                disabled=presets.busy
                on_click=Callback::new(move |()| confirm.set(true))
            >
                "Delete"
            </Button>
            <ConfirmDialog
                open=confirm
                on_close=Callback::new(move |()| confirm.set(false))
                on_confirm=Callback::new(move |()| {
                    confirm.set(false);
                    presets.delete(p.get_value().id);
                })
                title="Delete preset?"
                message="The preset is removed. A variant that started its flights with it goes back to the fit last flown."
                confirm_label="Delete"
            />
        </div>
    }
}

#[component]
fn StatusChips(snapshot: Snapshot) -> impl IntoView {
    view! {
        <div class="mb-8 flex flex-wrap gap-2">
            {snapshot.variant.map(|v| view! { <Badge tone=Tone::Neutral>{v}</Badge> })}
            {if snapshot.professional {
                view! { <Badge tone=Tone::Accent icon=icons::CIRCLE_CHECK>"Professional equipment unlocked"</Badge> }
                    .into_any()
            } else {
                view! {
                    <Badge tone=Tone::Warning>"Freeware fit: sign in with a Professional licence for more"</Badge>
                }
                    .into_any()
            }}
        </div>
    }
}

#[component]
fn CategoryCard(
    category: EquipmentCategory,
    on_select: Callback<i64>,
    pending: Signal<Option<i64>>,
    locked: bool,
) -> impl IntoView {
    let active = category
        .options
        .iter()
        .find(|o| o.active)
        .map_or_else(|| "None".to_owned(), |o| o.label.clone());
    let selectable = category.selectable && !locked;
    view! {
        <section class="group relative border-t border-white/10 pt-4">
            <div class="mb-4 flex items-baseline gap-3">
                <span class="text-sm font-medium text-white/85">{category.category.clone()}</span>
                {(!category.selectable)
                    .then(|| {
                        view! {
                            <Tooltip text="Fitted by this airframe, not a choice">
                                <span class="flex items-center gap-1 text-xs text-white/40">
                                    <Icon icon=LOCK class="h-3 w-3" />
                                    "Fitted"
                                </span>
                            </Tooltip>
                        }
                    })}
                <span class="ml-auto truncate font-mono text-[11px] text-accent-300/80">{active}</span>
            </div>
            <div role="radiogroup" class="flex flex-col gap-1.5">
                {category
                    .options
                    .into_iter()
                    .map(|o| view! { <OptionRow option=o selectable=selectable on_select=on_select pending=pending /> })
                    .collect_view()}
            </div>
        </section>
    }
}

#[component]
fn OptionRow(
    option: EquipmentOption,
    selectable: bool,
    on_select: Callback<i64>,
    pending: Signal<Option<i64>>,
) -> impl IntoView {
    let idx = option.idx;
    let active = option.active;
    let interactive = selectable && !active;
    let mine = Signal::derive(move || pending.get() == Some(idx));
    // Any change in flight locks every row: the gauge answers one command
    // at a time.
    let disabled = Signal::derive(move || !interactive || pending.get().is_some());
    let tone = if active {
        "border-accent-400/70 bg-accent-500/10"
    } else if interactive {
        "border-white/10 hover:border-white/30 hover:bg-white/[0.03]"
    } else {
        "border-white/5"
    };
    view! {
        <button
            type="button"
            role="radio"
            aria-checked=active.to_string()
            disabled=disabled
            class=cn!("flex items-center gap-3 border-l-2 px-4 py-2.5 text-left transition-colors disabled:cursor-default", tone)
            on:click=move |_| on_select.run(idx)
        >
            <span class=cn!(
                "h-2.5 w-2.5 shrink-0 rounded-full border",
                if active { "border-accent-400 bg-accent-400" } else { "border-white/30" },
            )></span>
            <span class=cn!("min-w-0 flex-1 truncate text-sm", if active { "text-white" } else { "text-white/75" })>
                {option.label}
            </span>
            {(option.tier.as_deref() == Some("professional"))
                .then(|| view! { <Badge tone=Tone::Info size=Size::Xs>"Pro"</Badge> })}
            {move || mine.get().then(|| view! { <Spinner size=Size::Xs /> })}
        </button>
    }
}
