mod app;
mod auth;
mod catalog;
mod prefs;
mod sim;

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
    auth::auth_session,
    auth::auth_begin_login,
    auth::auth_logout,
    prefs::prefs_load,
    prefs::prefs_save,
    sim::sim_status,
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
