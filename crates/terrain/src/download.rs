//! Downloading the terrain database.
//!
//! The database is one large raw file (`world.itr`), not a package: the
//! community installer only installs zip packages into an install folder and
//! records them as products, so this is a small resumable fetch of its own.
//! It downloads to `world.itr.partial` beside the database, resumes that
//! file with a `Range` request, verifies the SHA-256 when one is given, and
//! leaves the file for [`TerrainEngine::replace_database`] to swap in.
//!
//! [`TerrainEngine::replace_database`]: crate::TerrainEngine::replace_database

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use reqwest::header::{CONTENT_RANGE, RANGE};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use url::Url;

use crate::error::{IoContext, Result, TerrainError};

const PROGRESS_EVERY: Duration = Duration::from_millis(200);
const HASH_BUFFER: usize = 1 << 20;

/// Where to fetch the database from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub url: Url,
    /// Lowercase hex SHA-256 of the whole file.
    pub sha256: Option<String>,
}

impl Source {
    pub fn parse(url: &str, sha256: Option<String>) -> Result<Self> {
        let url = Url::parse(url).map_err(|e| TerrainError::Url(format!("{url}: {e}")))?;
        if !matches!(url.scheme(), "https" | "http") {
            return Err(TerrainError::Url(url.to_string()));
        }
        let sha256 = sha256
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty());
        Ok(Self { url, sha256 })
    }
}

/// What the download is doing, for a progress report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Downloading { done: u64, total: Option<u64> },
    Verifying,
}

/// The partial file a download of `dest` writes.
pub fn partial_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".partial");
    dest.with_file_name(name)
}

/// Deletes a partial download of `dest`, if there is one.
pub async fn discard(dest: &Path) {
    let _ = tokio::fs::remove_file(partial_path(dest)).await;
}

/// Downloads `source` to [`partial_path`]`(dest)`, resuming what is already
/// there, and verifies it. Returns the verified partial file. Dropping the
/// future pauses: the next call picks up where this one stopped.
pub async fn fetch(
    http: &reqwest::Client,
    source: &Source,
    dest: &Path,
    progress: &mut (dyn FnMut(Step) + Send),
) -> Result<PathBuf> {
    let partial = partial_path(dest);
    if let Some(dir) = partial.parent() {
        tokio::fs::create_dir_all(dir).await.at(dir)?;
    }
    // A server that ignores or rejects the range gets one fresh attempt.
    let mut fresh = false;
    loop {
        match fetch_once(http, source, &partial, fresh, progress).await {
            Err(TerrainError::Http(StatusCode::RANGE_NOT_SATISFIABLE)) if !fresh => fresh = true,
            Err(e @ TerrainError::Checksum { .. }) => {
                let _ = tokio::fs::remove_file(&partial).await;
                return Err(e);
            }
            other => return other.map(|()| partial),
        }
    }
}

async fn fetch_once(
    http: &reqwest::Client,
    source: &Source,
    partial: &Path,
    fresh: bool,
    progress: &mut (dyn FnMut(Step) + Send),
) -> Result<()> {
    let have = if fresh {
        0
    } else {
        tokio::fs::metadata(partial)
            .await
            .map(|m| m.len())
            .unwrap_or(0)
    };
    let mut request = http.get(source.url.clone());
    if have > 0 {
        request = request.header(RANGE, format!("bytes={have}-"));
    }
    let mut response = request.send().await?;
    let status = response.status();
    let resumed = status == StatusCode::PARTIAL_CONTENT
        && content_range_start(response.headers().get(CONTENT_RANGE)) == Some(have);
    if !status.is_success() {
        return Err(TerrainError::Http(status));
    }
    if status == StatusCode::PARTIAL_CONTENT && !resumed {
        // A range other than the one asked for: start over.
        return Err(TerrainError::Http(StatusCode::RANGE_NOT_SATISFIABLE));
    }
    let start = if resumed { have } else { 0 };
    let total = if resumed {
        content_range_total(response.headers().get(CONTENT_RANGE))
            .or_else(|| response.content_length().map(|n| n + start))
    } else {
        response.content_length()
    };
    if have > 0 && !resumed {
        tracing::info!(
            have,
            "terrain: server did not resume; downloading from the start"
        );
    }

    let mut file = if resumed {
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(partial)
            .await
            .at(partial)?
    } else {
        tokio::fs::File::create(partial).await.at(partial)?
    };
    let mut done = start;
    let mut last = Instant::now();
    progress(Step::Downloading { done, total });
    while let Some(chunk) = response.chunk().await? {
        file.write_all(&chunk).await.at(partial)?;
        done += chunk.len() as u64;
        if last.elapsed() >= PROGRESS_EVERY {
            last = Instant::now();
            progress(Step::Downloading { done, total });
        }
    }
    file.flush().await.at(partial)?;
    file.sync_all().await.at(partial)?;
    drop(file);
    progress(Step::Downloading { done, total });
    if let Some(expected) = total
        && done != expected
    {
        return Err(TerrainError::Truncated {
            received: done,
            expected,
        });
    }

    if let Some(expected) = &source.sha256 {
        progress(Step::Verifying);
        let path = partial.to_path_buf();
        let actual = tokio::task::spawn_blocking(move || sha256_file(&path))
            .await
            .map_err(|e| TerrainError::Engine(format!("checksum task: {e}")))??;
        if &actual != expected {
            return Err(TerrainError::Checksum {
                expected: expected.clone(),
                actual,
            });
        }
    }
    Ok(())
}

