//! The configurator's frame (v3 `ConfiguratorChrome.tsx`, `actions.ts`):
//! heading, sidebar with indicators, tabs and actions, and the content
//! area in the product's `stacked` or `tabs` layout.

use std::sync::Arc;

use infinity_ipc::IpcError;
use infinity_ipc::catalog::{ActionSpec, IndicatorSpec, Product, UiLayout, UiSpec};
use infinity_ipc::commands as cmd;
use infinity_ipc::dto::{ConfiguratorEmit, EmitResult, ProductRef};
use infinity_ui::prelude::*;
use serde_json::Value;

use super::indicators::indicator;
use super::renderer::{group_header, group_id, group_title, is_page, render_group};
use super::state::{Configurator, SaveState};
use super::toast::{ToastLayer, Toasts};
use crate::editorial::{EYEBROW, ErrorNote};
use crate::ipc;
use crate::routes;
use crate::state::use_app;

fn action_id(a: &ActionSpec) -> &str {
    match a {
        ActionSpec::EmitOutput { id, .. }
        | ActionSpec::ShareableSave { id, .. }
        | ActionSpec::ShareableLoad { id, .. }
        | ActionSpec::ResetConfig { id, .. } => id,
    }
}

fn action_label(a: &ActionSpec) -> &str {
    match a {
        ActionSpec::EmitOutput { label, .. }
        | ActionSpec::ShareableSave { label, .. }
        | ActionSpec::ShareableLoad { label, .. }
        | ActionSpec::ResetConfig { label, .. } => label,
    }
}

fn action_icon(a: &ActionSpec) -> IconData {
    match a {
        ActionSpec::EmitOutput { .. } => icons::ARROW_UP_RIGHT,
        ActionSpec::ShareableSave { .. } => icons::DOWNLOAD,
        ActionSpec::ShareableLoad { .. } => icons::FILE_TEXT,
        ActionSpec::ResetConfig { .. } => icons::REFRESH_CW,
    }
}

/// Runs an action and reports the outcome as a toast.
async fn run_action(cfg: Configurator, spec: ActionSpec) {
    let label = action_label(&spec).to_owned();
    let toasts = cfg.toasts;
    let product = cfg.product_ref();
    // Outputs and shares read the saved file: write pending edits first.
    if !matches!(
        spec,
        ActionSpec::ResetConfig { .. } | ActionSpec::ShareableLoad { .. }
    ) && let Err(e) = cfg.flush().await
    {
        toasts.error(format!("{label} failed"), &e);
        return;
    }
    let result: Result<(), IpcError> = async {
        match &spec {
            ActionSpec::EmitOutput { output_id, .. } => {
                let args = ConfiguratorEmit {
                    product: product.clone(),
                    output_id: output_id.clone(),
                };
                match ipc::invoke::<cmd::ConfiguratorEmit>(&args).await? {
                    EmitResult::Bridge { .. } => {
                        toasts.success(&label, "Configuration sent to aircraft.")
                    }
                    EmitResult::File { path } => toasts.success(&label, format!("Wrote {path}")),
                }
            }
            ActionSpec::ShareableSave { .. } => {
                if let Some(path) = ipc::invoke::<cmd::ConfiguratorShareSave>(&product).await? {
                    toasts.success(&label, format!("Saved to {path}"));
                }
            }
            ActionSpec::ShareableLoad { .. } => {
                if ipc::invoke::<cmd::ConfiguratorShareLoad>(&product)
                    .await?
                    .is_some()
                {
                    // The import is saved at its own schema version; a load
                    // migrates it to the current one.
                    cfg.load().await?;
                    toasts.success(&label, "Configuration loaded.");
                }
            }
            ActionSpec::ResetConfig { .. } => {
                ipc::invoke::<cmd::ConfiguratorReset>(&product).await?;
                cfg.load().await?;
                toasts.info(&label, "Configuration reset to defaults.");
            }
        }
        Ok(())
    }
    .await;
    if let Err(e) = result {
        toasts.error(format!("{label} failed"), &e);
    }
}

fn save_label(state: &SaveState) -> (&'static str, Tone) {
    match state {
        SaveState::Clean => ("Up to date", Tone::Neutral),
        SaveState::Pending => ("Unsaved changes", Tone::Warning),
        SaveState::Saving => ("Saving…", Tone::Info),
        SaveState::Saved => ("Saved", Tone::Accent),
        SaveState::Failed(_) => ("Save failed", Tone::Danger),
    }
}

