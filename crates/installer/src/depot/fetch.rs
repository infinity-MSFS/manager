//! Downloading chunks: many range requests in parallel, each streamed and
//! cut into chunks as bytes arrive.
//!
//! A request covers a run of needed chunks in one pack. Each chunk is
//! decoded and hashed on a worker thread and written straight into the
//! staged files; a chunk that fails its hash is asked for again (three
//! times at most, then the job fails rather than install bad data). A
//! connection that drops mid-range only re-requests the chunks it had not
//! delivered. Transient failures back off and retry for as long as the
//! network keeps failing within [`PATIENCE`] of the last progress, so a
//! Wi-Fi blip or a router restart pauses the download rather than failing
//! it. Presigned URLs are refreshed before they expire and whenever the
//! storage refuses one.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use infinity_depot::Manifest;
use infinity_depot::ranges::Range;
use reqwest::{StatusCode, header};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use url::Url;

use super::PackUrls;
use super::stage::{Sink, Target};
use crate::error::{Error, Result};
use crate::job::{self, StopRx};

/// Concurrent range requests.
pub(crate) const CONNECTIONS: usize = 8;
/// Chunks decoding or being written at once, bounding memory.
const DECODING: usize = 16;
/// How long the network may keep failing, counted from the last chunk
/// that arrived, before the job gives up.
pub(crate) const PATIENCE: Duration = Duration::from_secs(5 * 60);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// A chunk that fails its hash this often is damaged at the source.
const CORRUPT_AFTER: u32 = 3;
/// Storage refusing a fresh URL this often in a row is not an expiry.
const AUTH_RETRIES: u32 = 3;
/// Presigned URLs live 30 minutes; refresh well before.
const URL_FRESH_FOR: Duration = Duration::from_secs(20 * 60);
const URL_BATCH: usize = 200;
const CHECKPOINT_EVERY: Duration = Duration::from_secs(2);

/// Download URLs of a depot's packs, cached and refreshed.
#[derive(Debug)]
pub(crate) struct UrlBook {
    source: Arc<dyn PackUrls>,
    keys: Vec<String>,
    cache: tokio::sync::Mutex<HashMap<u32, (Url, Instant)>>,
}

