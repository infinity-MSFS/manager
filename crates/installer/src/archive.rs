//! Installing from a single archive: the legacy release format (the T-38
//! and every release before depots).
//!
//! Download (resumable), verify the archive, extract into staging while
//! hashing every file, check the result against the package manifest, then
//! swap the whole package into place in one rename. An update downloads
//! the whole archive again.

use std::sync::Arc;

use infinity_ipc::catalog::ArchiveFormat;
use infinity_ipc::dto::{JobKind, JobPhase};
use url::Url;

use crate::error::{Error, IoContext, Result};
use crate::job::{self, StopRx};
use crate::manifest::{self, Manifest};
use crate::staging::Staging;
use crate::{Ctx, Plan, download, extract, inspect, staging, swap};

/// A package that fails its manifest is downloaded once more from scratch
/// before the job gives up.
const ATTEMPTS: u32 = 2;
const MAX_LISTED_PROBLEMS: usize = 5;

/// Where a legacy release comes from.
#[derive(Debug, Clone)]
pub struct ArchiveSource {
    pub url: Url,
    /// The catalog's storage key for the archive. Identifies the file
    /// across restarts, when `url` (presigned) has expired.
    pub key: String,
    /// What the catalog says the archive is. Checked against the file.
    pub format: ArchiveFormat,
    /// The package manifest. Without one, only the zip's own checksums
    /// guard the install, and the package cannot be verified later.
    pub manifest_url: Option<Url>,
}

pub(crate) async fn run(
    ctx: &Ctx<'_>,
    plan: &Plan,
    source: &ArchiveSource,
    kind: JobKind,
    staging: &Staging,
    stop: &mut StopRx,
) -> Result<()> {
    let manifest = match &source.manifest_url {
        Some(url) => Some(Arc::new(fetch_manifest(ctx, url, stop).await?)),
        None => {
            tracing::warn!(product = %plan.product_id, "no package manifest; relying on zip checksums");
            None
        }
    };

    if kind == JobKind::Verify {
        let Some(manifest) = manifest.clone() else {
            return Err(Error::Unsupported(format!(
                "{} cannot be checked file by file; reinstall it instead",
                plan.name
            )));
        };
        let target = plan.install_dir.join(&plan.package_dir_name);
        if !target.is_dir() {
            return Err(Error::NotInstalled(plan.name.clone()));
        }
        ctx.phase(JobPhase::Checking);
        let problems = job::blocking(stop, &mut ctx.reporter(), move |p| {
            Ok(check_installed(&manifest, &target, p))
        })
        .await?;
        if problems.is_empty() {
            tracing::info!(product = %plan.product_id, "every installed file is intact");
            return Ok(());
        }
        // One damaged file means the whole archive: this format has nothing
        // smaller to fetch.
        tracing::warn!(product = %plan.product_id, problems = problems.len(), "installed files are damaged; reinstalling");
    }

    let mut attempt = 1;
    loop {
        let attempted = async {
            download(ctx, plan, source, staging, manifest.as_deref(), stop).await?;
            extract_and_check(ctx, source.format, staging, manifest.clone(), stop).await
        };
        match attempted.await {
            Ok(()) => break,
            Err(e @ (Error::Archive(_) | Error::Checksum { .. })) if attempt < ATTEMPTS => {
                tracing::warn!(product = %plan.product_id, error = %e, "package damaged, downloading again");
                download::forget(staging).await;
                attempt += 1;
            }
            Err(e) => {
                if matches!(e, Error::Archive(_) | Error::Checksum { .. }) {
                    download::forget(staging).await;
                }
                return Err(e);
            }
        }
    }

    ctx.phase(JobPhase::Installing);
    let permit = ctx.acquire_install(stop).await?;
    let target = plan.install_dir.join(&plan.package_dir_name);
    let staged = staging.extract();
    let marker = inspect::marker_json(&plan.version, &plan.package_dir_name);
    let swap_staging = staging.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let marker_path = staged.join(inspect::MARKER_FILE);
        std::fs::write(&marker_path, marker).at(&marker_path)?;
        swap::install(&staged, &target, &swap_staging)
    })
    .await
    .map_err(|e| Error::Internal(e.to_string()))??;
    drop(permit);
    Ok(())
}

async fn download(
    ctx: &Ctx<'_>,
    plan: &Plan,
    source: &ArchiveSource,
    staging: &Staging,
    manifest: Option<&Manifest>,
    stop: &mut StopRx,
) -> Result<()> {
    let archive = manifest.and_then(|m| m.archive.as_ref());
    let _permit = ctx.acquire_download(stop).await?;
    ctx.phase(JobPhase::Downloading);
    let wanted = download::Source {
        url: &source.url,
        version: &plan.version,
        size: archive.map(|a| a.size),
        record: download::Record {
            product_id: plan.product_id.clone(),
            name: plan.name.clone(),
            channel: plan.channel,
            package_dir_name: plan.package_dir_name.clone(),
            install_dir: plan.install_dir.clone(),
            format: source.format,
            archive_key: source.key.clone(),
        },
    };
    download::fetch(ctx.http(), staging, &wanted, stop, &mut ctx.reporter()).await?;

    let Some(expected) = archive.cloned() else {
        return Ok(());
    };
    ctx.phase(JobPhase::Verifying);
    let path = staging.download();
    let total = expected.size;
    let actual = job::blocking(stop, &mut ctx.reporter(), move |p| {
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
    ctx: &Ctx<'_>,
    format: ArchiveFormat,
    staging: &Staging,
    manifest: Option<Arc<Manifest>>,
    stop: &mut StopRx,
) -> Result<()> {
    let _permit = ctx.acquire_install(stop).await?;
    ctx.phase(JobPhase::Extracting);
    let archive = staging.download();
    let dest = staging.extract();
    let scratch = staging.scratch();
    let size_hint = manifest.as_deref().map(Manifest::total_size);
    job::blocking(stop, &mut ctx.reporter(), move |progress| {
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

async fn fetch_manifest(ctx: &Ctx<'_>, url: &Url, stop: &mut StopRx) -> Result<Manifest> {
    let request = async {
        let response = ctx.http().get(url.clone()).send().await?;
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

/// Hashes the installed files the manifest lists. Blocking; `None` when
/// stopped.
fn check_installed(
    manifest: &Manifest,
    package: &std::path::Path,
    progress: &extract::Progress,
) -> Option<Vec<manifest::Discrepancy>> {
    use std::sync::atomic::Ordering;
    progress
        .total
        .store(manifest.total_size().max(1), Ordering::Relaxed);
    let mut problems = Vec::new();
    let mut before = 0;
    for (rel, want) in &manifest.files {
        let path = package.join(rel);
        let got = manifest::digest_with_progress(
            &path,
            |n| progress.done.store(before + n, Ordering::Relaxed),
            || progress.stop.load(Ordering::Relaxed),
        );
        before += want.size;
        match got {
            Ok(None) => return None,
            Ok(Some(d)) if d == *want => {}
            Ok(Some(_)) => problems.push(manifest::Discrepancy::Mismatch(rel.clone())),
            Err(_) => problems.push(manifest::Discrepancy::Missing(rel.clone())),
        }
    }
    Some(problems)
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