#[component]
fn Actions(cfg: Configurator, actions: Vec<ActionSpec>) -> impl IntoView {
    let pending = RwSignal::new(None::<String>);
    let confirm = RwSignal::new(None::<ActionSpec>);
    let run = move |spec: ActionSpec| {
        if pending.get_untracked().is_some() {
            return;
        }
        pending.set(Some(action_id(&spec).to_owned()));
        leptos::task::spawn_local(async move {
            run_action(cfg, spec).await;
            let _ = pending.try_set(None);
        });
    };
    let primary_pos = actions
        .iter()
        .position(|a| matches!(a, ActionSpec::EmitOutput { primary: true, .. }));
    let mut ordered = actions;
    if let Some(i) = primary_pos {
        let p = ordered.remove(i);
        ordered.insert(0, p);
    }
    let buttons = ordered
        .into_iter()
        .enumerate()
        .map(|(i, spec)| {
            let is_primary = primary_pos.is_some() && i == 0;
            let id = action_id(&spec).to_owned();
            let label = action_label(&spec).to_owned();
            let icon = action_icon(&spec);
            let is_reset = matches!(spec, ActionSpec::ResetConfig { .. });
            let spec = StoredValue::new(spec);
            let busy = Signal::derive(move || pending.get().as_deref() == Some(id.as_str()));
            view! {
                <Button
                    variant=if is_primary { Variant::Solid } else { Variant::Glass }
                    tone=if is_reset { Tone::Danger } else { Tone::Accent }
                    size=if is_primary { Size::Md } else { Size::Sm }
                    icon=icon
                    full_width=true
                    loading=busy
                    disabled=Signal::derive(move || pending.get().is_some() && !busy.get())
                    on_click=Callback::new(move |()| {
                        let s = spec.get_value();
                        if is_reset {
                            confirm.set(Some(s));
                        } else {
                            run(s);
                        }
                    })
                >
                    {label}
                </Button>
            }
        })
        .collect_view();
    view! {
        <div class="flex flex-col gap-2">{buttons}</div>
        <ConfirmDialog
            open=Signal::derive(move || confirm.with(Option::is_some))
            on_close=Callback::new(move |()| confirm.set(None))
            on_confirm=Callback::new(move |()| {
                if let Some(s) = confirm.get_untracked() {
                    confirm.set(None);
                    run(s);
                }
            })
            title="Reset configuration?"
            message="Your saved settings for this aircraft are deleted and the defaults load. This cannot be undone."
            confirm_label="Reset"
        />
    }
}

#[component]
fn SaveStatus(cfg: Configurator) -> impl IntoView {
    move || {
        if cfg.live {
            return view! {
                <p class=cn!(EYEBROW, "text-white/40")>"Live from the aircraft"</p>
            }
            .into_any();
        }
        let state = cfg.save_state.get();
        let (text, tone) = save_label(&state);
        let detail = match &state {
            SaveState::Failed(e) => Some(e.message.clone()),
            _ => None,
        };
        view! {
            <div class="flex flex-col gap-1">
                <StatusDot tone=tone label=text />
                {detail.map(|d| view! { <p data-selectable class="text-xs font-light text-red-300/80">{d}</p> })}
            </div>
        }
        .into_any()
    }
}

#[component]
fn TabList(groups: Arc<Vec<Arc<Value>>>, active: RwSignal<String>) -> impl IntoView {
    view! {
        <nav class="flex flex-col border-t border-white/10">
            {groups
                .iter()
                .enumerate()
                .map(|(i, g)| {
                    let id = group_id(g);
                    let title = group_title(g);
                    let mine = id.clone();
                    let selected = Signal::derive(move || active.get() == mine);
                    view! {
                        <button
                            type="button"
                            aria-current=move || selected.get().then_some("page")
                            class=move || {
                                cn!(
                                    "group relative flex items-baseline gap-3 border-b py-3 text-left transition-colors",
                                    if selected.get() { "border-accent-400 text-white" } else { "border-white/10 text-white/55 hover:border-white/30 hover:text-white" },
                                )
                            }
                            on:click=move |_| active.set(id.clone())
                        >
                            <span class="font-mono text-[11px] text-accent-400/80">{format!("{:02}", i + 1)}</span>
                            <span class="text-sm">{title}</span>
                        </button>
                    }
                })
                .collect_view()}
        </nav>
    }
}

