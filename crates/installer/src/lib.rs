//! Installs product packages into the sim's install folder, and verifies
//! and repairs them.
//!
//! Two release formats are supported:
//!
//! - **Depots** ([`depot`]): content-addressed chunks in shared packs, as
//!   described in `infinity-depot`. Updates download only changed chunks,
//!   downloads resume per chunk, every file is verified chunk by chunk
//!   before it is installed, and a damaged install is repaired by fetching
//!   only the damaged chunks.
//! - **Archives** ([`archive`]): one zip or `.tar.zst` per release, the
//!   legacy format. Resumable, verified against its manifest, swapped in
//!   whole; an update or repair downloads everything again.
//!
//! Either way an install runs as a job, and the install folder does not
//! change until the very last step, which can never leave a mix of two
//! versions behind.
//!
//! Jobs are keyed by product: one per product at a time. A paused or
//! failed job is replaced by the next install of that product, which picks
//! up its partial download. Downloads outlive the app: on the next start
//! [`Installer::find_interrupted`] and [`Installer::restore`] list them as
//! paused jobs again.

mod archive;
pub mod depot;
mod download;
mod error;
mod extract;
mod inspect;
mod job;
pub mod manifest;
pub mod presets;
mod resume;
mod space;
mod staging;
mod swap;
mod tarzst;

