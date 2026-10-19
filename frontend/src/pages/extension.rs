use infinity_ui::prelude::*;
use leptos_router::hooks::use_params_map;

use super::NotFound;
use crate::ext::use_extensions;
use crate::state::use_app;

#[component]
pub(crate) fn Extension() -> impl IntoView {
    let app = use_app();
    let params = use_params_map();
    let extensions = use_extensions();

    move || {
        let slug = params.with(|p| p.get("slug").unwrap_or_default());
        let page = extensions.pages.iter().find(|p| p.slug == slug).cloned();
        match page {
            Some(page) if app.session.with(|s| page.gate.allows(s)) => (page.view)(),
            _ => view! { <NotFound /> }.into_any(),
        }
    }
}
