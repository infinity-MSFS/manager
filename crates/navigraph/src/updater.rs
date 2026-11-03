//! Navdata status across products and planning an update. Paths come in
//! resolved: the app locates each product's package `work` folder (with
//! `infinity-sim-paths`) and passes it here.

use std::path::{Path, PathBuf};

use infinity_ipc::catalog::{NavdataSpec, Product};

use crate::account::Navigraph;
use crate::error::{NavigraphError, Result};
use crate::navdata::{
    CycleJson, NavdataPackage, airac_validity, is_update, select_package, sha256_hex,
};
use crate::secret::Secret;
use infinity_ipc::navigraph::{NavdataItem, NavdataLatest, NavdataReport};

/// Folder inside the package `work` folder that holds the navdata.
pub const NAVDATA_DIR: &str = "NavigationData";
/// Installed-cycle marker inside [`NAVDATA_DIR`].
pub const CYCLE_FILE: &str = "cycle.json";

/// Install job id for a product's navdata.
pub fn navdata_job_id(product_id: &str) -> String {
    format!("navdata-{product_id}")
}

/// A product that takes Navigraph navdata.
#[derive(Debug, Clone)]
pub struct NavdataTarget {
    pub product_id: String,
    pub product_name: String,
    pub spec: NavdataSpec,
    /// The package `work` folder; `None` when the aircraft is not installed.
    pub work_dir: Option<PathBuf>,
}

impl NavdataTarget {
    /// `None` for products that declare no navdata.
    pub fn from_product(product: &Product, work_dir: Option<PathBuf>) -> Option<Self> {
        Some(Self {
            product_id: product.id.clone(),
            product_name: product.name.clone(),
            spec: product.navdata.clone()?,
            work_dir,
        })
    }

    fn select<'a>(&self, packages: &'a [NavdataPackage]) -> Option<&'a NavdataPackage> {
        select_package(
            packages,
            self.spec.format.as_deref(),
            self.spec.package_match.as_deref(),
        )
    }
}

/// Everything [`install_navdata`](crate::install_navdata) needs.
#[derive(Debug, Clone)]
pub struct NavdataUpdatePlan {
    pub product_id: String,
    pub job_id: String,
    /// Display name, e.g. `T-38C Navdata — AIRAC 2606`.
    pub name: String,
    /// Short-lived signed download URL.
    pub url: Secret,
    /// Lowercase hex SHA-256 of the download, when Navigraph lists one.
    pub sha256: Option<String>,
    pub work_dir: PathBuf,
    /// Written as `cycle.json` if the archive does not ship one.
    pub cycle: CycleJson,
}

/// Reads `<work_dir>/NavigationData/cycle.json`. `None` when missing or
/// unreadable.
pub async fn read_cycle_json(work_dir: &Path) -> Option<CycleJson> {
    let path = work_dir.join(NAVDATA_DIR).join(CYCLE_FILE);
    let bytes = tokio::fs::read(&path).await.ok()?;
    match serde_json::from_slice::<CycleJson>(&bytes) {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "navdata: cycle.json unreadable");
            None
        }
    }
}

/// Installed vs latest navdata for every target. Never fails: Navigraph
/// trouble lands in `fetch_error` and local state is still reported.
/// Navigraph is only asked when at least one target is installed.
pub async fn navdata_report(nav: &Navigraph, targets: &[NavdataTarget]) -> NavdataReport {
    let any_installed = targets.iter().any(|t| t.work_dir.is_some());
    let (linked, packages, fetch_error) = if any_installed {
        fetch_packages(nav).await
    } else {
        (false, None, None)
    };

    let mut items = Vec::with_capacity(targets.len());
    for target in targets {
        let job_id = navdata_job_id(&target.product_id);
        let Some(work_dir) = target.work_dir.as_deref() else {
            items.push(NavdataItem {
                product_id: target.product_id.clone(),
                product_name: target.product_name.clone(),
                installed: false,
                current: None,
                latest: None,
                update_available: false,
                job_id,
                note: None,
            });
            continue;
        };

        let current = read_cycle_json(work_dir).await;
        let matched = packages.as_deref().and_then(|p| target.select(p));
        let note = (packages.is_some() && matched.is_none())
            .then(|| "No matching Navigraph package for this product.".to_string());
        let update_available = match (&current, matched) {
            // No cycle.json: offer an install.
            (None, _) => true,
            (Some(cur), Some(pkg)) => is_update(cur, pkg),
            (Some(_), None) => false,
        };
        items.push(NavdataItem {
            product_id: target.product_id.clone(),
            product_name: target.product_name.clone(),
            installed: true,
            current,
            latest: matched.map(|p| NavdataLatest {
                cycle: p.cycle.clone(),
                revision: p.revision_or_default().to_owned(),
                name: p.description.clone(),
                validity_period: airac_validity(&p.cycle),
            }),
            update_available,
            job_id,
            note,
        });
    }

    NavdataReport {
        linked,
        fetch_error,
        items,
    }
}

async fn fetch_packages(nav: &Navigraph) -> (bool, Option<Vec<NavdataPackage>>, Option<String>) {
    let bearer = match nav.bearer().await {
        Ok(b) => b,
        Err(NavigraphError::NotLinked) => return (false, None, None),
        Err(e @ NavigraphError::RelinkRequired) => return (false, None, Some(e.to_string())),
        Err(e) => {
            tracing::warn!(error = %e, "navdata: bearer unavailable");
            return (true, None, Some(e.to_string()));
        }
    };
    match nav.client().navdata_packages(&bearer).await {
        Ok(pkgs) => {
            tracing::info!(count = pkgs.len(), "navdata: packages fetched");
            (true, Some(pkgs), None)
        }
        Err(e) => {
            tracing::warn!(error = %e, "navdata: packages fetch failed");
            (true, None, Some(e.to_string()))
        }
    }
}

/// Picks the package to install for `target`.
pub async fn plan_navdata_update(
    nav: &Navigraph,
    target: &NavdataTarget,
) -> Result<NavdataUpdatePlan> {
    let work_dir = target
        .work_dir
        .clone()
        .ok_or_else(|| NavigraphError::WorkDirNotFound {
            product: target.product_name.clone(),
        })?;
    let packages = nav.navdata_packages().await?;
    let pkg = target
        .select(&packages)
        .ok_or(NavigraphError::NoMatchingPackage)?;
    let file = pkg.primary_file().ok_or(NavigraphError::NoPackageFiles)?;
    Ok(NavdataUpdatePlan {
        product_id: target.product_id.clone(),
        job_id: navdata_job_id(&target.product_id),
        name: format!("{} Navdata — AIRAC {}", target.product_name, pkg.cycle),
        url: file.signed_url.clone(),
        sha256: sha256_hex(&file.hash),
        work_dir,
        cycle: crate::navdata::cycle_json_for(pkg),
    })
}
