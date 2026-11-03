mod app;
mod auth;
mod bridge;
mod catalog;
mod configurator;
mod efb;
pub(crate) mod install;
pub(crate) mod migrate;
pub(crate) mod nav;
pub(crate) mod navigraph;
mod prefs;
mod sim;
mod simbrief;
mod supervisor;

use infinity_ipc::{Command, IpcResult};

pub(crate) type Args<C> = <C as Command>::Args;
pub(crate) type Out<C> = IpcResult<<C as Command>::Output>;

macro_rules! handlers {
    ($($module:ident::$name:ident),* $(,)?) => {
        #[cfg(test)]
        const REGISTERED: &[&str] = &[$(stringify!($name)),*];

        pub(crate) fn invoke_handler()
            -> impl Fn(tauri::ipc::Invoke<tauri::Wry>) -> bool + Send + Sync + 'static
        {
            tauri::generate_handler![$($module::$name),*]
        }
    };
}

handlers![
    app::app_info,
    app::capabilities,
    catalog::catalog_fetch,
    catalog::catalog_changelog,
    catalog::catalog_liveries,
    catalog::account_entitlements,
    auth::auth_session,
    auth::auth_begin_login,
    auth::auth_logout,
    prefs::prefs_load,
    prefs::prefs_save,
    sim::sim_status,
    sim::sim_paths,
    sim::sim_probe_work_root,
    sim::sim_probe_wine_prefix,
    sim::sim_detected_prefixes,
    sim::dialog_pick_folder,
    install::installed_list,
    install::installed_rescan,
    install::installed_size,
    install::installed_uninstall,
    install::install_start,
    install::install_pause,
    install::install_cancel,
    install::jobs_list,
    configurator::configurator_load,
    configurator::configurator_save,
    configurator::configurator_emit,
    configurator::configurator_export,
    configurator::configurator_share_save,
    configurator::configurator_share_load,
    configurator::configurator_reset,
    bridge::bridge_ensure,
    bridge::bridge_status,
    bridge::bridge_command,
    nav::nav_cycle,
    nav::nav_is_valid_icao,
    nav::nav_runways,
    nav::nav_runway_details,
    nav::nav_airports_by_name,
    nav::nav_approach_types,
    nav::nav_approach_data,
    nav::nav_search_waypoints,
    nav::nav_nearest_waypoints,
    nav::nav_waypoint,
    nav::nav_airspace_boundary,
    nav::nav_airspace_boundary_normalized,
    nav::nav_nearby_airspace,
    nav::nav_search_airspace,
    nav::nav_is_valid_airspace,
    nav::nav_expand_training_areas,
    nav::mtr_search,
    nav::mtr_nearby,
    nav::mtr_segments,
    navigraph::navigraph_status,
    navigraph::navigraph_link_start,
    navigraph::navigraph_link_poll,
    navigraph::navigraph_unlink,
    navigraph::navdata_status,
    navigraph::navdata_update,
    simbrief::simbrief_set_pilot_id,
    simbrief::simbrief_fetch_plan,
    efb::efb_status,
    efb::efb_lan_urls,
    efb::efb_pairing_code,
    efb::efb_disconnect,
    efb::efb_forget_devices,
    efb::efb_open_docs_folder,
    supervisor::svc_status,
    supervisor::svc_install,
    supervisor::svc_install_and_start,
    supervisor::svc_start,
    supervisor::svc_stop,
    supervisor::svc_uninstall,
    supervisor::svc_update,
    migrate::config_migrate_v1,
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    #[test]
    fn every_contract_command_has_exactly_one_handler() {
        let contract: BTreeSet<_> = infinity_ipc::commands::ALL.iter().collect();
        let registered: BTreeSet<_> = super::REGISTERED.iter().collect();
        assert_eq!(
            super::REGISTERED.len(),
            registered.len(),
            "duplicate handler registered"
        );
        let missing: Vec<_> = contract.difference(&registered).collect();
        let extra: Vec<_> = registered.difference(&contract).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "handlers out of sync with infinity_ipc::commands. missing: {missing:?}, not in contract: {extra:?}"
        );
    }
}
