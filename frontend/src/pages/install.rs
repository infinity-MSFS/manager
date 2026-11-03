use infinity_ipc::IpcError;
use infinity_ipc::commands as cmd;
use infinity_ipc::dto::{Channel, InstallRequest, InstalledPackage, Job, JobPhase, Progress};
use infinity_ui::prelude::*;

use super::ErrorNote;
use crate::editorial::EYEBROW;
use crate::ipc;
use crate::state::use_app;

/// What an installable row offers, driven by its job and installed record.
/// `id` is a product id or a [`infinity_ipc::dto::livery_job_id`].
#[component]
pub(crate) fn InstallControls(
    #[prop(into)] id: String,
    #[prop(into)] name: String,
    /// The version an install would get.
    #[prop(into)]
    latest: String,
    channel: Channel,
    #[prop(optional)] coming_soon: bool,
) -> impl IntoView {
    let app = use_app();
    let job = app.job(id.clone());
    let installed = app.installed_package(id.clone());
    let id = StoredValue::new(id);
    let error = RwSignal::new(None::<IpcError>);
    let confirm_uninstall = RwSignal::new(false);

    let start = Action::new_local(move |(): &()| {
        let request = InstallRequest {
            product_id: id.get_value(),
            channel,
        };
        async move {
            error.set(ipc::invoke::<cmd::InstallStart>(&request).await.err());
        }
    });
    let pause = Action::new_local(move |(): &()| async move {
        error.set(
            ipc::invoke::<cmd::InstallPause>(&id.get_value())
                .await
                .err(),
        );
    });
    let cancel = Action::new_local(move |(): &()| async move {
        error.set(
            ipc::invoke::<cmd::InstallCancel>(&id.get_value())
                .await
                .err(),
        );
    });
    let uninstall = Action::new_local(move |(): &()| async move {
        error.set(
            ipc::invoke::<cmd::InstalledUninstall>(&id.get_value())
                .await
                .err(),
        );
    });
    let on = |action: Action<(), ()>| {
        Callback::new(move |()| {
            action.dispatch(());
        })
    };

    let body = move || {
        let latest = latest.clone();
        match (job.get(), installed.get()) {
            (Some(job), _) if job.is_active() => view! {
                <Running job=job on_pause=on(pause) on_cancel=on(cancel) />
            }
            .into_any(),
            (Some(job), _) => view! {
                <Stopped job=job on_resume=on(start) on_discard=on(cancel) />
            }
            .into_any(),
            (None, Some(package)) => view! {
                <Installed
                    package=package
                    latest=latest
                    busy=start.pending()
                    on_update=on(start)
                    on_uninstall=Callback::new(move |()| confirm_uninstall.set(true))
                />
            }
            .into_any(),
            (None, None) => view! {
                <div class="self-start">
                <Button
                    variant=Variant::Solid
                    icon=icons::DOWNLOAD
                    loading=start.pending()
                    disabled=coming_soon
                    on_click=on(start)
                >
                    {if coming_soon { "Coming soon" } else { "Install" }}
                </Button>
                </div>
            }
            .into_any(),
        }
    };

    view! {
        <div class="mt-6 flex flex-col gap-4">
            {body}
            {move || error.get().map(|e| view! { <ErrorNote error=e /> })}
            <ConfirmDialog
                open=confirm_uninstall
                on_close=Callback::new(move |()| confirm_uninstall.set(false))
                on_confirm=Callback::new(move |()| {
                    confirm_uninstall.set(false);
                    uninstall.dispatch(());
                })
                title=format!("Uninstall {name}?")
                message="The package folder is deleted from your install folder."
                confirm_label="Uninstall"
            />
        </div>
    }
}

