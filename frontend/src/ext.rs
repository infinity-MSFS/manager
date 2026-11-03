//! What an edition can add to the community UI without forking it.
//!
//! Every slot takes plain `fn` pointers so an [`Extensions`] value is built
//! once at start-up, before any reactive owner exists, and each view is
//! created where the slot renders: inside the router, the app state and the
//! notice layer, so extension views can use all three.

use std::sync::Arc;

use infinity_ipc::catalog::Product;
use infinity_ipc::dto::SessionState;
use leptos::prelude::*;

#[derive(Debug, Clone, Default)]
pub struct Extensions {
    /// Pages of their own, linked from the nav bar at `/x/<slug>`.
    pub pages: Vec<Page>,
    /// Sections on the Settings page, after the built-in integrations.
    pub settings: Vec<SettingsSection>,
    /// Panels at the top of every configurator, above the product's own
    /// schema. Each decides for itself whether it applies to the product.
    pub configurator: Vec<ConfiguratorPanel>,
    /// Views mounted once for the life of the window, for work that is
    /// not tied to a page: listening for events, raising notices.
    pub background: Vec<fn() -> AnyView>,
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

#[derive(Debug, Clone)]
pub struct SettingsSection {
    pub label: &'static str,
    pub gate: Gate,
    pub view: fn() -> AnyView,
}

#[derive(Debug, Clone)]
pub struct ConfiguratorPanel {
    /// Called with the product being configured; render nothing (an empty
    /// view) when the panel does not apply to it.
    pub view: fn(&Product) -> AnyView,
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
