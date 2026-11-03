//! SimBrief flight plan fetching and conversion.
//!
//! [`SimBriefClient`] queries the public SimBrief fetcher by pilot ID and
//! lowers the OFP to [`SimBriefPayload`], the wire payload the WASM gauges
//! (T-38, UNS-1) and the EFB consume. Its schema mirrors
//! `wasm/shared/flight-plan-import/src/sources_simbrief.rs` in the
//! T-38C-WASM repo; keep them in lockstep.
//!
//! **Airway rule:** the gauges need per-fix waypoints, which SimBrief gives
//! in `navlog.fix[]`. The airway-collapsed `route` string is never used.
//!
//! [`wire`] holds the bridge/EFB envelopes and pilot-ID handling shared by
//! every transport.

mod client;
mod error;
mod payload;
mod response;
pub mod wire;

#[cfg(test)]
mod test_server;

pub use client::{DEFAULT_BASE_URL, SimBriefClient, to_payload};
pub use error::SimBriefError;
pub use payload::{SIMBRIEF_WIRE_VERSION, SimBriefPayload, SimBriefWaypoint};
pub use response::{
    SimBriefAirport, SimBriefFetchStatus, SimBriefGeneral, SimBriefNavlog, SimBriefOfp,
    SimBriefOfpFix, parse_navlog_fixes,
};
