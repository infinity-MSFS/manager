//! Schema renderer (v3 `ConfiguratorRenderer.tsx`): groups to fields to
//! widgets. A group with `page` hands the whole tab to a registered page.

use std::sync::Arc;

use infinity_ui::prelude::*;
use serde_json::Value;

use super::pages::{self, PageCtx};
use super::schema::{field_path, fields_of, label_of, str_prop};
use super::state::Configurator;
use super::widgets::{self, SchemaNote, WidgetCtx, WidgetProps};
use crate::editorial::EYEBROW;

/// Widgets that need the full row.
const WIDE: &[&str] = &[
    "list",
    "keyedList",
    "toggleList",
    "trainingZones",
    "noFlyZones",
];

pub(crate) fn group_id(group: &Value) -> String {
    str_prop(group, "id").unwrap_or_default().to_owned()
}

pub(crate) fn group_title(group: &Value) -> String {
    str_prop(group, "title")
        .or_else(|| str_prop(group, "id"))
        .unwrap_or_default()
        .to_owned()
}

pub(crate) fn is_page(group: &Value) -> bool {
    str_prop(group, "page").is_some()
}

/// One group: its page, or its fields under an optional numbered heading.
pub(crate) fn render_group(
    cfg: Configurator,
    group: Arc<Value>,
    parent_path: &str,
    heading: Option<usize>,
) -> AnyView {
    if let Some(page) = str_prop(&group, "page").map(str::to_owned) {
        let id = group_id(&group);
        return pages::render(&page, PageCtx { cfg, group }).unwrap_or_else(|| {
            view! {
                <SchemaNote message=format!("Page \u{201c}{page}\u{201d} is not registered (group {id}).") />
            }
            .into_any()
        });
    }
    let id = group_id(&group);
    let group_path: Arc<str> = if parent_path.is_empty() {
        Arc::from(id.as_str())
    } else {
        Arc::from(format!("{parent_path}.{id}"))
    };
    let title = group_title(&group);
    let fields = fields_of(&group);
    let ctx = WidgetCtx { cfg, group_path };
    view! {
        <section class="flex flex-col">
            {heading
                .map(|n| {
                    view! {
                        <div class=cn!("mb-8 flex items-center gap-4", EYEBROW)>
                            <span class="text-accent-400/80">{format!("{n:02}")}</span>
                            <span>{title.clone()}</span>
                            <span class="h-px flex-1 bg-white/10"></span>
                        </div>
                    }
                })}
            {fields.is_empty().then(|| view! { <p class="text-sm font-light text-white/40">"Nothing to configure here."</p> })}
            <div class="grid gap-x-10 gap-y-6 md:grid-cols-2">
                {fields
                    .into_iter()
                    .map(|f| render_field(&id, f, ctx.clone()))
                    .collect_view()}
            </div>
        </section>
    }
    .into_any()
}

fn render_field(group_id: &str, field: Value, ctx: WidgetCtx) -> AnyView {
    let ty = str_prop(&field, "type").unwrap_or_default();
    let wide = WIDE.contains(&ty) || !widgets::is_supported(ty);
    let id = str_prop(&field, "id").unwrap_or_default();
    let path = field_path(group_id, id);
    let cfg = ctx.cfg;
    let value = cfg.at(path.clone());
    let on_change = Callback::new(move |next: Value| cfg.set_at(&path, next));
    let props = WidgetProps {
        field: Arc::new(field),
        value: value.into(),
        on_change,
        ctx,
    };
    view! {
        <div class=cn!("border-t border-white/10 pt-4", wide.then_some("md:col-span-2"))>
            {widgets::render(props)}
        </div>
    }
    .into_any()
}

/// A tab's heading inside the tabs layout.
pub(crate) fn group_header(group: &Value) -> impl IntoView + use<> {
    let description = str_prop(group, "description").map(str::to_owned);
    let title = label_of(group);
    view! {
        <header class="mb-10">
            <h2 class="text-3xl font-extralight tracking-tight">{title}</h2>
            {description
                .map(|d| {
                    view! {
                        <p class="mt-3 max-w-xl border-l border-accent-400/40 pl-5 text-sm leading-relaxed font-light text-white/60">
                            {d}
                        </p>
                    }
                })}
        </header>
    }
}
