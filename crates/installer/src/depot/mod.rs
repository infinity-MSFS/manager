//! Installing from a depot: the chunked, content-addressed release format
//! of `infinity-depot`.
//!
//! One algorithm serves install, update, verify and repair — make the
//! package folder match a manifest:
//!
//! 1. **Check** the installed files against the manifest ([`scan`]).
//!    Updates trust files that are recorded as verified, untouched since
//!    and unchanged in the new version; verification reads everything.
//! 2. **Stage** each file that is not exactly right, full size, beside the
//!    install ([`stage`]): chunks already on disk are copied, the rest
//!    downloaded ([`fetch`]). Progress is journaled per chunk, so pausing,
//!    quitting or crashing resumes where it left off.
//! 3. **Verify** every staged file by reading it back. Damaged chunks are
//!    fetched again; nothing is installed until all of them hash right.
//! 4. **Commit** ([`commit`]): one folder rename for a first install, a
//!    journaled two-phase swap of just the changed files otherwise.
//!
//! The install folder is untouched until step 4, and step 4 cannot leave
//! a mix of versions behind.

pub(crate) mod commit;
mod fetch;
pub(crate) mod io;
pub(crate) mod scan;
pub(crate) mod stage;
pub(crate) mod state;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use infinity_depot::ranges::{self, Coalesce};
use infinity_depot::{Hash, Manifest};
use infinity_ipc::IpcResult;
use infinity_ipc::dto::{Channel, JobKind, JobPhase};
use serde::{Deserialize, Serialize};
use url::Url;

use self::commit::Commit;
use self::fetch::{Fetch, UrlBook};
use self::stage::{Dir, Journal, Sink, Target};
use self::state::InstalledState;
use crate::error::{Error, IoContext, Result};
use crate::job::{self, StopRx};
use crate::staging::{Staging, remove_dir};
use crate::{Ctx, Plan, space};

/// Rounds of re-downloading chunks that failed verification before the
/// job gives up on the disk.
const REPAIR_ROUNDS: u32 = 3;
const MAX_MANIFEST_BYTES: usize = 64 * 1024 * 1024;

/// Connections a depot download opens at once.
pub(crate) const fn fetch_connections() -> usize {
    fetch::CONNECTIONS
}

/// Resolves storage keys of a product's packs to download URLs.
#[async_trait::async_trait]
pub trait PackUrls: Send + Sync + std::fmt::Debug {
    /// URLs for `keys`, in the same order. Called again for a key whose URL
    /// the storage refused or that is about to expire.
    async fn resolve(&self, keys: &[String]) -> IpcResult<Vec<Url>>;
}

/// Where a depot release comes from.
#[derive(Debug, Clone)]
pub struct DepotSource {
    pub manifest_url: Url,
    /// The manifest's hash as published in the catalog.
    pub manifest_hash: Hash,
    pub packs: Arc<dyn PackUrls>,
}

/// What a depot job is for, kept in its folder so it can be listed as a
/// paused job after a restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JobRecord {
    pub(crate) product_id: String,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) channel: Channel,
    #[serde(default)]
    pub(crate) kind: JobKind,
    pub(crate) package_dir_name: String,
    pub(crate) install_dir: PathBuf,
    pub(crate) manifest_hash: Hash,
    /// Stored bytes downloaded and still to download, as of the last stop.
    #[serde(default)]
    pub(crate) downloaded: u64,
    #[serde(default)]
    pub(crate) to_download: u64,
    pub(crate) updated_at: u64,
}

impl JobRecord {
    pub(crate) fn read(path: &Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    fn write(&self, dir: &Dir) -> Result<()> {
        let path = dir.job();
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| Error::Internal(e.to_string()))?;
        io::write_atomic(&path, &bytes).at(&path)
    }
}

