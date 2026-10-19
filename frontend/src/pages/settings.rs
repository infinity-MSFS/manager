use infinity_ipc::commands as cmd;
use infinity_ipc::dto::{MotionPreference, Sim};
use infinity_ui::prelude::*;

use super::{ErrorNote, Page};
use crate::editorial::{EYEBROW, PageHead, SectionLabel};
use crate::ipc;
use crate::state::use_app;

#[component]
pub(crate) fn Settings() -> impl IntoView {
    let app = use_app();
    let yes_no = |b: bool| if b { "Yes" } else { "No" }.to_owned();

    view! {
        <Page>
            <PageHead eyebrow="Infinity / Settings" title="Settings." />
            {(!ipc::in_shell())
                .then(|| {
                    view! {
                        <p class="mb-12 max-w-xl border-l border-accent-400/40 pl-5 font-light text-white/65">
                            "Browser preview: there is no backend outside the desktop shell. Run `cargo xtask dev` for the full app."
                        </p>
                    }
                })}
            <SectionLabel num="01" label="Appearance" />
            <Motion />
            <div class="h-14"></div>
            <SectionLabel num="02" label="About this build" />
            {move || {
                let info = app.info.get();
                let caps = app.capabilities.get();
                let channels = caps.channels.iter().map(|c| c.as_str()).collect::<Vec<_>>().join(", ");
                view! {
                    <dl class="max-w-2xl">
                        <Field label="Version" value=info.as_ref().map(|i| i.version.clone()).unwrap_or_default() />
                        <Field label="Edition" value=info.as_ref().map(|i| format!("{:?}", i.edition)).unwrap_or_default() />
                        <Field label="Platform" value=info.as_ref().map(|i| format!("{} / {}", i.os, i.arch)).unwrap_or_default() />
                        <Field label="Accounts" value=yes_no(caps.accounts) />
                        <Field label="Store" value=yes_no(caps.store) />
                        <Field label="In-sim DRM" value=yes_no(caps.drm) />
                        <Field label="Channels" value=channels />
                    </dl>
                }
            }}
        </Page>
    }
}

#[component]
fn Field(label: &'static str, value: String) -> impl IntoView {
    view! {
        <div class="flex items-baseline justify-between border-t border-white/10 py-3">
            <dt class=cn!(EYEBROW, "text-white/40")>{label}</dt>
            <dd data-selectable class="text-sm font-light text-white/85">{value}</dd>
        </div>
    }
}

const MOTION_OPTIONS: [(MotionPreference, &str, &str); 3] = [
    (
        MotionPreference::Auto,
        "Auto",
        "Full motion, reduced while a simulator is running.",
    ),
    (MotionPreference::Full, "Full", "Always animate."),
    (
        MotionPreference::Reduced,
        "Reduced",
        "No animated backdrop or transitions.",
    ),
];

#[component]
fn Motion() -> impl IntoView {
    let app = use_app();
    let error = RwSignal::new(None);
    let save = Action::new_local(move |motion: &MotionPreference| {
        let mut prefs = app.prefs.get_untracked();
        prefs.motion = *motion;
        async move {
            error.set(ipc::invoke::<cmd::PrefsSave>(&prefs).await.err());
        }
    });

    let current = move || app.prefs.with(|p| p.motion);
    let note = move || {
        let sim = app.sim.get().running?;
        (current() == MotionPreference::Auto).then(|| {
            let name = match sim {
                Sim::Msfs2024 => "MSFS 2024",
                Sim::Msfs2020 => "MSFS 2020",
            };
            format!("Reduced now: {name} is running.")
        })
    };

    view! {
        <div class="max-w-2xl">
            <div class="mb-4 flex items-baseline justify-between">
                <span class="text-lg font-light">"Motion"</span>
                <span class="font-mono text-[11px] text-accent-300">{note}</span>
            </div>
            <div class="grid grid-cols-3 border-t border-white/10">
                {MOTION_OPTIONS
                    .into_iter()
                    .map(|(value, label, hint)| {
                        let selected = move || current() == value;
                        view! {
                            <button
                                type="button"
                                aria-pressed=move || selected().to_string()
                                class=move || {
                                    cn!(
                                        "group relative flex flex-col gap-1 border-b py-4 pr-4 text-left transition-colors",
                                        if selected() { "border-accent-400" } else { "border-white/10 hover:border-white/30" },
                                    )
                                }
                                on:click=move |_| {
                                    save.dispatch(value);
                                }
                            >
                                <span class=move || {
                                    cn!(EYEBROW, if selected() { "text-white" } else { "text-white/50" })
                                }>{label}</span>
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
