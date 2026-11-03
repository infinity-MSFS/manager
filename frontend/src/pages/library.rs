use infinity_ipc::IpcErrorCode;
use infinity_ipc::catalog::{ChangelogEntry, OwnershipPolicy, Product};
use infinity_ipc::commands as cmd;
use infinity_ipc::dto::Channel;
use infinity_ui::prelude::*;
use leptos_router::hooks::use_navigate;

use super::install::{InstallControls, human_bytes};
use super::{ErrorNote, Page};
use crate::editorial::{EYEBROW, HeaderStat, HoverRule, PageHead, SectionLabel};
use crate::ipc;
use crate::markdown::Markdown;
use crate::routes;
use crate::state::use_app;

#[component]
pub(crate) fn Library() -> impl IntoView {
    let app = use_app();
    let channel = app.channel();
    let catalog = LocalResource::new(move || {
        let channel = channel.get();
        async move { ipc::invoke::<cmd::CatalogFetch>(&channel).await }
    });
    // Picks up packages added or removed outside the manager. The result
    // also arrives as `InstalledChanged`; only a failure needs showing here.
    let rescan = LocalResource::new(move || {
        let channel = channel.get();
        async move { ipc::invoke::<cmd::InstalledRescan>(&channel).await }
    });
    let visible = Signal::derive(move || match catalog.get() {
        Some(Ok(c)) => Some(Ok(c
            .products
            .into_iter()
            .filter(|p| app.shows(p))
            .collect::<Vec<_>>())),
        Some(Err(e)) => Some(Err(e)),
        None => None,
    });
    let count = Signal::derive(move || match visible.get() {
        Some(Ok(p)) => format!("{:02}", p.len()),
        _ => "—".to_owned(),
    });
    let channel_label = Signal::derive(move || title_case(channel.get()));

    view! {
        <Page>
            <PageHead
                eyebrow="Infinity / Library"
                title="My"
                accent="Library."
                lead="Manage your installed products and downloads."
                aside=ViewFn::from(move || {
                    view! {
                        <dl class="flex gap-10 font-mono text-[11px] md:flex-col md:gap-4 md:text-right">
                            <HeaderStat label="Products" value=count />
                            <HeaderStat label="Channel" value=channel_label />
                        </dl>
                    }
                })
            />
            {move || {
                rescan
                    .get()
                    .and_then(Result::err)
                    .filter(|e| e.code != IpcErrorCode::Unsupported)
                    .map(|e| view! { <div class="mb-10"><ErrorNote error=e /></div> })
            }}
            <SectionLabel num="01" label="Products" />
            <Suspense fallback=|| view! { <RowsSkeleton /> }>
                {move || {
                    visible
                        .get()
                        .map(|result| match result {
                            Ok(products) if products.is_empty() => {
                                view! { <Empty /> }.into_any()
                            }
                            Ok(products) => {
                                view! {
                                    <div class="flex flex-col gap-10">
                                        {products
                                            .into_iter()
                                            .enumerate()
                                            .map(|(i, p)| {
                                                view! { <ProductRow index=i + 1 product=p channel=channel.get_untracked() /> }
                                            })
                                            .collect_view()}
                                    </div>
                                }
                                    .into_any()
                            }
                            Err(error) => view! { <ErrorNote error=error /> }.into_any(),
                        })
                }}
            </Suspense>
        </Page>
    }
}

#[component]
fn Empty() -> impl IntoView {
    let app = use_app();
    let hint = move || {
        if app.capabilities.get().accounts && app.entitlements.get().is_none() {
            "Sign in to see the products you own."
        } else {
            "No products on this channel yet."
        }
    };
    view! { <p class="border-t border-white/10 py-10 font-light text-white/55">{hint}</p> }
}

