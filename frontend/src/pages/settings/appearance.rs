use infinity_ipc::IpcError;
use infinity_ipc::dto::{Preferences, Theme};
use infinity_ui::prelude::*;
use wasm_bindgen::JsCast;

use super::{ErrorNote, save_prefs};
use crate::editorial::EYEBROW;
use crate::state::use_app;
use crate::theme::Rgb;

const THEMES: [(Theme, &str, &str); 3] = [
    (
        Theme::Dark,
        "Dark",
        "The animated gradient, in your accent colour.",
    ),
    (Theme::Amoled, "AMOLED", "True black, nothing animated."),
    (
        Theme::Custom,
        "Custom",
        "A gradient in its own colour, apart from the accent.",
    ),
];

/// Accent presets; the first is the brand emerald (no override).
const ACCENTS: [(&str, Option<&str>); 6] = [
    ("Emerald", None),
    ("Sky", Some("#38bdf8")),
    ("Violet", Some("#8b5cf6")),
    ("Rose", Some("#f43f5e")),
    ("Amber", Some("#f59e0b")),
    ("Orange", Some("#f97316")),
];

const DEFAULT_CUSTOM: &str = "#7c3aed";
const EMERALD: &str = "#34d399";

#[component]
pub(super) fn Appearance() -> impl IntoView {
    let app = use_app();
    let error = RwSignal::new(None::<IpcError>);
    let theme = Signal::derive(move || app.prefs.with(|p| p.theme));
    let choose = move |t: Theme| {
        save_prefs(app, error, |p| {
            p.theme = t;
            if t == Theme::Custom && p.theme_accent.is_none() {
                p.theme_accent = Some(DEFAULT_CUSTOM.to_owned());
            }
        });
    };

    view! {
        <div class="flex max-w-2xl flex-col gap-10">
            <div>
                <span class="mb-4 block text-lg font-light">"Theme"</span>
                <div class="grid grid-cols-3 border-t border-white/10">
                    {THEMES
                        .into_iter()
                        .map(|(value, label, hint)| {
                            let selected = move || theme.get() == value;
                            view! {
                                <button
                                    type="button"
                                    aria-pressed=move || selected().to_string()
                                    class=move || {
                                        cn!(
                                            "flex flex-col gap-1 border-b py-4 pr-4 text-left transition-colors",
                                            if selected() { "border-accent-400" } else { "border-white/10 hover:border-white/30" },
                                        )
                                    }
                                    on:click=move |_| choose(value)
                                >
                                    <span class=move || cn!(EYEBROW, if selected() { "text-white" } else { "text-white/50" })>
                                        {label}
                                    </span>
                                    <span class="text-sm font-light text-white/55">{hint}</span>
                                </button>
                            }
                        })
                        .collect_view()}
                </div>
                <Show when=move || theme.get() == Theme::Custom>
                    <div class="mt-4">
                        <ColorField
                            label="Background colour"
                            value=Signal::derive(move || {
                                app.prefs.with(|p| p.theme_accent.clone().unwrap_or_else(|| DEFAULT_CUSTOM.to_owned()))
                            })
                            set=|p: &mut Preferences, c| p.theme_accent = Some(c)
                            error=error
                        />
                    </div>
                </Show>
            </div>
            <div>
                <span class="mb-4 block text-lg font-light">"Accent"</span>
                <div class="flex flex-wrap items-center gap-3">
                    {ACCENTS
                        .into_iter()
                        .map(|(name, hex)| {
                            let selected = move || app.prefs.with(|p| p.accent_color.as_deref() == hex);
                            let swatch = hex.unwrap_or(EMERALD);
                            view! {
                                <button
                                    type="button"
                                    title=name
                                    aria-label=name
                                    aria-pressed=move || selected().to_string()
                                    class=move || {
                                        cn!(
                                            "h-8 w-8 rounded-full border-2 transition-transform hover:scale-110",
                                            if selected() { "border-white" } else { "border-transparent" },
                                        )
                                    }
                                    style=format!("background: {swatch};")
                                    on:click=move |_| {
                                        save_prefs(app, error, |p| p.accent_color = hex.map(str::to_owned));
                                    }
                                ></button>
                            }
                        })
                        .collect_view()}
                    <ColorField
                        label="Custom"
                        value=Signal::derive(move || {
                            app.prefs.with(|p| p.accent_color.clone().unwrap_or_else(|| EMERALD.to_owned()))
                        })
                        set=|p: &mut Preferences, c| p.accent_color = Some(c)
                        error=error
                    />
                </div>
            </div>
            {move || error.get().map(|e| view! { <ErrorNote error=e /> })}
        </div>
    }
}

/// A native colour picker: previews while dragging, saves on release.
#[component]
fn ColorField(
    label: &'static str,
    #[prop(into)] value: Signal<String>,
    set: fn(&mut Preferences, String),
    error: RwSignal<Option<IpcError>>,
) -> impl IntoView {
    let app = use_app();
    let read = |ev: &leptos::ev::Event| {
        ev.target()
            .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
            .map(|i| i.value())
            .and_then(|v| Rgb::parse(&v).map(Rgb::hex))
    };
    view! {
        <label class="flex items-center gap-3 text-sm font-light text-white/70">
            <input
                type="color"
                class="h-8 w-12 cursor-pointer rounded border border-white/15 bg-transparent"
                prop:value=move || value.get()
                on:input=move |ev| {
                    if let Some(c) = read(&ev) {
                        app.prefs.update(|p| set(p, c));
                    }
                }
                on:change=move |ev| {
                    if let Some(c) = read(&ev) {
                        save_prefs(app, error, |p| set(p, c));
                    }
                }
            />
            {label}
        </label>
    }
}
