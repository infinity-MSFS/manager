//! Who wants the FLIR, and when the DLLs should leave the sim.
//!
//! Every HUD gauge reports under its own `source` about 15 times a second
//! while it is alive, wanting a picture or not. From that:
//!
//! - the picture to develop is the freshest *active* report;
//! - the DLLs come out of the sim [`GOODBYE_GRACE`] after the last gauge
//!   says goodbye, unless a gauge reports again first; when every gauge has
//!   gone quiet for [`GONE_AFTER`] (killed without a goodbye, the sim back
//!   at the menu); or when the gauges are alive but nobody has wanted a
//!   picture for [`UNUSED_AFTER`] (a stowed HUD should not cost a second
//!   scene view).
//!
//! A goodbye does not unload at once because "restart flight" says goodbye
//! too, and the sim is then tearing down and rebuilding the world: the worst
//! moment to unhook its GPU and erase a scene view (a restart that did that
//! hung the GPU). The core recreates its nose view by itself when the
//! reload takes it, so a restarted flight finds everything in place, and a
//! flight that really ended is unloaded once the sim has settled.
//!
//! Pure bookkeeping, no clock of its own: every call takes `now`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use flir_stream::StateReport;

/// A gauge counts as alive this long after its last report.
pub const REQUEST_TTL: Duration = Duration::from_secs(2);
/// Every gauge quiet this long: they are gone. Longer than a flight reload,
/// for the same reason as [`GOODBYE_GRACE`].
pub const GONE_AFTER: Duration = Duration::from_secs(45);
/// After the last goodbye, how long a restarted flight has to report again
/// before the DLLs come out.
pub const GOODBYE_GRACE: Duration = Duration::from_secs(45);
/// Gauges alive but none wanting a picture for this long: release the view.
pub const UNUSED_AFTER: Duration = Duration::from_secs(60);

/// Why the DLLs are coming out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Release {
    /// The last gauge said goodbye.
    Goodbye,
    /// Every gauge went quiet.
    Gone,
    /// Nobody wanted a picture for a while.
    Unused,
    /// Asked for (the panel's button, the manager exiting).
    Asked,
    /// The DLLs on disk changed under the injected copies, or the sim holds
    /// a build other than this manager's.
    Rebuilt,
    /// The bridge stopped beating (the sim exited or the bridge hung).
    Stalled,
    /// The core could not get its nose view back for [`nose_patience`]:
    /// a fresh injection is the last resort.
    NoseLost,
}

/// How long the core may try to get its nose view back by itself before the
/// manager re-injects it. The core recreates a lost view within seconds; it
/// failing for this long means its state is wrong, not the sim's. Doubles
/// for each re-inject that did not bring the view back, so a sim that
/// cannot give a view at all is not churned.
pub fn nose_patience(resets: u32) -> Duration {
    const FIRST: Duration = Duration::from_secs(30);
    FIRST.saturating_mul(1 << resets.min(3))
}

impl Release {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Goodbye => "the aircraft's HUD closed (flight ended or restarted)",
            Self::Gone => "the HUD stopped reporting",
            Self::Unused => "the FLIR went unused",
            Self::Asked => "unload requested",
            Self::Rebuilt => "a different build of the viewports DLLs",
            Self::Stalled => "the bridge stopped responding",
            Self::NoseLost => "the nose view could not be reacquired",
        }
    }
}

#[derive(Debug)]
struct Seen {
    at: Instant,
    report: StateReport,
}

#[derive(Debug, Default)]
pub struct Demand {
    sources: HashMap<String, Seen>,
    /// Last report of any kind, and last active one.
    last_report: Option<Instant>,
    last_active: Option<Instant>,
    /// When a goodbye left no gauge behind.
    farewell: Option<Instant>,
}

impl Demand {
    /// A state report, active or not.
    pub fn report(&mut self, report: StateReport, now: Instant) {
        self.last_report = Some(now);
        if report.active {
            self.last_active = Some(now);
        }
        // A gauge that reports again (a restarted flight) cancels a pending
        // farewell: the release would only be followed by a re-inject.
        self.farewell = None;
        self.sources
            .insert(report.source.clone(), Seen { at: now, report });
    }

    /// A gauge leaving. The last one alive starts [`GOODBYE_GRACE`].
    pub fn goodbye(&mut self, source: &str, now: Instant) {
        self.sources.remove(source);
        self.prune(now);
        if self.sources.is_empty() && self.farewell.is_none() {
            self.farewell = Some(now);
        }
    }

    fn prune(&mut self, now: Instant) {
        self.sources
            .retain(|_, s| now.saturating_duration_since(s.at) <= REQUEST_TTL);
    }

