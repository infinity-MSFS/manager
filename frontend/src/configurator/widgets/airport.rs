//! airportPick (v3 `AirportPickWidget`): an ICAO checked against the
//! product's navigation database, with suggestions while typing. Blank is
//! allowed and means unset.

use infinity_ipc::commands as cmd;
use infinity_ipc::nav::{NavAirport, NavSearch};
use infinity_ui::prelude::*;
use serde_json::Value;

use super::WidgetProps;
use super::common::{SuggestionItem, Suggestions, WidgetLabel, debounce};
use crate::configurator::schema::display_scalar;
use crate::ipc;

const DEBOUNCE_MS: u32 = 300;

pub(super) fn airport_widget(props: WidgetProps) -> AnyView {
    let value = props.value;
    let on_change = props.on_change;
    let cfg = props.ctx.cfg;
    let current = Memo::new(move |_| value.with(display_scalar));
    let draft = RwSignal::new(current.get_untracked());
    let valid = RwSignal::new(None::<bool>);
    let suggestions = RwSignal::new(Vec::<String>::new());
    let open = RwSignal::new(false);

    // Outside changes (share-load, reset) replace the draft.
    Effect::new(move |_| draft.set(current.get()));

    debounce(
        move || draft.get().trim().to_uppercase(),
        DEBOUNCE_MS,
        move |icao: String| {
            if icao.is_empty() {
                valid.set(None);
                return;
            }
            leptos::task::spawn_local(async move {
                let args = NavAirport {
                    product: cfg.product_ref(),
                    icao: icao.clone(),
                };
                let verdict = ipc::invoke::<cmd::NavIsValidIcao>(&args).await.ok();
                if draft.try_with_untracked(|d| d.trim().to_uppercase() == icao) == Some(true) {
                    valid.set(verdict);
                }
            });
        },
    );
    debounce(
        move || draft.get(),
        DEBOUNCE_MS,
        move |query: String| {
            if query.is_empty() {
                suggestions.set(Vec::new());
                return;
            }
            leptos::task::spawn_local(async move {
                let args = NavSearch {
                    product: cfg.product_ref(),
                    query,
                };
                let rows = ipc::invoke::<cmd::NavAirportsByName>(&args)
                    .await
                    .unwrap_or_default();
                let _ = suggestions.try_set(rows.into_iter().take(10).collect());
            });
        },
    );

    let commit = move |next: String| {
        let clean = next.trim().to_uppercase();
        draft.set(clean.clone());
        on_change.run(Value::String(clean));
        open.set(false);
    };
    let dot = move || {
        let d = draft.get();
        if d.trim().is_empty() {
            "bg-white/30"
        } else {
            match valid.get() {
                None => "bg-white/40",
                Some(true) => "bg-accent-400",
                Some(false) => "bg-red-400",
            }
        }
    };

    view! {
        <div class="flex flex-col gap-1.5">
            <WidgetLabel label=props.label() />
            <div class="flex items-center gap-3">
                <span aria-hidden="true" class=move || cn!("h-2 w-2 shrink-0 rounded-full", dot())></span>
                <div
                    class="relative w-32"
                    on:focusin=move |_| open.set(true)
                    on:focusout=move |_| {
                        open.set(false);
                        let d = draft.get_untracked();
                        if d != current.get_untracked() {
                            on_change.run(Value::String(d.trim().to_uppercase()));
                        }
                    }
                    on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                        if ev.key() == "Enter" {
                            ev.prevent_default();
                            commit(draft.get_untracked());
                        }
                    }
                >
                    <TextInput
                        value=draft
                        placeholder="ICAO"
                        class="uppercase"
                        on_input=Callback::new(move |s: String| {
                            draft.set(s.chars().take(4).collect());
                            open.set(true);
                        })
                    />
                    <Suggestions open=Signal::derive(move || open.get() && suggestions.with(|s| !s.is_empty()))>
                        {move || {
                            suggestions
                                .get()
                                .into_iter()
                                .map(|icao| {
                                    let pick = icao.clone();
                                    view! {
                                        <SuggestionItem on_pick=Callback::new(move |()| commit(pick.clone()))>
                                            <span class="font-mono">{icao}</span>
                                        </SuggestionItem>
                                    }
                                })
                                .collect_view()
                        }}
                    </Suggestions>
                </div>
            </div>
        </div>
    }
    .into_any()
}
