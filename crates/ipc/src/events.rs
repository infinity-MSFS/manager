use crate::define_events;
use crate::dto;

define_events! {
    /// Sign-in state changed: login completed, refresh failed, or logout.
    "auth:session" => SessionChanged(dto::SessionState);
    /// Preferences were saved.
    "prefs:changed" => PreferencesChanged(dto::Preferences);
    /// A simulator started or stopped.
    "sim:status" => SimStatusChanged(dto::SimStatus);
}
