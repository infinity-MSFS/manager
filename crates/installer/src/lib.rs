//! Installs product packages into the sim's install folder.
//!
//! An install runs as a job: download (resumable), verify the archive,
//! extract into staging while hashing every file, check the result against
//! the package manifest, then swap it into place in one rename. Nothing in
//! the install folder changes until the very last step, and a package that
//! does not match its manifest never gets there.
//!
//! Jobs are keyed by product: one per product at a time. A paused or
//! failed job is replaced by the next install of that product, which picks
//! up its partial download. Downloads outlive the app: on the next start
//! [`Installer::find_interrupted`] and [`Installer::restore`] list them as
//! paused jobs again.
//!
//! Packages are zip or `.tar.zst`, as the catalog declares; the file must
//! agree with the declaration.

mod download;
mod error;
mod extract;
mod inspect;
mod job;
pub mod manifest;
mod resume;
mod space;
mod staging;
mod swap;
mod tarzst;

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use infinity_ipc::catalog::{ArchiveFormat, ProductId};
use infinity_ipc::dto::{Channel, InstalledPackage, Job, JobPhase, Progress};
use infinity_ipc::{IpcError, IpcErrorCode, IpcResult};
use infinity_persistence::InstalledStore;
use tokio::sync::{Semaphore, broadcast, watch};
use url::Url;

pub use inspect::{Wanted, dir_size, package_version, rescan};
pub use manifest::Manifest;
pub use resume::{Interrupted, KEEP_INTERRUPTED_FOR};

use crate::error::{Error, IoContext, Result, Stop};
use crate::job::StopRx;
use crate::staging::Staging;

const PARALLEL_DOWNLOADS: usize = 2;
/// Extraction is disk-bound; two at once only slows both down.
const PARALLEL_INSTALLS: usize = 1;
/// A package that fails its manifest is downloaded once more from scratch
/// before the job gives up.
const ATTEMPTS: u32 = 2;
const PROGRESS_EVERY: Duration = Duration::from_millis(150);
const MAX_LISTED_PROBLEMS: usize = 5;

/// Everything needed to install one product version.
#[derive(Debug, Clone)]
pub struct Plan {
    pub product_id: ProductId,
    pub name: String,
    pub version: String,
    pub channel: Channel,
    pub archive_url: Url,
    /// The catalog's storage key for the archive. Identifies the file
    /// across restarts, when `archive_url` (presigned) has expired.
    pub archive_key: String,
    /// What the catalog says the archive is. Checked against the file.
    pub format: ArchiveFormat,
    /// The package manifest. Without one, only the zip's own checksums
    /// guard the install.
    pub manifest_url: Option<Url>,
    pub package_dir_name: String,
    /// The Community folder.
    pub install_dir: PathBuf,
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

    /// Starts installing. Returns the running job instead when one for the
    /// product is already active, so a double click is harmless.
    pub fn start(&self, plan: Plan) -> IpcResult<Job> {
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
        tracing::info!(product = %plan.product_id, version = %plan.version, "install queued");

        let inner = self.inner.clone();
        tokio::spawn(async move { inner.run(plan, generation, stop_rx).await });
        Ok(job)
    }

    /// Finds downloads a previous run of the app left in the staging
    /// folders of `install_dirs`, deleting ones untouched for longer than
    /// `max_age`. List the ones worth keeping with [`Installer::restore`]
    /// and drop the rest with [`Installer::discard`].
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
        let _ = tokio::task::spawn_blocking(move || staging.discard_download()).await;
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

    fn set_phase(&self, id: &str, generation: u64, phase: JobPhase) {
        self.update(id, generation, |s| {
            s.job.phase = phase;
            s.job.progress = None;
        });
    }

