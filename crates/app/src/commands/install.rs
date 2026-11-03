use std::sync::Arc;

use infinity_installer::{Installer, Interrupted, KEEP_INTERRUPTED_FOR, Plan, Wanted};
use infinity_ipc::catalog::{ArchiveFormat, PresignArtifact, Product};
use infinity_ipc::commands as cmd;
use infinity_ipc::dto::{Channel, ProductRef, livery_job_id, parse_livery_job_id};
use infinity_ipc::events::InstalledChanged;
use infinity_ipc::{IpcError, IpcErrorCode, IpcResult};
use infinity_persistence::{InstalledStore, PreferencesStore};
use tauri::{Manager, State};

use super::{Args, Out};
use crate::events::emit;
use crate::paths;
use crate::state::AppState;

#[tauri::command]
pub(crate) async fn installed_list(
    registry: State<'_, Arc<InstalledStore>>,
) -> Out<cmd::InstalledList> {
    registry.list().await
}

#[tauri::command]
pub(crate) async fn installed_rescan(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    prefs: State<'_, PreferencesStore>,
    registry: State<'_, Arc<InstalledStore>>,
    args: Args<cmd::InstalledRescan>,
) -> Out<cmd::InstalledRescan> {
    let channel = args;
    let prefs = prefs.load().await?;
    let locator = paths::locate(&prefs).await?;
    let dir = paths::require_install_dir(&prefs, &locator)?;
    let catalog = state.fetch_catalog(channel).await?;
    let wanted: Vec<Wanted> = catalog
        .products
        .iter()
        .filter_map(|p| {
            Some(Wanted {
                product_id: p.id.clone(),
                package_dir_name: p.package.package_dir_name()?.to_owned(),
            })
        })
        .collect();
    let known = registry.list().await?;
    let found = tauri::async_runtime::spawn_blocking(move || {
        infinity_installer::rescan(known, &dir, &wanted, infinity_installer::unix_now())
    })
    .await
    .map_err(|e| IpcError::internal(format!("rescan: {e}")))?;
    registry.replace_all(found.clone()).await?;
    emit::<InstalledChanged>(&app, &found);
    Ok(found)
}

#[tauri::command]
pub(crate) async fn installed_uninstall(
    app: tauri::AppHandle,
    installer: State<'_, Installer>,
    registry: State<'_, Arc<InstalledStore>>,
    args: Args<cmd::InstalledUninstall>,
) -> Out<cmd::InstalledUninstall> {
    let package = registry.get(&args).await?.ok_or_else(|| {
        IpcError::new(
            IpcErrorCode::ProductUnknown,
            format!("{args} is not installed"),
        )
    })?;
    installer.uninstall(&package).await?;
    emit::<InstalledChanged>(&app, &registry.list().await?);
    Ok(())
}

#[tauri::command]
pub(crate) async fn installed_size(
    registry: State<'_, Arc<InstalledStore>>,
    args: Args<cmd::InstalledSize>,
) -> Out<cmd::InstalledSize> {
    let Some(package) = registry.get(&args).await? else {
        return Ok(None);
    };
    let path = std::path::PathBuf::from(package.path);
    tauri::async_runtime::spawn_blocking(move || {
        path.is_dir().then(|| infinity_installer::dir_size(&path))
    })
    .await
    .map_err(|e| IpcError::internal(format!("size: {e}")))
}

#[tauri::command]
pub(crate) async fn install_start(
    state: State<'_, AppState>,
    prefs: State<'_, PreferencesStore>,
    installer: State<'_, Installer>,
    args: Args<cmd::InstallStart>,
) -> Out<cmd::InstallStart> {
    if let Some((aircraft, livery)) = parse_livery_job_id(&args.product_id) {
        let plan = livery_plan(&state, &prefs, aircraft, livery, args.channel).await?;
        return installer.start(plan);
    }
    // Fresh, so an update installs the version the user was just shown.
    state.fetch_catalog(args.channel).await?;
    let product = state
        .product(&ProductRef {
            product_id: args.product_id.clone(),
            channel: args.channel,
        })
        .await?;
    let plan = plan(&state, &prefs, product, args.channel).await?;
    installer.start(plan)
}

/// A livery installs like a product, from the presigned URL in the
/// aircraft's livery catalog, into the livery folder if one is chosen.
async fn livery_plan(
    state: &AppState,
    prefs: &PreferencesStore,
    aircraft: &str,
    livery_id: &str,
    channel: Channel,
) -> IpcResult<Plan> {
    let catalog = state.providers.content.liveries(aircraft).await?;
    let livery = catalog
        .liveries
        .into_iter()
        .find(|l| l.id == livery_id)
        .ok_or_else(|| {
            IpcError::new(
                IpcErrorCode::ProductUnknown,
                format!("{aircraft} has no livery {livery_id}"),
            )
        })?;
    let archive_url = url::Url::parse(&livery.archive_url)
        .map_err(|e| IpcError::new(IpcErrorCode::PresignFailed, format!("livery url: {e}")))?;
    let prefs = prefs.load().await?;
    let install_dir = match prefs
        .livery_install_dir
        .as_deref()
        .filter(|p| !p.trim().is_empty())
    {
        Some(dir) => {
            let dir = std::path::PathBuf::from(dir);
            if !dir.is_dir() {
                return Err(IpcError::new(
                    IpcErrorCode::PackageDirNotFound,
                    format!("the livery folder {} does not exist", dir.display()),
                ));
            }
            dir
        }
        None => {
            let locator = paths::locate(&prefs).await?;
            paths::require_install_dir(&prefs, &locator)?
        }
    };
    Ok(Plan {
        product_id: livery_job_id(aircraft, &livery.id),
        name: livery.name,
        version: livery.version,
        channel,
        archive_url,
        archive_key: livery.archive_key,
        // The livery catalog has no format field; liveries are zips. A
        // file that is not one fails clearly at extraction.
        format: ArchiveFormat::Zip,
        manifest_url: None,
        package_dir_name: livery.package_dir_name,
        install_dir,
    })
}

