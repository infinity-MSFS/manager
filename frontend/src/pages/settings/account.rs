use infinity_ipc::commands as cmd;
use infinity_ipc::dto::{Channel, SessionState};
use infinity_ui::prelude::*;

use super::{ErrorNote, Field, save_prefs};
use crate::editorial::EYEBROW;
use crate::ipc;
use crate::pages::library::title_case;
use crate::routes;
use crate::state::use_app;

#[component]
pub(super) fn Account() -> impl IntoView {
    let app = use_app();
    let error = RwSignal::new(None);
    let sign_out = Action::new_local(move |(): &()| async move {
        error.set(ipc::invoke::<cmd::AuthLogout>(&()).await.err());
    });
    let owned = move || {
        app.entitlements.with(|e| match e {
            Some(e) if e.products.is_empty() => "None yet".to_owned(),
            Some(e) => e.products.join(", "),
            None => "Unknown".to_owned(),
        })
    };
    move || {
        match app.session.get() {
        SessionState::SignedIn { profile } => view! {
            <div class="max-w-2xl">
                <dl>
                    <Field label="Name" value=profile.name.clone().unwrap_or_default() />
                    <Field label="Email" value=profile.email.clone().unwrap_or_default() />
                    <Field label="Products" value=owned() />
                </dl>
                <div class="mt-4">
                    <Button
                        variant=Variant::Ghost
                        tone=Tone::Danger
                        icon=icons::LOG_OUT
                        loading=sign_out.pending()
                        on_click=Callback::new(move |()| {
                            sign_out.dispatch(());
                        })
                    >
                        "Sign out"
                    </Button>
                </div>
                {move || error.get().map(|e| view! { <div class="mt-4"><ErrorNote error=e /></div> })}
            </div>
        }
        .into_any(),
        _ => view! {
            <div class="flex max-w-2xl items-center justify-between border-t border-white/10 py-4">
                <span class="font-light text-white/65">"Not signed in."</span>
                <Button variant=Variant::Solid icon=icons::LOG_IN href=routes::LOGIN>
                    "Sign in"
                </Button>
            </div>
        }
        .into_any(),
    }
    }
}

const CHANNELS: [(Channel, &str); 3] = [
    (Channel::Stable, "Released versions."),
    (Channel::Beta, "Release candidates, for testers."),
    (Channel::Nightly, "Daily builds. Expect breakage."),
];

/// Channels the build offers, narrowed to the account's rings when the
/// account's entitlements are known.
#[component]
pub(super) fn ChannelPicker() -> impl IntoView {
    let app = use_app();
    let error = RwSignal::new(None);
    let allowed = move |c: Channel| {
        let offered = app.capabilities.with(|caps| caps.channels.contains(&c));
        let granted = c == Channel::Stable
            || app
                .entitlements
                .with(|e| e.as_ref().is_none_or(|e| e.channels.contains(&c)));
        offered && granted
    };
    let current = app.channel();

    view! {
        <div class="max-w-2xl">
            <div class="grid grid-cols-3 border-t border-white/10">
                {CHANNELS
                    .into_iter()
                    .map(|(channel, hint)| {
                        let selected = move || current.get() == channel;
                        view! {
                            <button
                                type="button"
                                disabled=move || !allowed(channel)
                                aria-pressed=move || selected().to_string()
                                class=move || {
                                    cn!(
                                        "relative flex flex-col gap-1 border-b py-4 pr-4 text-left transition-colors disabled:opacity-35",
                                        if selected() { "border-accent-400" } else { "border-white/10 hover:border-white/30" },
                                    )
                                }
                                on:click=move |_| {
                                    save_prefs(app, error, |p| p.channel = channel);
                                }
                            >
                                <span class=move || {
                                    cn!(EYEBROW, if selected() { "text-white" } else { "text-white/50" })
                                }>{title_case(channel)}</span>
                                <span class="text-sm font-light text-white/55">{hint}</span>
                            </button>
                        }
                    })
                    .collect_view()}
            </div>
            {move || error.get().map(|e| view! { <div class="mt-4"><ErrorNote error=e /></div> })}
        </div>
    }
}
