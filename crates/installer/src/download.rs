//! Resumable archive downloads.
//!
//! A partial download is only resumed when it is provably the same file:
//! the state file pins the product version and the server's validator
//! (`ETag`, else `Last-Modified`), and the resume request carries
//! `If-Range`, so a server whose file changed answers with the whole new
//! file instead of the tail of it.
//!
//! The state file also records which job the download is for
//! ([`Record`]), so a download interrupted by quitting the app can be
//! found and listed as paused on the next start (see [`crate::resume`]).

use std::path::{Path, PathBuf};
use std::time::Duration;

use infinity_ipc::catalog::ArchiveFormat;
use infinity_ipc::dto::Channel;
use reqwest::{StatusCode, header};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use url::Url;

use crate::error::{Error, IoContext, Result};
use crate::job::StopRx;
use crate::space;
use crate::staging::Staging;

const ATTEMPTS: u32 = 4;
const BUFFER: usize = 1 << 20;

pub(crate) struct Source<'a> {
    pub(crate) url: &'a Url,
    pub(crate) version: &'a str,
    /// Archive size from the manifest, used when the server sends none.
    pub(crate) size: Option<u64>,
    pub(crate) record: Record,
}

/// What a download is for: enough to list it as a job after a restart
/// and to tell whether a later install may resume it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Record {
    pub(crate) product_id: String,
    pub(crate) name: String,
    pub(crate) channel: Channel,
    pub(crate) package_dir_name: String,
    pub(crate) install_dir: PathBuf,
    pub(crate) format: ArchiveFormat,
    /// The archive's storage key. Download URLs are presigned and expire,
    /// so the key, not the URL, identifies the file.
    pub(crate) archive_key: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct State {
    pub(crate) version: String,
    etag: Option<String>,
    last_modified: Option<String>,
    pub(crate) total: Option<u64>,
    pub(crate) complete: bool,
    /// Absent in state files written before downloads survived restarts.
    #[serde(default)]
    pub(crate) record: Option<Record>,
    /// Unix seconds of the last write.
    #[serde(default)]
    pub(crate) updated_at: u64,
}

impl State {
    fn validator(&self) -> Option<&str> {
        self.etag.as_deref().or(self.last_modified.as_deref())
    }
}