#[tauri::command]
pub(crate) async fn install_pause(
    installer: State<'_, Installer>,
    args: Args<cmd::InstallPause>,
) -> Out<cmd::InstallPause> {
    installer.pause(&args)
}

#[tauri::command]
pub(crate) async fn install_cancel(
    installer: State<'_, Installer>,
    args: Args<cmd::InstallCancel>,
) -> Out<cmd::InstallCancel> {
    installer.cancel(&args).await
}

#[tauri::command]
pub(crate) async fn jobs_list(installer: State<'_, Installer>) -> Out<cmd::JobsList> {
    Ok(installer.jobs())
}

/// Everything about the install decided up front, so a job never starts
/// for something it cannot finish.
async fn plan(
    state: &AppState,
    prefs: &PreferencesStore,
    product: Product,
    channel: Channel,
) -> IpcResult<Plan> {
    if product.coming_soon {
        return Err(IpcError::new(
            IpcErrorCode::ProductUnknown,
            format!("{} is not released yet", product.name),
        ));
    }
    let package_dir_name = product
        .package
        .package_dir_name()
        .ok_or_else(|| {
            IpcError::unsupported(format!(
                "{} cannot be installed by this version",
                product.name
            ))
        })?
        .to_owned();
    let archive = product.package.archive().clone();
    if archive.format == ArchiveFormat::Unknown {
        return Err(IpcError::unsupported(format!(
            "{} uses an archive format this version cannot install; update the manager",
            product.name
        )));
    }

    let prefs = prefs.load().await?;
    let locator = paths::locate(&prefs).await?;
    let install_dir = paths::require_install_dir(&prefs, &locator)?;

    let content = &state.providers.content;
    let archive_url = content
        .resolve_download(&product, PresignArtifact::Archive)
        .await?
        .url;
    // A declared manifest is required: installing unchecked because it
    // could not be fetched would defeat the point of shipping one.
    let manifest_url = match product.package.manifest_key() {
        Some(_) => Some(
            content
                .resolve_download(&product, PresignArtifact::Manifest)
                .await?
                .url,
        ),
        None => None,
    };
    Ok(Plan {
        product_id: product.id,
        name: product.name,
        version: product.version,
        channel,
        archive_url,
        archive_key: archive.key,
        format: archive.format,
        manifest_url,
        package_dir_name,
        install_dir,
    })
}

/// Lists downloads a previous run left unfinished as paused jobs. The
/// installer broadcasts each one, so the UI hears of it as a job change,
/// and `jobs_list` includes it for a UI that loads later.
///
/// A download of a version its product has moved on from is deleted.
/// When the catalog cannot be reached the download is kept: resuming
/// plans afresh, and a version change then starts the download over.
pub(crate) fn restore_interrupted(app: &tauri::AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let installer = app.state::<Installer>().inner().clone();
        let dirs = staging_candidates(&app).await;
        let found = installer.find_interrupted(dirs, KEEP_INTERRUPTED_FOR).await;
        let state = app.state::<AppState>();
        for found in found {
            match latest_version(&state, &found).await {
                Ok(Some(latest)) if latest != found.version() => installer.discard(found).await,
                Ok(_) => {
                    installer.restore(found);
                }
                Err(e) => {
                    tracing::debug!(product = found.product_id(), error = %e, "could not check an interrupted download's version");
                    installer.restore(found);
                }
            }
        }
    });
}

/// Every folder an install may have gone into: the install folder, each
/// sim's Community folder and the livery folder.
async fn staging_candidates(app: &tauri::AppHandle) -> Vec<std::path::PathBuf> {
    let prefs = match app.state::<PreferencesStore>().load().await {
        Ok(prefs) => prefs,
        Err(e) => {
            tracing::warn!(error = %e, "preferences unreadable; not looking for interrupted downloads");
            return Vec::new();
        }
    };
    let mut dirs: Vec<std::path::PathBuf> = prefs
        .livery_install_dir
        .iter()
        .chain(prefs.install_dir.iter())
        .filter(|p| !p.trim().is_empty())
        .map(std::path::PathBuf::from)
        .collect();
    match paths::locate(&prefs).await {
        Ok(locator) => {
            dirs.extend(
                locator
                    .installs()
                    .iter()
                    .filter_map(|i| i.community.clone()),
            );
        }
        Err(e) => tracing::debug!(error = %e, "sim discovery for interrupted downloads"),
    }
    dirs.retain(|d| d.is_absolute());
    dirs
}

/// The version an install of the found product would get now, if it is
/// still offered.
async fn latest_version(state: &AppState, found: &Interrupted) -> IpcResult<Option<String>> {
    if let Some((aircraft, livery)) = parse_livery_job_id(found.product_id()) {
        let catalog = state.providers.content.liveries(aircraft).await?;
        return Ok(catalog
            .liveries
            .into_iter()
            .find(|l| l.id == livery)
            .map(|l| l.version));
    }
    let catalog = state.catalog(found.channel()).await?;
    Ok(catalog
        .products
        .iter()
        .find(|p| p.id == found.product_id())
        .map(|p| p.version.clone()))
}
