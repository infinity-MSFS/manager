//! Transient notices for action results (v3 `lib/toast`).

use infinity_ipc::IpcError;
use infinity_ui::prelude::*;

use crate::ipc;

const TOAST_MS: u32 = 4500;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Toasts {
    list: RwSignal<Vec<Toast>>,
    next: StoredValue<u64>,
}

impl Toasts {
    pub(crate) fn new() -> Self {
        Self {
            list: RwSignal::new(Vec::new()),
            next: StoredValue::new(0),
        }
    }

    pub(crate) fn show(self, tone: Tone, title: impl Into<String>, description: Option<String>) {
        let Some(id) = self.next.try_update_value(|n| {
            *n += 1;
            *n
        }) else {
            return;
        };
        let mut toast = Toast::new(id, title).with_tone(tone);
        if let Some(d) = description {
            toast = toast.with_description(d);
        }
        self.list.update(|l| l.push(toast));
        leptos::task::spawn_local(async move {
            ipc::sleep(TOAST_MS).await;
            self.dismiss(id);
        });
    }

    pub(crate) fn success(self, title: impl Into<String>, description: impl Into<String>) {
        self.show(Tone::Accent, title, Some(description.into()));
    }

    pub(crate) fn info(self, title: impl Into<String>, description: impl Into<String>) {
        self.show(Tone::Info, title, Some(description.into()));
    }

    pub(crate) fn warn(self, title: impl Into<String>, description: impl Into<String>) {
        self.show(Tone::Warning, title, Some(description.into()));
    }

    pub(crate) fn error(self, title: impl Into<String>, error: &IpcError) {
        self.show(Tone::Danger, title, Some(error.message.clone()));
    }

    pub(crate) fn dismiss(self, id: u64) {
        let _ = self.list.try_update(|l| l.retain(|t| t.id != id));
    }
}

#[component]
pub(crate) fn ToastLayer(toasts: Toasts) -> impl IntoView {
    view! {
        <ToastHost
            toasts=toasts.list
            on_dismiss=Callback::new(move |id| toasts.dismiss(id))
            bottom=true
        />
    }
}