/// Installs, updates, verifies or repairs a package from a depot.
pub(crate) async fn run(
    ctx: &Ctx<'_>,
    plan: &Plan,
    source: &DepotSource,
    kind: JobKind,
    staging: &Staging,
    stop: &mut StopRx,
) -> Result<()> {
    let dir = Dir::new(staging.depot());
    let target = plan.install_dir.join(&plan.package_dir_name);
    if kind == JobKind::Verify && !target.is_dir() {
        return Err(Error::NotInstalled(plan.name.clone()));
    }
    let downloads = ctx.acquire_download(stop).await?;

    // The manifest: cached by an interrupted run, else downloaded.
    ctx.phase(JobPhase::Downloading);
    let manifest = Arc::new(load_manifest(ctx, &dir, source, stop).await?);
    if manifest.product_id != plan.product_id || manifest.version != plan.version {
        return Err(Error::Archive(format!(
            "the package manifest is for {} {}, not {} {}",
            manifest.product_id, manifest.version, plan.product_id, plan.version
        )));
    }
    let mut record = JobRecord {
        product_id: plan.product_id.clone(),
        name: plan.name.clone(),
        version: plan.version.clone(),
        channel: plan.channel,
        kind,
        package_dir_name: plan.package_dir_name.clone(),
        install_dir: plan.install_dir.clone(),
        manifest_hash: source.manifest_hash,
        downloaded: 0,
        to_download: 0,
        updated_at: crate::unix_now(),
    };
    record.write(&dir)?;
    drop(downloads);

    // 1. Check what is installed.
    ctx.phase(JobPhase::Checking);
    let previous = {
        let target = target.clone();
        tokio::task::spawn_blocking(move || InstalledState::read(&target))
            .await
            .map_err(|e| Error::Internal(e.to_string()))?
    };
    let scan = {
        let (m, t, p) = (manifest.clone(), target.clone(), previous.clone());
        let full = kind == JobKind::Verify;
        job::blocking(stop, &mut ctx.reporter(), move |progress| {
            Ok(scan::scan(&m, &t, p.as_ref(), full, progress))
        })
        .await?
    };
    let staged: Vec<u32> = (0..manifest.files.len() as u32)
        .filter(|&i| !scan.intact[i as usize])
        .collect();
    tracing::info!(
        product = %plan.product_id,
        intact = scan.intact_count(),
        to_stage = staged.len(),
        "checked the installed files"
    );

    // 2. Stage.
    let (journal, placed, fresh) = Journal::open(&dir.journal(), &source.manifest_hash)?;
    if fresh {
        remove_dir(&dir.tree());
    }
    let needed = staged_bytes_needed(&manifest, &dir.tree(), &staged);
    space::ensure(dir.root(), needed, "install this package")?;
    let sink = {
        let (m, tree, s) = (manifest.clone(), dir.tree(), staged.clone());
        Arc::new(
            tokio::task::spawn_blocking(move || Sink::open(&m, &tree, &s, journal))
                .await
                .map_err(|e| Error::Internal(e.to_string()))??,
        )
    };
    let mut work = stage::plan(&manifest, &staged, &placed, &scan);
    drop(scan);
    if !work.local.is_empty() {
        ctx.phase(JobPhase::Assembling);
        let total = work.local_bytes(&manifest);
        let (m, s) = (manifest.clone(), sink.clone());
        let mut moved = std::mem::take(&mut work);
        work = job::blocking(stop, &mut ctx.reporter(), move |progress| {
            progress
                .total
                .store(total.max(1), std::sync::atomic::Ordering::Relaxed);
            Ok(stage::copy_local(&m, &s, &mut moved, progress)?.map(|()| moved))
        })
        .await?;
    }

    // 3. Download what is missing; verify everything staged; repeat for
    // whatever fails.
    let urls = Arc::new(UrlBook::new(source.packs.clone(), &manifest));
    let mut to_verify = staged.clone();
    let mut round = 0;
    loop {
        if !work.remote.is_empty() {
            let result = download(
                ctx,
                &manifest,
                &urls,
                &sink,
                std::mem::take(&mut work.remote),
                &mut record,
                stop,
            )
            .await;
            record.updated_at = crate::unix_now();
            let _ = record.write(&dir);
            result?;
        }
        ctx.phase(JobPhase::Verifying);
        let bad = {
            let (m, s, files) = (manifest.clone(), sink.clone(), to_verify.clone());
            job::blocking(stop, &mut ctx.reporter(), move |progress| {
                Ok(stage::verify(&m, &s, &files, progress))
            })
            .await?
        };
        if bad.is_empty() {
            break;
        }
        round += 1;
        tracing::warn!(
            chunks = bad.len(),
            round,
            "staged chunks failed verification; fetching them again"
        );
        if round > REPAIR_ROUNDS {
            return Err(Error::Corrupt(format!(
                "{} chunks keep failing verification after being downloaded again; the disk may be failing",
                bad.len()
            )));
        }
        let files: HashSet<u32> = bad.iter().map(|(_, t)| t.file).collect();
        to_verify = files.into_iter().collect();
        for (chunk, target) in bad {
            work.remote.entry(chunk).or_default().push(target);
        }
    }
    // Windows cannot rename files that are still open.
    drop(sink);

    // 4. Commit.
    ctx.phase(JobPhase::Installing);
    let installs = ctx.acquire_install(stop).await?;
    {
        let (dir, staging, manifest, target) = (
            dir.clone(),
            staging.clone(),
            manifest.clone(),
            target.clone(),
        );
        let package_dir_name = plan.package_dir_name.clone();
        let manifest_hash = source.manifest_hash;
        tokio::task::spawn_blocking(move || {
            Commit {
                dir: &dir,
                staging: &staging,
                manifest: &manifest,
                manifest_hash,
                target: &target,
                package_dir_name: &package_dir_name,
                staged: &staged,
                previous: previous.as_ref(),
            }
            .run()
        })
        .await
        .map_err(|e| Error::Internal(e.to_string()))??;
    }
    drop(installs);
    Ok(())
}