use std::collections::{HashMap, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use infinity_ipc::catalog::{ArchiveFormat, ProductId};
use infinity_ipc::dto::{Channel, InstalledPackage, Job, JobKind, JobPhase, Progress};
use infinity_ipc::{IpcError, IpcErrorCode, IpcResult};
use infinity_persistence::InstalledStore;
use tokio::sync::{Semaphore, SemaphorePermit, broadcast, watch};

pub use archive::ArchiveSource;
pub use depot::{DepotSource, PackUrls};
pub use inspect::{MARKER_FILE, Wanted, dir_size, package_version, rescan};
pub use manifest::Manifest;
pub use resume::{Interrupted, KEEP_INTERRUPTED_FOR};

use crate::error::{Error, Result, Stop};
use crate::job::StopRx;
use crate::staging::Staging;

const PARALLEL_DOWNLOADS: usize = 2;
/// Extraction and commits are disk-bound; two at once only slows both down.
const PARALLEL_INSTALLS: usize = 1;
const PROGRESS_EVERY: Duration = Duration::from_millis(150);
/// Throughput is averaged over this long.
const RATE_WINDOW: Duration = Duration::from_secs(5);

/// Everything needed to install one product version.
#[derive(Debug, Clone)]
pub struct Plan {
    pub product_id: ProductId,
    pub name: String,
    pub version: String,
    pub channel: Channel,
    pub package_dir_name: String,
    /// The Community folder.
    pub install_dir: PathBuf,
    pub source: Source,
}

/// Where the release comes from.
#[derive(Debug, Clone)]
pub enum Source {
    Archive(ArchiveSource),
    Depot(DepotSource),
}

#[derive(Debug, Clone)]
pub struct Installer {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    http: reqwest::Client,
    registry: Arc<InstalledStore>,
    slots: Mutex<HashMap<ProductId, Slot>>,
    changes: broadcast::Sender<Job>,
    downloads: Semaphore,
    installs: Semaphore,
    generation: std::sync::atomic::AtomicU64,
}

#[derive(Debug)]
struct Slot {
    job: Job,
    generation: u64,
    stop: watch::Sender<Option<Stop>>,
    staging: Option<Staging>,
}

impl Installer {
    pub fn new(registry: Arc<InstalledStore>) -> IpcResult<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("infinity-manager/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
            .pool_max_idle_per_host(depot::fetch_connections())
            .build()
            .map_err(|e| IpcError::internal(format!("http client: {e}")))?;
        Ok(Self {
            inner: Arc::new(Inner {
                http,
                registry,
                slots: Mutex::new(HashMap::new()),
                changes: broadcast::channel(256).0,
                downloads: Semaphore::new(PARALLEL_DOWNLOADS),
                installs: Semaphore::new(PARALLEL_INSTALLS),
                generation: Default::default(),
            }),
        })
    }

    /// Every job change, progress included (throttled).
    pub fn subscribe(&self) -> broadcast::Receiver<Job> {
        self.inner.changes.subscribe()
    }

    pub fn jobs(&self) -> Vec<Job> {
        let mut jobs: Vec<Job> = self.inner.slots().values().map(|s| s.job.clone()).collect();
        jobs.sort_by(|a, b| a.product_id.cmp(&b.product_id));
        jobs
    }

    /// Whether a job for `product_id` is queued or working.
    pub fn is_busy(&self, product_id: &str) -> bool {
        self.inner
            .slots()
            .get(product_id)
            .is_some_and(|s| s.job.is_active())
    }

    /// Starts installing or updating. Returns the running job instead when
    /// one for the product is already active, so a double click is
    /// harmless.
    pub fn start(&self, plan: Plan) -> IpcResult<Job> {
        self.begin(plan, JobKind::Install)
    }

    /// Checks every installed file of the product against `plan`'s release
    /// and repairs what is damaged or missing. Returns the running job
    /// instead when one for the product is already active.
    pub fn verify(&self, plan: Plan) -> IpcResult<Job> {
        self.begin(plan, JobKind::Verify)
    }

    fn begin(&self, plan: Plan, kind: JobKind) -> IpcResult<Job> {
        validate(&plan)?;
        let mut slots = self.inner.slots();
        if let Some(slot) = slots.get(&plan.product_id)
            && slot.job.is_active()
        {
            return Ok(slot.job.clone());
        }
        let generation = self
            .inner
            .generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (stop, stop_rx) = watch::channel(None);
        let job = Job {
            product_id: plan.product_id.clone(),
            name: plan.name.clone(),
            version: plan.version.clone(),
            channel: plan.channel,
            kind,
            phase: JobPhase::Queued,
            progress: None,
        };
        slots.insert(
            plan.product_id.clone(),
            Slot {
                job: job.clone(),
                generation,
                stop,
                staging: None,
            },
        );
        drop(slots);
        let _ = self.inner.changes.send(job.clone());
        tracing::info!(product = %plan.product_id, version = %plan.version, ?kind, "job queued");

        let inner = self.inner.clone();
        tokio::spawn(async move { inner.run(plan, kind, generation, stop_rx).await });
        Ok(job)
    }

    /// Finds downloads a previous run of the app left in the staging
    /// folders of `install_dirs`, deleting ones untouched for longer than
    /// `max_age`, and finishes updates a crash interrupted. List the ones
    /// worth keeping with [`Installer::restore`] and drop the rest with
    /// [`Installer::discard`].
    pub async fn find_interrupted(
        &self,
        install_dirs: Vec<PathBuf>,
        max_age: Duration,
    ) -> Vec<Interrupted> {
        let now = unix_now();
        tokio::task::spawn_blocking(move || resume::find(&install_dirs, now, max_age))
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "scanning for interrupted downloads");
                Vec::new()
            })
    }

    /// Lists a found download as a paused job. Resuming it is an
    /// [`Installer::start`] of the product, which continues the download;
    /// [`Installer::cancel`] deletes it. Returns `false`, leaving it alone,
    /// when the product already has a job.
    pub fn restore(&self, found: Interrupted) -> bool {
        let job = found.job.clone();
        {
            let mut slots = self.inner.slots();
            if slots.contains_key(&job.product_id) {
                return false;
            }
            let generation = self
                .inner
                .generation
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            slots.insert(
                job.product_id.clone(),
                Slot {
                    job: job.clone(),
                    generation,
                    stop: watch::channel(None).0,
                    staging: Some(found.staging),
                },
            );
        }
        tracing::info!(product = %job.product_id, version = %job.version, "found an interrupted download");
        let _ = self.inner.changes.send(job);
        true
    }

    /// Deletes a found download that is not worth resuming.
    pub async fn discard(&self, found: Interrupted) {
        tracing::info!(product = %found.job.product_id, version = %found.job.version, "discarding an outdated interrupted download");
        let staging = found.staging;
        let _ = tokio::task::spawn_blocking(move || {
            staging.discard_download();
            depot::discard(&staging);
        })
        .await;
    }

    /// Stops a job and keeps its download for a later resume.
    pub fn pause(&self, product_id: &str) -> IpcResult<()> {
        self.signal(product_id, Stop::Pause)
    }

    /// Stops a job and deletes everything it downloaded.
    pub async fn cancel(&self, product_id: &str) -> IpcResult<()> {
        let stopped = {
            let mut slots = self.inner.slots();
            match slots.get(product_id) {
                None => return Err(job_not_found(product_id)),
                Some(slot) if slot.job.is_active() => None,
                Some(_) => slots.remove(product_id),
            }
        };
        let Some(slot) = stopped else {
            return self.signal(product_id, Stop::Cancel);
        };
        if let Some(staging) = slot.staging {
            let _ = tokio::task::spawn_blocking(move || staging.discard()).await;
        }
        let mut job = slot.job;
        job.phase = JobPhase::Cancelled;
        let _ = self.inner.changes.send(job);
        Ok(())
    }

    /// Deletes an installed package and forgets it.
    ///
    /// Refuses a recorded path that is not a folder named after the
    /// package, directly inside an install folder: a damaged or hand-edited
    /// registry must never turn uninstall into "delete this directory".
    pub async fn uninstall(&self, package: &InstalledPackage) -> IpcResult<()> {
        if self.is_busy(&package.product_id) {
            return Err(IpcError::new(
                IpcErrorCode::IoError,
                format!("{} is being installed", package.product_id),
            ));
        }
        let path = PathBuf::from(&package.path);
        let parent = path
            .parent()
            .filter(|_| path.file_name() == Some(package.package_dir_name.as_ref()))
            .filter(|_| is_plain_name(&package.package_dir_name))
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                IpcError::new(
                    IpcErrorCode::PackageDirNotFound,
                    format!("refusing to delete unexpected path {}", path.display()),
                )
            })?;
        let id = package.product_id.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let staging = Staging::create(&parent, &id)?;
            let removed = swap::remove(&path, &staging);
            staging.discard();
            removed
        })
        .await
        .map_err(|e| IpcError::internal(e.to_string()))??;
        self.inner.registry.remove(&package.product_id).await?;
        tracing::info!(product = %package.product_id, "uninstalled");
        Ok(())
    }

    fn signal(&self, product_id: &str, stop: Stop) -> IpcResult<()> {
        let slots = self.inner.slots();
        let slot = slots
            .get(product_id)
            .ok_or_else(|| job_not_found(product_id))?;
        if slot.job.is_active() {
            slot.stop.send_replace(Some(stop));
        }
        Ok(())
    }
}

