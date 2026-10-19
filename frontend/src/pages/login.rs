use infinity_ipc::commands as cmd;
use infinity_ipc::dto::SessionState;
use infinity_ui::prelude::*;
use leptos_router::hooks::use_navigate;

use super::{ErrorNote, Page};
use crate::editorial::PageHead;
use crate::ipc;
use crate::routes;
use crate::state::use_app;

#[component]
pub(crate) fn Login() -> impl IntoView {
    let app = use_app();
    let navigate = use_navigate();
    let error = RwSignal::new(None);

    // Back to the library as soon as the session lands.
    Effect::new(move |_| {
        if matches!(app.session.get(), SessionState::SignedIn { .. }) {
            navigate(routes::LIBRARY, Default::default());
        }
    });

    let sign_in = Action::new_local(move |_: &()| async move {
        match ipc::invoke::<cmd::AuthBeginLogin>(&()).await {
            Ok(_) => error.set(None),
            Err(e) => error.set(Some(e)),
        }
    });
    let waiting = Signal::derive(move || {
        sign_in.pending().get() || matches!(app.session.get(), SessionState::Pending)
    });

    view! {
        <Page>
            <PageHead
                eyebrow="Infinity / Account"
                title="Sign in to"
                accent="Infinity."
                lead="Your products, downloads and updates follow your account."
            />
            <div class="flex max-w-xl flex-col gap-6">
                {move || error.get().map(|e| view! { <ErrorNote error=e /> })}
                <div class="flex items-center gap-4">
                    <Button
                        variant=Variant::Solid
                        icon=icons::LOG_IN
                        loading=waiting
                        on_click=Callback::new(move |()| {
                            sign_in.dispatch(());
                        })
                    >
                        "Continue in browser"
                    </Button>
                    <Show when=move || waiting.get()>
                        <span class="text-sm font-light text-white/55">
                            "Finish signing in in your browser. This page updates on its own."
                        </span>
                    </Show>
                </div>
            </div>
        </Page>
    }
}
