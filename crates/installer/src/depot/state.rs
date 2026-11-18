//! What a depot install leaves in the package folder: the manifest it was
//! installed from and the size and modification time each file had when
//! it was last verified. An update trusts a file whose chunks did not
//! change and whose size and time still match, without reading it; any
//! other file is hashed.

use std::collections::HashMap;
use std::path::Path;
use std::time::UNIX_EPOCH;

use infinity_depot::{Hash, Manifest, STATE_FILE};
use serde::{Deserialize, Serialize};

const SCHEMA_VERSION: u32 = 1;
const MAX_STATE_BYTES: u64 = 1 << 30;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstalledState {
    pub(crate) schema_version: u32,
    pub(crate) version: String,
    pub(crate) manifest_hash: Hash,
    pub(crate) manifest: Manifest,
    pub(crate) stats: HashMap<String, Stat>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Stat {
    pub(crate) size: u64,
    /// Nanoseconds since the Unix epoch.
    pub(crate) mtime: u64,
}

impl InstalledState {
    pub(crate) fn new(manifest: Manifest, manifest_hash: Hash, package: &Path) -> Self {
        let stats = manifest
            .files
            .iter()
            .filter_map(|f| Some((f.path.clone(), stat(&package.join(&f.path))?)))
            .collect();
        Self {
            schema_version: SCHEMA_VERSION,
            version: manifest.version.clone(),
            manifest_hash,
            manifest,
            stats,
        }
    }

    /// The state of the package at `package`, if it has a readable one.
    pub(crate) fn read(package: &Path) -> Option<Self> {
        let path = package.join(STATE_FILE);
        let file = std::fs::File::open(&path).ok()?;
        let parsed = zstd::stream::read::Decoder::new(file)
            .map_err(|e| e.to_string())
            .and_then(|d| {
                serde_json::from_reader::<_, Self>(std::io::BufReader::new(std::io::Read::take(
                    d,
                    MAX_STATE_BYTES,
                )))
                .map_err(|e| e.to_string())
            });
        match parsed {
            Ok(state)
                if state.schema_version <= SCHEMA_VERSION && state.manifest.validate().is_ok() =>
            {
                Some(state)
            }
            Ok(_) => {
                tracing::info!(path = %path.display(), "install state from another version; files will be checked");
                None
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "unreadable install state; files will be checked");
                None
            }
        }
    }

    pub(crate) fn write(&self, package: &Path) -> std::io::Result<()> {
        let json = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        let bytes = zstd::bulk::compress(&json, 3)?;
        super::io::write_atomic(&package.join(STATE_FILE), &bytes)
    }

    /// Whether `path` is still exactly as verified, with the same chunks as
    /// `want` in `manifest`.
    pub(crate) fn unchanged(
        &self,
        manifest: &Manifest,
        want: &infinity_depot::File,
        on_disk: Option<Stat>,
    ) -> bool {
        let Some(had) = self.manifest.file(&want.path) else {
            return false;
        };
        let same_chunks = had.size == want.size
            && had.chunks.len() == want.chunks.len()
            && had
                .chunks
                .iter()
                .zip(&want.chunks)
                .all(|(a, b)| self.manifest.chunk(*a).id == manifest.chunk(*b).id);
        same_chunks && on_disk.is_some() && self.stats.get(&want.path).copied() == on_disk
    }
}

pub(crate) fn stat(path: &Path) -> Option<Stat> {
    let meta = std::fs::metadata(path)
        .ok()
        .filter(std::fs::Metadata::is_file)?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
    Some(Stat {
        size: meta.len(),
        mtime,
    })
}
