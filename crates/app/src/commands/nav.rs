use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use infinity_ipc::catalog::WorkDirSpec;
use infinity_ipc::commands as cmd;
use infinity_ipc::dto::ProductRef;
use infinity_ipc::{IpcError, IpcErrorCode, IpcResult};
use infinity_nav_db::{DEFAULT_NEARBY_ROUTES, MtrDb, NavDb};
use infinity_persistence::PreferencesStore;
use tauri::State;
use tokio::sync::{Mutex, OnceCell};

use super::{Args, Out};
use crate::paths;
use crate::state::AppState;

/// Open navigation databases, one per product, reopened when the file
/// changes underneath (a navdata update).
#[derive(Debug, Default)]
pub(crate) struct NavDbs {
    open: Mutex<HashMap<String, Arc<NavDb>>>,
    mtr: OnceCell<Arc<MtrDb>>,
    mtr_path: std::sync::OnceLock<PathBuf>,
}

impl NavDbs {
    pub(crate) fn new(mtr_path: Option<PathBuf>) -> Self {
        let dbs = Self::default();
        if let Some(path) = mtr_path {
            let _ = dbs.mtr_path.set(path);
        }
        dbs
    }

    /// Closes every handle, so a navdata install can replace the files.
    pub(crate) async fn close_all(&self) {
        self.open.lock().await.clear();
    }

    async fn for_product(
        &self,
        state: &AppState,
        prefs: &PreferencesStore,
        at: &ProductRef,
    ) -> IpcResult<Arc<NavDb>> {
        if let Some(db) = self.open.lock().await.get(&at.product_id)
            && !db.is_stale()
        {
            return Ok(db.clone());
        }
        let product = state.product(at).await?;
        let Some(WorkDirSpec::MsfsPackageWork { package_prefix }) = &product.work_dir else {
            return Err(IpcError::new(
                IpcErrorCode::PackageDirNotFound,
                format!("{} has no navigation database", product.name),
            ));
        };
        let locator = paths::locate(&prefs.load().await?).await?;
        let prefix = package_prefix.clone();
        let sims = paths::sims_for(&product);
        let db = paths::blocking(move || -> IpcResult<NavDb> {
            let path = infinity_nav_db::locate_nav_db(&locator, &prefix, &sims)?;
            Ok(NavDb::open(path)?)
        })
        .await??;
        let db = Arc::new(db);
        self.open
            .lock()
            .await
            .insert(at.product_id.clone(), db.clone());
        Ok(db)
    }

    async fn mtr(&self) -> IpcResult<Arc<MtrDb>> {
        let path = self
            .mtr_path
            .get()
            .cloned()
            .ok_or_else(|| IpcError::unsupported("this build has no route database"))?;
        self.mtr
            .get_or_try_init(|| async move {
                Ok::<_, IpcError>(Arc::new(paths::blocking(move || MtrDb::open(path)).await??))
            })
            .await
            .cloned()
    }
}

/// Runs a query against a product's database on the blocking pool.
async fn query<T: Send + 'static>(
    dbs: &NavDbs,
    state: &AppState,
    prefs: &PreferencesStore,
    at: &ProductRef,
    f: impl FnOnce(&NavDb) -> infinity_nav_db::Result<T> + Send + 'static,
) -> IpcResult<T> {
    let db = dbs.for_product(state, prefs, at).await?;
    Ok(paths::blocking(move || f(&db)).await??)
}

async fn query_mtr<T: Send + 'static>(
    dbs: &NavDbs,
    f: impl FnOnce(&MtrDb) -> infinity_nav_db::Result<T> + Send + 'static,
) -> IpcResult<T> {
    let db = dbs.mtr().await?;
    Ok(paths::blocking(move || f(&db)).await??)
}

macro_rules! nav_command {
    ($name:ident, $cmd:ty, |$db:ident, $a:ident| $body:expr) => {
        #[tauri::command]
        pub(crate) async fn $name(
            dbs: State<'_, NavDbs>,
            state: State<'_, AppState>,
            prefs: State<'_, PreferencesStore>,
            args: Args<$cmd>,
        ) -> Out<$cmd> {
            let at = args.product.clone();
            let $a = args;
            query(&dbs, &state, &prefs, &at, move |$db| $body).await
        }
    };
}

