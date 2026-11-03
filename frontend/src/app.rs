use infinity_ui::BackgroundGradientAnimation;
use infinity_ui::prelude::*;
use leptos_router::components::{Route, Router, Routes};
use leptos_router::path;

use crate::brand::{PALETTE, ramp};
use crate::ext::{self, Extensions, use_extensions};
use crate::notices::{self, NoticeLayer};
use crate::pages;
use crate::shell::{NavBar, TitleBar};
use crate::state::{provide_app_state, use_app};
use crate::theme;
use infinity_ipc::dto::Theme;

#[component]
pub(crate) fn App(extensions: Extensions) -> impl IntoView {
    provide_app_state();
    theme::provide(use_app().prefs);
    ext::provide(extensions);
    let notices = notices::provide();

    view! {
        <Router>
            <div class="relative h-screen w-screen overflow-hidden bg-black">
                <Backdrop />
                <Chrome />
            </div>
            <NoticeLayer notices=notices />
            <Background />
        </Router>

    }
}

/// Extension views that live as long as the window.
#[component]
fn Background() -> impl IntoView {
    use_extensions()
        .background
        .iter()
        .map(|view| view())
        .collect_view()
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

    let app = use_app();
    let theme = Signal::derive(move || app.prefs.with(|p| p.theme));
    let still_style =
        Signal::derive(move || app.prefs.with(theme::still_backdrop).unwrap_or_else(ramp));

    view! {
        <div class="absolute inset-0">
            {move || {
                if theme.get() == Theme::Amoled {
                    view! { <div class="h-full w-full bg-black"></div> }.into_any()
                } else if still.get() {
                    view! { <div class="h-full w-full" style=move || still_style.get()></div> }.into_any()
                } else {
                    view! {
                        <BackgroundGradientAnimation
                            palette=PALETTE
                            container_class=format!("h-full w-full {}", theme::BACKDROP_CLASS)
                        >
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
    let app = use_app();
    let surface = move || {
        if app.prefs.with(|p| p.theme) == Theme::Amoled {
            "relative flex h-full w-full flex-col overflow-hidden bg-black text-white"
        } else {
            "relative flex h-full w-full flex-col overflow-hidden bg-black/80 text-white"
        }
    };
    view! {
        <div class=surface>
            <TitleBar />
            <NavBar />
            <pages::FirstRunGate />
            <main class="min-h-0 flex-1 overflow-y-auto">
                <Routes fallback=|| view! { <pages::NotFound /> }>
                    <Route path=path!("/") view=pages::Library />
                    <Route path=path!("/settings") view=pages::Settings />
                    <Route path=path!("/liveries/:product") view=pages::Liveries />
                    <Route path=path!("/configure/:product") view=crate::configurator::ConfigurePage />
                    <Route path=path!("/login") view=pages::Login />
                    <Route path=path!("/welcome") view=pages::FirstRun />
                    <Route path=path!("/x/:slug") view=pages::Extension />
                </Routes>
            </main>
        </div>
    }
}
