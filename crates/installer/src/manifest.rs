//! Package integrity manifests.
//!
//! A manifest records the size and SHA-256 of the archive and of every file
//! in the package. The upload tool builds one from the package folder with
//! [`Manifest::build`] and publishes it next to the archive under the
//! catalog's `manifestKey`. The installer hashes each file as it extracts
//! it and refuses to swap a package into the sim unless the staged tree
//! matches the manifest exactly.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::inspect::MARKER_FILE;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub schema_version: u32,
    /// The archive as uploaded. Checked before extraction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<FileDigest>,
    /// Every file in the package, keyed by `/`-separated path relative to
    /// the package folder.
    pub files: BTreeMap<String, FileDigest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDigest {
    pub size: u64,
    /// Lowercase hex.
    pub sha256: String,
}

/// One way a package differs from its manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Discrepancy {
    Missing(String),
    Unexpected(String),
    Mismatch(String),
}

impl std::fmt::Display for Discrepancy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(p) => write!(f, "{p} is missing"),
            Self::Unexpected(p) => write!(f, "{p} is not part of the package"),
            Self::Mismatch(p) => write!(f, "{p} is damaged"),
        }
    }
}

impl Manifest {
    /// Hashes every file under `package_root`, and the archive if given.
    /// Blocking; reads every byte.
    pub fn build(package_root: &Path, archive: Option<&Path>) -> std::io::Result<Self> {
        let mut files = BTreeMap::new();
        let mut pending = vec![package_root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir)? {
                let entry = entry?;
                let kind = entry.file_type()?;
                let path = entry.path();
                if kind.is_dir() {
                    pending.push(path);
                    continue;
                }
                let rel = path
                    .strip_prefix(package_root)
                    .ok()
                    .and_then(manifest_path)
                    .ok_or_else(|| std::io::Error::other(format!("odd path {}", path.display())))?;
                if rel != MARKER_FILE {
                    files.insert(rel, digest_file(&path)?);
                }
            }
        }
        Ok(Self {
            schema_version: SCHEMA_VERSION,
            archive: archive.map(digest_file).transpose()?,
            files,
        })
    }

    pub fn total_size(&self) -> u64 {
        self.files.values().map(|f| f.size).sum()
    }

    /// Compares what was actually written against the manifest.
    pub fn compare(&self, actual: &BTreeMap<String, FileDigest>) -> Vec<Discrepancy> {
        let mut out = Vec::new();
        for (path, want) in &self.files {
            match actual.get(path) {
                None => out.push(Discrepancy::Missing(path.clone())),
                Some(got) if got != want => out.push(Discrepancy::Mismatch(path.clone())),
                Some(_) => {}
            }
        }
        out.extend(
            actual
                .keys()
                .filter(|p| !self.files.contains_key(*p))
                .map(|p| Discrepancy::Unexpected(p.clone())),
        );
        out
    }
}

/// `/`-separated relative path, or `None` for anything that could escape.
pub(crate) fn manifest_path(rel: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(p) => parts.push(p.to_str()?),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn digest_file(path: &Path) -> std::io::Result<FileDigest> {
    let mut hasher = Hashing::new(std::io::sink());
    let mut file = std::fs::File::open(path)?;
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hasher.finish())
}

/// A writer that hashes what passes through it.
pub(crate) struct Hashing<W> {
    inner: W,
    hasher: Sha256,
    size: u64,
}

impl<W: std::io::Write> Hashing<W> {
    pub(crate) fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            size: 0,
        }
    }

    pub(crate) fn into_parts(self) -> (W, FileDigest) {
        let digest = FileDigest {
            size: self.size,
            sha256: hex(&self.hasher.finalize()),
        };
        (self.inner, digest)
    }

    fn finish(self) -> FileDigest {
        self.into_parts().1
    }
}

impl<W: std::io::Write> std::io::Write for Hashing<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.size += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// SHA-256 of a whole file, calling `progress` with bytes read so far and
/// checking `stopped` between reads. Blocking.
pub(crate) fn digest_with_progress(
    path: &Path,
    mut progress: impl FnMut(u64),
    stopped: impl Fn() -> bool,
) -> std::io::Result<Option<FileDigest>> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 1 << 20];
    let mut size = 0;
    loop {
        if stopped() {
            return Ok(None);
        }
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
        progress(size);
    }
    Ok(Some(FileDigest {
        size,
        sha256: hex(&hasher.finalize()),
    }))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    const HELLO_SHA: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    #[test]
    fn builds_from_a_package_folder() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("SimObjects/a")).unwrap();
        fs::write(tmp.path().join("manifest.json"), "hello").unwrap();
        fs::write(tmp.path().join("SimObjects/a/b.cfg"), "").unwrap();
        fs::write(tmp.path().join(MARKER_FILE), "{}").unwrap();

        let m = Manifest::build(tmp.path(), Some(&tmp.path().join("manifest.json"))).unwrap();
        assert_eq!(
            m.files.keys().collect::<Vec<_>>(),
            vec!["SimObjects/a/b.cfg", "manifest.json"]
        );
        assert_eq!(m.files["manifest.json"].sha256, HELLO_SHA);
        assert_eq!(m.files["manifest.json"].size, 5);
        assert_eq!(m.archive.as_ref().unwrap().sha256, HELLO_SHA);
        assert_eq!(m.total_size(), 5);

        let json = serde_json::to_value(&m).unwrap();
        assert_eq!(json["schemaVersion"], 1);
        assert_eq!(serde_json::from_value::<Manifest>(json).unwrap(), m);
    }

    #[test]
    fn compare_reports_every_difference() {
        let d = |size, sha: &str| FileDigest {
            size,
            sha256: sha.to_owned(),
        };
        let manifest = Manifest {
            schema_version: SCHEMA_VERSION,
            archive: None,
            files: [("a".into(), d(1, "aa")), ("b".into(), d(1, "bb"))].into(),
        };
        let actual = [("a".into(), d(1, "xx")), ("c".into(), d(1, "cc"))].into();
        assert_eq!(
            manifest.compare(&actual),
            vec![
                Discrepancy::Mismatch("a".into()),
                Discrepancy::Missing("b".into()),
                Discrepancy::Unexpected("c".into()),
            ]
        );
        assert!(manifest.compare(&manifest.files).is_empty());
    }

    #[test]
    fn manifest_paths_never_escape() {
        assert_eq!(manifest_path(Path::new("a/./b")), Some("a/b".into()));
        assert_eq!(manifest_path(Path::new("../a")), None);
        assert_eq!(manifest_path(Path::new("")), None);
    }

    #[test]
    fn hashing_writer_matches_a_direct_digest() {
        let mut w = Hashing::new(Vec::new());
        std::io::Write::write_all(&mut w, b"hello").unwrap();
        let (buf, digest) = w.into_parts();
        assert_eq!(buf, b"hello");
        assert_eq!(digest.sha256, HELLO_SHA);
    }
}
