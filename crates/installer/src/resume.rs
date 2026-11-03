//! Finding downloads a previous run of the app left unfinished.
//!
//! Jobs live in memory, but a paused or interrupted download stays in the
//! staging folder with a state file saying what it is for (see
//! [`crate::download::Record`]). On startup the app scans the staging
//! folders of the install folders it knows, and lists each find as a
//! paused job. Resuming one is an ordinary install of the product: the new
//! job plans afresh (a fresh presigned URL, the current version) and the
//! download picks up the partial file when version and archive key still
//! match, with `If-Range` guarding against a changed file on the server.
//!
//! The partial file is only ever appended to, so after a crash it is a
//! prefix of the archive, possibly short of what the last progress update
//! said. Should the OS have lost data past its last flush, the archive
//! checksum, zip CRCs and package manifest catch it and the job downloads
//! again from scratch.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use infinity_ipc::dto::{Channel, Job, JobPhase, Progress};

use crate::download;
use crate::staging::Staging;

/// How long an untouched download is kept for a resume.
pub const KEEP_INTERRUPTED_FOR: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// A download found on disk, not yet listed as a job.
#[derive(Debug)]
pub struct Interrupted {
    pub(crate) job: Job,
    pub(crate) staging: Staging,
    /// Unix seconds the download was last written to.
    pub(crate) touched_at: u64,
}

impl Interrupted {
    pub fn product_id(&self) -> &str {
        &self.job.product_id
    }

    /// The version the partial download is of.
    pub fn version(&self) -> &str {
        &self.job.version
    }

    pub fn channel(&self) -> Channel {
        self.job.channel
    }

    /// The job it would be listed as: paused, with the bytes on disk as
    /// its progress.
    pub fn job(&self) -> &Job {
        &self.job
    }
}

/// Scans the staging folders `install_dirs` may have used. Downloads
/// untouched for longer than `max_age` are deleted, as are state files
/// that are unreadable and as old. A recent download from before state
/// files recorded their job cannot be listed; it is left for an install of
/// its product to resume or replace. Blocking.
pub(crate) fn find(install_dirs: &[PathBuf], now: u64, max_age: Duration) -> Vec<Interrupted> {
    let mut roots: Vec<PathBuf> = install_dirs
        .iter()
        .flat_map(|d| Staging::roots(d))
        .collect();
    roots.sort();
    roots.dedup();

    let mut found: Vec<Interrupted> = Vec::new();
    for root in roots {
        for (stem, state_path) in Staging::download_states(&root) {
            let Some(item) = inspect(&root, &stem, &state_path, now, max_age) else {
                continue;
            };
            // The same product staged in two places: keep the newer.
            match found
                .iter_mut()
                .find(|f| f.job.product_id == item.job.product_id)
            {
                Some(existing) if existing.touched_at >= item.touched_at => {}
                Some(existing) => *existing = item,
                None => found.push(item),
            }
        }
    }
    found.sort_by(|a, b| a.job.product_id.cmp(&b.job.product_id));
    found
}

fn inspect(
    root: &Path,
    stem: &str,
    state_path: &Path,
    now: u64,
    max_age: Duration,
) -> Option<Interrupted> {
    let stale = |touched: u64| now.saturating_sub(touched) > max_age.as_secs();
    let Some(state) = download::read_state_blocking(state_path) else {
        if stale(modified(state_path)) {
            remove(state_path);
        }
        return None;
    };
    let Some(record) = state.record.as_ref() else {
        let partial = root.join(format!("{stem}.download"));
        if stale(
            state
                .updated_at
                .max(modified(state_path))
                .max(modified(&partial)),
        ) {
            remove(&partial);
            remove(state_path);
        }
        return None;
    };
    let staging = Staging::existing(root, &record.product_id);
    if staging.download_state() != state_path {
        tracing::debug!(stem, "download state names another product; ignored");
        return None;
    }
    let have = std::fs::metadata(staging.download()).map_or(0, |m| m.len());
    let touched = state.updated_at.max(modified(&staging.download()));
    if stale(touched) {
        tracing::info!(product = %record.product_id, version = %state.version, "discarding an old interrupted download");
        staging.discard_download();
        return None;
    }
    if have == 0 {
        // Nothing to resume; the next install starts cleanly anyway.
        staging.discard_download();
        return None;
    }
    let total = if state.complete {
        Some(have)
    } else {
        state.total
    };
    Some(Interrupted {
        job: Job {
            product_id: record.product_id.clone(),
            name: record.name.clone(),
            version: state.version.clone(),
            channel: record.channel,
            phase: JobPhase::Paused,
            progress: Some(Progress { done: have, total }),
        },
        staging,
        touched_at: touched,
    })
}

/// Unix seconds of the last modification, 0 when unknown.
fn modified(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

fn remove(path: &Path) {
    if let Err(e) = std::fs::remove_file(path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), error = %e, "could not remove staging file");
    }
}
