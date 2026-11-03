use infinity_ipc::commands as cmd;
use infinity_ipc::dto::{PickFolder, Preferences, PrefixProbe, SimInstall, Storefront};
use infinity_ui::prelude::*;

use super::{ErrorNote, Field, save_prefs};
use crate::editorial::EYEBROW;
use crate::ipc;
use crate::state::{AppState, use_app};

/// The aircraft and livery install folders.
#[component]
pub(super) fn InstallFolders() -> impl IntoView {
    let app = use_app();
    let error = RwSignal::new(None);
    // Re-probed whenever the preferences change, so "detected" stays true.
    let paths = LocalResource::new(move || {
        app.prefs.track();
        ipc::invoke::<cmd::SimPaths>(&())
    });

    let current = move || {
        let Some(Ok(paths)) = paths.get() else {
            return view! { <Skeleton class="h-5 w-96" /> }.into_any();
        };
        let Some(dir) = paths.install_dir else {
            return view! {
                <p class="font-light text-white/65">
                    "No simulator found. Choose your Community folder below."
                </p>
            }
            .into_any();
        };
        view! {
            <div class="flex flex-col gap-1">
                <span class=cn!(EYEBROW, "text-white/40")>
                    {if dir.chosen { "Products install into" } else { "Detected, products install into" }}
                </span>
                <span data-selectable class="font-light text-white/85">{dir.path}</span>
                {(!dir.exists)
                    .then(|| view! {
                        <span class="text-sm text-red-300/80">"This folder does not exist. Is the drive connected?"</span>
                    })}
            </div>
        }
        .into_any()
    };
    let detected = move || {
        paths.get().and_then(Result::ok).map(|p| {
            p.installs
                .into_iter()
                .map(|i| view! { <InstallRow install=i /> })
                .collect_view()
        })
    };

    view! {
        <div class="flex max-w-2xl flex-col gap-10">
            {current}
            <FolderField
                label="Aircraft folder"
                hint="Your sim's Community folder. Leave empty to follow the detected sim."
                placeholder=r"D:\MSFS\Community"
                value=pref(app, |p| p.install_dir.clone())
                on_save=Callback::new(move |dir| save_prefs(app, error, |p| p.install_dir = dir))
            />
            <FolderField
                label="Livery folder"
                hint="Leave empty to install liveries beside aircraft."
                placeholder=r"D:\MSFS\Liveries"
                value=pref(app, |p| p.livery_install_dir.clone())
                on_save=Callback::new(move |dir| save_prefs(app, error, |p| p.livery_install_dir = dir))
            />
            {move || error.get().map(|e| view! { <ErrorNote error=e /> })}
            <dl>{detected}</dl>
        </div>
    }
}

/// Overrides for installs discovery cannot find.
#[component]
pub(super) fn SimOverrides() -> impl IntoView {
    let app = use_app();
    let error = RwSignal::new(None);
    let work_root = pref(app, |p| p.msfs_work_root.clone());
    let work_probe = LocalResource::new(move || {
        let root = work_root.get();
        async move {
            match root {
                Some(r) => ipc::invoke::<cmd::SimProbeWorkRoot>(&r).await.ok(),
                None => None,
            }
        }
    });
    let linux = move || {
        app.info
            .with(|i| i.as_ref().is_some_and(|i| i.os == "linux"))
    };

    view! {
        <div class="flex max-w-2xl flex-col gap-10">
            <div class="flex flex-col gap-3">
                <FolderField
                    label="MSFS work folder"
                    hint="The WASM/MSFS2024 folder holding aircraft work folders. Leave empty to auto-detect."
                    placeholder=r"...\WASM\MSFS2024"
                    value=work_root
                    on_save=Callback::new(move |dir| save_prefs(app, error, |p| p.msfs_work_root = dir))
                />
                {move || {
                    work_probe
                        .get()
                        .flatten()
                        .map(|probe| {
                            let text = match (&probe.resolved, probe.packages.len()) {
                                (None, _) => "That folder does not exist.".to_owned(),
                                (Some(r), 0) => format!("{r} holds no aircraft yet."),
                                (Some(r), n) => format!("{r}: {n} aircraft ({})", probe.packages.join(", ")),
                            };
                            view! { <p class="text-sm font-light text-white/55">{text}</p> }
                        })
                }}
            </div>
            <Show when=linux>
                <WinePrefix error=error />
            </Show>
            {move || error.get().map(|e| view! { <ErrorNote error=e /> })}
        </div>
    }
}