impl UrlBook {
    pub(crate) fn new(source: Arc<dyn PackUrls>, manifest: &Manifest) -> Self {
        let keys = manifest
            .packs
            .iter()
            .map(|p| infinity_depot::keys::pack_key(&manifest.product_id, &p.id))
            .collect();
        Self {
            source,
            keys,
            cache: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Resolves the packs that are not freshly cached, in batches.
    pub(crate) async fn prefetch(&self, packs: &[u32]) -> Result<()> {
        let stale: Vec<u32> = {
            let cache = self.cache.lock().await;
            packs
                .iter()
                .copied()
                .collect::<HashSet<_>>()
                .into_iter()
                .filter(|p| {
                    cache
                        .get(p)
                        .is_none_or(|(_, at)| at.elapsed() > URL_FRESH_FOR)
                })
                .collect()
        };
        for batch in stale.chunks(URL_BATCH) {
            self.resolve(batch).await?;
        }
        Ok(())
    }

    async fn get(&self, pack: u32) -> Result<Url> {
        if let Some((url, at)) = self.cache.lock().await.get(&pack)
            && at.elapsed() <= URL_FRESH_FOR
        {
            return Ok(url.clone());
        }
        self.resolve(&[pack]).await?;
        self.cache
            .lock()
            .await
            .get(&pack)
            .map(|(u, _)| u.clone())
            .ok_or_else(|| Error::Internal("no URL for a pack".into()))
    }

    async fn refresh(&self, pack: u32) -> Result<()> {
        self.resolve(&[pack]).await
    }

    async fn resolve(&self, packs: &[u32]) -> Result<()> {
        let keys: Vec<String> = packs
            .iter()
            .map(|&p| {
                self.keys
                    .get(p as usize)
                    .cloned()
                    .ok_or_else(|| Error::Internal("pack out of range".into()))
            })
            .collect::<Result<_>>()?;
        let urls = self.source.resolve(&keys).await.map_err(Error::Remote)?;
        if urls.len() != keys.len() {
            return Err(Error::Internal(format!(
                "asked for {} URLs, got {}",
                keys.len(),
                urls.len()
            )));
        }
        let now = Instant::now();
        let mut cache = self.cache.lock().await;
        for (&p, url) in packs.iter().zip(urls) {
            cache.insert(p, (url, now));
        }
        Ok(())
    }
}

/// One download pass over a set of ranges.
#[derive(Debug)]
pub(crate) struct Fetch {
    pub(crate) http: reqwest::Client,
    pub(crate) manifest: Arc<Manifest>,
    pub(crate) urls: Arc<UrlBook>,
    pub(crate) sink: Arc<Sink>,
    /// Chunk index -> where it goes.
    pub(crate) targets: HashMap<u32, Vec<Target>>,
    /// Stored bytes delivered, for progress.
    pub(crate) received: AtomicU64,
    decoding: Arc<Semaphore>,
}

impl Fetch {
    pub(crate) fn new(
        http: reqwest::Client,
        manifest: Arc<Manifest>,
        urls: Arc<UrlBook>,
        sink: Arc<Sink>,
        targets: HashMap<u32, Vec<Target>>,
    ) -> Self {
        Self {
            http,
            manifest,
            urls,
            sink,
            targets,
            received: AtomicU64::new(0),
            decoding: Arc::new(Semaphore::new(DECODING)),
        }
    }

    /// Downloads every range, reporting delivered bytes through `report`.
    pub(crate) async fn run(
        self: Arc<Self>,
        ranges: Vec<Range>,
        stop: &mut StopRx,
        report: &mut (dyn FnMut(u64) + Send),
    ) -> Result<()> {
        let packs: Vec<u32> = ranges.iter().map(|r| r.pack).collect();
        tokio::select! {
            r = self.urls.prefetch(&packs) => r?,
            s = job::stopped(stop) => return Err(Error::Stopped(s)),
        }
        let mut queue = ranges.into_iter();
        let mut tasks = JoinSet::new();
        let mut checkpoint = tokio::time::interval(CHECKPOINT_EVERY);
        let mut tick = tokio::time::interval(Duration::from_millis(150));
        for range in queue.by_ref().take(CONNECTIONS) {
            tasks.spawn(self.clone().range(range));
        }
        let outcome = loop {
            tokio::select! {
                joined = tasks.join_next() => match joined {
                    None => break Ok(()),
                    Some(Ok(Ok(()))) => {
                        if let Some(range) = queue.next() {
                            tasks.spawn(self.clone().range(range));
                        }
                    }
                    Some(Ok(Err(e))) => break Err(e),
                    Some(Err(e)) => break Err(Error::Internal(format!("download task: {e}"))),
                },
                _ = checkpoint.tick() => {
                    let sink = self.sink.clone();
                    tokio::task::spawn_blocking(move || sink.checkpoint())
                        .await
                        .map_err(|e| Error::Internal(e.to_string()))??;
                }
                _ = tick.tick() => report(self.received.load(Ordering::Relaxed)),
                s = job::stopped(stop) => break Err(Error::Stopped(s)),
            }
        };
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        if outcome.is_err() {
            self.sink.close();
        }
        // Keep whatever arrived, whatever the outcome.
        let sink = self.sink.clone();
        let saved = tokio::task::spawn_blocking(move || sink.checkpoint())
            .await
            .map_err(|e| Error::Internal(e.to_string()))
            .and_then(|r| r);
        report(self.received.load(Ordering::Relaxed));
        outcome?;
        saved
    }

    /// Fetches one range until every chunk in it is delivered.
    async fn range(self: Arc<Self>, range: Range) -> Result<()> {
        let mut pending = range.chunks.clone();
        let mut failures: HashMap<u32, u32> = HashMap::new();
        let mut last_progress = Instant::now();
        let mut backoff = Duration::from_secs(1);
        let mut auth_failures = 0;
        while !pending.is_empty() {
            let before = pending.len();
            let attempt = self.request(range.pack, &pending, &mut failures).await;
            let delivered = attempt.delivered;
            pending.retain(|c| !delivered.contains(c));
            if pending.len() < before {
                last_progress = Instant::now();
                backoff = Duration::from_secs(1);
                auth_failures = 0;
            }
            let error = match attempt.outcome {
                Ok(()) if pending.is_empty() => return Ok(()),
                // Chunks that failed their hash; ask again at once.
                Ok(()) if pending.iter().all(|c| failures.contains_key(c)) => continue,
                Ok(()) => Error::Truncated {
                    got: 0,
                    expected: 0,
                },
                Err(e) => e,
            };
            match &error {
                Error::Http(s)
                    if matches!(*s, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) =>
                {
                    auth_failures += 1;
                    if auth_failures > AUTH_RETRIES {
                        return Err(error);
                    }
                    tracing::debug!(pack = range.pack, "storage refused the URL; refreshing it");
                    self.urls.refresh(range.pack).await?;
                    continue;
                }
                e if e.is_transient() => {}
                _ => return Err(error),
            }
            if last_progress.elapsed() > PATIENCE {
                tracing::warn!(error = %error, "the network kept failing; giving up");
                return Err(error);
            }
            tracing::debug!(error = %error, wait = ?backoff, "chunk download failed; retrying");
            tokio::time::sleep(jitter(backoff)).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
        Ok(())
    }

    /// One request for the span from the first to the last pending chunk.
    async fn request(
        &self,
        pack: u32,
        pending: &[u32],
        failures: &mut HashMap<u32, u32>,
    ) -> Attempt {
        let mut delivered = HashSet::new();
        let outcome = self.stream(pack, pending, failures, &mut delivered).await;
        Attempt { delivered, outcome }
    }

    async fn stream(
        &self,
        pack: u32,
        pending: &[u32],
        failures: &mut HashMap<u32, u32>,
        delivered: &mut HashSet<u32>,
    ) -> Result<()> {
        let m = &self.manifest;
        let Some(&first) = pending.first() else {
            return Ok(());
        };
        let start = m.chunk(first).offset;
        let end = pending
            .iter()
            .map(|&c| m.chunk(c).stored_end())
            .max()
            .unwrap_or(start);
        let url = self.urls.get(pack).await?;
        let response = self
            .http
            .get(url)
            .header(header::RANGE, format!("bytes={start}-{}", end - 1))
            .send()
            .await?;
        let base = match response.status() {
            StatusCode::PARTIAL_CONTENT => {
                let at = response
                    .headers()
                    .get(header::CONTENT_RANGE)
                    .and_then(|v| v.to_str().ok())
                    .and_then(content_range_start);
                if at != Some(start) {
                    return Err(Error::Truncated {
                        got: 0,
                        expected: end - start,
                    });
                }
                start
            }
            // A server that ignores ranges sends the whole pack.
            StatusCode::OK => 0,
            other => return Err(Error::Http(other)),
        };
        let mut response = response;
        let mut buf: Vec<u8> = Vec::new();
        let mut buf_start = base;
        let mut next = 0usize;
        let mut inflight: Vec<tokio::task::JoinHandle<(u32, Result<()>)>> = Vec::new();
        let read = loop {
            let bytes = match response.chunk().await {
                Ok(Some(b)) => b,
                Ok(None) => break Ok(()),
                Err(e) => break Err(Error::from(e)),
            };
            buf.extend_from_slice(&bytes);
            while let Some(&c) = pending.get(next) {
                let chunk = m.chunk(c);
                if buf_start < chunk.offset {
                    let skip = (chunk.offset - buf_start).min(buf.len() as u64) as usize;
                    buf.drain(..skip);
                    buf_start += skip as u64;
                    if buf_start < chunk.offset {
                        break;
                    }
                }
                if buf_start > chunk.offset {
                    // Overlaps the previous chunk: left for the next request.
                    next += 1;
                    continue;
                }
                let need = chunk.stored as usize;
                if buf.len() < need {
                    break;
                }
                let stored = buf[..need].to_vec();
                next += 1;
                let permit = match self.decoding.clone().acquire_owned().await {
                    Ok(p) => p,
                    Err(e) => return Err(Error::Internal(e.to_string())),
                };
                inflight.push(self.decode(c, stored, permit));
                if inflight.len() >= 4 {
                    let handle = inflight.remove(0);
                    self.settle(handle, failures, delivered).await?;
                }
            }
        };
        for handle in inflight {
            self.settle(handle, failures, delivered).await?;
        }
        read
    }

    fn decode(
        &self,
        c: u32,
        stored: Vec<u8>,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> tokio::task::JoinHandle<(u32, Result<()>)> {
        let chunk = *self.manifest.chunk(c);
        let targets = self.targets.get(&c).cloned().unwrap_or_default();
        let sink = self.sink.clone();
        tokio::task::spawn_blocking(move || {
            let result = infinity_depot::codec::decode(&chunk, &stored)
                .map_err(Error::from)
                .and_then(|raw| sink.place(&targets, &raw));
            drop(permit);
            (c, result)
        })
    }

    async fn settle(
        &self,
        handle: tokio::task::JoinHandle<(u32, Result<()>)>,
        failures: &mut HashMap<u32, u32>,
        delivered: &mut HashSet<u32>,
    ) -> Result<()> {
        let (c, result) = handle
            .await
            .map_err(|e| Error::Internal(format!("decode: {e}")))?;
        match result {
            Ok(()) => {
                delivered.insert(c);
                failures.remove(&c);
                self.received
                    .fetch_add(u64::from(self.manifest.chunk(c).stored), Ordering::Relaxed);
                Ok(())
            }
            Err(Error::Depot(e @ infinity_depot::Error::Chunk { .. })) => {
                let n = failures.entry(c).or_default();
                *n += 1;
                tracing::warn!(error = %e, attempt = *n, "a downloaded chunk was damaged");
                if *n >= CORRUPT_AFTER {
                    return Err(Error::Corrupt(e.to_string()));
                }
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}

struct Attempt {
    delivered: HashSet<u32>,
    outcome: Result<()>,
}

/// The first byte of a `Content-Range: bytes a-b/n` header.
fn content_range_start(v: &str) -> Option<u64> {
    v.strip_prefix("bytes ")?
        .split('-')
        .next()?
        .trim()
        .parse()
        .ok()
}

/// ±25 %, so retrying connections do not move in lockstep.
fn jitter(d: Duration) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |t| t.subsec_nanos());
    let factor = 0.75 + f64::from(nanos % 1000) / 2000.0;
    d.mul_f64(factor)
}

/// Downloads a small object whole (the manifest), with the same patience.
pub(crate) async fn fetch_small(
    http: &reqwest::Client,
    url: &Url,
    max: usize,
    stop: &mut StopRx,
) -> Result<Vec<u8>> {
    let started = Instant::now();
    let mut backoff = Duration::from_secs(1);
    loop {
        let attempt = async {
            let response = http.get(url.clone()).send().await?;
            if !response.status().is_success() {
                return Err(Error::Http(response.status()));
            }
            if response.content_length().is_some_and(|n| n > max as u64) {
                return Err(Error::Archive(
                    "the package manifest is implausibly large".into(),
                ));
            }
            let mut response = response;
            let mut out = Vec::new();
            while let Some(b) = response.chunk().await? {
                out.extend_from_slice(&b);
                if out.len() > max {
                    return Err(Error::Archive(
                        "the package manifest is implausibly large".into(),
                    ));
                }
            }
            Ok(out)
        };
        let result = tokio::select! {
            r = attempt => r,
            s = job::stopped(stop) => return Err(Error::Stopped(s)),
        };
        match result {
            Err(e) if e.is_transient() && started.elapsed() < PATIENCE => {
                tracing::debug!(error = %e, "manifest download failed; retrying");
                tokio::select! {
                    () = tokio::time::sleep(jitter(backoff)) => {}
                    s = job::stopped(stop) => return Err(Error::Stopped(s)),
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
            other => return other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_range() {
        assert_eq!(content_range_start("bytes 100-199/1000"), Some(100));
        assert_eq!(content_range_start("bytes */1000"), None);
    }

    #[test]
    fn jitter_stays_in_bounds() {
        let d = jitter(Duration::from_secs(4));
        assert!(d >= Duration::from_secs(3) && d <= Duration::from_secs(5));
    }
}
