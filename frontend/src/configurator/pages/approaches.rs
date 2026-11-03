//! Approaches page (v3 `ApproachesPage.tsx`): ten fixed slots,
//! `APP.APP_500` to `APP.APP_509`, as cards in a horizontal scroller (the
//! mouse wheel pans it, with momentum unless motion is reduced). Each card
//! is a draft written to the config only on Save; see [`card`].

mod card;
mod model;

use std::array;

use infinity_ui::prelude::*;
use leptos::html::Div;
use serde_json::Value;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;

use self::card::{ApproachCard, card_dom_id};
use self::model::{Config, Draft, SLOT_BASE, SLOT_COUNT, slot_key};
use super::PageCtx;
use crate::configurator::schema::{object_mut, path_set};
use crate::editorial::EYEBROW;
use crate::state::use_app;

fn slot_of(i: usize) -> u32 {
    SLOT_BASE + u32::try_from(i).unwrap_or(0)
}

/// Whether an inner vertical scroller can take this wheel step itself.
fn can_scroll_y(el: &web_sys::Element, delta_y: f64) -> bool {
    let top = el.scroll_top();
    let at_top = top <= 0;
    let at_bottom = top + el.client_height() >= el.scroll_height() - 1;
    if delta_y < 0.0 { !at_top } else { !at_bottom }
}

/// One frame of momentum panning.
fn glide(node: NodeRef<Div>, velocity: StoredValue<f64>, running: StoredValue<bool>) {
    let (Some(el), Some(v)) = (node.get_untracked(), velocity.try_get_value()) else {
        let _ = running.try_set_value(false);
        return;
    };
    if v.abs() > 0.5 {
        #[allow(clippy::cast_possible_truncation)]
        let next = (f64::from(el.scroll_left()) + v).round() as i32;
        el.set_scroll_left(next);
        velocity.set_value(v * 0.95);
        request_animation_frame(move || glide(node, velocity, running));
    } else {
        running.set_value(false);
    }
}

fn scroll_card_into_view(slot: u32, smooth: bool) {
    let Some(el) = document().get_element_by_id(&card_dom_id(slot)) else {
        return;
    };
    let opts = js_sys::Object::new();
    let set = |k: &str, v: &str| {
        let _ = js_sys::Reflect::set(&opts, &JsValue::from_str(k), &JsValue::from_str(v));
    };
    set("behavior", if smooth { "smooth" } else { "auto" });
    set("inline", "center");
    set("block", "nearest");
    if let Ok(f) = js_sys::Reflect::get(&el, &JsValue::from_str("scrollIntoView"))
        && let Ok(f) = f.dyn_into::<js_sys::Function>()
    {
        let _ = f.call1(&el, &opts);
    }
}

