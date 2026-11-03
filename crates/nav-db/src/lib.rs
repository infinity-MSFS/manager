//! Navigation database queries: airports, runways, approaches, waypoints,
//! restrictive airspace and military training routes.
//!
//! Two read-only SQLite sources back the configurator's pickers:
//!
//! - [`NavDb`]: the aircraft's own navigation database, which its gauges
//!   keep at `<package work>/NavigationData/nav.s3db` and Navigraph navdata
//!   updates replace. One per product; find it with [`locate_nav_db`].
//! - [`MtrDb`]: the FAA military training routes, shipped with the app as
//!   `faa_mtr.sqlite`.
//!
//! Both hold one connection opened from an explicit path and do blocking
//! SQLite work on the calling thread: callers run queries on the blocking
//! pool. [`arc`] turns airspace boundaries into drawable rings.

mod approach;
pub mod arc;
mod db;
mod error;
mod helpers;
mod mtr;
mod nav;

#[cfg(test)]
mod fixture;

use std::path::{Path, PathBuf};

use infinity_sim_paths::{Locator, Sim};

pub use arc::{expand_training_areas, normalize_boundary_ring};
pub use error::{Error, Result};
pub use helpers::{
    clean_runway_number, haversine_km, haversine_nm, km_to_nm, normalize_approach_prefix,
};
pub use infinity_ipc::nav::{
    ApproachData, AvailableApproachTypes, BoundaryPointLL, MtrFamily, MtrLatLon, MtrRouteSummary,
    MtrRouteWithDistance, MtrSegment, RestrictiveAirspaceBoundaryPoint, RestrictiveAirspaceData,
    RestrictiveAirspaceDataWithDistance, RunwayData, RunwayDetails, WaypointData,
};
pub use mtr::{DEFAULT_NEARBY_ROUTES, MtrDb};
pub use nav::{DEFAULT_ARC_RESOLUTION, DEFAULT_NEAREST_WAYPOINTS, NavDb};

/// File name of the bundled MTR database, as a Tauri resource path.
pub const MTR_RESOURCE: &str = "resources/faa_mtr.sqlite";

/// Where an aircraft keeps its navigation database inside its work folder.
pub fn nav_db_path(work: &Path) -> PathBuf {
    work.join("NavigationData").join("nav.s3db")
}

/// The navigation database of the first package matching
/// `package_prefix`, searching `sims` in order. Fails with
/// [`Error::WorkDirMissing`] when no sim has a work folder for it and
/// [`Error::NotFound`] when the folder has no database yet. Blocking.
pub fn locate_nav_db(locator: &Locator, package_prefix: &str, sims: &[Sim]) -> Result<PathBuf> {
    let work = locator
        .package_work(package_prefix, sims)
        .ok_or_else(|| Error::WorkDirMissing {
            package_prefix: package_prefix.to_owned(),
        })?;
    let path = nav_db_path(&work.work);
    if path.is_file() {
        Ok(path)
    } else {
        Err(Error::NotFound { path })
    }
}

#[cfg(test)]
mod tests {
    use infinity_sim_paths::{ALL_SIMS, Overrides};

    use super::*;

    #[test]
    fn locates_the_database_in_a_package_work_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("WASM").join("MSFS2020");
        let work = root.join("infinity-t38-zz-test").join("work");
        std::fs::create_dir_all(&work).unwrap();
        let locator = Locator::discover(&Overrides {
            work_root: Some(root),
            ..Overrides::default()
        });

        let missing = locate_nav_db(&locator, "infinity-t38-zz", &ALL_SIMS);
        assert!(
            matches!(missing, Err(Error::NotFound { .. })),
            "{missing:?}"
        );

        std::fs::create_dir_all(work.join("NavigationData")).unwrap();
        std::fs::write(nav_db_path(&work), b"").unwrap();
        assert_eq!(
            locate_nav_db(&locator, "infinity-t38-zz", &ALL_SIMS).unwrap(),
            nav_db_path(&work)
        );

        let none = locate_nav_db(&locator, "no-such-package-zz", &ALL_SIMS);
        assert!(
            matches!(none, Err(Error::WorkDirMissing { .. })),
            "{none:?}"
        );
    }
}
