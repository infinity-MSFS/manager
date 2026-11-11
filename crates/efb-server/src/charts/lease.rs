//! The sim-session lease: whether charts may be served at all.
//!
//! Three independent signals, all required, checked on every request:
//!
//! 1. **The simulator process is running** ([`SimPresence::sim_running`],
//!    the host's process watch).
//! 2. **An aircraft module is connected** ([`SimPresence::aircraft_connected`],
//!    the host's WASM bridges). A WASM gauge only runs inside a loaded
//!    flight, so this is the aircraft, not merely the sim's main menu.
//! 3. **The in-sim EFB panel checked in** within [`PANEL_LEASE`]: a loopback
//!    client's `charts.alive` cmd, or any `sim` frame it publishes. Remote
//!    tablets can never refresh it.
//!
//! No presence plugged in reads as locked: the lease fails CLOSED. Each
//! signal alone is easy to fake; faking all three means running the
//! simulator with the aircraft loaded, which is the licence's condition.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

/// How long one panel check-in keeps the lease up. The panel checks in every
/// 3 s and on every telemetry frame; 15 s rides out a stutter, a
/// loading screen or the panel's own remount, and still revokes a tablet
/// within seconds of the aircraft going away.
pub const PANEL_LEASE: Duration = Duration::from_secs(15);

/// What the host knows about the simulator. Must answer instantly (cached
/// state, no I/O): it is asked on every chart request.
pub trait SimPresence: Send + Sync {
    fn sim_running(&self) -> bool;
    fn aircraft_connected(&self) -> bool;
}

/// Why the lease is locked, first failing signal first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LockReason {
    /// The host wired no presence; charts are off in this build.
    NoPresence,
    SimNotRunning,
    AircraftNotLoaded,
    /// The sim and aircraft are up but the cockpit EFB has not checked in.
    PanelNotRunning,
}

impl LockReason {
    pub fn message(self) -> &'static str {
        match self {
            Self::NoPresence => "Charts are not available in this build.",
            Self::SimNotRunning => "Charts are only available while the simulator is running.",
            Self::AircraftNotLoaded => "Charts are only available with the aircraft loaded.",
            Self::PanelNotRunning => {
                "Charts are only available while the aircraft's EFB is running in the cockpit."
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Access {
    pub live: bool,
    pub reason: Option<LockReason>,
}

impl Access {
    pub const LIVE: Self = Self {
        live: true,
        reason: None,
    };

    pub fn locked(reason: LockReason) -> Self {
        Self {
            live: false,
            reason: Some(reason),
        }
    }
}

#[derive(Debug)]
pub(crate) struct Lease {
    window: Duration,
    panel_seen: Mutex<Option<Instant>>,
}

impl Lease {
    pub(crate) fn new(window: Duration) -> Self {
        Self {
            window,
            panel_seen: Mutex::new(None),
        }
    }

    /// The in-sim panel checked in. Callers must have established the
    /// client is loopback.
    pub(crate) fn panel_alive(&self) {
        self.panel_alive_at(Instant::now());
    }

    pub(crate) fn panel_alive_at(&self, at: Instant) {
        if let Ok(mut seen) = self.panel_seen.lock() {
            *seen = Some(seen.map_or(at, |prev| prev.max(at)));
        }
    }

    pub(crate) fn access(&self, presence: Option<&dyn SimPresence>) -> Access {
        self.access_at(presence, Instant::now())
    }

    pub(crate) fn access_at(&self, presence: Option<&dyn SimPresence>, now: Instant) -> Access {
        let Some(presence) = presence else {
            return Access::locked(LockReason::NoPresence);
        };
        if !presence.sim_running() {
            return Access::locked(LockReason::SimNotRunning);
        }
        if !presence.aircraft_connected() {
            return Access::locked(LockReason::AircraftNotLoaded);
        }
        // A poisoned lock fails closed like everything else here.
        let seen = self.panel_seen.lock().ok().and_then(|s| *s);
        match seen {
            Some(at) if now.saturating_duration_since(at) <= self.window => Access::LIVE,
            _ => Access::locked(LockReason::PanelNotRunning),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[derive(Default)]
    struct Fake {
        sim: AtomicBool,
        aircraft: AtomicBool,
    }

    impl SimPresence for Fake {
        fn sim_running(&self) -> bool {
            self.sim.load(Ordering::SeqCst)
        }
        fn aircraft_connected(&self) -> bool {
            self.aircraft.load(Ordering::SeqCst)
        }
    }

    #[test]
    fn every_signal_is_required_and_reported_in_order() {
        let lease = Lease::new(PANEL_LEASE);
        let fake = Fake::default();
        let t0 = Instant::now();
        let at = |lease: &Lease, p: &Fake| lease.access_at(Some(p), t0);

        assert_eq!(
            lease.access_at(None, t0),
            Access::locked(LockReason::NoPresence)
        );
        lease.panel_alive_at(t0);
        assert_eq!(at(&lease, &fake), Access::locked(LockReason::SimNotRunning));
        fake.sim.store(true, Ordering::SeqCst);
        assert_eq!(
            at(&lease, &fake),
            Access::locked(LockReason::AircraftNotLoaded)
        );
        fake.aircraft.store(true, Ordering::SeqCst);
        assert_eq!(at(&lease, &fake), Access::LIVE);
        // The sim quitting revokes at once, whatever the panel said.
        fake.sim.store(false, Ordering::SeqCst);
        assert!(!at(&lease, &fake).live);
    }

    #[test]
    fn the_panel_check_in_expires() {
        let lease = Lease::new(PANEL_LEASE);
        let fake = Fake::default();
        fake.sim.store(true, Ordering::SeqCst);
        fake.aircraft.store(true, Ordering::SeqCst);
        let t0 = Instant::now();
        assert_eq!(
            lease.access_at(Some(&fake), t0),
            Access::locked(LockReason::PanelNotRunning)
        );
        lease.panel_alive_at(t0);
        assert!(lease.access_at(Some(&fake), t0 + PANEL_LEASE).live);
        assert_eq!(
            lease.access_at(Some(&fake), t0 + PANEL_LEASE + Duration::from_millis(1)),
            Access::locked(LockReason::PanelNotRunning)
        );
        // A stale check-in arriving late cannot rewind a newer one.
        lease.panel_alive_at(t0 + PANEL_LEASE);
        lease.panel_alive_at(t0);
        assert!(lease.access_at(Some(&fake), t0 + PANEL_LEASE * 2).live);
    }

    #[test]
    fn reasons_serialise_for_the_wire() {
        assert_eq!(
            serde_json::to_value(Access::locked(LockReason::AircraftNotLoaded)).ok(),
            Some(serde_json::json!({ "live": false, "reason": "aircraft_not_loaded" }))
        );
    }
}
