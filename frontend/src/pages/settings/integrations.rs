//! Navigraph, SimBrief and the EFB tablet server.

use infinity_ipc::commands as cmd;
use infinity_ipc::dto::{EfbStatus, JobPhase, ProductRef};
use infinity_ipc::events::EfbChanged;
use infinity_ipc::navigraph::{LinkPoll, LinkStart, NavdataItem};
use infinity_ipc::{IpcError, IpcErrorCode};
use infinity_ui::prelude::*;

use super::{ErrorNote, Field, save_prefs};
use crate::editorial::EYEBROW;
use crate::ipc;
use crate::state::use_app;

#[component]
pub(super) fn Navigraph() -> impl IntoView {
    let error = RwSignal::new(None::<IpcError>);
    let status = LocalResource::new(|| ipc::invoke::<cmd::NavigraphStatus>(&()));
    let linking = RwSignal::new(None::<LinkStart>);

    let link = Action::new_local(move |(): &()| async move {
        let start = match ipc::invoke::<cmd::NavigraphLinkStart>(&()).await {
            Ok(s) => s,
            Err(e) => return error.set(Some(e)),
        };
        error.set(None);
        let mut interval = start.interval.max(1);
        let tries = start.expires_in / interval + 1;
        linking.set(Some(start));
        for _ in 0..tries {
            ipc::sleep(interval * 1000).await;
            match ipc::invoke::<cmd::NavigraphLinkPoll>(&()).await {
                Ok(LinkPoll::Pending) => {}
                Ok(LinkPoll::SlowDown) => interval += 5,
                Ok(LinkPoll::Linked) => break,
                Ok(other) => {
                    error.set(Some(IpcError::new(
                        IpcErrorCode::AuthForbidden,
                        format!("Navigraph sign-in {}", other.as_str().replace('_', " ")),
                    )));
                    break;
                }
                Err(e) => {
                    error.set(Some(e));
                    break;
                }
            }
        }
        linking.set(None);
        status.refetch();
    });
    let unlink = Action::new_local(move |(): &()| async move {
        error.set(ipc::invoke::<cmd::NavigraphUnlink>(&()).await.err());
        status.refetch();
    });

    let summary = move || {
        let Some(Ok(s)) = status.get() else {
            return view! { <Skeleton class="h-5 w-64" /> }.into_any();
        };
        let text = if s.relink_required {
            "Sign in again: Navigraph ended the session."
        } else if s.linked {
            "Linked. Charts and navdata updates are available."
        } else {
            "Not linked."
        };
        let linked = s.linked && !s.relink_required;
        view! {
            <div class="flex items-center justify-between gap-4 border-t border-white/10 py-4">
                <span class="font-light text-white/75">{text}</span>
                {if linked {
                    view! {
                        <Button variant=Variant::Ghost tone=Tone::Danger loading=unlink.pending() on_click=Callback::new(move |()| {
                            unlink.dispatch(());
                        })>
                            "Unlink"
                        </Button>
                    }
                        .into_any()
                } else {
                    view! {
                        <Button variant=Variant::Solid icon=icons::LINK_2 loading=link.pending() on_click=Callback::new(move |()| {
                            link.dispatch(());
                        })>
                            "Link Navigraph"
                        </Button>
                    }
                        .into_any()
                }}
            </div>
        }
        .into_any()
    };

    view! {
        <div class="flex max-w-2xl flex-col gap-4">
            {summary}
            {move || {
                linking
                    .get()
                    .map(|l| {
                        view! {
                            <p class="border-l border-accent-400/40 pl-5 text-sm font-light text-white/70">
                                "Finish in your browser. If asked, enter "
                                <span data-selectable class="font-mono text-accent-300">{l.user_code}</span>
                                "."
                            </p>
                        }
                    })
            }}
            {move || error.get().map(|e| view! { <ErrorNote error=e /> })}
            <Show when=move || matches!(status.get(), Some(Ok(s)) if s.linked)>
                <Navdata />
            </Show>
        </div>
    }
}

/// Installed and latest AIRAC cycles per aircraft, with updates.
#[component]
fn Navdata() -> impl IntoView {
    let app = use_app();
    let channel = app.channel();
    let report = LocalResource::new(move || {
        let channel = channel.get();
        async move { ipc::invoke::<cmd::NavdataStatus>(&channel).await }
    });
    // A finished update drops out of the job list: look again.
    let running = Memo::new(move |_| {
        app.jobs
            .with(|j| j.keys().filter(|k| k.starts_with("navdata-")).count())
    });
    Effect::new(move |previous: Option<usize>| {
        let now = running.get();
        if previous.is_some_and(|p| p > now) {
            report.refetch();
        }
        now
    });

    move || match report.get() {
        None => view! { <Skeleton class="h-16 w-full" /> }.into_any(),
        Some(Err(e)) => view! { <ErrorNote error=e /> }.into_any(),
        Some(Ok(r)) => view! {
            <div>
                <p class=cn!("mb-2", EYEBROW)>"Navdata"</p>
                {r.fetch_error.map(|e| view! { <p class="text-sm text-red-300/80">{e}</p> })}
                {r.items
                    .into_iter()
                    .filter(|i| i.installed)
                    .map(|item| view! { <NavdataRow item=item /> })
                    .collect_view()}
            </div>
        }
        .into_any(),
    }
}

