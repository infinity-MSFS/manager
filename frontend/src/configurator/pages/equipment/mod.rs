//! Equipment page (v3 `EquipmentPage.tsx`): the live fit of the running
//! aircraft, rendered entirely from what the gauge reports. Every category
//! is a radio group; unowned options are filtered out by the gauge.

mod bridge;

use infinity_ui::prelude::*;

use self::bridge::{Equipment, EquipmentCategory, EquipmentOption, Snapshot};
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
    let eq = Equipment::new(ns);
    let refresh = move |()| leptos::task::spawn_local(eq.refresh());
    let categories = Memo::new(move |_| {
        eq.snapshot
            .with(|s| s.as_ref().map(|s| s.categories.clone()).unwrap_or_default())
    });

    view! {
        <div class="flex flex-col">
            <header class="mb-10 flex flex-col gap-6 md:flex-row md:items-end md:justify-between">
                <div>
                    <h2 class="text-3xl font-extralight tracking-tight">"Equipment"</h2>
                    <p class="mt-3 max-w-xl border-l border-accent-400/40 pl-5 text-sm leading-relaxed font-light text-white/60">
                        "Changes apply to the running aircraft immediately. Nothing is saved to disk: the fit lives in the sim."
                    </p>
                </div>
                <div class="flex shrink-0 items-center gap-4">
                    {move || {
                        if eq.connected.get() {
                            view! { <StatusDot tone=Tone::Accent label="Aircraft connected" /> }.into_any()
                        } else {
                            view! { <StatusDot tone=Tone::Danger label="No aircraft" /> }.into_any()
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

            {move || {
                eq.error
                    .get()
                    .map(|e| {
                        view! {
                            <div class="mb-8 border-l border-red-400/50 py-1 pl-5">
                                <p class=cn!("mb-1", EYEBROW, "text-white/40")>"Equipment change failed"</p>
                                <p data-selectable class="text-sm font-light text-white/70">{e}</p>
                            </div>
                        }
                    })
            }}

            {move || eq.snapshot.get().map(|s| view! { <StatusChips snapshot=s /> })}

            {move || {
                let empty = categories.with(Vec::is_empty);
                let loading = eq.loading.get();
                let connected = eq.connected.get();
                let snap = eq.snapshot.get();
                if loading && empty {
                    return view! { <Notice title="Querying the aircraft…" spinning=true /> }.into_any();
                }
                if !loading && !connected && empty {
                    return view! {
                        <Notice
                            title="No aircraft connected"
                            body="Load into the aircraft in the simulator. This page talks to the running gauge; it can\u{2019}t change anything while the sim is on the main menu."
                        >
                            <Button variant=Variant::Glass size=Size::Sm icon=icons::REFRESH_CW on_click=Callback::new(refresh)>
                                "Try again"
                            </Button>
                        </Notice>
                    }
                        .into_any();
                }
                match snap {
                    Some(s) if connected && s.variant.is_none() => view! {
                        <Notice
                            title="Waiting for the aircraft to identify itself"
                            body="The gauge is connected but hasn\u{2019}t resolved which airframe it\u{2019}s in yet. This usually clears within a few seconds of the aircraft loading."
                            spinning=true
                        />
                    }
                        .into_any(),
                    Some(Snapshot { variant: Some(v), swappable: false, .. }) => view! {
                        <Notice
                            title=format!("{v} has a fixed flight deck")
                            body="This airframe ships one configuration; there is no equipment to swap."
                        />
                    }
                        .into_any(),
                    _ => ().into_any(),
                }
            }}

            <div class="grid grid-cols-1 gap-x-10 gap-y-10 md:grid-cols-2 xl:grid-cols-3">
                // Every push replaces the whole fit; nothing here holds focus.
                {move || {
                    categories
                        .get()
                        .into_iter()
                        .map(|category| view! { <CategoryCard category=category eq=eq /> })
                        .collect_view()
                }}
            </div>
        </div>
    }
    .into_any()
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
fn CategoryCard(category: EquipmentCategory, eq: Equipment) -> impl IntoView {
    let active = category
        .options
        .iter()
        .find(|o| o.active)
        .map_or_else(|| "None".to_owned(), |o| o.label.clone());
    let selectable = category.selectable;
    view! {
        <section class="group relative border-t border-white/10 pt-4">
            <div class="mb-4 flex items-baseline gap-3">
                <span class="text-sm font-medium text-white/85">{category.category.clone()}</span>
                {(!selectable)
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
                    .map(|o| view! { <OptionRow option=o selectable=selectable eq=eq /> })
                    .collect_view()}
            </div>
        </section>
    }
}

#[component]
fn OptionRow(option: EquipmentOption, selectable: bool, eq: Equipment) -> impl IntoView {
    let idx = option.idx;
    let active = option.active;
    let interactive = selectable && !active;
    let pending = Signal::derive(move || eq.pending.get() == Some(idx));
    // Any change in flight locks every row: the gauge answers one command
    // at a time.
    let disabled = Signal::derive(move || !interactive || eq.pending.get().is_some());
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
            on:click=move |_| eq.select(idx)
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
            {move || pending.get().then(|| view! { <Spinner size=Size::Xs /> })}
        </button>
    }
}
