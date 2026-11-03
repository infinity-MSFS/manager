//! Pieces shared by widgets and pages.

use std::cell::Cell;
use std::rc::Rc;

use infinity_ui::prelude::*;
use infinity_ui::style::GLASS_RAISED;
use serde_json::Value;

use crate::editorial::EYEBROW;
use crate::ipc;

/// An inline schema problem: unknown widget, missing props.
#[component]
pub(crate) fn SchemaNote(#[prop(into)] message: String) -> impl IntoView {
    view! {
        <div class="border-l border-white/20 py-1 pl-4">
            <p class=cn!("mb-1", EYEBROW, "text-white/40")>"Schema"</p>
            <p data-selectable class="text-sm font-light text-white/60">{message}</p>
        </div>
    }
}

/// The small uppercase caption above a control, with an optional aside.
#[component]
pub(crate) fn WidgetLabel(
    #[prop(into)] label: String,
    #[prop(into, optional)] aside: Option<Signal<String>>,
) -> impl IntoView {
    view! {
        <span class="flex items-center gap-2 text-xs font-medium tracking-[0.14em] text-white/50 uppercase">
            {label}
            {aside.map(|a| view! { <span class="font-mono text-[10px] text-accent-300/80">{move || a.get()}</span> })}
        </span>
    }
}

/// Text the user is editing for `value`. Follows outside changes (a
/// share-load, a reset) unless the draft already parses to the new value,
/// so typing `1.` is not snapped back to `1`.
pub(crate) fn use_draft(
    value: Signal<Value>,
    show: fn(&Value) -> String,
    parse: impl Fn(&str) -> Option<Value> + 'static,
) -> RwSignal<String> {
    let draft = RwSignal::new(value.with_untracked(show));
    Effect::new(move |_| {
        let v = value.get();
        let current = draft.get_untracked();
        if parse(&current).as_ref() != Some(&v) {
            draft.set(show(&v));
        }
    });
    draft
}

/// Runs `run` with `source`'s value once it has been still for `ms`.
pub(crate) fn debounce<T: Clone + 'static>(
    source: impl Fn() -> T + 'static,
    ms: u32,
    run: impl Fn(T) + 'static,
) {
    let generation = Rc::new(Cell::new(0u64));
    let run = Rc::new(run);
    Effect::new(move |_| {
        let value = source();
        let mine = generation.get() + 1;
        generation.set(mine);
        let generation = generation.clone();
        let run = run.clone();
        leptos::task::spawn_local(async move {
            ipc::sleep(ms).await;
            if generation.get() == mine {
                run(value);
            }
        });
    });
}

/// A floating list of suggestions under a text input.
#[component]
pub(crate) fn Suggestions(
    #[prop(into)] open: Signal<bool>,
    children: ChildrenFn,
    #[prop(into, optional)] class: String,
) -> impl IntoView {
    let class = StoredValue::new(class);
    move || {
        open.get().then(|| {
            view! {
                <div
                    role="listbox"
                    class=cn!(
                        GLASS_RAISED,
                        "absolute top-full left-0 z-30 mt-1 max-h-72 min-w-full overflow-y-auto rounded-2xl border p-1",
                        class.get_value(),
                    )
                >
                    {children()}
                </div>
            }
        })
    }
}

/// One row in [`Suggestions`]. Acts on mousedown so the input's blur does
/// not close the list first.
#[component]
pub(crate) fn SuggestionItem(
    #[prop(into)] on_pick: Callback<()>,
    children: Children,
) -> impl IntoView {
    view! {
        <button
            type="button"
            role="option"
            class="block w-full rounded-xl px-3 py-1.5 text-left text-sm text-white/85 transition-colors hover:bg-white/10"
            on:mousedown=move |ev| {
                ev.prevent_default();
                on_pick.run(());
            }
        >
            {children()}
        </button>
    }
}

/// The message of an IPC error for inline display.
pub(crate) fn describe(e: &infinity_ipc::IpcError) -> String {
    format!("{}: {}", e.code, e.message)
}