pub(super) fn page(ctx: PageCtx) -> AnyView {
    let cfg = ctx.cfg;
    let reduce = use_app().reduce_motion();
    let app = Memo::new(move |_| {
        cfg.value
            .with(|v| v.get("APP").cloned().unwrap_or(Value::Null))
    });
    let committed: [Memo<Option<Config>>; SLOT_COUNT] = array::from_fn(|i| {
        let key = slot_key(slot_of(i));
        Memo::new(move |_| app.with(|a| a.get(&key).and_then(Config::from_value)))
    });
    let drafts: [RwSignal<Draft>; SLOT_COUNT] =
        array::from_fn(|i| RwSignal::new(Draft::blank(committed[i].get_untracked().as_ref())));

    // A share-load or reset replaces the saved slots: refill the drafts
    // that hold nothing of the user's.
    Effect::new(move |first: Option<()>| {
        app.track();
        if first.is_none() {
            return;
        }
        for i in 0..SLOT_COUNT {
            let c = committed[i].get_untracked();
            if let Some(c) = c
                && drafts[i].with_untracked(Draft::is_empty)
            {
                let expanded = drafts[i].with_untracked(|d| d.expanded);
                drafts[i].set(Draft {
                    expanded,
                    ..Draft::blank(Some(&c))
                });
            }
        }
    });

    let visible = Memo::new(move |_| {
        (0..SLOT_COUNT)
            .filter(|&i| {
                committed[i].with(Option::is_some)
                    || drafts[i].with(|d| d.has_something() || d.expanded)
            })
            .collect::<Vec<_>>()
    });

    let add = move |()| {
        let free = (0..SLOT_COUNT).find(|&i| {
            committed[i].with_untracked(Option::is_none)
                && drafts[i].with_untracked(|d| !d.has_something() && !d.expanded)
        });
        let Some(i) = free else { return };
        drafts[i].update(|d| d.expanded = true);
        let smooth = !reduce.get_untracked();
        request_animation_frame(move || scroll_card_into_view(slot_of(i), smooth));
    };

    let save = move |i: usize, c: Config| {
        cfg.update(|root| path_set(root, &format!("APP.{}", slot_key(slot_of(i))), c.to_value()));
    };
    let delete = move |i: usize| {
        let key = slot_key(slot_of(i));
        cfg.update(|root| {
            if let Some(app) = root.get_mut("APP").and_then(object_mut) {
                app.remove(&key);
            }
        });
        // A collapsed blank draft drops the card from view.
        drafts[i].set(Draft::blank(None));
    };

    let scroller = NodeRef::<Div>::new();
    let velocity = StoredValue::new(0.0f64);
    let running = StoredValue::new(false);
    let on_wheel = move |e: leptos::ev::WheelEvent| {
        let dy = e.delta_y();
        if dy == 0.0 {
            return;
        }
        let inner = e
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            .and_then(|t| t.closest("[data-yscroll]").ok().flatten())
            .filter(|y| y.scroll_height() > y.client_height());
        if let Some(y) = inner {
            // A scrollable card body keeps the wheel until it hits an edge.
            if !can_scroll_y(&y, dy) {
                e.prevent_default();
            }
            return;
        }
        e.prevent_default();
        if reduce.get_untracked() {
            if let Some(el) = scroller.get_untracked() {
                #[allow(clippy::cast_possible_truncation)]
                el.set_scroll_left(el.scroll_left() + dy.round() as i32);
            }
            return;
        }
        velocity.set_value(dy * 0.2);
        if running.get_value() {
            return;
        }
        running.set_value(true);
        request_animation_frame(move || glide(scroller, velocity, running));
    };

    view! {
        <div class="flex flex-col">
            <header class="mb-8 flex items-end justify-between gap-4 border-b border-white/10 pb-6">
                <div>
                    <p class=cn!("mb-3", EYEBROW)>"Procedures"</p>
                    <h2 class="text-3xl font-extralight tracking-tight">"Approaches"</h2>
                </div>
                <Button variant=Variant::Solid icon=icons::PLUS on_click=Callback::new(add)>
                    "Add approach"
                </Button>
            </header>
            <div
                node_ref=scroller
                on:wheel=on_wheel
                class="flex flex-row items-start gap-3 overflow-x-auto overflow-y-hidden pb-6 [scrollbar-width:thin]"
            >
                <Show
                    when=move || !visible.with(Vec::is_empty)
                    fallback=move || {
                        view! {
                            <div class="flex w-[340px] shrink-0 flex-col gap-4 border-t border-white/10 py-6">
                                <div>
                                    <p class="text-sm font-medium">"No approaches yet"</p>
                                    <p class="mt-1 text-xs font-light text-white/55">
                                        "Add an approach slot to start configuring procedures."
                                    </p>
                                </div>
                                <div>
                                    <Button variant=Variant::Glass size=Size::Sm icon=icons::PLUS on_click=Callback::new(add)>
                                        "Add approach"
                                    </Button>
                                </div>
                            </div>
                        }
                    }
                >
                    <For
                        each=move || visible.get()
                        key=|i| *i
                        children=move |i| {
                            view! {
                                <ApproachCard
                                    cfg=cfg
                                    number=slot_of(i)
                                    draft=drafts[i]
                                    committed=committed[i]
                                    on_save=Callback::new(move |c: Config| save(i, c))
                                    on_delete=Callback::new(move |()| delete(i))
                                />
                            }
                        }
                    />
                </Show>
            </div>
        </div>
    }
    .into_any()
}
