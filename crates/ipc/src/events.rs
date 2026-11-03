use crate::define_events;
use crate::dto;

define_events! {
    /// Sign-in state changed: login completed, refresh failed, or logout.
    "auth:session" => SessionChanged(dto::SessionState);
    /// Preferences were saved.
    "prefs:changed" => PreferencesChanged(dto::Preferences);
    /// A simulator started or stopped.
    "sim:status" => SimStatusChanged(dto::SimStatus);
    /// A job changed phase or made progress. Progress is throttled.
    "jobs:changed" => JobChanged(dto::Job);
    /// The set of installed products changed.
    "installed:changed" => InstalledChanged(Vec<dto::InstalledPackage>);
    /// A gauge sent an event over a bridge.
    "bridge:event" => BridgeEventReceived(dto::BridgeMessage);
    /// A bridge gained or lost its gauges.
    "bridge:connection" => BridgeConnectionChanged(dto::ConnectionChange);
    /// The EFB server started, stopped, or a tablet came or went.
    "efb:changed" => EfbChanged(dto::EfbStatus);
}
