//! Custom pages (v3 `pages/index.tsx`): tabs whose UX does not fit a flat
//! field grid. A group `{ "id": ..., "page": "<name>" }` renders the page
//! instead of fields; the page reads and writes the whole value tree and
//! takes page-specific props from its group node.

mod aircraft_defaults;
mod approaches;
mod communications;
mod declutter;
mod destinations;
mod equipment;
mod flight_plans;
mod weapons;
mod zones;

use std::sync::Arc;

use infinity_ui::prelude::*;
use serde_json::Value;

use super::state::Configurator;

#[derive(Debug, Clone)]
pub(crate) struct PageCtx {
    pub cfg: Configurator,
    /// The schema group that declared the page.
    pub group: Arc<Value>,
}

/// The page registered as `name`.
pub(crate) fn render(name: &str, ctx: PageCtx) -> Option<AnyView> {
    Some(match name {
        "aircraftDefaults" => aircraft_defaults::page(ctx),
        "approaches" => approaches::page(ctx),
        "communications" => communications::page(ctx),
        "declutter" => declutter::page(ctx),
        "destinations" => destinations::page(ctx),
        // Live bridge page: state lives in the running aircraft.
        "equipment" => equipment::page(ctx),
        "flightPlans" => flight_plans::page(ctx),
        "weapons" => weapons::page(ctx),
        "zones" => zones::page(ctx),
        _ => return None,
    })
}