#[tauri::command]
pub(crate) async fn nav_cycle(
    dbs: State<'_, NavDbs>,
    state: State<'_, AppState>,
    prefs: State<'_, PreferencesStore>,
    args: Args<cmd::NavCycle>,
) -> Out<cmd::NavCycle> {
    query(&dbs, &state, &prefs, &args, |db| db.cycle()).await
}

nav_command!(nav_is_valid_icao, cmd::NavIsValidIcao, |db, a| db
    .is_valid_icao(&a.icao));
nav_command!(nav_runways, cmd::NavRunways, |db, a| db
    .runways_for_airport(&a.icao));
nav_command!(nav_runway_details, cmd::NavRunwayDetails, |db, a| db
    .runway_details(&a.icao, &a.runway));
nav_command!(nav_airports_by_name, cmd::NavAirportsByName, |db, a| db
    .airports_by_name(&a.query));
nav_command!(nav_approach_types, cmd::NavApproachTypes, |db, a| db
    .available_approach_types(&a.icao, &a.runway));
nav_command!(nav_approach_data, cmd::NavApproachData, |db, a| db
    .approach_data(&a.icao, &a.runway, &a.approach_type));
nav_command!(nav_search_waypoints, cmd::NavSearchWaypoints, |db, a| db
    .search_waypoints(&a.query));
nav_command!(nav_nearest_waypoints, cmd::NavNearestWaypoints, |db, a| db
    .nearest_waypoints(a.lat, a.lon, a.limit));
nav_command!(nav_waypoint, cmd::NavWaypointByIdentifier, |db, a| db
    .waypoint_by_identifier(&a.identifier, a.area_code.as_deref()));
nav_command!(nav_airspace_boundary, cmd::NavAirspaceBoundary, |db, a| db
    .restrictive_airspace_boundary(&a.designation));
nav_command!(
    nav_airspace_boundary_normalized,
    cmd::NavAirspaceBoundaryNormalized,
    |db, a| db.restrictive_airspace_boundary_normalized(&a.designation, a.resolution)
);
nav_command!(nav_nearby_airspace, cmd::NavNearbyAirspace, |db, a| db
    .nearby_restrictive_airspace(a.lat, a.lon, a.max_nm));
nav_command!(nav_search_airspace, cmd::NavSearchAirspace, |db, a| db
    .search_restrictive_airspace(&a.query));
nav_command!(nav_is_valid_airspace, cmd::NavIsValidAirspace, |db, a| db
    .is_valid_restrictive_airspace(&a.designation));

#[tauri::command]
pub(crate) async fn nav_expand_training_areas(
    args: Args<cmd::NavExpandTrainingAreas>,
) -> Out<cmd::NavExpandTrainingAreas> {
    paths::blocking(move || infinity_nav_db::expand_training_areas(args)).await
}

#[tauri::command]
pub(crate) async fn mtr_search(
    dbs: State<'_, NavDbs>,
    args: Args<cmd::MtrSearch>,
) -> Out<cmd::MtrSearch> {
    query_mtr(&dbs, move |db| db.search(&args)).await
}

#[tauri::command]
pub(crate) async fn mtr_nearby(
    dbs: State<'_, NavDbs>,
    args: Args<cmd::MtrNearbyRoutes>,
) -> Out<cmd::MtrNearbyRoutes> {
    let limit = args.limit.unwrap_or(DEFAULT_NEARBY_ROUTES);
    query_mtr(&dbs, move |db| {
        db.nearby(args.lat, args.lon, args.max_nm, limit)
    })
    .await
}

#[tauri::command]
pub(crate) async fn mtr_segments(
    dbs: State<'_, NavDbs>,
    args: Args<cmd::MtrSegments>,
) -> Out<cmd::MtrSegments> {
    query_mtr(&dbs, move |db| db.segments_by_ident(&args)).await
}
