use crate::catalog;
use crate::define_events;
use crate::dto;

define_events! {
    /// Sign-in state changed: login completed, refresh failed, or logout.
    "auth:session" => SessionChanged(dto::SessionState);
    /// What the account owns, after the manager rechecked it (window focus,
    /// a refused download, or Refresh purchases). `None` when signed out.
    "account:entitlements" => EntitlementsChanged(Option<catalog::EntitlementsResponse>);
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
    /// A namespace's saved equipment changed.
    "equipment:profile" => EquipmentProfileUpdated(dto::EquipmentProfileChanged);
    /// Preset visibility changed under a running sim: restart it to see.
    "presets:restart-sim" => SimRestartNeeded(dto::PresetsChanged);
    /// The EFB server started, stopped, or a tablet came or went.
    "efb:changed" => EfbChanged(dto::EfbStatus);
}
