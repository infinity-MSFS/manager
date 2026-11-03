//! First-run setup: where products install, the launch supervisor, and
//! signing in. Finishing or skipping sets `firstRunComplete` (v3's key), so
//! it shows once. v1 settings are imported automatically at startup; the
//! welcome step says so when it happened.

use infinity_ipc::commands as cmd;
use infinity_ipc::dto::{PickFolder, Preferences, SessionState, SupervisorStatus};
use infinity_ipc::{IpcError, IpcResult};
use infinity_ui::prelude::*;
use leptos_router::hooks::{use_location, use_navigate};

use super::ErrorNote;
use super::settings::supervisor::{SupervisorControls, explain};
use crate::editorial::{EYEBROW, Page, PageHead};
use crate::ipc;
use crate::routes;
use crate::state::{AppState, use_app};

/// Sends a fresh install to the wizard once, at startup.
#[component]
pub(crate) fn FirstRunGate() -> impl IntoView {
    let navigate = use_navigate();
    let location = use_location();
    if ipc::in_shell() {
        leptos::task::spawn_local(async move {
            // Read from disk rather than app state, which starts out as
            // defaults until its own load lands.
            let Ok(prefs) = ipc::invoke::<cmd::PrefsLoad>(&()).await else {
                return;
            };
            if !prefs.first_run_complete && location.pathname.get_untracked() != routes::WELCOME {
                navigate(routes::WELCOME, Default::default());
            }
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Welcome,
    Folder,
    Supervisor,
    Account,
}

impl Step {
    const fn label(self) -> &'static str {
        match self {
            Self::Welcome => "Welcome",
            Self::Folder => "Install folder",
            Self::Supervisor => "Launch supervisor",
            Self::Account => "Account",
        }
    }
}

async fn save(app: AppState, edit: impl FnOnce(&mut Preferences)) -> IpcResult<()> {
    let mut prefs = ipc::invoke::<cmd::PrefsLoad>(&())
        .await
        .unwrap_or_else(|_| app.prefs.get_untracked());
    edit(&mut prefs);
    ipc::invoke::<cmd::PrefsSave>(&prefs).await
}

#[component]
pub(crate) fn FirstRun() -> impl IntoView {
    let app = use_app();
    let navigate = use_navigate();
    let error = RwSignal::new(None::<IpcError>);
    let supervisor = LocalResource::new(|| ipc::invoke::<cmd::SvcStatus>(&()));
    let steps = Signal::derive(move || {
        let mut steps = vec![Step::Welcome, Step::Folder];
        if matches!(supervisor.get(), Some(Ok(ref s)) if s.supported) {
            steps.push(Step::Supervisor);
        }
        if app.capabilities.get().accounts {
            steps.push(Step::Account);
        }
        steps
    });
    let step = RwSignal::new(Step::Welcome);
    let position = move || {
        let current = step.get();
        steps.with(|s| s.iter().position(|x| *x == current).unwrap_or(0))
    };

    // `then` runs after the preference is saved: back to the library, or
    // on to sign in.
    let finish = Action::new_local(move |then: &&'static str| {
        let then = *then;
        let navigate = navigate.clone();
        async move {
            match save(app, |p| p.first_run_complete = true).await {
                Ok(()) => navigate(then, Default::default()),
                Err(e) => error.set(Some(e)),
            }
        }
    });
    // Signed in some other way meanwhile: nothing left to ask.
    Effect::new(move |_| {
        if matches!(app.session.get(), SessionState::SignedIn { .. }) && step.get() == Step::Account
        {
            finish.dispatch(routes::LIBRARY);
        }
    });

    let next = move || {
        let i = position();
        match steps.with(|s| s.get(i + 1).copied()) {
            Some(n) => step.set(n),
            None => {
                finish.dispatch(routes::LIBRARY);
            }
        }
    };
    let back = move || {
        let i = position();
        if let Some(p) = i
            .checked_sub(1)
            .and_then(|i| steps.with(|s| s.get(i).copied()))
        {
            step.set(p);
        }
    };

    let progress = move || {
        let current = step.get();
        steps
            .get()
            .into_iter()
            .enumerate()
            .map(|(i, s)| {
                view! {
                    <span class=cn!(EYEBROW, if s == current { "text-accent-300" } else { "text-white/35" })>
                        {format!("{:02} {}", i + 1, s.label())}
                    </span>
                }
            })
            .collect_view()
    };

    let body = move || {
        match step.get() {
        Step::Welcome => view! { <Welcome /> }.into_any(),
        Step::Folder => view! { <Folder /> }.into_any(),
        Step::Supervisor => match supervisor.get() {
            Some(Ok(s)) => view! { <SupervisorStep status=s on_change=Callback::new(move |()| supervisor.refetch()) /> }.into_any(),
            _ => view! { <Skeleton class="h-24 w-full" /> }.into_any(),
        },
        Step::Account => view! { <Account /> }.into_any(),
    }
    };
    let last = move || position() + 1 >= steps.with(Vec::len);

    view! {
        <Page>
            <PageHead
                eyebrow="Infinity / Welcome"
                title="Let's get you"
                accent="set up."
                lead="A few questions, once. You can change every answer later in Settings."
            />
            <div class="flex max-w-2xl flex-col gap-10">
                <div class="flex flex-wrap gap-6 border-b border-white/10 pb-4">{progress}</div>
                {body}
                {move || error.get().map(|e| view! { <ErrorNote error=e /> })}
                <div class="flex items-center justify-between border-t border-white/10 pt-6">
                    <Button
                        variant=Variant::Ghost
                        icon=icons::ARROW_LEFT
                        disabled=Signal::derive(move || position() == 0)
                        on_click=Callback::new(move |()| back())
                    >
                        "Back"
                    </Button>
                    <div class="flex gap-2">
                        <Button
                            variant=Variant::Ghost
                            on_click=Callback::new(move |()| {
                                finish.dispatch(routes::LIBRARY);
                            })
                        >
                            "Skip setup"
                        </Button>
                        <Show when=move || step.get() == Step::Account>
                            <Button
                                variant=Variant::Solid
                                icon=icons::LOG_IN
                                loading=finish.pending()
                                on_click=Callback::new(move |()| {
                                    finish.dispatch(routes::LOGIN);
                                })
                            >
                                "Sign in"
                            </Button>
                        </Show>
                        <Button
                            variant=Variant::Solid
                            trailing_icon=icons::ARROW_RIGHT
                            loading=finish.pending()
                            on_click=Callback::new(move |()| next())
                        >
                            {move || {
                                if step.get() == Step::Account {
                                    "Not now"
                                } else if last() {
                                    "Finish"
                                } else {
                                    "Next"
                                }
                            }}
                        </Button>
                    </div>
                </div>
            </div>
        </Page>
    }
}

#[component]
fn Welcome() -> impl IntoView {
    let imported = LocalResource::new(|| ipc::invoke::<cmd::ConfigMigrateV1>(&()));
    let note = move || {
        let report = imported.get()?.ok()??;
        let text = if !report.migrated_configs.is_empty() {
            format!(
                "Your settings from Infinity Manager v1 were imported ({}). The old file is kept beside it as config.json.v2.bak.",
                report.migrated_configs.join(", ")
            )
        } else if !report.errors.is_empty() {
            format!(
                "Infinity Manager v1 settings were found but could not be imported: {}",
                report.errors.join("; ")
            )
        } else {
            return None;
        };
        Some(view! {
            <p class="border-l border-accent-400/40 pl-5 text-sm font-light text-white/70">{text}</p>
        })
    };
    view! {
        <div class="flex flex-col gap-4">
            <p class="font-light text-white/75">
                "Next: confirm where products install, let aircraft start the manager on their own, and sign in if you own any paid products. Free products work without an account."
            </p>
            {note}
        </div>
    }
}

#[component]
fn Folder() -> impl IntoView {
    let app = use_app();
    let error = RwSignal::new(None::<IpcError>);
    let paths = LocalResource::new(move || {
        app.prefs.track();
        ipc::invoke::<cmd::SimPaths>(&())
    });
    let browse = Action::new_local(move |(): &()| async move {
        let start = app.prefs.with_untracked(|p| p.install_dir.clone());
        let request = PickFolder {
            title: "Community folder".to_owned(),
            start,
        };
        match ipc::invoke::<cmd::DialogPickFolder>(&request).await {
            Ok(Some(dir)) => error.set(save(app, |p| p.install_dir = Some(dir)).await.err()),
            Ok(None) => {}
            Err(e) => error.set(Some(e)),
        }
    });
    let reset = Action::new_local(move |(): &()| async move {
        error.set(save(app, |p| p.install_dir = None).await.err());
    });

    let current = move || {
        let Some(Ok(paths)) = paths.get() else {
            return view! { <Skeleton class="h-5 w-96" /> }.into_any();
        };
        match paths.install_dir {
            None => view! {
                <p class="font-light text-white/65">
                    "No simulator found. Choose your sim's Community folder."
                </p>
            }
            .into_any(),
            Some(dir) => view! {
                <div class="flex flex-col gap-1">
                    <span class=cn!(EYEBROW, "text-white/40")>
                        {if dir.chosen { "Products install into" } else { "Detected. Products install into" }}
                    </span>
                    <span data-selectable class="font-light break-all text-white/85">{dir.path}</span>
                    {(!dir.exists)
                        .then(|| view! { <span class="text-sm text-red-300/80">"This folder does not exist."</span> })}
                </div>
            }
            .into_any(),
        }
    };
    let chosen = Signal::derive(move || {
        app.prefs.with(|p| {
            p.install_dir
                .as_deref()
                .is_some_and(|d| !d.trim().is_empty())
        })
    });

    view! {
        <div class="flex flex-col gap-6">
            <p class="font-light text-white/75">
                "Products install into your simulator's Community folder. It is usually found on its own."
            </p>
            {current}
            <div class="flex gap-2">
                <Button variant=Variant::Glass icon=icons::HARD_DRIVE loading=browse.pending() on_click=Callback::new(move |()| {
                    browse.dispatch(());
                })>
                    "Choose folder"
                </Button>
                <Show when=move || chosen.get()>
                    <Button variant=Variant::Ghost loading=reset.pending() on_click=Callback::new(move |()| {
                        reset.dispatch(());
                    })>
                        "Use detected"
                    </Button>
                </Show>
            </div>
            {move || error.get().map(|e| view! { <ErrorNote error=e /> })}
        </div>
    }
}

#[component]
fn SupervisorStep(status: SupervisorStatus, on_change: Callback<()>) -> impl IntoView {
    view! {
        <div class="flex flex-col gap-6">
            <p class="font-light text-white/75">
                "Aircraft talk to the manager from inside the sim. With the launch supervisor, loading one starts the manager in the background; without it, start the manager yourself before every flight."
            </p>
            <p class="text-sm font-light text-white/55">{explain(&status)}</p>
            <SupervisorControls status=status on_change=on_change compact=true />
        </div>
    }
}

#[component]
fn Account() -> impl IntoView {
    view! {
        <p class="font-light text-white/75">
            "Sign in to download products you own. Free products work without an account, and you can sign in later from the top bar."
        </p>
    }
}