#[component]
fn NavdataRow(item: NavdataItem) -> impl IntoView {
    let app = use_app();
    let job = app.job(item.job_id.clone());
    let error = RwSignal::new(None::<IpcError>);
    let at = ProductRef {
        product_id: item.product_id.clone(),
        channel: app.channel().get_untracked(),
    };
    let update = Action::new_local(move |(): &()| {
        let at = at.clone();
        async move { error.set(ipc::invoke::<cmd::NavdataUpdate>(&at).await.err()) }
    });
    let current = item
        .current
        .as_ref()
        .map_or_else(|| "none".to_owned(), |c| format!("AIRAC {}", c.cycle));
    let latest = item.latest.as_ref().map(|l| format!("AIRAC {}", l.cycle));
    let can_update = item.update_available;

    view! {
        <div class="flex flex-col gap-2 border-t border-white/10 py-3">
            <div class="flex items-center justify-between gap-4">
                <div>
                    <p class="text-sm text-white/85">{item.product_name.clone()}</p>
                    <p class="text-xs font-light text-white/50">
                        {format!("Installed: {current}")}
                        {latest.map(|l| format!(" · Latest: {l}"))}
                        {item.note.clone().map(|n| format!(" · {n}"))}
                    </p>
                </div>
                {move || match job.get() {
                    Some(j) if j.is_active() => {
                        let pct = j.progress.and_then(|p| p.fraction()).unwrap_or(0.0) * 100.0;
                        view! { <div class="w-40"><Progress value=Signal::derive(move || pct) /></div> }.into_any()
                    }
                    _ => view! {
                        <Button
                            variant=if can_update { Variant::Solid } else { Variant::Glass }
                            size=Size::Sm
                            icon=icons::DOWNLOAD
                            loading=update.pending()
                            on_click=Callback::new(move |()| {
                                update.dispatch(());
                            })
                        >
                            {if can_update { "Update" } else { "Reinstall" }}
                        </Button>
                    }
                    .into_any(),
                }}
            </div>
            {move || match job.get().map(|j| j.phase) {
                Some(JobPhase::Failed { error }) => Some(view! { <ErrorNote error=error /> }),
                _ => None,
            }}
            {move || error.get().map(|e| view! { <ErrorNote error=e /> })}
        </div>
    }
}

#[component]
pub(super) fn SimBrief() -> impl IntoView {
    let app = use_app();
    let draft = RwSignal::new(String::new());
    Effect::new(move |_| {
        draft.set(
            app.prefs
                .with(|p| p.simbrief_pilot_id.clone().unwrap_or_default()),
        )
    });
    let error = RwSignal::new(None::<IpcError>);
    let result = RwSignal::new(None::<String>);
    let save = Action::new_local(move |(): &()| async move {
        let id = Some(draft.get_untracked().trim().to_owned()).filter(|s| !s.is_empty());
        error.set(ipc::invoke::<cmd::SimBriefSetPilotId>(&id).await.err());
    });
    let fetch = Action::new_local(move |(): &()| async move {
        match ipc::invoke::<cmd::SimBriefFetchPlan>(&()).await {
            Ok(plan) => {
                error.set(None);
                result.set(Some(format!(
                    "{} → {} · {} waypoints",
                    plan.origin,
                    plan.destination,
                    plan.waypoints.len()
                )));
            }
            Err(e) => error.set(Some(e)),
        }
    });

    view! {
        <div class="flex max-w-2xl flex-col gap-3">
            <span class="text-sm font-light text-white/55">
                "Your SimBrief pilot ID (Account → Settings on simbrief.com). Aircraft and the EFB import your latest flight plan with it."
            </span>
            <div class="flex gap-2">
                <div class="min-w-0 flex-1">
                    <TextInput value=draft on_input=Callback::new(move |v| draft.set(v)) placeholder="123456" icon=icons::PLANE />
                </div>
                <Button variant=Variant::Soft loading=save.pending() on_click=Callback::new(move |()| {
                    save.dispatch(());
                })>
                    "Save"
                </Button>
                <Button variant=Variant::Glass loading=fetch.pending() on_click=Callback::new(move |()| {
                    fetch.dispatch(());
                })>
                    "Fetch latest"
                </Button>
            </div>
            {move || result.get().map(|r| view! { <p class="text-sm text-accent-300">{r}</p> })}
            {move || error.get().map(|e| view! { <ErrorNote error=e /> })}
        </div>
    }
}

