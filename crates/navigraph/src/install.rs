//! Downloads a navdata package and installs it as
//! `<work>/NavigationData`, the layout the v3 manager and Navigraph Hub
//! produce: the archive's contents (minus a single shared top folder) plus a
//! `cycle.json`. The previous folder is replaced only once the new one is
//! fully extracted.
//!
//! Dropping the future cancels the install; temporary files are removed
//! then or by the next install.

use std::io::{BufReader, BufWriter};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::error::{IoContext, NavigraphError, Result};
use crate::updater::{CYCLE_FILE, NAVDATA_DIR, NavdataUpdatePlan};

const PROGRESS_EVERY: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub enum NavdataProgress {
    Downloading { done: u64, total: Option<u64> },
    Verifying,
    Extracting,
    Finalizing,
}

/// A client suited to package downloads: no overall timeout, a stall
/// timeout instead.
pub fn download_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("infinity-manager/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .build()?)
}

struct Paths {
    target: PathBuf,
    download: PathBuf,
    staging: PathBuf,
    old: PathBuf,
}

impl Paths {
    fn new(work_dir: &Path) -> Self {
        Self {
            target: work_dir.join(NAVDATA_DIR),
            download: work_dir.join(format!(".{NAVDATA_DIR}.download")),
            staging: work_dir.join(format!(".{NAVDATA_DIR}.staging")),
            old: work_dir.join(format!(".{NAVDATA_DIR}.old")),
        }
    }
}

/// Removes the download and staging leftovers however the install ends.
struct Cleanup<'a>(&'a Paths);

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0.download);
        let _ = std::fs::remove_dir_all(&self.0.staging);
    }
}

/// Installs `plan`, returning the `NavigationData` folder.
pub async fn install_navdata(
    http: &reqwest::Client,
    plan: &NavdataUpdatePlan,
    progress: &mut (dyn FnMut(NavdataProgress) + Send),
) -> Result<PathBuf> {
    let paths = Paths::new(&plan.work_dir);
    tokio::fs::create_dir_all(&plan.work_dir)
        .await
        .at(&plan.work_dir)?;
    recover(&paths).await?;
    let _cleanup = Cleanup(&paths);

    let digest = download(http, plan, &paths.download, progress).await?;

    progress(NavdataProgress::Verifying);
    if let Some(expected) = plan.sha256.as_deref()
        && digest != expected
    {
        return Err(NavigraphError::Checksum {
            expected: expected.to_owned(),
            actual: digest,
        });
    }

    progress(NavdataProgress::Extracting);
    let (archive, staging) = (paths.download.clone(), paths.staging.clone());
    tokio::task::spawn_blocking(move || extract(&archive, &staging))
        .await
        .map_err(|e| NavigraphError::Internal(format!("extract worker: {e}")))??;

    progress(NavdataProgress::Finalizing);
    let cycle_path = paths.staging.join(CYCLE_FILE);
    if tokio::fs::try_exists(&cycle_path).await.unwrap_or(false) {
        tracing::info!("navdata: package shipped cycle.json");
    } else {
        let bytes = serde_json::to_vec_pretty(&plan.cycle)
            .map_err(|e| NavigraphError::Internal(e.to_string()))?;
        tokio::fs::write(&cycle_path, bytes).await.at(&cycle_path)?;
        tracing::info!(cycle = %plan.cycle.cycle, "navdata: synthesized cycle.json (zip had none)");
    }

    swap(&paths).await?;
    tracing::info!(path = %paths.target.display(), cycle = %plan.cycle.cycle, "navdata: installed");
    Ok(paths.target.clone())
}

/// Undoes a swap interrupted between its two renames, then clears leftovers.
async fn recover(paths: &Paths) -> Result<()> {
    if exists(&paths.old).await {
        if exists(&paths.target).await {
            tokio::fs::remove_dir_all(&paths.old).await.at(&paths.old)?;
        } else {
            tracing::warn!("navdata: restoring folder from an interrupted install");
            tokio::fs::rename(&paths.old, &paths.target)
                .await
                .at(&paths.target)?;
        }
    }
    if exists(&paths.staging).await {
        tokio::fs::remove_dir_all(&paths.staging)
            .await
            .at(&paths.staging)?;
    }
    if exists(&paths.download).await {
        tokio::fs::remove_file(&paths.download)
            .await
            .at(&paths.download)?;
    }
    Ok(())
}

async fn exists(path: &Path) -> bool {
    tokio::fs::try_exists(path).await.unwrap_or(false)
}

