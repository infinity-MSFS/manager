use std::collections::HashSet;
use std::sync::Arc;

use infinity_ipc::catalog::{Product, WorkDirSpec};
use infinity_ipc::commands as cmd;
use infinity_ipc::dto::{Job, JobPhase, Progress};
use infinity_ipc::events::JobChanged;
use infinity_ipc::{IpcError, IpcErrorCode, IpcResult};
use infinity_navigraph::{
    Endpoints, FileTokenStore, NavdataProgress, NavdataTarget, Navigraph, NavigraphClient,
};
use infinity_persistence::PreferencesStore;
use tauri::State;
use tauri_plugin_opener::OpenerExt;
use tokio::sync::Mutex;

use super::nav::NavDbs;
use super::{Args, Out};
use crate::events::emit;
use crate::paths;
use crate::state::AppState;

/// The linked account, when the build can link one.
#[derive(Debug)]
pub(crate) struct NavigraphState {
    account: Option<Navigraph>,
    updating: Mutex<HashSet<String>>,
}

impl NavigraphState {
    pub(crate) fn new(
        access: Option<&infinity_providers::NavigraphAccess>,
        app_data: &std::path::Path,
    ) -> IpcResult<Self> {
        let account = match access {
            Some(access) => {
                let client = NavigraphClient::new(Endpoints::navigraph()?)?;
                let tokens = access.tokens.clone().unwrap_or_else(|| {
                    Arc::new(FileTokenStore::new(app_data.join("navigraph.json")))
                });
                Some(Navigraph::new(client, tokens, access.credentials.clone()))
            }
            None => None,
        };
        Ok(Self {
            account,
            updating: Mutex::new(HashSet::new()),
        })
    }

    pub(crate) fn account(&self) -> IpcResult<&Navigraph> {
        self.account
            .as_ref()
            .ok_or_else(|| IpcError::unsupported("this build cannot link a Navigraph account"))
    }
}

#[tauri::command]
pub(crate) async fn navigraph_status(nav: State<'_, NavigraphState>) -> Out<cmd::NavigraphStatus> {
    Ok(nav.account()?.status().await)
}

#[tauri::command]
pub(crate) async fn navigraph_link_start(
    app: tauri::AppHandle,
    nav: State<'_, NavigraphState>,
) -> Out<cmd::NavigraphLinkStart> {
    let start = nav.account()?.link_start().await?;
    if let Err(e) = app
        .opener()
        .open_url(&start.verification_uri_complete, None::<&str>)
    {
        tracing::warn!(error = %e, "could not open the Navigraph sign-in page");
    }
    Ok(start)
}

#[tauri::command]
pub(crate) async fn navigraph_link_poll(
    nav: State<'_, NavigraphState>,
) -> Out<cmd::NavigraphLinkPoll> {
    Ok(nav.account()?.link_poll().await?)
}

#[tauri::command]
pub(crate) async fn navigraph_unlink(nav: State<'_, NavigraphState>) -> Out<cmd::NavigraphUnlink> {
    Ok(nav.account()?.unlink().await?)
}

#[tauri::command]
pub(crate) async fn navdata_status(
    state: State<'_, AppState>,
    prefs: State<'_, PreferencesStore>,
    nav: State<'_, NavigraphState>,
    args: Args<cmd::NavdataStatus>,
) -> Out<cmd::NavdataStatus> {
    let account = nav.account()?;
    let catalog = state.catalog(args).await?;
    let prefs = prefs.load().await?;
    let locator = paths::locate(&prefs).await?;
    let targets: Vec<NavdataTarget> = catalog
        .products
        .iter()
        .filter_map(|p| NavdataTarget::from_product(p, work_folder(&locator, p)))
        .collect();
    Ok(infinity_navigraph::navdata_report(account, &targets).await)
}

#[tauri::command]
pub(crate) async fn navdata_update(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    prefs: State<'_, PreferencesStore>,
    nav: State<'_, NavigraphState>,
    args: Args<cmd::NavdataUpdate>,
) -> Out<cmd::NavdataUpdate> {
    let account = nav.account()?;
    let product = state.product(&args).await?;
    let locator = paths::locate(&prefs.load().await?).await?;
    let target = NavdataTarget::from_product(&product, work_folder(&locator, &product))
        .ok_or_else(|| {
            IpcError::new(
                IpcErrorCode::ProductUnknown,
                format!("{} takes no navdata", product.name),
            )
        })?;
    let plan = infinity_navigraph::plan_navdata_update(account, &target).await?;
    if !nav.updating.lock().await.insert(plan.job_id.clone()) {
        return Ok(());
    }

    let mut job = Job {
        product_id: plan.job_id.clone(),
        name: plan.name.clone(),
        version: plan.cycle.cycle.clone(),
        channel: args.channel,
        phase: JobPhase::Queued,
        progress: None,
    };
    emit::<JobChanged>(&app, &job);
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        use tauri::Manager;
        // Open handles block replacing the database on Windows.
        handle.state::<NavDbs>().close_all().await;
        let result = async {
            let http = infinity_navigraph::download_client()?;
            infinity_navigraph::install_navdata(&http, &plan, &mut |p| {
                let (phase, progress) = match p {
                    NavdataProgress::Downloading { done, total } => {
                        (JobPhase::Downloading, Some(Progress { done, total }))
                    }
                    NavdataProgress::Verifying => (JobPhase::Verifying, None),
                    NavdataProgress::Extracting => (JobPhase::Extracting, None),
                    NavdataProgress::Finalizing => (JobPhase::Installing, None),
                };
                job.phase = phase;
                job.progress = progress;
                emit::<JobChanged>(&handle, &job);
            })
            .await
        }
        .await;
        job.progress = None;
        job.phase = match result {
            Ok(path) => {
                tracing::info!(path = %path.display(), "navdata installed");
                JobPhase::Completed
            }
            Err(e) => JobPhase::Failed { error: e.into() },
        };
        emit::<JobChanged>(&handle, &job);
        handle
            .state::<NavigraphState>()
            .updating
            .lock()
            .await
            .remove(&job.product_id);
    });
    Ok(())
}

fn work_folder(
    locator: &infinity_sim_paths::Locator,
    product: &Product,
) -> Option<std::path::PathBuf> {
    match &product.work_dir {
        Some(WorkDirSpec::MsfsPackageWork { package_prefix }) => locator
            .package_work(package_prefix, &paths::sims_for(product))
            .map(|w| w.work),
        _ => None,
    }
}