/// The configurator for a product with a `configurator` block.
#[component]
pub(crate) fn Chrome(product: Product, product_ref: ProductRef) -> impl IntoView {
    let app = use_app();
    let reduce = app.reduce_motion();
    let toasts = Toasts::new();
    let spec = product.configurator.clone();
    let ui: UiSpec = spec.as_ref().and_then(|c| c.ui.clone()).unwrap_or_default();
    let groups: Arc<Vec<Arc<Value>>> = Arc::new(
        spec.as_ref()
            .and_then(|c| c.schema.as_array())
            .map(|a| a.iter().cloned().map(Arc::new).collect())
            .unwrap_or_default(),
    );
    let schema_ok = spec.as_ref().is_some_and(|c| c.schema.is_array());
    let has_bridges = !product.bridges.is_empty();
    let name = product.name.clone();
    let extension_panels = crate::ext::use_extensions()
        .configurator
        .iter()
        .map(|panel| (panel.view)(&product))
        .collect_view();
    let cfg = Configurator::new(product, product_ref.clone(), toasts);
    cfg.save_on_cleanup();

    let loading = RwSignal::new(true);
    let load_error = RwSignal::new(None::<IpcError>);
    leptos::task::spawn_local(async move {
        let result = cfg.load().await;
        let _ = load_error.try_set(result.err());
        let _ = loading.try_set(false);
    });
    if has_bridges {
        let pr = product_ref;
        leptos::task::spawn_local(async move {
            if let Err(e) = ipc::invoke::<cmd::BridgeEnsure>(&pr).await {
                ipc::warn(&format!("bridge start failed: {e}"));
            }
        });
    }

    let layout = ui.layout;
    let title = ui.title.clone().unwrap_or(name);
    let subtitle = ui.subtitle.clone();
    let indicators: Vec<IndicatorSpec> = ui.indicators.clone();
    let actions = ui.actions.clone();
    let has_sidebar = !(ui.title.is_none() && indicators.is_empty() && actions.is_empty())
        || layout == UiLayout::Tabs;
    let active = RwSignal::new(groups.first().map(|g| group_id(g)).unwrap_or_default());

    let content = {
        let groups = groups.clone();
        move || {
            if loading.get() {
                return view! { <Skeleton class="h-64 w-full" /> }.into_any();
            }
            if let Some(e) = load_error.get() {
                return view! { <ErrorNote error=e /> }.into_any();
            }
            if !schema_ok {
                return view! {
                    <super::widgets::SchemaNote message="This product's configurator schema is not a list of groups." />
                }
                .into_any();
            }
            if groups.is_empty() {
                return view! { <p class="font-light text-white/40">"No groups defined."</p> }
                    .into_any();
            }
            match layout {
                UiLayout::Tabs => {
                    let groups = groups.clone();
                    (move || {
                        let id = active.get();
                        let group = groups
                            .iter()
                            .find(|g| group_id(g) == id)
                            .or_else(|| groups.first())
                            .cloned();
                        group.map(|g| {
                            let page = is_page(&g);
                            view! {
                                <div class=move || if reduce.get() { "" } else { "iui-pop" }>
                                    {(!page).then(|| group_header(&g))}
                                    {render_group(cfg, g.clone(), "", None)}
                                </div>
                            }
                        })
                    })
                    .into_any()
                }
                UiLayout::Stacked => view! {
                    <div class="flex flex-col gap-16">
                        {groups
                            .iter()
                            .enumerate()
                            .map(|(i, g)| render_group(cfg, g.clone(), "", (!is_page(g)).then_some(i + 1)))
                            .collect_view()}
                    </div>
                }
                .into_any(),
            }
        }
    };

    let sidebar = has_sidebar.then(|| {
        let groups = groups.clone();
        view! {
            <aside class="flex flex-col gap-10 self-start md:sticky md:top-8">
                {(!indicators.is_empty())
                    .then(|| {
                        view! {
                            <div>
                                <p class=cn!("mb-3", EYEBROW)>"Status"</p>
                                <div class="border-t border-white/10 pt-2">
                                    {indicators.into_iter().map(|s| indicator(cfg, s)).collect_view()}
                                </div>
                            </div>
                        }
                    })}
                {(layout == UiLayout::Tabs && !groups.is_empty())
                    .then(|| {
                        view! {
                            <div>
                                <p class=cn!("mb-3", EYEBROW)>"Sections"</p>
                                <TabList groups=groups active=active />
                            </div>
                        }
                    })}
                {(!actions.is_empty())
                    .then(|| {
                        view! {
                            <div>
                                <p class=cn!("mb-3", EYEBROW)>"Actions"</p>
                                <Actions cfg=cfg actions=actions />
                            </div>
                        }
                    })}
                <SaveStatus cfg=cfg />
            </aside>
        }
    });

    view! {
        <header class="mb-12 flex flex-col gap-8 md:flex-row md:items-end md:justify-between">
            <div class="max-w-3xl">
                <p class=cn!("mb-8", EYEBROW)>"Infinity / Configure"</p>
                <h1 class="text-5xl leading-[1.02] font-extralight tracking-tight">{title}</h1>
                {subtitle
                    .map(|s| {
                        view! {
                            <p class="mt-6 max-w-xl border-l border-accent-400/40 pl-5 text-base leading-relaxed font-light text-white/65">
                                {s}
                            </p>
                        }
                    })}
            </div>
            <Button variant=Variant::Ghost href=routes::LIBRARY icon=icons::ARROW_LEFT>
                "Library"
            </Button>
        </header>
        <div class="max-w-3xl empty:hidden">{extension_panels}</div>
        {move || {
            cfg.migrated
                .get()
                .then(|| {
                    view! {
                        <div class="mb-10 max-w-3xl">
                            <Alert
                                tone=Tone::Info
                                title="Settings upgraded"
                                description="Your saved configuration was migrated to this version's format."
                                on_dismiss=Callback::new(move |()| cfg.migrated.set(false))
                            />
                        </div>
                    }
                })
        }}
        <div class=if has_sidebar { "grid gap-12 md:grid-cols-[220px_minmax(0,1fr)]" } else { "" }>
            {sidebar}
            <div class="min-w-0">{content}</div>
        </div>
        <ToastLayer toasts=toasts />
    }
}
