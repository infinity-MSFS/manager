//! App-wide notices: short messages that can carry one action, raised by
//! any page or extension and shown in a corner stack above every page.
//!
//! A notice with a `key` replaces an earlier one with the same key, and a
//! key that was dismissed stays quiet for the rest of the session, so a
//! source that repeats itself (a gauge re-announcing every few seconds)
//! raises one notice, not a stream of them.

use std::collections::HashSet;

use infinity_ui::prelude::*;
use leptos_router::hooks::use_navigate;

use crate::ipc;

/// What the action button does.
#[derive(Debug, Clone)]
pub enum NoticeAction {
    /// Opens an in-app route.
    Navigate(String),
    /// Runs a callback.
    Run(Callback<()>),
}

#[derive(Debug, Clone)]
pub struct Notice {
    pub key: Option<String>,
    pub tone: Tone,
    pub title: String,
    pub body: Option<String>,
    pub action: Option<(String, NoticeAction)>,
    /// Time on screen; `None` stays until dismissed.
    pub duration_ms: Option<u32>,
}

impl Notice {
    pub fn new(tone: Tone, title: impl Into<String>) -> Self {
        Self {
            key: None,
            tone,
            title: title.into(),
            body: None,
            action: None,
            duration_ms: Some(8_000),
        }
    }

    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    pub fn action(mut self, label: impl Into<String>, action: NoticeAction) -> Self {
        self.action = Some((label.into(), action));
        self
    }

    pub fn duration_ms(mut self, ms: Option<u32>) -> Self {
        self.duration_ms = ms;
        self
    }
}

#[derive(Debug, Clone)]
struct Shown {
    id: u64,
    notice: Notice,
}

#[derive(Debug, Clone, Copy)]
pub struct Notices {
    shown: RwSignal<Vec<Shown>>,
    dismissed: StoredValue<HashSet<String>>,
    next: StoredValue<u64>,
}

impl Notices {
    fn new() -> Self {
        Self {
            shown: RwSignal::new(Vec::new()),
            dismissed: StoredValue::new(HashSet::new()),
            next: StoredValue::new(0),
        }
    }

    /// Shows a notice, unless one with its key was dismissed this session.
    pub fn show(self, notice: Notice) {
        if let Some(key) = &notice.key
            && self.dismissed.with_value(|d| d.contains(key))
        {
            return;
        }
        let Some(id) = self.next.try_update_value(|n| {
            *n += 1;
            *n
        }) else {
            return;
        };
        let duration = notice.duration_ms;
        self.shown.update(|list| {
            if let Some(key) = &notice.key {
                list.retain(|s| s.notice.key.as_ref() != Some(key));
            }
            list.push(Shown { id, notice });
        });
        if let Some(ms) = duration {
            leptos::task::spawn_local(async move {
                ipc::sleep(ms).await;
                self.expire(id);
            });
        }
    }

    /// Removes a notice without silencing its key.
    fn expire(self, id: u64) {
        let _ = self.shown.try_update(|l| l.retain(|s| s.id != id));
    }

    /// The user closed it (or acted on it): its key stays quiet.
    fn dismiss(self, id: u64) {
        let key = self.shown.with_untracked(|l| {
            l.iter()
                .find(|s| s.id == id)
                .and_then(|s| s.notice.key.clone())
        });
        if let Some(key) = key {
            self.dismissed.update_value(|d| {
                d.insert(key);
            });
        }
        self.expire(id);
    }
}

pub(crate) fn provide() -> Notices {
    let notices = Notices::new();
    provide_context(notices);
    notices
}

/// The app's notices. Outside the app (a stray root) this is a detached
/// set nobody renders, so a call is harmless rather than a panic.
pub fn use_notices() -> Notices {
    use_context::<Notices>().unwrap_or_else(Notices::new)
}

/// The stack itself. Must sit inside the router: actions navigate.
#[component]
pub(crate) fn NoticeLayer(notices: Notices) -> impl IntoView {
    let navigate = use_navigate();
    view! {
        <div
            aria-live="polite"
            class="pointer-events-none fixed right-4 bottom-4 z-[210] flex w-[min(26rem,calc(100vw-2rem))] flex-col-reverse gap-2"
        >
            <For each=move || notices.shown.get() key=|s| s.id let:shown>
                {
                    let id = shown.id;
                    let Notice { tone, title, body, action, .. } = shown.notice;
                    let navigate = navigate.clone();
                    let action = action
                        .map(|(label, action)| {
                            view! {
                                <div class="mt-3">
                                    <Button
                                        variant=Variant::Solid
                                        size=Size::Sm
                                        on_click=Callback::new(move |()| {
                                            match &action {
                                                NoticeAction::Navigate(to) => navigate(to, Default::default()),
                                                NoticeAction::Run(run) => run.run(()),
                                            }
                                            notices.dismiss(id);
                                        })
                                    >
                                        {label}
                                    </Button>
                                </div>
                            }
                        });
                    view! {
                        <div role="status" class="pointer-events-auto rounded-2xl bg-black/90 shadow-2xl">
                            <div class=cn!("flex items-start gap-3 rounded-2xl border p-4", tone.soft())>
                                <div class="min-w-0 flex-1">
                                    <div class="text-sm font-semibold">{title}</div>
                                    {body.map(|b| view! { <div class="mt-1 text-sm leading-6 opacity-80">{b}</div> })}
                                    {action}
                                </div>
                                <button
                                    type="button"
                                    aria-label="Dismiss"
                                    class="-m-1 shrink-0 rounded-lg p-1 opacity-60 transition-opacity hover:opacity-100"
                                    on:click=move |_| notices.dismiss(id)
                                >
                                    <Icon icon=icons::X class="h-4 w-4" />
                                </button>
                            </div>
                        </div>
                    }
                }
            </For>
        </div>
    }
}
