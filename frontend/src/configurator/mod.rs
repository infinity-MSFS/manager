//! Per-aircraft settings (v3 `features/configurator`, `pages/configure`).
//!
//! The catalog declares a product's configurator: a schema of groups and
//! fields, sidebar indicators and actions, and outputs the backend writes.
//! [`ConfigurePage`] resolves the product on the current channel and
//! mounts [`chrome::Chrome`], which loads the saved value, autosaves edits
//! and renders the schema through the widget registry and custom pages.
//!
//! `serde_json` comes through Leptos's re-export; the frontend crate does
//! not depend on it directly.

mod assets;
mod chrome;
mod indicators;
mod map;
mod mtr;
mod pages;
mod renderer;
mod schema;
mod state;
mod toast;
mod widgets;
mod zones;

use infinity_ipc::commands as cmd;
use infinity_ipc::dto::ProductRef;
use infinity_ui::prelude::*;
use leptos_router::hooks::use_params_map;

use crate::editorial::{ErrorNote, Page};
use crate::ipc;
use crate::state::use_app;

/// `/configure/:product`.
#[component]
pub(crate) fn ConfigurePage() -> impl IntoView {
    let app = use_app();
    let channel = app.channel();
    let params = use_params_map();
    let product_id = Memo::new(move |_| params.with(|p| p.get("product").unwrap_or_default()));
    let catalog = LocalResource::new(move || {
        let channel = channel.get();
        async move { ipc::invoke::<cmd::CatalogFetch>(&channel).await }
    });

    view! {
        <Page>
            <Suspense fallback=|| view! { <Skeleton class="h-96 w-full" /> }>
                {move || {
                    let id = product_id.get();
                    catalog
                        .get()
                        .map(|result| match result {
                            Err(error) => view! { <ErrorNote error=error /> }.into_any(),
                            Ok(catalog) => {
                                match catalog.products.into_iter().find(|p| p.id == id) {
                                    None => view! { <Missing title="Unknown product" detail=format!("No catalog entry for \u{201c}{id}\u{201d}.") /> }
                                        .into_any(),
                                    Some(p) if p.configurator.is_none() => view! {
                                        <Missing title=p.name.clone() detail="This product doesn\u{2019}t ship a configurator yet." />
                                    }
                                        .into_any(),
                                    Some(p) => {
                                        let product_ref = ProductRef {
                                            product_id: p.id.clone(),
                                            channel: channel.get_untracked(),
                                        };
                                        view! { <chrome::Chrome product=p product_ref=product_ref /> }.into_any()
                                    }
                                }
                            }
                        })
                }}
            </Suspense>
        </Page>
    }
}

#[component]
fn Missing(#[prop(into)] title: String, #[prop(into)] detail: String) -> impl IntoView {
    view! {
        <div class="max-w-2xl">
            <h1 class="mb-6 text-5xl leading-[1.02] font-extralight tracking-tight">{title}</h1>
            <p class="border-t border-white/10 py-6 font-light text-white/55">{detail}</p>
        </div>
    }
}

/// Whether the library should offer a Configure button.
pub(crate) fn has_configurator(product: &infinity_ipc::catalog::Product) -> bool {
    product.configurator.is_some()
}
