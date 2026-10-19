use infinity_ipc::catalog::{OwnershipPolicy, Product};
use infinity_ipc::commands as cmd;
use infinity_ipc::dto::Channel;
use infinity_ui::prelude::*;

use super::{ErrorNote, Page};
use crate::editorial::{EYEBROW, HeaderStat, HoverRule, PageHead, SectionLabel};
use crate::ipc;

#[component]
pub(crate) fn Library() -> impl IntoView {
    let catalog = LocalResource::new(|| ipc::invoke::<cmd::CatalogFetch>(&Channel::Stable));
    let count = Signal::derive(move || match catalog.get() {
        Some(Ok(c)) => format!("{:02}", c.products.len()),
        _ => "—".to_owned(),
    });

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
                            <HeaderStat label="Channel" value=Signal::derive(|| "Stable".to_owned()) />
                        </dl>
                    }
                })
            />
            <SectionLabel num="01" label="Products" />
            <Suspense fallback=|| view! { <RowsSkeleton /> }>
                {move || {
                    catalog
                        .get()
                        .map(|result| match result {
                            Ok(catalog) if catalog.products.is_empty() => {
                                view! {
                                    <p class="border-t border-white/10 py-10 font-light text-white/55">
                                        "No products on this channel yet."
                                    </p>
                                }
                                    .into_any()
                            }
                            Ok(catalog) => {
                                view! {
                                    <div class="flex flex-col gap-10">
                                        {catalog
                                            .products
                                            .into_iter()
                                            .enumerate()
                                            .map(|(i, p)| view! { <ProductRow index=i + 1 product=p /> })
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
fn ProductRow(index: usize, product: Product) -> impl IntoView {
    let status = if product.coming_soon {
        "Coming soon"
    } else if matches!(product.ownership, OwnershipPolicy::Free) {
        "Free"
    } else {
        "Payware"
    };

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
            </div>
        </article>
    }
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