    /// A progress callback that broadcasts at most every [`PROGRESS_EVERY`].
    fn reporter<'a>(
        &'a self,
        id: &'a str,
        generation: u64,
    ) -> impl FnMut(u64, Option<u64>) + Send + 'a {
        let mut last = None::<Instant>;
        move |done, total| {
            let due = last.is_none_or(|t| t.elapsed() >= PROGRESS_EVERY)
                || total.is_some_and(|t| done >= t);
            if due {
                last = Some(Instant::now());
                self.update(id, generation, |s| {
                    s.job.progress = Some(Progress { done, total });
                });
            }
        }
    }

    async fn run(self: Arc<Self>, plan: Plan, generation: u64, mut stop: StopRx) {
        let id = plan.product_id.as_str();
        let result = self.pipeline(&plan, generation, &mut stop).await;
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
                tracing::info!(product = id, version = %plan.version, "installed");
                cleanup(true).await;
                self.finish(id, generation, JobPhase::Completed);
            }
            Err(Error::Stopped(Stop::Pause)) => {
                tracing::info!(product = id, "install paused");
                cleanup(false).await;
                self.update(id, generation, |s| s.job.phase = JobPhase::Paused);
            }
            Err(Error::Stopped(Stop::Cancel)) => {
                tracing::info!(product = id, "install cancelled");
                cleanup(true).await;
                self.finish(id, generation, JobPhase::Cancelled);
            }
            Err(e) => {
                tracing::warn!(product = id, error = %e, "install failed");
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

    async fn pipeline(&self, plan: &Plan, generation: u64, stop: &mut StopRx) -> Result<()> {
        let id = plan.product_id.as_str();
        let install_dir = plan.install_dir.clone();
        let staging = tokio::task::spawn_blocking({
            let id = id.to_owned();
            move || Staging::create(&install_dir, &id)
        })
        .await
        .map_err(|e| Error::Internal(e.to_string()))??;
        self.update(id, generation, |s| s.staging = Some(staging.clone()));

        let manifest = match &plan.manifest_url {
            Some(url) => Some(self.fetch_manifest(url, stop).await?),
            None => {
                tracing::warn!(
                    product = id,
                    "no package manifest; relying on zip checksums"
                );
                None
            }
        };
        let manifest = manifest.map(Arc::new);

        let mut attempt = 1;
        loop {
            let attempted = async {
                self.download(plan, &staging, manifest.as_deref(), generation, stop)
                    .await?;
                self.extract_and_check(plan, &staging, manifest.clone(), generation, stop)
                    .await
            };
            match attempted.await {
                Ok(()) => break,
                Err(e @ (Error::Archive(_) | Error::Checksum { .. })) if attempt < ATTEMPTS => {
                    tracing::warn!(product = id, error = %e, "package damaged, downloading again");
                    download::forget(&staging).await;
                    attempt += 1;
                }
                Err(e) => {
                    if matches!(e, Error::Archive(_) | Error::Checksum { .. }) {
                        download::forget(&staging).await;
                    }
                    return Err(e);
                }
            }
        }

        self.set_phase(id, generation, JobPhase::Installing);
        let permit = acquire(&self.installs, stop).await?;
        let target = plan.install_dir.join(&plan.package_dir_name);
        let staged = staging.extract();
        let marker = inspect::marker_json(&plan.version, &plan.package_dir_name);
        let swap_target = target.clone();
        let swap_staging = staging.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let marker_path = staged.join(inspect::MARKER_FILE);
            std::fs::write(&marker_path, marker).at(&marker_path)?;
            swap::install(&staged, &swap_target, &swap_staging)
        })
        .await
        .map_err(|e| Error::Internal(e.to_string()))??;
        drop(permit);

        let record = InstalledPackage {
            product_id: plan.product_id.clone(),
            version: Some(plan.version.clone()),
            path: target.to_string_lossy().into_owned(),
            package_dir_name: plan.package_dir_name.clone(),
            installed_at: Some(unix_now()),
        };
        // The package is in place either way; a rescan repairs the record.
        if let Err(e) = self.registry.upsert(record).await {
            tracing::error!(product = id, error = %e, "installed but could not record it");
        }
        Ok(())
    }

    async fn download(
        &self,
        plan: &Plan,
        staging: &Staging,
        manifest: Option<&Manifest>,
        generation: u64,
        stop: &mut StopRx,
    ) -> Result<()> {
        let id = plan.product_id.as_str();
        let archive = manifest.and_then(|m| m.archive.as_ref());
        let _permit = acquire(&self.downloads, stop).await?;
        self.set_phase(id, generation, JobPhase::Downloading);
        let source = download::Source {
            url: &plan.archive_url,
            version: &plan.version,
            size: archive.map(|a| a.size),
            record: download::Record {
                product_id: plan.product_id.clone(),
                name: plan.name.clone(),
                channel: plan.channel,
                package_dir_name: plan.package_dir_name.clone(),
                install_dir: plan.install_dir.clone(),
                format: plan.format,
                archive_key: plan.archive_key.clone(),
            },
        };
        download::fetch(
            &self.http,
            staging,
            &source,
            stop,
            &mut self.reporter(id, generation),
        )
        .await?;

        let Some(expected) = archive.cloned() else {
            return Ok(());
        };
        self.set_phase(id, generation, JobPhase::Verifying);
        let path = staging.download();
        let total = expected.size;
        let actual = job::blocking(stop, &mut self.reporter(id, generation), move |p| {
            p.total.store(total, std::sync::atomic::Ordering::Relaxed);
            manifest::digest_with_progress(
                &path,
                |n| p.done.store(n, std::sync::atomic::Ordering::Relaxed),
                || p.stop.load(std::sync::atomic::Ordering::Relaxed),
            )
            .at(&path)
        })
        .await?;
        if actual != expected {
            download::forget(staging).await;
            return Err(Error::Checksum {
                expected: expected.sha256,
                actual: actual.sha256,
            });
        }
        Ok(())
    }

    async fn extract_and_check(
        &self,
        plan: &Plan,
        staging: &Staging,
        manifest: Option<Arc<Manifest>>,
        generation: u64,
        stop: &mut StopRx,
    ) -> Result<()> {
        let id = plan.product_id.as_str();
        let _permit = acquire(&self.installs, stop).await?;
        self.set_phase(id, generation, JobPhase::Extracting);
        let archive = staging.download();
        let dest = staging.extract();
        let scratch = staging.scratch();
        let format = plan.format;
        let size_hint = manifest.as_deref().map(Manifest::total_size);
        job::blocking(stop, &mut self.reporter(id, generation), move |progress| {
            staging::remove_dir(&dest);
            let target = extract::Target {
                dest: &dest,
                scratch: &scratch,
                size_hint,
            };
            let Some(written) = extract::extract(format, &archive, target, progress)? else {
                return Ok(None);
            };
            if let Some(manifest) = manifest {
                let problems = manifest.compare(&written);
                if !problems.is_empty() {
                    return Err(damaged(&problems));
                }
            }
            Ok(Some(()))
        })
        .await
    }

    async fn fetch_manifest(&self, url: &Url, stop: &mut StopRx) -> Result<Manifest> {
        let request = async {
            let response = self.http.get(url.clone()).send().await?;
            if !response.status().is_success() {
                return Err(Error::Http(response.status()));
            }
            let manifest: Manifest = response
                .json()
                .await
                .map_err(|e| Error::Archive(format!("the package manifest is unreadable: {e}")))?;
            if manifest.schema_version > manifest::SCHEMA_VERSION {
                return Err(Error::Archive(format!(
                    "the package manifest is format {}; update the manager",
                    manifest.schema_version
                )));
            }
            Ok(manifest)
        };
        tokio::select! {
            result = request => result,
            stop = job::stopped(stop) => Err(Error::Stopped(stop)),
        }
    }
}

async fn acquire<'a>(
    semaphore: &'a Semaphore,
    stop: &mut StopRx,
) -> Result<tokio::sync::SemaphorePermit<'a>> {
    tokio::select! {
        permit = semaphore.acquire() => permit.map_err(|e| Error::Internal(e.to_string())),
        stop = job::stopped(stop) => Err(Error::Stopped(stop)),
    }
}

fn damaged(problems: &[manifest::Discrepancy]) -> Error {
    let mut listed: Vec<String> = problems
        .iter()
        .take(MAX_LISTED_PROBLEMS)
        .map(ToString::to_string)
        .collect();
    if problems.len() > MAX_LISTED_PROBLEMS {
        listed.push(format!("and {} more", problems.len() - MAX_LISTED_PROBLEMS));
    }
    Error::Archive(format!(
        "it does not match the package manifest: {}",
        listed.join(", ")
    ))
}

fn validate(plan: &Plan) -> IpcResult<()> {
    if plan.format == ArchiveFormat::Unknown {
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
mod tests;
