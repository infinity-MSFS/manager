//! Navigraph account linking (device flow) and navdata downloads.
//!
//! [`Navigraph`] owns the link: it runs the OAuth device flow, keeps a live
//! access token through [`TokenManager`] and persists the rotating refresh
//! token through a [`RefreshTokenStore`]. The Navigraph app credentials come
//! from a [`CredentialsSource`]; this crate holds no secrets and knows no
//! backend, so builds plug their own implementations in.
//!
//! Navdata: [`navdata_report`] compares each product's installed
//! `cycle.json` with the latest package, [`plan_navdata_update`] picks the
//! package and [`install_navdata`] puts it in `<work>/NavigationData`.

mod account;
mod client;
mod error;
mod install;
mod navdata;
mod secret;
mod store;
mod token;
mod updater;

#[cfg(test)]
mod test_server;
#[cfg(test)]
mod tests;

pub use account::Navigraph;
pub use client::{
    AppCredentials, DeviceAuth, Endpoints, NavTokens, NavigraphClient, Pkce, Poll, SCOPES,
    TileCookies, parse_tile_cookies, token_scopes,
};
pub use error::{NavigraphError, Result};
pub use infinity_ipc::navigraph::{
    LinkPoll, LinkStart, NavdataItem, NavdataLatest, NavdataReport, NavigraphStatus,
};
pub use install::{NavdataProgress, download_client, install_navdata};
pub use navdata::{
    CycleJson, NavdataPackage, NavdataPackageFile, airac_validity, is_update, select_package,
    sha256_hex,
};
pub use secret::Secret;
pub use store::{
    CredentialsSource, FileTokenStore, MemoryTokenStore, NoCredentials, RefreshTokenStore,
    StaticCredentials,
};
pub use token::{TokenExchanger, TokenManager, TokenStatus};
pub use updater::{
    CYCLE_FILE, NAVDATA_DIR, NavdataTarget, NavdataUpdatePlan, navdata_job_id, navdata_report,
    plan_navdata_update, read_cycle_json,
};