/// One job's handle on the installer, for the pipelines.
pub(crate) struct Ctx<'a> {
    inner: &'a Inner,
    id: &'a str,
    generation: u64,
}

impl Ctx<'_> {
    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.inner.http
    }

    pub(crate) fn phase(&self, phase: JobPhase) {
        self.inner.update(self.id, self.generation, |s| {
            s.job.phase = phase;
            s.job.progress = None;
        });
    }

    /// A progress callback that broadcasts at most every
    /// [`PROGRESS_EVERY`], with the recent throughput.
    pub(crate) fn reporter(&self) -> impl FnMut(u64, Option<u64>) + Send + '_ {
        let mut last = None::<Instant>;
        let mut rate = Rate::default();
        move |done, total| {
            let due = last.is_none_or(|t| t.elapsed() >= PROGRESS_EVERY)
                || total.is_some_and(|t| done >= t);
            if due {
                last = Some(Instant::now());
                let rate = rate.sample(done);
                self.inner.update(self.id, self.generation, |s| {
                    s.job.progress = Some(Progress { done, total, rate });
                });
            }
        }
    }

    pub(crate) async fn acquire_download(&self, stop: &mut StopRx) -> Result<SemaphorePermit<'_>> {
        acquire(&self.inner.downloads, stop).await
    }

    pub(crate) async fn acquire_install(&self, stop: &mut StopRx) -> Result<SemaphorePermit<'_>> {
        acquire(&self.inner.installs, stop).await
    }
}

