use std::sync::Arc;

use infinity_ipc::dto::SessionState;
use leptos::prelude::*;

#[derive(Debug, Clone, Default)]
pub struct Extensions {
    pub pages: Vec<Page>,
}

#[derive(Debug, Clone)]
pub struct Page {
    pub slug: &'static str,
    pub label: &'static str,
    pub gate: Gate,
    pub view: fn() -> AnyView,
}

impl Page {
    pub fn path(&self) -> String {
        path(self.slug)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    Everyone,
    SignedIn,
    Permission(&'static str),
}

impl Gate {
    pub fn allows(self, session: &SessionState) -> bool {
        match (self, session) {
            (Self::Everyone, _) => true,
            (Self::SignedIn, SessionState::SignedIn { .. }) => true,
            (Self::Permission(p), SessionState::SignedIn { profile }) => profile.has_permission(p),
            _ => false,
        }
    }
}

pub(crate) fn path(slug: &str) -> String {
    format!("/x/{slug}")
}

pub(crate) fn provide(extensions: Extensions) {
    provide_context(Arc::new(extensions));
}

pub(crate) fn use_extensions() -> Arc<Extensions> {
    use_context::<Arc<Extensions>>().unwrap_or_default()
}