/// Lowercase hex SHA-256 of a file. Blocking.
pub fn sha256_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).at(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; HASH_BUFFER];
    loop {
        let n = file.read(&mut buf).at(path)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// `bytes <start>-<end>/<total>` → `start`.
fn content_range_start(value: Option<&reqwest::header::HeaderValue>) -> Option<u64> {
    let text = value?.to_str().ok()?.strip_prefix("bytes ")?;
    text.split('-').next()?.trim().parse().ok()
}

/// `bytes <start>-<end>/<total>` → `total` (unless `*`).
fn content_range_total(value: Option<&reqwest::header::HeaderValue>) -> Option<u64> {
    value?
        .to_str()
        .ok()?
        .rsplit('/')
        .next()?
        .trim()
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    /// A one-file HTTP server that honours `Range` when `ranges` is set.
    async fn serve(body: Vec<u8>, ranges: bool) -> Url {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let body = body.clone();
                tokio::spawn(async move {
                    let mut req = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = sock.read(&mut buf).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        req.extend_from_slice(&buf[..n]);
                    }
                    let text = String::from_utf8_lossy(&req).to_ascii_lowercase();
                    let from = text
                        .lines()
                        .find_map(|l| l.strip_prefix("range: bytes="))
                        .and_then(|r| r.trim_end_matches('-').trim().parse::<usize>().ok())
                        .filter(|_| ranges);
                    let head = match from {
                        Some(f) if f >= body.len() => {
                            "HTTP/1.1 416 Range Not Satisfiable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_string()
                        }
                        Some(f) => format!(
                            "HTTP/1.1 206 Partial Content\r\ncontent-length: {}\r\ncontent-range: bytes {f}-{}/{}\r\nconnection: close\r\n\r\n",
                            body.len() - f,
                            body.len() - 1,
                            body.len()
                        ),
                        None => format!(
                            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                            body.len()
                        ),
                    };
                    let payload = match from {
                        Some(f) if f >= body.len() => &[][..],
                        Some(f) => &body[f..],
                        None => &body[..],
                    };
                    use tokio::io::AsyncWriteExt;
                    let _ = sock.write_all(head.as_bytes()).await;
                    let _ = sock.write_all(payload).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        Url::parse(&format!("http://{addr}/world.itr")).unwrap()
    }

    fn body() -> Vec<u8> {
        (0..200_000u32).map(|i| (i % 251) as u8).collect()
    }

    fn sha(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    #[tokio::test]
    async fn downloads_and_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("terrain").join("world.itr");
        let body = body();
        let source = Source {
            url: serve(body.clone(), true).await,
            sha256: Some(sha(&body)),
        };
        let mut steps = Vec::new();
        let got = fetch(&reqwest::Client::new(), &source, &dest, &mut |s| {
            steps.push(s)
        })
        .await
        .unwrap();
        assert_eq!(got, partial_path(&dest));
        assert_eq!(std::fs::read(&got).unwrap(), body);
        assert!(steps.contains(&Step::Verifying));
        assert!(steps.contains(&Step::Downloading {
            done: body.len() as u64,
            total: Some(body.len() as u64)
        }));
    }

    #[tokio::test]
    async fn resumes_a_partial_download() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("world.itr");
        let body = body();
        std::fs::write(partial_path(&dest), &body[..70_000]).unwrap();
        let source = Source {
            url: serve(body.clone(), true).await,
            sha256: Some(sha(&body)),
        };
        let mut first = None;
        fetch(&reqwest::Client::new(), &source, &dest, &mut |s| {
            first.get_or_insert(s);
        })
        .await
        .unwrap();
        assert_eq!(
            first,
            Some(Step::Downloading {
                done: 70_000,
                total: Some(body.len() as u64)
            })
        );
        assert_eq!(std::fs::read(partial_path(&dest)).unwrap(), body);
    }

    #[tokio::test]
    async fn restarts_when_the_server_ignores_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("world.itr");
        let body = body();
        std::fs::write(partial_path(&dest), b"stale bytes").unwrap();
        let source = Source {
            url: serve(body.clone(), false).await,
            sha256: None,
        };
        fetch(&reqwest::Client::new(), &source, &dest, &mut |_| {})
            .await
            .unwrap();
        assert_eq!(std::fs::read(partial_path(&dest)).unwrap(), body);
    }

    #[tokio::test]
    async fn an_oversized_partial_starts_over() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("world.itr");
        let body = body();
        let mut long = body.clone();
        long.extend_from_slice(b"junk");
        std::fs::write(partial_path(&dest), &long).unwrap();
        let source = Source {
            url: serve(body.clone(), true).await,
            sha256: Some(sha(&body)),
        };
        fetch(&reqwest::Client::new(), &source, &dest, &mut |_| {})
            .await
            .unwrap();
        assert_eq!(std::fs::read(partial_path(&dest)).unwrap(), body);
    }

    #[tokio::test]
    async fn a_checksum_mismatch_discards_the_download() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("world.itr");
        let source = Source {
            url: serve(body(), true).await,
            sha256: Some("00".repeat(32)),
        };
        let err = fetch(&reqwest::Client::new(), &source, &dest, &mut |_| {})
            .await
            .unwrap_err();
        assert!(matches!(err, TerrainError::Checksum { .. }));
        assert!(!partial_path(&dest).exists());
    }

    #[test]
    fn parses_content_range() {
        let v = HeaderValue::from_static("bytes 100-199/200");
        assert_eq!(content_range_start(Some(&v)), Some(100));
        assert_eq!(content_range_total(Some(&v)), Some(200));
        let star = HeaderValue::from_static("bytes 100-199/*");
        assert_eq!(content_range_total(Some(&star)), None);
    }

    #[test]
    fn source_rejects_other_schemes() {
        assert!(Source::parse("file:///c:/world.itr", None).is_err());
        let s = Source::parse("https://example.com/w.itr", Some(" ABCD ".into())).unwrap();
        assert_eq!(s.sha256.as_deref(), Some("abcd"));
    }
}
