use infinity_ipc::catalog::Livery;
use infinity_ipc::commands as cmd;
use infinity_ipc::dto::{Channel, livery_job_id};
use infinity_ui::prelude::*;
use leptos_router::hooks::use_params_map;

use super::install::InstallControls;
use super::{ErrorNote, Page};
use crate::editorial::{EYEBROW, HoverRule, PageHead, SectionLabel};
use crate::ipc;
use crate::routes;
use crate::state::use_app;

/// An aircraft's published liveries, each installable like a product.
#[component]
pub(crate) fn Liveries() -> impl IntoView {
    let app = use_app();
    let params = use_params_map();
    let product = Memo::new(move |_| params.with(|p| p.get("product").unwrap_or_default()));
    let catalog = LocalResource::new(move || {
        let product = product.get();
        async move { ipc::invoke::<cmd::CatalogLiveries>(&product).await }
    });
    let filter = RwSignal::new(String::new());
    let channel = app.channel();

    view! {
        <Page>
            <PageHead eyebrow="Infinity / Liveries" title="Livery" accent="manager." />
            <div class="mb-10 flex items-center gap-4">
                <Button variant=Variant::Ghost href=routes::LIBRARY icon=icons::ARROW_LEFT>
                    "Library"
                </Button>
                <div class="max-w-sm flex-1">
                    <TextInput
                        value=filter
                        on_input=Callback::new(move |v| filter.set(v))
                        placeholder="Filter by name, creator or tag"
                        icon=icons::SEARCH
                    />
                </div>
            </div>
            <SectionLabel num="01" label="Available" />
            <Suspense fallback=|| view! { <Skeleton class="h-64 w-full" /> }>
                {move || {
                    catalog
                        .get()
                        .map(|result| match result {
                            Ok(catalog) => {
                                let needle = filter.get().to_lowercase();
                                let shown: Vec<Livery> = catalog
                                    .liveries
                                    .into_iter()
                                    .filter(|l| matches(l, &needle))
                                    .collect();
                                if shown.is_empty() {
                                    return view! {
                                        <p class="border-t border-white/10 py-10 font-light text-white/55">
                                            "No liveries match."
                                        </p>
                                    }
                                        .into_any();
                                }
                                let aircraft = catalog.product_id;
                                view! {
                                    <div class="grid gap-10 md:grid-cols-2">
                                        {shown
                                            .into_iter()
                                            .map(|l| {
                                                view! {
                                                    <LiveryCard
                                                        aircraft=aircraft.clone()
                                                        livery=l
                                                        channel=channel.get_untracked()
                                                    />
                                                }
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

fn matches(livery: &Livery, needle: &str) -> bool {
    needle.is_empty()
        || [&livery.name, &livery.creator]
            .into_iter()
            .chain(&livery.tags)
            .any(|s| s.to_lowercase().contains(needle))
}

#[component]
fn LiveryCard(aircraft: String, livery: Livery, channel: Channel) -> impl IntoView {
    let gallery = if livery.gallery_urls.is_empty() {
        vec![livery.preview_url.clone()]
    } else {
        livery.gallery_urls.clone()
    };
    let count = gallery.len();
    let shown = RwSignal::new(0usize);
    let gallery = StoredValue::new(gallery);
    let step = move |delta: usize| {
        Callback::new(move |()| shown.update(|i| *i = (*i + delta) % count.max(1)))
    };

    view! {
        <article class="group relative flex flex-col border-t border-white/10 pt-6">
            <HoverRule />
            <div class="relative mb-5 aspect-video overflow-hidden rounded-sm border border-white/10 bg-white/5">
                <img
                    src=move || gallery.with_value(|g| g.get(shown.get()).cloned().unwrap_or_default())
                    alt=livery.name.clone()
                    class="h-full w-full object-cover"
                />
                {(count > 1)
                    .then(|| {
                        view! {
                            <div class="absolute right-2 bottom-2 flex gap-1">
                                <Button variant=Variant::Glass size=Size::Sm icon=icons::CHEVRON_LEFT on_click=step(count - 1)>
                                    ""
                                </Button>
                                <Button variant=Variant::Glass size=Size::Sm icon=icons::CHEVRON_RIGHT on_click=step(1)>
                                    ""
                                </Button>
                            </div>
                        }
                    })}
            </div>
            <p class=cn!("mb-2", EYEBROW)>{format!("{} · v{}", livery.creator, livery.version)}</p>
            <h2 class="text-xl font-medium tracking-tight">{livery.name.clone()}</h2>
            <p class="mt-2 line-clamp-3 text-sm font-light text-white/65">{livery.description.clone()}</p>
            {(!livery.tags.is_empty())
                .then(|| {
                    let tags = livery.tags.clone();
                    view! {
                        <div class="mt-3 flex flex-wrap gap-1.5">
                            {tags.into_iter().map(|t| view! { <Badge>{t}</Badge> }).collect_view()}
                        </div>
                    }
                })}
            <InstallControls
                id=livery_job_id(&aircraft, &livery.id)
                name=livery.name.clone()
                latest=livery.version.clone()
                channel=channel
            />
        </article>
    }
}