/// Bytes per second over the last [`RATE_WINDOW`].
#[derive(Debug, Default)]
struct Rate {
    samples: VecDeque<(Instant, u64)>,
}

impl Rate {
    fn sample(&mut self, done: u64) -> Option<u64> {
        let now = Instant::now();
        if self.samples.back().is_some_and(|&(_, d)| d > done) {
            self.samples.clear();
        }
        self.samples.push_back((now, done));
        while self
            .samples
            .front()
            .is_some_and(|&(t, _)| now.duration_since(t) > RATE_WINDOW)
        {
            self.samples.pop_front();
        }
        let &(t0, d0) = self.samples.front()?;
        let elapsed = now.duration_since(t0).as_secs_f64();
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        (elapsed >= 1.0).then(|| ((done - d0) as f64 / elapsed) as u64)
    }
}

impl Inner {
    fn slots(&self) -> MutexGuard<'_, HashMap<ProductId, Slot>> {
        self.slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Applies `f` to the job if it is still this generation's, and
    /// broadcasts the result.
    fn update(&self, id: &str, generation: u64, f: impl FnOnce(&mut Slot)) {
        let job = {
            let mut slots = self.slots();
            let Some(slot) = slots.get_mut(id).filter(|s| s.generation == generation) else {
                return;
            };
            f(slot);
            slot.job.clone()
        };
        let _ = self.changes.send(job);
    }

    async fn run(self: Arc<Self>, plan: Plan, kind: JobKind, generation: u64, mut stop: StopRx) {
        let id = plan.product_id.as_str();
        let result = self.pipeline(&plan, kind, generation, &mut stop).await;
        let staging = self
            .slots()
            .get(id)
            .filter(|s| s.generation == generation)
            .and_then(|s| s.staging.clone());
        let cleanup = |discard: bool| async move {
            if let Some(staging) = staging {
                let _ = tokio::task::spawn_blocking(move || {
                    if discard {
                        staging.discard();
                    } else {
                        staging.clear_work();
                    }
                })
                .await;
            }
        };
        match result {
            Ok(()) => {
                tracing::info!(product = id, version = %plan.version, ?kind, "job done");
                cleanup(true).await;
                self.finish(id, generation, JobPhase::Completed);
            }
            Err(Error::Stopped(Stop::Pause)) => {
                tracing::info!(product = id, "job paused");
                cleanup(false).await;
                self.update(id, generation, |s| s.job.phase = JobPhase::Paused);
            }
            Err(Error::Stopped(Stop::Cancel)) => {
                tracing::info!(product = id, "job cancelled");
                cleanup(true).await;
                self.finish(id, generation, JobPhase::Cancelled);
            }
            Err(e) => {
                tracing::warn!(product = id, error = %e, "job failed");
                cleanup(false).await;
                let error = IpcError::from(e);
                self.update(id, generation, |s| s.job.phase = JobPhase::Failed { error });
            }
        }
    }

    /// Broadcasts a terminal phase and forgets the job.
    fn finish(&self, id: &str, generation: u64, phase: JobPhase) {
        let job = {
            let mut slots = self.slots();
            if slots.get(id).is_none_or(|s| s.generation != generation) {
                return;
            }
            slots.remove(id).map(|mut s| {
                s.job.phase = phase;
                s.job.progress = None;
                s.job
            })
        };
        if let Some(job) = job {
            let _ = self.changes.send(job);
        }
    }