#[component]
fn WinePrefix(error: RwSignal<Option<infinity_ipc::IpcError>>) -> impl IntoView {
    let app = use_app();
    let detected = LocalResource::new(|| ipc::invoke::<cmd::SimDetectedPrefixes>(&()));
    let current = pref(app, |p| p.wine_prefix.clone());
    let list = move || {
        detected.get().and_then(Result::ok).map(|found| {
            found
                .into_iter()
                .map(|probe: PrefixProbe| {
                    let path = probe.prefix.clone();
                    let sims = probe
                        .installs
                        .iter()
                        .map(|i| i.sim.label())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let chosen = move || current.get().as_deref() == Some(path.as_str());
                    let pick = probe.prefix.clone();
                    view! {
                        <div class="flex items-center justify-between gap-4 border-t border-white/10 py-3">
                            <div class="min-w-0">
                                <p class="truncate text-sm font-light text-white/85">{probe.prefix.clone()}</p>
                                <p class=cn!(EYEBROW, "text-white/40")>{sims}</p>
                            </div>
                            <Button
                                variant=Variant::Glass
                                size=Size::Sm
                                disabled=Signal::derive(chosen)
                                on_click=Callback::new(move |()| {
                                    let pick = pick.clone();
                                    save_prefs(app, error, move |p| p.wine_prefix = Some(pick));
                                })
                            >
                                "Use"
                            </Button>
                        </div>
                    }
                })
                .collect_view()
        })
    };
    view! {
        <div class="flex flex-col gap-3">
            <FolderField
                label="Proton prefix"
                hint="The folder containing drive_c, for launchers other than Steam. Leave empty to scan Steam."
                placeholder="~/.steam/steam/steamapps/compatdata/2537590/pfx"
                value=current
                on_save=Callback::new(move |dir| save_prefs(app, error, |p| p.wine_prefix = dir))
            />
            <div>{list}</div>
        </div>
    }
}

#[component]
fn InstallRow(install: SimInstall) -> impl IntoView {
    let store = match install.storefront {
        Storefront::MicrosoftStore => "Microsoft Store",
        Storefront::Steam => "Steam",
    };
    view! {
        <Field
            label=install.sim.label()
            value=format!(
                "{store} · {}",
                install.community.unwrap_or_else(|| "no Community folder yet".to_owned()),
            )
        />
    }
}

/// An optional folder preference: type it, browse for it, or clear it.
#[component]
fn FolderField(
    label: &'static str,
    hint: &'static str,
    placeholder: &'static str,
    #[prop(into)] value: Signal<Option<String>>,
    on_save: Callback<Option<String>>,
) -> impl IntoView {
    let draft = RwSignal::new(String::new());
    Effect::new(move |_| draft.set(value.get().unwrap_or_default()));
    let browse = Action::new_local(move |(): &()| async move {
        let request = PickFolder {
            title: label.to_owned(),
            start: Some(draft.get_untracked()).filter(|d| !d.trim().is_empty()),
        };
        if let Ok(Some(picked)) = ipc::invoke::<cmd::DialogPickFolder>(&request).await {
            draft.set(picked.clone());
            on_save.run(Some(picked));
        }
    });
    let changed = Signal::derive(move || {
        let d = draft.get();
        let d = d.trim();
        value.with(|v| v.as_deref().unwrap_or_default() != d)
    });

    view! {
        <div class="flex flex-col gap-2">
            <span class="text-lg font-light">{label}</span>
            <span class="text-sm font-light text-white/55">{hint}</span>
            <div class="flex gap-2">
                <div class="min-w-0 flex-1">
                    <TextInput
                        value=draft
                        on_input=Callback::new(move |v| draft.set(v))
                        placeholder=placeholder
                        icon=icons::HARD_DRIVE
                    />
                </div>
                <Button variant=Variant::Glass loading=browse.pending() on_click=Callback::new(move |()| {
                    browse.dispatch(());
                })>
                    "Browse"
                </Button>
                <Button
                    variant=Variant::Soft
                    disabled=Signal::derive(move || !changed.get())
                    on_click=Callback::new(move |()| {
                        let d = draft.get_untracked().trim().to_owned();
                        on_save.run(Some(d).filter(|d| !d.is_empty()));
                    })
                >
                    "Save"
                </Button>
                <Button
                    variant=Variant::Ghost
                    disabled=Signal::derive(move || value.with(Option::is_none))
                    on_click=Callback::new(move |()| on_save.run(None))
                >
                    "Clear"
                </Button>
            </div>
        </div>
    }
}

fn pref(
    app: AppState,
    get: impl Fn(&Preferences) -> Option<String> + Send + Sync + 'static,
) -> Signal<Option<String>> {
    Signal::derive(move || app.prefs.with(|p| get(p).filter(|s| !s.trim().is_empty())))
}
