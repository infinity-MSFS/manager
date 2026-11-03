//! Turning preferences and sim discovery into the folders the app uses.

use std::path::PathBuf;

use infinity_ipc::dto::{self, Preferences};
use infinity_ipc::{IpcError, IpcErrorCode, IpcResult};
use infinity_sim_paths::{ALL_SIMS, Locator, Overrides, Storefront};

/// A discovery snapshot honouring the user's path overrides. Probes the
/// disk, so it runs on the blocking pool.
pub(crate) async fn locate(prefs: &Preferences) -> IpcResult<Locator> {
    let overrides = Overrides {
        wine_prefix: prefs.wine_prefix.as_deref().map(PathBuf::from),
        work_root: prefs.msfs_work_root.as_deref().map(PathBuf::from),
    };
    tauri::async_runtime::spawn_blocking(move || Locator::discover(&overrides))
        .await
        .map_err(|e| IpcError::internal(format!("sim discovery: {e}")))
}

/// The chosen install folder, else the newest sim's Community folder.
pub(crate) fn install_dir(prefs: &Preferences, locator: &Locator) -> Option<dto::InstallDir> {
    let chosen = prefs
        .install_dir
        .as_deref()
        .filter(|p| !p.trim().is_empty());
    let (path, chosen) = match chosen {
        Some(p) => (PathBuf::from(p), true),
        None => (locator.community(&ALL_SIMS)?.community.clone()?, false),
    };
    Some(dto::InstallDir {
        exists: path.is_dir(),
        path: path.to_string_lossy().into_owned(),
        chosen,
    })
}

/// The install folder, or an error the UI can act on.
pub(crate) fn require_install_dir(prefs: &Preferences, locator: &Locator) -> IpcResult<PathBuf> {
    let dir = install_dir(prefs, locator).ok_or_else(|| {
        IpcError::new(
            IpcErrorCode::SimNotFound,
            "no simulator found; choose an install folder in Settings",
        )
    })?;
    if !dir.exists {
        return Err(IpcError::new(
            IpcErrorCode::PackageDirNotFound,
            format!("the install folder {} does not exist", dir.path),
        )
        .with_details(serde_json::json!({ "path": dir.path })));
    }
    Ok(PathBuf::from(dir.path))
}

pub(crate) fn sim_paths(prefs: &Preferences, locator: &Locator) -> dto::SimPaths {
    dto::SimPaths {
        installs: locator.installs().iter().map(sim_install).collect(),
        install_dir: install_dir(prefs, locator),
    }
}

fn text(p: Option<&std::path::Path>) -> Option<String> {
    p.map(|p| p.to_string_lossy().into_owned())
}

fn sim_install(i: &infinity_sim_paths::SimInstall) -> dto::SimInstall {
    dto::SimInstall {
        sim: i.sim,
        storefront: match i.storefront {
            Storefront::MicrosoftStore => dto::Storefront::MicrosoftStore,
            Storefront::Steam => dto::Storefront::Steam,
        },
        community: text(i.community.as_deref()),
        wasm_root: text(i.wasm_root.as_deref()),
        wine_prefix: text(i.wine_prefix.as_deref()),
    }
}

pub(crate) fn work_root_probe(p: infinity_sim_paths::WorkRootProbe) -> dto::WorkRootProbe {
    dto::WorkRootProbe {
        resolved: text(p.resolved.as_deref()),
        sim: p.sim,
        packages: p.packages,
    }
}

pub(crate) fn prefix_probe(p: infinity_sim_paths::PrefixProbe) -> dto::PrefixProbe {
    dto::PrefixProbe {
        prefix: p.prefix.to_string_lossy().into_owned(),
        is_prefix: p.is_prefix,
        installs: p.installs.iter().map(sim_install).collect(),
    }
}

/// Runs blocking filesystem work off the async runtime.
pub(crate) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> IpcResult<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| IpcError::internal(format!("worker thread: {e}")))
}

/// Sims to search for a product's files, its own sim first.
pub(crate) fn sims_for(product: &infinity_ipc::catalog::Product) -> [infinity_sim_paths::Sim; 2] {
    use infinity_ipc::catalog::PackageSpec;
    use infinity_sim_paths::Sim;
    match product.package {
        PackageSpec::Msfs2020 { .. } => [Sim::Msfs2020, Sim::Msfs2024],
        _ => [Sim::Msfs2024, Sim::Msfs2020],
    }
}

/// The folder a product's configurator writes into, from its catalog
/// `workDir`: the aircraft's `work` folder in the sim, or a folder in app
/// data.
pub(crate) async fn work_dir(
    prefs: &Preferences,
    app_data: &std::path::Path,
    product: &infinity_ipc::catalog::Product,
) -> IpcResult<PathBuf> {
    use infinity_ipc::catalog::WorkDirSpec;
    match &product.work_dir {
        Some(WorkDirSpec::MsfsPackageWork { package_prefix }) => {
            let locator = locate(prefs).await?;
            locator
                .package_work(package_prefix, &sims_for(product))
                .map(|found| found.work)
                .ok_or_else(|| {
                    IpcError::new(
                        IpcErrorCode::PackageDirNotFound,
                        format!(
                            "{}'s work folder was not found. Fly it once in the sim, or set the MSFS work folder in Settings.",
                            product.name
                        ),
                    )
                })
        }
        Some(WorkDirSpec::AppData { relative }) => {
            let rel = std::path::Path::new(relative);
            if !rel
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
            {
                return Err(IpcError::new(
                    IpcErrorCode::CatalogInvalid,
                    format!("bad workDir {relative:?}"),
                ));
            }
            Ok(app_data.join(rel))
        }
        None => Err(IpcError::new(
            IpcErrorCode::CatalogInvalid,
            format!("{} declares no workDir", product.name),
        )),
    }
}
