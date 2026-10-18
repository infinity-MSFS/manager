use crate::catalog;
use crate::define_commands;
use crate::dto;

define_commands! {
    /// The product catalog for a release channel.
    catalog_fetch => CatalogFetch(dto::Channel) -> catalog::Catalog;

    /// Build, version and platform.
    app_info => AppInfo(()) -> dto::AppInfo;
    /// Feature switches the UI renders from.
    capabilities => Capabilities(()) -> dto::Capabilities;

    /// Current sign-in state.
    auth_session => AuthSession(()) -> dto::SessionState;
    /// Starts a browser sign-in. Completion arrives as
    /// [`crate::events::SessionChanged`].
    auth_begin_login => AuthBeginLogin(()) -> dto::LoginStarted;
    auth_logout => AuthLogout(()) -> ();

    prefs_load => PrefsLoad(()) -> dto::Preferences;
    /// Saves and broadcasts [`crate::events::PreferencesChanged`].
    prefs_save => PrefsSave(dto::Preferences) -> ();

    /// Which simulator is running now. Changes arrive as
    /// [`crate::events::SimStatusChanged`].
    sim_status => SimStatus(()) -> dto::SimStatus;
}
