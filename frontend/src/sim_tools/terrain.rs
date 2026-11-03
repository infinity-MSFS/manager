//! The custom terrain radar's database: what is on disk, and the download.
//! Shown as a Settings section, and on the configurator of any product that
//! talks to the terrain bridge while the database is missing.
//!
//! The download runs in the plugin and reports as a `jobs:changed` job
//! ([`TERRAIN_JOB_ID`]), which the core app state already tracks.

use crate::editorial::{EYEBROW, ErrorNote};
use crate::ipc;
use crate::state::use_app;
use infinity_ipc::IpcError;
use infinity_ipc::catalog::Product;
use infinity_ipc::dto::{Job, JobPhase};
use infinity_sim_tools_ipc::terrain::{TERRAIN_JOB_ID, TerrainDownloadArgs, TerrainStatus};
use infinity_sim_tools_ipc::{TerrainCancel, TerrainDownload, TerrainStatusGet};
use infinity_ui::prelude::*;

/// Namespace of the bridge the T-38 terrain relay connects on; a product
/// declaring it renders the custom radar from this database.
const TERRAIN_NAMESPACE: &str = "t38_terrain";

/// The Settings section.
#[component]
pub fn TerrainSettings() -> impl IntoView {
    view! {
        <div class="max-w-2xl">
            <p class="mb-6 font-light text-white/60">
                "The T-38N's custom ground-map radar renders from this terrain database. Without it the aircraft uses the simulator's built-in terrain radar."
            </p>
            <Database />
        </div>
    }
}

/// The configurator panel: only for products with the terrain bridge, and
/// only while the database is missing or downloading, since a working
/// install needs no attention there.
pub fn configurator_panel(product: &Product) -> AnyView {
    if !product
        .bridges
        .iter()
        .any(|b| b.namespace == TERRAIN_NAMESPACE)
    {
        return ().into_any();
    }
    let app = use_app();
    let job = app.job(TERRAIN_JOB_ID.to_owned());
    let status = LocalResource::new(|| ipc::invoke::<TerrainStatusGet>(&()));
    let needed = move || {
        job.get().is_some_and(|j| j.is_active())
            || matches!(status.get(), Some(Ok(s)) if !s.installed)
    };
    view! {
        <Show when=needed>
            <div class="mb-10 border-l border-accent-400/40 pl-5">
                <p class=cn!("mb-3", EYEBROW)>"Terrain radar data"</p>
                <p class="mb-4 font-light text-white/60">
                    "The custom radar provider needs the terrain database. Download it here or later from Settings."
                </p>
                <Database />
            </div>
        </Show>
    }
    .into_any()
}

fn phase_label(phase: &JobPhase) -> &'static str {
    match phase {
        JobPhase::Queued => "Starting",
        JobPhase::Downloading => "Downloading",
        JobPhase::Verifying => "Verifying",
        JobPhase::Extracting | JobPhase::Installing => "Loading",
        JobPhase::Paused => "Paused",
        JobPhase::Cancelled => "Cancelled",
        JobPhase::Failed { .. } => "Failed",
        JobPhase::Completed => "Done",
    }
}

fn megabytes(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / 1e6)
}