#[component]
pub(super) fn Efb() -> impl IntoView {
    let app = use_app();
    let status = RwSignal::new(None::<EfbStatus>);
    let refresh = move || {
        leptos::task::spawn_local(async move {
            if let Ok(s) = ipc::invoke::<cmd::EfbStatusGet>(&()).await {
                status.set(Some(s));
            }
        });
    };
    refresh();
    ipc::listen::<EfbChanged>(move |s| status.set(Some(s)));
    let urls = LocalResource::new(move || {
        status.track();
        ipc::invoke::<cmd::EfbLanUrls>(&())
    });
    let error = RwSignal::new(None::<IpcError>);
    let device = RwSignal::new(String::new());
    let code = RwSignal::new(None::<String>);
    let pair = Action::new_local(move |(): &()| async move {
        match ipc::invoke::<cmd::EfbPairingCode>(&device.get_untracked()).await {
            Ok(c) => {
                error.set(None);
                code.set(Some(c));
            }
            Err(e) => error.set(Some(e)),
        }
    });
    let confirm_forget = RwSignal::new(false);
    let sharing = Signal::derive(move || app.prefs.with(|p| p.efb_lan_share));

    view! {
        <div class="flex max-w-2xl flex-col gap-6">
            <div class="flex items-center justify-between gap-4">
                <div>
                    <p class="text-lg font-light">"Share with tablets"</p>
                    <p class="text-sm font-light text-white/55">
                        "Serve the EFB on your network so a tablet or phone can mirror it. Off keeps it to the sim."
                    </p>
                </div>
                <Switch
                    checked=sharing
                    on_change=Callback::new(move |on: bool| save_prefs(app, error, |p| p.efb_lan_share = on))
                />
            </div>
            {move || {
                let s = status.get()?;
                let state = match (&s.bind_addr, s.running) {
                    (Some(addr), true) => format!("Running on {addr}"),
                    _ => "Stopped".to_owned(),
                };
                Some(view! { <dl><Field label="Server" value=state /></dl> })
            }}
            <Show when=move || sharing.get()>
                <div class="flex flex-col gap-2">
                    <p class=EYEBROW>"Open on the tablet"</p>
                    {move || {
                        urls.get()
                            .and_then(Result::ok)
                            .map(|u| {
                                u.into_iter()
                                    .map(|u| view! { <p data-selectable class="font-mono text-sm text-accent-300">{u}</p> })
                                    .collect_view()
                            })
                    }}
                </div>
                <div class="flex flex-col gap-2">
                    <p class=EYEBROW>"Pair a tablet"</p>
                    <div class="flex gap-2">
                        <div class="min-w-0 flex-1">
                            <TextInput value=device on_input=Callback::new(move |v| device.set(v)) placeholder="Tablet name" icon=icons::MONITOR />
                        </div>
                        <Button variant=Variant::Soft loading=pair.pending() on_click=Callback::new(move |()| {
                            pair.dispatch(());
                        })>
                            "Get code"
                        </Button>
                    </div>
                    {move || {
                        code.get()
                            .map(|c| {
                                view! {
                                    <p class="text-sm font-light text-white/70">
                                        "Enter "
                                        <span data-selectable class="font-mono text-lg text-accent-300">{c}</span>
                                        " on the tablet within 10 minutes."
                                    </p>
                                }
                            })
                    }}
                </div>
            </Show>
            {move || {
                let clients = status.get()?.clients;
                (!clients.is_empty())
                    .then(|| {
                        view! {
                            <div>
                                <p class=cn!("mb-2", EYEBROW)>"Connected"</p>
                                {clients
                                    .into_iter()
                                    .map(|c| {
                                        let id = c.client_id;
                                        view! {
                                            <div class="flex items-center justify-between border-t border-white/10 py-2">
                                                <span class="text-sm text-white/80">
                                                    {format!("{} · {}", c.device, if c.loopback { "in sim" } else { c.addr.as_str() })}
                                                </span>
                                                <Button variant=Variant::Ghost size=Size::Sm icon=icons::X on_click=Callback::new(move |()| {
                                                    leptos::task::spawn_local(async move {
                                                        let _ = ipc::invoke::<cmd::EfbDisconnect>(&id).await;
                                                    });
                                                })>
                                                    "Disconnect"
                                                </Button>
                                            </div>
                                        }
                                    })
                                    .collect_view()}
                            </div>
                        }
                    })
            }}
            <div class="flex gap-2">
                <Button variant=Variant::Glass icon=icons::FILE_TEXT on_click=Callback::new(move |()| {
                    leptos::task::spawn_local(async move {
                        error.set(ipc::invoke::<cmd::EfbOpenDocsFolder>(&()).await.err());
                    });
                })>
                    "My documents folder"
                </Button>
                <Button variant=Variant::Ghost tone=Tone::Danger icon=icons::TRASH_2 on_click=Callback::new(move |()| confirm_forget.set(true))>
                    "Unpair all tablets"
                </Button>
            </div>
            {move || error.get().map(|e| view! { <ErrorNote error=e /> })}
            <ConfirmDialog
                open=confirm_forget
                on_close=Callback::new(move |()| confirm_forget.set(false))
                on_confirm=Callback::new(move |()| {
                    confirm_forget.set(false);
                    leptos::task::spawn_local(async move {
                        error.set(ipc::invoke::<cmd::EfbForgetDevices>(&()).await.err());
                        refresh();
                    });
                })
                title="Unpair all tablets?"
                message="Every paired tablet must pair again with a new code."
                confirm_label="Unpair all"
            />
        </div>
    }
}
