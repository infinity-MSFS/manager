use infinity_ui::BackgroundGradientAnimation;
use infinity_ui::prelude::*;
use leptos_router::components::{Route, Router, Routes};
use leptos_router::path;

use crate::brand::{PALETTE, ramp};
use crate::ext::{self, Extensions};
use crate::pages;
use crate::shell::{NavBar, TitleBar};
use crate::state::{provide_app_state, use_app};

#[component]
pub(crate) fn App(extensions: Extensions) -> impl IntoView {
    provide_app_state();
    ext::provide(extensions);

    view! {
        <Router>
            <div class="relative h-screen w-screen overflow-hidden bg-black">
                <Backdrop />
                <Chrome />
            </div>
        </Router>

    }
}

#[component]
fn Backdrop() -> impl IntoView {
    let reduce = use_app().reduce_motion();
    let low_performance = webgl_support().is_low_performance();
    let still = Signal::derive(move || low_performance || reduce.get());

    Effect::new(move |_| {
        if let Some(root) = document().document_element() {
            let _ = if reduce.get() {
                root.set_attribute("data-motion", "reduced")
            } else {
                root.remove_attribute("data-motion")
            };
        }
    });

    view! {
        <div class="absolute inset-0">
            {move || {
                if still.get() {
                    view! { <div class="h-full w-full" style=ramp()></div> }.into_any()
                } else {
                    view! {
                        <BackgroundGradientAnimation palette=PALETTE container_class="h-full w-full">
                            <span class="hidden"></span>
                        </BackgroundGradientAnimation>
                    }
                        .into_any()
                }
            }}
        </div>
    }
}

#[component]
fn Chrome() -> impl IntoView {
    view! {
        <div class="relative flex h-full w-full flex-col overflow-hidden bg-black/80 text-white">
            <TitleBar />
            <NavBar />
            <main class="min-h-0 flex-1 overflow-y-auto">
                <Routes fallback=|| view! { <pages::NotFound /> }>
                    <Route path=path!("/") view=pages::Library />
                    <Route path=path!("/settings") view=pages::Settings />
                    <Route path=path!("/login") view=pages::Login />
                    <Route path=path!("/x/:slug") view=pages::Extension />
                </Routes>
            </main>
        </div>
    }
}