#[component]
fn ProductRow(index: usize, product: Product, channel: Channel) -> impl IntoView {
    let app = use_app();
    let navigate = use_navigate();
    let status = if product.coming_soon {
        "Coming soon"
    } else if matches!(product.ownership, OwnershipPolicy::Free) {
        "Free"
    } else {
        "Owned"
    };
    let installed = app.installed_package(product.id.clone());
    let id = StoredValue::new(product.id.clone());
    // Re-measured whenever the installed version changes.
    let size = LocalResource::new(move || {
        let version = installed.get().map(|p| p.version);
        async move {
            match version {
                Some(_) => ipc::invoke::<cmd::InstalledSize>(&id.get_value())
                    .await
                    .ok()
                    .flatten(),
                None => None,
            }
        }
    });
    let changelog = LocalResource::new(move || async move {
        ipc::invoke::<cmd::CatalogChangelog>(&id.get_value())
            .await
            .unwrap_or_default()
    });
    let configure = (crate::configurator::has_configurator(&product)).then(|| {
        let go = navigate.clone();
        let is_installed = Signal::derive(move || installed.get().is_some());
        move || {
            let go = go.clone();
            is_installed.get().then(|| {
                view! {
                    <Button
                        variant=Variant::Glass
                        size=Size::Sm
                        icon=icons::SETTINGS
                        on_click=Callback::new(move |()| go(&routes::configure(&id.get_value()), Default::default()))
                    >
                        "Configure"
                    </Button>
                }
            })
        }
    });
    let liveries = product.has_liveries.then(|| {
        let go = navigate.clone();
        view! {
            <Button
                variant=Variant::Glass
                size=Size::Sm
                icon=icons::LAYERS
                on_click=Callback::new(move |()| go(&routes::liveries(&id.get_value()), Default::default()))
            >
                "Liveries"
            </Button>
        }
    });

    view! {
        <article class="group relative grid gap-8 border-t border-white/10 pt-6 md:grid-cols-[300px_1fr]">
            <HoverRule />
            <div class="relative aspect-video overflow-hidden rounded-sm border border-white/10 bg-white/5">
                {match product.image_src.clone() {
                    Some(src) => {
                        view! {
                            <img
                                src=src
                                alt=product.name.clone()
                                class="h-full w-full object-cover transition-transform duration-700 group-hover:scale-[1.03]"
                            />
                        }
                            .into_any()
                    }
                    None => {
                        view! {
                            <div class="flex h-full w-full items-center justify-center text-white/25">
                                <Icon icon=icons::PACKAGE class="h-10 w-10" />
                            </div>
                        }
                            .into_any()
                    }
                }}
            </div>
            <div class="flex min-w-0 flex-col">
                <div class="mb-3 flex items-center justify-between font-mono text-[11px]">
                    <span class="text-accent-400/80">{format!("{index:02}")}</span>
                    <span class=cn!(EYEBROW, "text-white/60")>{status}</span>
                </div>
                <h2 class="text-2xl font-medium tracking-tight">{product.name.clone()}</h2>
                <p class="mt-1 mb-4 text-sm font-light text-white/45">
                    {format!("{} · v{}", product.vendor, product.version)}
                    {move || size.get().flatten().map(|b| format!(" · {} on disk", human_bytes(b)))}
                </p>
                {product
                    .description
                    .clone()
                    .map(|d| {
                        view! {
                            <p class="line-clamp-2 max-w-3xl text-sm leading-relaxed font-light text-white/65">
                                {d}
                            </p>
                        }
                    })}
                <InstallControls
                    id=product.id.clone()
                    name=product.name.clone()
                    latest=product.version.clone()
                    channel=channel
                    coming_soon=product.coming_soon
                />
                <div class="mt-4 flex gap-2">{configure}{liveries}</div>
                {move || {
                    changelog
                        .get()
                        .filter(|c| !c.is_empty())
                        .map(|entries| view! { <ReleaseNotes entries=entries /> })
                }}
            </div>
        </article>
    }
}

/// The newest entry open, older ones behind a toggle.
#[component]
fn ReleaseNotes(entries: Vec<ChangelogEntry>) -> impl IntoView {
    let expanded = RwSignal::new(false);
    let older = entries.len().saturating_sub(1);
    let entries = StoredValue::new(entries);
    view! {
        <div class="mt-6 max-w-3xl border-l border-white/10 pl-5">
            <p class=cn!("mb-3", EYEBROW)>"Release notes"</p>
            {move || {
                let shown = if expanded.get() { usize::MAX } else { 1 };
                entries
                    .get_value()
                    .into_iter()
                    .take(shown)
                    .map(|e| view! { <Markdown text=e.changelog /> })
                    .collect_view()
            }}
            {(older > 0)
                .then(|| {
                    view! {
                        <Button
                            variant=Variant::Link
                            on_click=Callback::new(move |()| expanded.update(|e| *e = !*e))
                        >
                            {move || {
                                if expanded.get() {
                                    "Show less".to_owned()
                                } else {
                                    format!("{older} earlier versions")
                                }
                            }}
                        </Button>
                    }
                })}
        </div>
    }
}

pub(crate) fn title_case(channel: Channel) -> String {
    let s = channel.as_str();
    s.get(..1).map(str::to_uppercase).unwrap_or_default() + s.get(1..).unwrap_or_default()
}

#[component]
fn RowsSkeleton() -> impl IntoView {
    view! {
        <div class="flex flex-col gap-10">
            {(0..3)
                .map(|_| {
                    view! {
                        <div class="grid gap-8 border-t border-white/10 pt-6 md:grid-cols-[300px_1fr]">
                            <Skeleton class="aspect-video w-full rounded-sm" />
                            <div class="flex flex-col gap-3">
                                <Skeleton class="h-3 w-12" />
                                <Skeleton class="h-7 w-72" />
                                <Skeleton class="h-4 w-48" />
                                <Skeleton class="h-4 w-full max-w-xl" />
                            </div>
                        </div>
                    }
                })
                .collect_view()}
        </div>
    }
}