/// Streams the archive to `dest`, returning its SHA-256 as lowercase hex.
async fn download(
    http: &reqwest::Client,
    plan: &NavdataUpdatePlan,
    dest: &Path,
    progress: &mut (dyn FnMut(NavdataProgress) + Send),
) -> Result<String> {
    let mut resp = http.get(plan.url.expose()).send().await?;
    if !resp.status().is_success() {
        return Err(NavigraphError::Download(resp.status().as_u16()));
    }
    let total = resp.content_length();
    let mut file = tokio::fs::File::create(dest).await.at(dest)?;
    let mut hasher = Sha256::new();
    let mut done = 0u64;
    let mut last = Instant::now();
    progress(NavdataProgress::Downloading { done, total });
    while let Some(chunk) = resp.chunk().await? {
        hasher.update(&chunk);
        file.write_all(&chunk).await.at(dest)?;
        done += chunk.len() as u64;
        if last.elapsed() >= PROGRESS_EVERY {
            last = Instant::now();
            progress(NavdataProgress::Downloading { done, total });
        }
    }
    file.flush().await.at(dest)?;
    drop(file);
    if let Some(expected) = total
        && done != expected
    {
        return Err(NavigraphError::Archive(format!(
            "download ended early: got {done} of {expected} bytes"
        )));
    }
    progress(NavdataProgress::Downloading { done, total });
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Moves `staging` into place, keeping the previous folder until the new
/// one has landed.
async fn swap(paths: &Paths) -> Result<()> {
    let had_previous = exists(&paths.target).await;
    if had_previous {
        tokio::fs::rename(&paths.target, &paths.old)
            .await
            .at(&paths.target)?;
    }
    if let Err(e) = tokio::fs::rename(&paths.staging, &paths.target).await {
        if had_previous {
            let _ = tokio::fs::rename(&paths.old, &paths.target).await;
        }
        let hint = if e.kind() == std::io::ErrorKind::PermissionDenied {
            " If the simulator is running, close it and try again."
        } else {
            ""
        };
        return Err(NavigraphError::Io {
            path: paths.target.clone(),
            source: std::io::Error::new(e.kind(), format!("{e}.{hint}")),
        });
    }
    if had_previous && let Err(e) = tokio::fs::remove_dir_all(&paths.old).await {
        tracing::warn!(error = %e, "navdata: could not remove the previous navdata folder");
    }
    Ok(())
}

/// A zip entry name as a relative path, or `None` if it would escape.
fn safe_relative(name: &str) -> Option<PathBuf> {
    let normalized = name.replace('\\', "/");
    let mut out = PathBuf::new();
    for c in Path::new(&normalized).components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

/// The folder every entry sits under, if there is exactly one.
fn single_top_dir(entries: &[(PathBuf, bool)]) -> Option<PathBuf> {
    let mut top: Option<&std::ffi::OsStr> = None;
    for (path, is_dir) in entries {
        let mut parts = path.components();
        let Some(Component::Normal(first)) = parts.next() else {
            continue;
        };
        // A file at the root means there is no wrapper folder.
        if !is_dir && parts.next().is_none() {
            return None;
        }
        match top {
            None => top = Some(first),
            Some(t) if t == first => {}
            Some(_) => return None,
        }
    }
    top.map(PathBuf::from)
}

fn archive_err(e: zip::result::ZipError) -> NavigraphError {
    NavigraphError::Archive(e.to_string())
}

/// Extracts `archive` into `dest`, stripping a single top folder. Entry
/// CRCs are checked as they are read.
fn extract(archive: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(archive).at(archive)?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file)).map_err(archive_err)?;

    let mut entries = Vec::with_capacity(zip.len());
    for i in 0..zip.len() {
        let entry = zip.by_index_raw(i).map_err(archive_err)?;
        let rel = safe_relative(entry.name()).ok_or_else(|| {
            NavigraphError::Archive(format!("unsafe path in archive: {}", entry.name()))
        })?;
        entries.push((rel, entry.is_dir()));
    }
    let strip = single_top_dir(&entries);

    std::fs::create_dir_all(dest).at(dest)?;
    for (i, (rel, is_dir)) in entries.iter().enumerate() {
        let rel = match &strip {
            Some(top) => rel.strip_prefix(top).unwrap_or(rel),
            None => rel,
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let out = dest.join(rel);
        if *is_dir {
            std::fs::create_dir_all(&out).at(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).at(parent)?;
        }
        let mut entry = zip.by_index(i).map_err(archive_err)?;
        let mut writer = BufWriter::new(std::fs::File::create(&out).at(&out)?);
        std::io::copy(&mut entry, &mut writer).map_err(|e| {
            if e.kind() == std::io::ErrorKind::InvalidData {
                NavigraphError::Archive(format!("{}: {e}", rel.display()))
            } else {
                NavigraphError::Io {
                    path: out.clone(),
                    source: e,
                }
            }
        })?;
        std::io::Write::flush(&mut writer).at(&out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(list: &[(&str, bool)]) -> Vec<(PathBuf, bool)> {
        list.iter()
            .map(|(n, d)| (safe_relative(n).unwrap(), *d))
            .collect()
    }

    #[test]
    fn strips_only_a_shared_wrapper_folder() {
        let wrapped = entries(&[("nav/", true), ("nav/a.db", false), ("nav/sub/b", false)]);
        assert_eq!(single_top_dir(&wrapped), Some(PathBuf::from("nav")));
        let flat = entries(&[("a.db", false), ("sub/b", false)]);
        assert_eq!(single_top_dir(&flat), None);
        let single_file = entries(&[("cycle.json", false)]);
        assert_eq!(single_top_dir(&single_file), None);
        let two = entries(&[("x/a", false), ("y/b", false)]);
        assert_eq!(single_top_dir(&two), None);
    }

    #[test]
    fn rejects_escaping_paths() {
        assert!(safe_relative("../evil").is_none());
        assert!(safe_relative("a/../../evil").is_none());
        assert!(safe_relative("/abs").is_none());
        assert!(safe_relative("a\\..\\..\\evil").is_none());
        assert_eq!(safe_relative("a\\b").unwrap(), PathBuf::from("a").join("b"));
    }

    #[test]
    fn hex_is_lowercase() {
        assert_eq!(hex(&[0xAB, 0x01]), "ab01");
    }
}