#[component]
fn Database() -> impl IntoView {
    let app = use_app();
    let job = app.job(TERRAIN_JOB_ID.to_owned());
    let status = LocalResource::new(|| ipc::invoke::<TerrainStatusGet>(&()));
    let error = RwSignal::new(None::<IpcError>);

    // A finished download drops out of the job list: look again.
    let active = Memo::new(move |_| job.get().is_some_and(|j| j.is_active()));
    Effect::new(move |previous: Option<bool>| {
        let now = active.get();
        if previous == Some(true) && !now {
            status.refetch();
        }
        now
    });

    let download = Action::new_local(move |(): &()| async move {
        error.set(
            ipc::invoke::<TerrainDownload>(&TerrainDownloadArgs::default())
                .await
                .err(),
        );
    });
    let cancel = Action::new_local(move |(): &()| async move {
        error.set(ipc::invoke::<TerrainCancel>(&()).await.err());
        status.refetch();
    });

    let summary = move || {
        let Some(result) = status.get() else {
            return view! { <Skeleton class="h-5 w-72" /> }.into_any();
        };
        match result {
            Err(e) => view! { <ErrorNote error=e /> }.into_any(),
            Ok(s) => view! { <Summary status=s /> }.into_any(),
        }
    };

    let progress = move || {
        job.get().filter(Job::is_active).map(|j| {
            let fraction = j.progress.and_then(|p| p.fraction());
            let detail = j
                .progress
                .map(|p| match p.total {
                    Some(t) => format!("{} of {}", megabytes(p.done), megabytes(t)),
                    None => megabytes(p.done),
                })
                .unwrap_or_default();
            view! {
                <div class="flex flex-col gap-2 border-t border-white/10 py-4">
                    <div class="flex items-center justify-between text-sm">
                        <span class="text-white/75">{phase_label(&j.phase)}</span>
                        <span class="font-mono text-xs tabular-nums text-white/45">{detail}</span>
                    </div>
                    <Progress value=Signal::derive(move || fraction.unwrap_or(0.0)) show_value=fraction.is_some() />
                </div>
            }
        })
    };

    let failure = move || {
        job.get().and_then(|j| match j.phase {
            JobPhase::Failed { error } => Some(view! { <ErrorNote error=error /> }),
            _ => None,
        })
    };

    let installed = move || {
        matches!(
            status.get(),
            Some(Ok(TerrainStatus {
                installed: true,
                ..
            }))
        )
    };

    view! {
        <div class="flex max-w-2xl flex-col gap-4">
            {summary}
            {progress}
            {failure}
            {move || error.get().map(|e| view! { <ErrorNote error=e /> })}
            <div class="flex items-center gap-3 border-t border-white/10 pt-4">
                <Show
                    when=move || active.get()
                    fallback=move || {
                        view! {
                            <Button
                                variant=Variant::Solid
                                icon=icons::DOWNLOAD
                                loading=download.pending()
                                on_click=Callback::new(move |()| {
                                    download.dispatch(());
                                })
                            >
                                {move || if installed() { "Download again" } else { "Download terrain data" }}
                            </Button>
                        }
                    }
                >
                    <Button
                        variant=Variant::Ghost
                        tone=Tone::Danger
                        icon=icons::X
                        loading=cancel.pending()
                        on_click=Callback::new(move |()| {
                            cancel.dispatch(());
                        })
                    >
                        "Cancel"
                    </Button>
                </Show>
                <span class="text-xs font-light text-white/45">
                    "Stopping keeps nothing; a dropped connection resumes where it left off."
                </span>
            </div>
        </div>
    }
}

#[component]
fn Summary(status: TerrainStatus) -> impl IntoView {
    let (tone, text) = match (&status, status.loaded) {
        (s, true) => (
            Tone::Accent,
            format!(
                "Installed ({}), loaded and ready{}.",
                megabytes(s.size_bytes),
                s.tiles.map(|t| format!(": {t} tiles")).unwrap_or_default()
            ),
        ),
        (s, false) if s.installed => (
            Tone::Warning,
            format!("Installed ({}), but not loaded.", megabytes(s.size_bytes)),
        ),
        _ => (
            Tone::Neutral,
            "Not downloaded. The custom radar provider needs it.".to_owned(),
        ),
    };
    view! {
        <div class="flex flex-col gap-2">
            <div class="flex items-center gap-3">
                <StatusDot tone=tone pulse=status.downloading />
                <span class="font-light text-white/80">{text}</span>
            </div>
            {status.load_error.map(|e| view! { <p class="text-sm text-red-300/80">{e}</p> })}
            <p class=cn!("mt-1", EYEBROW, "normal-case tracking-normal")>
                <span data-selectable class="font-mono text-white/35">{status.path}</span>
            </p>
        </div>
    }
}
