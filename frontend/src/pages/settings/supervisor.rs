//! The launch supervisor, which starts the manager when an in-sim aircraft
//! asks for it: install, start, stop, update, remove.

use infinity_ipc::commands as cmd;
use infinity_ipc::dto::{SupervisorMechanism, SupervisorStatus};
use infinity_ipc::{IpcError, IpcResult};
use infinity_ui::prelude::*;

use super::{ErrorNote, Field};
use crate::editorial::EYEBROW;
use crate::ipc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    /// Install and start, under one elevation prompt.
    Install,
    Start,
    Stop,
    Update,
    Uninstall,
}

async fn perform(verb: Verb) -> IpcResult<()> {
    match verb {
        Verb::Install => ipc::invoke::<cmd::SvcInstallAndStart>(&()).await,
        Verb::Start => ipc::invoke::<cmd::SvcStart>(&()).await,
        Verb::Stop => ipc::invoke::<cmd::SvcStop>(&()).await,
        Verb::Update => ipc::invoke::<cmd::SvcUpdate>(&()).await,
        Verb::Uninstall => ipc::invoke::<cmd::SvcUninstall>(&()).await,
    }
}

/// What the supervisor does, in one sentence per mechanism.
pub(crate) fn explain(status: &SupervisorStatus) -> &'static str {
    match status.mechanism {
        SupervisorMechanism::WindowsService => {
            "A small Windows service starts the manager in the background when you load an aircraft \
             that needs it. It holds no credentials. Installing asks for administrator permission."
        }
        SupervisorMechanism::SystemdUser => {
            "Two systemd user units start the manager in the background when you load an aircraft \
             that needs it. No root and no password prompt."
        }
        SupervisorMechanism::None => {
            "This platform has no service manager the launch supervisor supports. Start the manager \
             yourself before you fly and leave it running."
        }
    }
}

/// Status, and the actions it allows. Calls `on_change` after each one.
#[component]
pub(crate) fn SupervisorControls(
    status: SupervisorStatus,
    #[prop(into)] on_change: Callback<()>,
    #[prop(optional)] compact: bool,
) -> impl IntoView {
    let error = RwSignal::new(None::<IpcError>);
    let busy = RwSignal::new(None::<Verb>);
    let act = Action::new_local(move |verb: &Verb| {
        let verb = *verb;
        async move {
            busy.set(Some(verb));
            error.set(perform(verb).await.err());
            busy.set(None);
            on_change.run(());
        }
    });
    let button = move |verb: Verb, label: &'static str, variant: Variant, tone: Tone| {
        let icon = match verb {
            Verb::Install | Verb::Update => icons::SHIELD_CHECK,
            Verb::Start => icons::PLAY,
            Verb::Stop => icons::CIRCLE_ALERT,
            Verb::Uninstall => icons::TRASH_2,
        };
        view! {
            <Button
                variant=variant
                tone=tone
                icon=icon
                loading=Signal::derive(move || busy.get() == Some(verb))
                disabled=Signal::derive(move || busy.get().is_some_and(|b| b != verb))
                on_click=Callback::new(move |()| {
                    act.dispatch(verb);
                })
            >
                {label}
            </Button>
        }
    };

    let s = status;
    if !s.supported {
        return view! { <p class="font-light text-white/65">{explain(&s)}</p> }.into_any();
    }
    let windows = s.mechanism == SupervisorMechanism::WindowsService;
    let yes_no = |b: bool| if b { "Yes" } else { "No" }.to_owned();
    let mut actions = Vec::new();
    if !s.installed {
        actions.push(button(
            Verb::Install,
            "Install",
            Variant::Solid,
            Tone::Accent,
        ));
    } else {
        if s.update_available() {
            actions.push(button(Verb::Update, "Update", Variant::Solid, Tone::Accent));
        }
        if s.running {
            actions.push(button(Verb::Stop, "Stop", Variant::Glass, Tone::Neutral));
        } else {
            actions.push(button(Verb::Start, "Start", Variant::Solid, Tone::Accent));
        }
        actions.push(button(
            Verb::Uninstall,
            "Remove",
            Variant::Ghost,
            Tone::Danger,
        ));
    }
    let version = match s.installed_version.as_deref() {
        Some("stale") => "Points at another copy of the manager".to_owned(),
        Some(v) => v.to_owned(),
        None => "Unknown".to_owned(),
    };

    view! {
        <div class="flex flex-col gap-4">
            {(!compact).then(|| view! { <p class="font-light text-white/65">{explain(&s)}</p> })}
            <dl>
                <Field label="Installed" value=yes_no(s.installed) />
                {s.installed.then(|| view! { <Field label="Running" value=yes_no(s.running) /> })}
                {(s.installed && windows).then(|| view! { <Field label="Version" value=version.clone() /> })}
                {(s.installed && !windows && s.update_available())
                    .then(|| view! { <Field label="Units" value=version.clone() /> })}
            </dl>
            {(windows && !s.bundled)
                .then(|| {
                    view! {
                        <p class="border-l border-white/20 pl-5 text-sm font-light text-white/55">
                            "This build does not include the supervisor service. Release builds made with `cargo xtask dist` do."
                        </p>
                    }
                })}
            {(s.update_available() && windows)
                .then(|| {
                    view! {
                        <p class="text-sm font-light text-accent-300">
                            {format!("Version {} is ready to install. The service restarts on its own.", s.bundled_version)}
                        </p>
                    }
                })}
            <div class="flex flex-wrap gap-2">{actions}</div>
            {move || error.get().map(|e| view! { <ErrorNote error=e /> })}
            {(!compact)
                .then(|| {
                    s.exe_path
                        .clone()
                        .map(|p| view! { <p data-selectable class=cn!(EYEBROW, "normal-case tracking-normal text-white/35")>{p}</p> })
                })}
        </div>
    }
    .into_any()
}

/// The Settings section.
#[component]
pub(super) fn Supervisor() -> impl IntoView {
    let status = LocalResource::new(|| ipc::invoke::<cmd::SvcStatus>(&()));
    let refresh = Callback::new(move |()| status.refetch());
    view! {
        <div class="max-w-2xl">
            {move || match status.get() {
                None => view! { <Skeleton class="h-24 w-full" /> }.into_any(),
                Some(Err(e)) => view! { <ErrorNote error=e /> }.into_any(),
                Some(Ok(s)) => view! { <SupervisorControls status=s on_change=refresh /> }.into_any(),
            }}
        </div>
    }
}