#[component]
fn Running(job: Job, on_pause: Callback<()>, on_cancel: Callback<()>) -> impl IntoView {
    let label = match job.phase {
        JobPhase::Queued => "Waiting",
        JobPhase::Downloading => "Downloading",
        JobPhase::Verifying => "Verifying download",
        JobPhase::Extracting => "Unpacking and checking files",
        _ => "Installing",
    };
    let fraction = job.progress.and_then(Progress::fraction).unwrap_or(0.0);
    let pausable = matches!(
        job.phase,
        JobPhase::Queued | JobPhase::Downloading | JobPhase::Verifying | JobPhase::Extracting
    );
    view! {
        <div class="max-w-xl">
            <div class="mb-2 flex items-baseline justify-between font-mono text-[11px]">
                <span class=cn!(EYEBROW, "text-white/70")>{format!("{label} · v{}", job.version)}</span>
                <span class="text-accent-300">{job.progress.map(amount)}</span>
            </div>
            <Progress value=Signal::derive(move || fraction * 100.0) />
            <div class="mt-3 flex gap-2">
                <Button variant=Variant::Glass size=Size::Sm icon=icons::PAUSE disabled=!pausable on_click=on_pause>
                    "Pause"
                </Button>
                <Button variant=Variant::Ghost size=Size::Sm icon=icons::X disabled=!pausable on_click=on_cancel>
                    "Cancel"
                </Button>
            </div>
        </div>
    }
}

#[component]
fn Stopped(job: Job, on_resume: Callback<()>, on_discard: Callback<()>) -> impl IntoView {
    let (status, resume) = match &job.phase {
        JobPhase::Failed { .. } => ("Install failed", "Try again"),
        _ => ("Paused", "Resume"),
    };
    let error = match job.phase {
        JobPhase::Failed { error } => Some(error),
        _ => None,
    };
    view! {
        <div class="flex max-w-xl flex-col gap-3">
            <p class=cn!(EYEBROW, "text-white/70")>{format!("{status} · v{}", job.version)}</p>
            {error.map(|e| view! { <ErrorNote error=e /> })}
            <div class="flex gap-2">
                <Button variant=Variant::Solid size=Size::Sm icon=icons::PLAY on_click=on_resume>
                    {resume}
                </Button>
                <Button variant=Variant::Ghost size=Size::Sm icon=icons::TRASH_2 on_click=on_discard>
                    "Discard download"
                </Button>
            </div>
        </div>
    }
}

#[component]
fn Installed(
    package: InstalledPackage,
    latest: String,
    busy: Memo<bool>,
    on_update: Callback<()>,
    on_uninstall: Callback<()>,
) -> impl IntoView {
    let current = package.version.as_deref() == Some(latest.as_str());
    let status = match &package.version {
        Some(v) if current => format!("Installed · v{v}"),
        Some(v) => format!("v{v} installed · v{latest} available"),
        None => "Installed · version unknown".to_owned(),
    };
    let update = (!current).then(|| {
        let label = if package.version.is_some() {
            format!("Update to v{latest}")
        } else {
            format!("Reinstall v{latest}")
        };
        view! {
            <Button variant=Variant::Solid size=Size::Sm icon=icons::REFRESH_CW loading=busy on_click=on_update>
                {label}
            </Button>
        }
    });
    view! {
        <div class="flex flex-wrap items-center gap-3">
            <span class=cn!(EYEBROW, if current { "text-accent-300" } else { "text-white/70" })>
                {status}
            </span>
            {update}
            <Button variant=Variant::Ghost tone=Tone::Danger size=Size::Sm icon=icons::TRASH_2 on_click=on_uninstall>
                "Uninstall"
            </Button>
        </div>
    }
}

fn amount(p: Progress) -> String {
    match p.total {
        Some(total) => format!("{} / {}", human_bytes(p.done), human_bytes(total)),
        None => human_bytes(p.done),
    }
}

#[allow(clippy::cast_precision_loss)]
pub(crate) fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    if value < 1024.0 {
        return format!("{n} B");
    }
    let mut unit = 0;
    value /= 1024.0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}