    async fn pipeline(
        &self,
        plan: &Plan,
        kind: JobKind,
        generation: u64,
        stop: &mut StopRx,
    ) -> Result<()> {
        let id = plan.product_id.as_str();
        let install_dir = plan.install_dir.clone();
        let depot_release = matches!(plan.source, Source::Depot(_));
        let staging = tokio::task::spawn_blocking({
            let id = id.to_owned();
            move || -> Result<Staging> {
                let staging = Staging::create(&install_dir, &id)?;
                // Whatever format the last job used, an update it left half
                // committed is finished first.
                depot::recover(&staging);
                // Leftovers of the other format would never resume.
                if depot_release {
                    staging.discard_download();
                    staging.clear_work();
                } else {
                    depot::discard(&staging);
                }
                Ok(staging)
            }
        })
        .await
        .map_err(|e| Error::Internal(e.to_string()))??;
        self.update(id, generation, |s| s.staging = Some(staging.clone()));

        let ctx = Ctx {
            inner: self,
            id,
            generation,
        };
        match &plan.source {
            Source::Archive(source) => {
                archive::run(&ctx, plan, source, kind, &staging, stop).await?
            }
            Source::Depot(source) => depot::run(&ctx, plan, source, kind, &staging, stop).await?,
        }

        let target = plan.install_dir.join(&plan.package_dir_name);
        let record = InstalledPackage {
            product_id: plan.product_id.clone(),
            version: Some(plan.version.clone()),
            path: target.to_string_lossy().into_owned(),
            package_dir_name: plan.package_dir_name.clone(),
            installed_at: Some(unix_now()),
        };
        // A verify of an already-current package keeps its install time.
        let keep = match (kind, self.registry.get(&plan.product_id).await) {
            (JobKind::Verify, Ok(Some(existing))) if existing.version == record.version => {
                Some(existing)
            }
            _ => None,
        };
        // The package is in place either way; a rescan repairs the record.
        if let Err(e) = self.registry.upsert(keep.unwrap_or(record)).await {
            tracing::error!(product = id, error = %e, "installed but could not record it");
        }
        Ok(())
    }
}

async fn acquire<'a>(semaphore: &'a Semaphore, stop: &mut StopRx) -> Result<SemaphorePermit<'a>> {
    tokio::select! {
        permit = semaphore.acquire() => permit.map_err(|e| Error::Internal(e.to_string())),
        stop = job::stopped(stop) => Err(Error::Stopped(stop)),
    }
}

fn validate(plan: &Plan) -> IpcResult<()> {
    if let Source::Archive(a) = &plan.source
        && a.format == ArchiveFormat::Unknown
    {
        return Err(IpcError::unsupported(format!(
            "{} uses an archive format this version cannot install; update the manager",
            plan.name
        )));
    }
    if !is_plain_name(&plan.package_dir_name) {
        return Err(IpcError::new(
            IpcErrorCode::ManifestInvalid,
            format!("bad package folder name {:?}", plan.package_dir_name),
        ));
    }
    if !plan.install_dir.is_absolute() {
        return Err(IpcError::new(
            IpcErrorCode::PackageDirNotFound,
            format!(
                "install folder {} is not absolute",
                plan.install_dir.display()
            ),
        ));
    }
    Ok(())
}

/// A single ordinary path component.
fn is_plain_name(name: &str) -> bool {
    let mut parts = Path::new(name).components();
    matches!(parts.next(), Some(Component::Normal(_))) && parts.next().is_none()
}

fn job_not_found(product_id: &str) -> IpcError {
    IpcError::new(
        IpcErrorCode::JobNotFound,
        format!("no install of {product_id} in progress"),
    )
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod depot_tests;
#[cfg(test)]
mod presets_tests;
#[cfg(test)]
mod tests;