/// Downloads into the staging folder, resuming a matching partial and
/// retrying transient failures from where they left off.
pub(crate) async fn fetch(
    http: &reqwest::Client,
    staging: &Staging,
    source: &Source<'_>,
    stop: &mut StopRx,
    progress: &mut (dyn FnMut(u64, Option<u64>) + Send),
) -> Result<()> {
    let mut attempt = 1;
    loop {
        match fetch_once(http, staging, source, stop, progress).await {
            Ok(()) => return Ok(()),
            Err(e) if e.is_transient() && attempt < ATTEMPTS => {
                let wait = Duration::from_secs(1 << (attempt - 1));
                tracing::warn!(error = %e, attempt, "download interrupted, retrying");
                tokio::select! {
                    () = tokio::time::sleep(wait) => {}
                    stop = crate::job::stopped(stop) => return Err(Error::Stopped(stop)),
                }
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

async fn fetch_once(
    http: &reqwest::Client,
    staging: &Staging,
    source: &Source<'_>,
    stop: &mut StopRx,
    progress: &mut (dyn FnMut(u64, Option<u64>) + Send),
) -> Result<()> {
    let path = staging.download();
    let state_path = staging.download_state();
    let previous = read_state(&state_path).await.filter(|s| {
        s.version == source.version
            && s.record
                .as_ref()
                .is_none_or(|r| r.archive_key == source.record.archive_key)
    });
    let mut have = match &previous {
        Some(_) => tokio::fs::metadata(&path).await.map_or(0, |m| m.len()),
        None => 0,
    };
    if let Some(state) = &previous
        && state.complete
        && state.total == Some(have)
    {
        progress(have, state.total);
        return Ok(());
    }
    let validator = previous.as_ref().and_then(State::validator);
    let resume = have > 0 && validator.is_some();

    let mut request = http
        .get(source.url.clone())
        .header(header::ACCEPT_ENCODING, "identity");
    if let (true, Some(validator)) = (resume, validator) {
        request = request
            .header(header::RANGE, format!("bytes={have}-"))
            .header(header::IF_RANGE, validator);
    }
    let mut response = request.send().await?;
    let status = response.status();

    let total = match status {
        StatusCode::PARTIAL_CONTENT if resume => {
            let range = content_range(response.headers());
            match range {
                Some((start, total)) if start == have => total,
                _ => {
                    // Not the range asked for: start over cleanly.
                    remove_partial(staging).await;
                    return Err(Error::Truncated {
                        got: 0,
                        expected: have,
                    });
                }
            }
        }
        StatusCode::RANGE_NOT_SATISFIABLE if resume => {
            remove_partial(staging).await;
            return Err(Error::Truncated {
                got: 0,
                expected: have,
            });
        }
        s if s.is_success() && s != StatusCode::PARTIAL_CONTENT => {
            have = 0;
            response.content_length()
        }
        s => return Err(Error::Http(s)),
    }
    .or(source.size);

    if let Some(total) = total {
        space::ensure(
            staging.root(),
            total.saturating_sub(have),
            "download this package",
        )?;
    }

    let headers = response.headers();
    let text = |name| {
        headers
            .get(name)
            .and_then(|v: &header::HeaderValue| v.to_str().ok())
            .map(str::to_owned)
    };
    let mut state = State {
        version: source.version.to_owned(),
        etag: text(header::ETAG).filter(|e| !e.starts_with("W/")),
        last_modified: text(header::LAST_MODIFIED),
        total,
        complete: false,
        record: Some(source.record.clone()),
        updated_at: crate::unix_now(),
    };
    write_state(&state_path, &state).await?;

    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(have > 0)
        .truncate(have == 0)
        .open(&path)
        .await
        .at(&path)?;
    let mut out = tokio::io::BufWriter::with_capacity(BUFFER, file);
    progress(have, total);

    loop {
        let chunk = tokio::select! {
            chunk = response.chunk() => chunk.map_err(Error::from),
            stop = crate::job::stopped(stop) => Err(Error::Stopped(stop)),
        };
        // Whatever arrived before a cut or a stop is kept for the resume.
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(e) => {
                out.flush().await.at(&path)?;
                return Err(e);
            }
        };
        let Some(chunk) = chunk else { break };
        out.write_all(&chunk).await.at(&path)?;
        have += chunk.len() as u64;
        progress(have, total);
    }
    out.flush().await.at(&path)?;
    out.into_inner().sync_all().await.at(&path)?;

    if let Some(expected) = total
        && have != expected
    {
        if have > expected {
            remove_partial(staging).await;
        }
        return Err(Error::Truncated {
            got: have,
            expected,
        });
    }
    state.total = Some(have);
    state.complete = true;
    state.updated_at = crate::unix_now();
    write_state(&state_path, &state).await
}

/// `Content-Range: bytes <start>-<end>/<total|*>`.
fn content_range(headers: &header::HeaderMap) -> Option<(u64, Option<u64>)> {
    let value = headers.get(header::CONTENT_RANGE)?.to_str().ok()?;
    let (range, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (start, _) = range.split_once('-')?;
    Some((start.parse().ok()?, total.parse().ok()))
}

async fn read_state(path: &Path) -> Option<State> {
    let bytes = tokio::fs::read(path).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// [`read_state`], for the blocking startup scan.
pub(crate) fn read_state_blocking(path: &Path) -> Option<State> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

async fn write_state(path: &Path, state: &State) -> Result<()> {
    let json = serde_json::to_vec(state).map_err(|e| Error::Internal(e.to_string()))?;
    tokio::fs::write(path, json).await.at(path)
}

async fn remove_partial(staging: &Staging) {
    let _ = tokio::fs::remove_file(staging.download()).await;
    let _ = tokio::fs::remove_file(staging.download_state()).await;
}

/// Marks a downloaded archive as unusable, so the next attempt fetches it
/// again.
pub(crate) async fn forget(staging: &Staging) {
    remove_partial(staging).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_range() {
        let mut h = header::HeaderMap::new();
        h.insert(header::CONTENT_RANGE, "bytes 100-199/200".parse().unwrap());
        assert_eq!(content_range(&h), Some((100, Some(200))));
        h.insert(header::CONTENT_RANGE, "bytes 5-9/*".parse().unwrap());
        assert_eq!(content_range(&h), Some((5, None)));
        h.insert(header::CONTENT_RANGE, "items 0-1/2".parse().unwrap());
        assert_eq!(content_range(&h), None);
    }
}