async fn download(
    ctx: &Ctx<'_>,
    manifest: &Arc<Manifest>,
    urls: &Arc<UrlBook>,
    sink: &Arc<Sink>,
    remote: HashMap<u32, Vec<Target>>,
    record: &mut JobRecord,
    stop: &mut StopRx,
) -> Result<()> {
    let _permit = ctx.acquire_download(stop).await?;
    ctx.phase(JobPhase::Downloading);
    // One fetch per stored copy; every slot wanting it is written.
    let mut by_place: HashMap<(u32, u64), u32> = HashMap::new();
    let mut targets: HashMap<u32, Vec<Target>> = HashMap::new();
    for (c, t) in remote {
        let chunk = manifest.chunk(c);
        let rep = *by_place.entry((chunk.pack, chunk.offset)).or_insert(c);
        if manifest.chunk(rep).id != chunk.id {
            return Err(Error::Archive(
                "the package manifest stores two chunks in one place".into(),
            ));
        }
        targets.entry(rep).or_default().extend(t);
    }
    let ranges = ranges::coalesce(manifest, targets.keys().copied(), Coalesce::DEFAULT);
    let total: u64 = targets
        .keys()
        .map(|&c| u64::from(manifest.chunk(c).stored))
        .sum();
    record.to_download = total;
    tracing::info!(
        chunks = targets.len(),
        requests = ranges.len(),
        bytes = total,
        "downloading"
    );
    let fetch = Arc::new(Fetch::new(
        ctx.http().clone(),
        manifest.clone(),
        urls.clone(),
        sink.clone(),
        targets,
    ));
    let mut reporter = ctx.reporter();
    let result = fetch
        .clone()
        .run(ranges, stop, &mut |done| reporter(done, Some(total)))
        .await;
    record.downloaded = fetch.received.load(std::sync::atomic::Ordering::Relaxed);
    result
}

/// The verified manifest, from the job folder when an earlier run saved
/// it, else downloaded and saved there.
async fn load_manifest(
    ctx: &Ctx<'_>,
    dir: &Dir,
    source: &DepotSource,
    stop: &mut StopRx,
) -> Result<Manifest> {
    let cached = dir.manifest();
    if let Ok(bytes) = tokio::fs::read(&cached).await {
        if let Ok(m) = Manifest::from_bytes(&bytes, Some(source.manifest_hash)) {
            return Ok(m);
        }
        // A different version's: everything staged is stale too.
        tracing::info!("the staged download is of another version; starting over");
        discard_work(dir).await;
    }
    let bytes =
        fetch::fetch_small(ctx.http(), &source.manifest_url, MAX_MANIFEST_BYTES, stop).await?;
    let manifest = Manifest::from_bytes(&bytes, Some(source.manifest_hash))?;
    tokio::fs::create_dir_all(dir.root()).await.at(dir.root())?;
    io::write_atomic(&cached, &bytes).at(&cached)?;
    Ok(manifest)
}

/// Bytes the staged files still have to grow by.
fn staged_bytes_needed(manifest: &Manifest, tree: &Path, staged: &[u32]) -> u64 {
    staged
        .iter()
        .map(|&fi| {
            let f = &manifest.files[fi as usize];
            let have = std::fs::metadata(tree.join(&f.path)).map_or(0, |m| m.len());
            f.size.saturating_sub(have)
        })
        .sum()
}

/// Deletes the staged files and journal, keeping the folder.
async fn discard_work(dir: &Dir) {
    let dir = dir.clone();
    let _ = tokio::task::spawn_blocking(move || {
        remove_dir(&dir.tree());
        let _ = std::fs::remove_file(dir.journal());
        let _ = std::fs::remove_file(dir.manifest());
    })
    .await;
}

/// Finishes or undoes an interrupted commit in `staging`'s job folder.
/// Blocking. Errors are logged: the next job tries again.
pub(crate) fn recover(staging: &Staging) {
    let dir = Dir::new(staging.depot());
    if let Err(e) = commit::recover(&dir) {
        tracing::error!(dir = %dir.root().display(), error = %e, "could not finish an interrupted update");
    }
}

/// Deletes a job folder, unless it holds files set aside by a commit that
/// could not be finished. Blocking.
pub(crate) fn discard(staging: &Staging) {
    let dir = Dir::new(staging.depot());
    if !dir.root().exists() {
        return;
    }
    recover(staging);
    if dir.commit().exists() {
        tracing::error!(dir = %dir.root().display(), "keeping a job folder with an unfinished update");
        return;
    }
    remove_dir(dir.root());
}
