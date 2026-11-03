use crate::catalog;
use crate::define_commands;
use crate::dto;
use crate::nav;
use crate::navigraph;

define_commands! {
    /// The product catalog for a release channel.
    catalog_fetch => CatalogFetch(dto::Channel) -> catalog::Catalog;

    /// Release notes, newest first. Empty when the product has none.
    catalog_changelog => CatalogChangelog(catalog::ProductId) -> Vec<catalog::ChangelogEntry>;
    /// Liveries published for an aircraft. Install one with
    /// [`InstallStart`] using [`dto::livery_job_id`].
    catalog_liveries => CatalogLiveries(catalog::ProductId) -> catalog::LiveryCatalog;
    /// What the signed-in account owns. `None` when this build has no
    /// store, in which case only free products are available.
    account_entitlements => AccountEntitlements(()) -> Option<catalog::EntitlementsResponse>;

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
    /// Discovered sim installs and the folder products install into.
    /// Probes the disk on every call.
    sim_paths => SimPaths(()) -> dto::SimPaths;
    /// What a candidate MSFS work folder holds, before saving it.
    sim_probe_work_root => SimProbeWorkRoot(String) -> dto::WorkRootProbe;
    /// What a candidate Proton/Wine prefix holds, before saving it.
    sim_probe_wine_prefix => SimProbeWinePrefix(String) -> dto::PrefixProbe;
    /// Proton prefixes found in Steam libraries that hold a sim.
    sim_detected_prefixes => SimDetectedPrefixes(()) -> Vec<dto::PrefixProbe>;
    /// The system folder picker. `None` when dismissed.
    dialog_pick_folder => DialogPickFolder(dto::PickFolder) -> Option<String>;

    /// Products installed by the manager or found on disk.
    installed_list => InstalledList(()) -> Vec<dto::InstalledPackage>;
    /// Looks in the install folder for every product in a channel's
    /// catalog, recording what is there and forgetting what is gone.
    installed_rescan => InstalledRescan(dto::Channel) -> Vec<dto::InstalledPackage>;
    /// Bytes on disk of an installed product, `None` when not installed.
    installed_size => InstalledSize(catalog::ProductId) -> Option<u64>;
    /// Deletes a product's package folder.
    installed_uninstall => InstalledUninstall(catalog::ProductId) -> ();

    /// Installs or updates a product, taking over a stopped job for it so a
    /// paused download resumes. Progress arrives as
    /// [`crate::events::JobChanged`].
    install_start => InstallStart(dto::InstallRequest) -> dto::Job;
    /// Stops a job, keeping the partial download.
    install_pause => InstallPause(catalog::ProductId) -> ();
    /// Stops a job and discards its partial download.
    install_cancel => InstallCancel(catalog::ProductId) -> ();
    /// A product's saved configuration, migrated to the catalog's current
    /// schema version.
    configurator_load => ConfiguratorLoad(dto::ProductRef) -> dto::LoadedConfig;
    configurator_save => ConfiguratorSave(dto::ConfiguratorSave) -> ();
    /// Runs one of the product's declared outputs (write a file, send to
    /// the aircraft over its bridge).
    configurator_emit => ConfiguratorEmit(dto::ConfiguratorEmit) -> dto::EmitResult;
    /// Writes the export file into the aircraft's work folder; returns its
    /// path.
    configurator_export => ConfiguratorExport(dto::ProductRef) -> String;
    /// Saves the configuration to a file the user picks. `None` when the
    /// dialog is dismissed.
    configurator_share_save => ConfiguratorShareSave(dto::ProductRef) -> Option<String>;
    /// Imports a configuration file the user picks, including v1 shares.
    configurator_share_load => ConfiguratorShareLoad(dto::ProductRef) -> Option<dto::ConfigEnvelope>;
    /// Deletes the saved configuration. `true` when there was one.
    configurator_reset => ConfiguratorReset(dto::ProductRef) -> bool;

    /// Starts the product's declared bridges if they are not running.
    bridge_ensure => BridgeEnsure(dto::ProductRef) -> Vec<dto::BridgeStatus>;
    bridge_status => BridgeStatusList(()) -> Vec<dto::BridgeStatus>;
    /// Sends a command to the gauges on a bridge and returns their answer.
    bridge_command => BridgeCommandSend(dto::BridgeCommand) -> serde_json::Value;

    /// AIRAC cycle of the product's navigation database.
    nav_cycle => NavCycle(dto::ProductRef) -> String;
    nav_is_valid_icao => NavIsValidIcao(nav::NavAirport) -> bool;
    nav_runways => NavRunways(nav::NavAirport) -> Vec<nav::RunwayData>;
    nav_runway_details => NavRunwayDetails(nav::NavRunway) -> nav::RunwayDetails;
    nav_airports_by_name => NavAirportsByName(nav::NavSearch) -> Vec<String>;
    nav_approach_types => NavApproachTypes(nav::NavRunway) -> nav::AvailableApproachTypes;
    nav_approach_data => NavApproachData(nav::NavApproach) -> nav::ApproachData;
    nav_search_waypoints => NavSearchWaypoints(nav::NavSearch) -> Vec<nav::WaypointData>;
    nav_nearest_waypoints => NavNearestWaypoints(nav::NavNearest) -> Vec<nav::WaypointData>;
    nav_waypoint => NavWaypointByIdentifier(nav::NavWaypoint) -> Option<nav::WaypointData>;
    nav_airspace_boundary => NavAirspaceBoundary(nav::NavAirspace) -> Vec<nav::RestrictiveAirspaceBoundaryPoint>;
    nav_airspace_boundary_normalized => NavAirspaceBoundaryNormalized(nav::NavAirspace) -> Vec<nav::BoundaryPointLL>;
    nav_nearby_airspace => NavNearbyAirspace(nav::NavNearby) -> Vec<nav::RestrictiveAirspaceDataWithDistance>;
    nav_search_airspace => NavSearchAirspace(nav::NavSearch) -> Vec<nav::RestrictiveAirspaceData>;
    nav_is_valid_airspace => NavIsValidAirspace(nav::NavAirspace) -> bool;
    /// Arcs in training-area boundaries expanded to plain polygons.
    nav_expand_training_areas => NavExpandTrainingAreas(Vec<nav::RestrictiveAirspaceBoundaryPoint>) -> Vec<Vec<nav::BoundaryPointLL>>;
    /// FAA military training routes (bundled database).
    mtr_search => MtrSearch(String) -> Vec<nav::MtrRouteSummary>;
    mtr_nearby => MtrNearbyRoutes(nav::MtrNearby) -> Vec<nav::MtrRouteWithDistance>;
    mtr_segments => MtrSegments(String) -> Vec<nav::MtrSegment>;

    navigraph_status => NavigraphStatus(()) -> navigraph::NavigraphStatus;
    /// Starts linking a Navigraph account (device flow) and opens the
    /// verification page in the browser. Poll with [`NavigraphLinkPoll`].
    navigraph_link_start => NavigraphLinkStart(()) -> navigraph::LinkStart;
    navigraph_link_poll => NavigraphLinkPoll(()) -> navigraph::LinkPoll;
    navigraph_unlink => NavigraphUnlink(()) -> ();
    /// Installed and latest navdata cycles for a channel's products.
    navdata_status => NavdataStatus(dto::Channel) -> navigraph::NavdataReport;
    /// Installs the latest navdata into an aircraft. Progress arrives as
    /// [`crate::events::JobChanged`] for the job `navdata-<productId>`.
    navdata_update => NavdataUpdate(dto::ProductRef) -> ();

    /// Saves the SimBrief pilot ID after checking its form; `None` clears it.
    simbrief_set_pilot_id => SimBriefSetPilotId(Option<String>) -> ();
    /// The latest OFP for the saved pilot ID.
    simbrief_fetch_plan => SimBriefFetchPlan(()) -> crate::simbrief::SimBriefPayload;

    /// The EFB server and the tablets connected to it. Sharing on the
    /// network follows [`dto::Preferences::efb_lan_share`].
    efb_status => EfbStatusGet(()) -> dto::EfbStatus;
    /// Addresses a tablet on this network can open.
    efb_lan_urls => EfbLanUrls(()) -> Vec<String>;
    /// A short code a tablet named `device` enters to pair.
    efb_pairing_code => EfbPairingCode(String) -> String;
    efb_disconnect => EfbDisconnect(u64) -> bool;
    /// Unpairs every tablet.
    efb_forget_devices => EfbForgetDevices(()) -> ();
    /// Opens the folder whose PDFs appear under "My Documents" in the EFB.
    efb_open_docs_folder => EfbOpenDocsFolder(()) -> ();

    /// Jobs in flight and stopped jobs.
    jobs_list => JobsList(()) -> Vec<dto::Job>;

    /// The launch supervisor. Probes the service manager on every call.
    svc_status => SvcStatus(()) -> dto::SupervisorStatus;
    /// On Windows every control verb shows an elevation prompt; declining
    /// it fails with `auth_forbidden`.
    svc_install => SvcInstall(()) -> ();
    /// Install and start under a single elevation prompt.
    svc_install_and_start => SvcInstallAndStart(()) -> ();
    svc_start => SvcStart(()) -> ();
    svc_stop => SvcStop(()) -> ();
    svc_uninstall => SvcUninstall(()) -> ();
    /// Replaces the installed supervisor with this build's.
    svc_update => SvcUpdate(()) -> ();

    /// Imports v1's settings once (it runs at startup too) and returns what
    /// that did. `None` when there is no v1 install.
    config_migrate_v1 => ConfigMigrateV1(()) -> Option<dto::V1Migration>;
}