    /// The picture to develop: the freshest active report of a live gauge.
    pub fn request(&mut self, now: Instant) -> Option<StateReport> {
        self.prune(now);
        self.sources
            .values()
            .filter(|s| s.report.active)
            .max_by_key(|s| s.at)
            .map(|s| s.report.clone())
    }

    /// The gauges currently alive, sorted.
    pub fn sources(&mut self, now: Instant) -> Vec<String> {
        self.prune(now);
        let mut v: Vec<String> = self.sources.keys().cloned().collect();
        v.sort();
        v
    }

    /// Whether DLLs injected at `since` should come out now, and why.
    pub fn release_due(&mut self, since: Instant, now: Instant) -> Option<Release> {
        if let Some(at) = self.farewell {
            // Nothing else releases during the grace: the gauges are silent
            // because the flight is reloading.
            if now.saturating_duration_since(at.max(since)) < GOODBYE_GRACE {
                return None;
            }
            self.farewell = None;
            return Some(Release::Goodbye);
        }
        let quiet =
            |t: Option<Instant>| now.saturating_duration_since(t.map_or(since, |t| t.max(since)));
        if quiet(self.last_report) > GONE_AFTER {
            return Some(Release::Gone);
        }
        if quiet(self.last_active) > UNUSED_AFTER {
            return Some(Release::Unused);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(source: &str, active: bool) -> StateReport {
        StateReport {
            active,
            size: 256,
            polarity: 0,
            env: Default::default(),
            source: source.into(),
        }
    }

    #[test]
    fn the_freshest_active_gauge_drives_the_picture() {
        let t = Instant::now();
        let mut d = Demand::default();
        d.report(report("capt", true), t);
        d.report(report("fo", false), t + Duration::from_millis(100));
        assert_eq!(
            d.request(t + Duration::from_millis(200)).unwrap().source,
            "capt"
        );
        assert!(
            d.request(t + REQUEST_TTL + Duration::from_secs(1))
                .is_none()
        );
    }

    #[test]
    fn one_hud_leaving_keeps_the_other() {
        let t = Instant::now();
        let mut d = Demand::default();
        d.report(report("capt", true), t);
        d.report(report("fo", true), t);
        d.goodbye("capt", t);
        assert_eq!(d.release_due(t, t), None);
        assert_eq!(d.request(t).unwrap().source, "fo");
        d.goodbye("fo", t);
        assert_eq!(d.release_due(t, t), None, "not during the grace");
        let later = t + GOODBYE_GRACE;
        assert_eq!(d.release_due(t, later), Some(Release::Goodbye));
        assert_eq!(d.release_due(t, later), None, "said once");
    }

    #[test]
    fn a_restarted_flight_cancels_the_farewell() {
        let t = Instant::now();
        let mut d = Demand::default();
        d.report(report("capt", true), t);
        d.goodbye("capt", t);
        // The flight reloads in silence, past GONE_AFTER's ordinary reach...
        assert_eq!(d.release_due(t, t + GOODBYE_GRACE / 2), None);
        // ...and its HUD reports again: nothing comes out.
        let back = t + GOODBYE_GRACE - Duration::from_secs(1);
        d.report(report("capt", true), back);
        assert_eq!(d.release_due(t, back + GOODBYE_GRACE), None);
    }

    #[test]
    fn silence_and_disuse_release() {
        let t = Instant::now();
        let mut d = Demand::default();
        d.report(report("capt", true), t);
        assert_eq!(d.release_due(t, t + GONE_AFTER), None);
        assert_eq!(
            d.release_due(t, t + GONE_AFTER + Duration::from_secs(1)),
            Some(Release::Gone)
        );

        // Alive (stowed HUD) but not wanting a picture.
        let mut d = Demand::default();
        d.report(report("capt", true), t);
        let mut now = t;
        while now < t + UNUSED_AFTER {
            now += Duration::from_secs(1);
            d.report(report("capt", false), now);
        }
        assert_eq!(d.release_due(t, now), None);
        now += Duration::from_secs(2);
        d.report(report("capt", false), now);
        assert_eq!(d.release_due(t, now), Some(Release::Unused));
    }

    #[test]
    fn nose_patience_grows_and_caps() {
        assert_eq!(nose_patience(0), Duration::from_secs(30));
        assert_eq!(nose_patience(1), Duration::from_secs(60));
        assert_eq!(nose_patience(3), Duration::from_secs(240));
        assert_eq!(nose_patience(50), Duration::from_secs(240));
    }

    #[test]
    fn the_clock_starts_at_injection() {
        // Reports from long before an injection do not release it at once.
        let t = Instant::now();
        let mut d = Demand::default();
        d.report(report("capt", true), t);
        let injected = t + Duration::from_secs(120);
        assert_eq!(d.release_due(injected, injected), None);
    }
}
