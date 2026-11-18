//! What one version of a package is made of.
//!
//! On the wire a manifest is zstd-compressed JSON. Chunks are listed once
//! and files refer to them by index, so a chunk shared by two files (or
//! repeated inside one) costs one entry. Field names of the bulky chunk
//! records are single letters: a 15 GB package has tens of thousands.
//!
//! [`Manifest::from_bytes`] checks the bytes against the hash the catalog
//! published and then [`Manifest::validate`]s the result, so code past it
//! may rely on: indices in range, chunks inside their packs, files tiled
//! exactly by their chunks, and paths that cannot leave the package folder.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::cdc::MAX_CHUNK;
use crate::error::{Error, Result};
use crate::{Hash, RESERVED_FILES};

pub const SCHEMA_VERSION: u32 = 1;
pub const HASH_ALGORITHM: &str = "blake3";

/// Larger compressed manifests are refused before decompressing.
const MAX_WIRE_BYTES: usize = 64 * 1024 * 1024;
/// And larger decompressed ones while decompressing.
const MAX_JSON_BYTES: u64 = 512 * 1024 * 1024;
const MAX_PATH_LEN: usize = 400;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub schema_version: u32,
    /// Hash function of chunk and pack names. Always [`HASH_ALGORITHM`]
    /// today; a different value is a format this build cannot read.
    pub hash: String,
    pub product_id: String,
    pub version: String,
    pub packs: Vec<Pack>,
    pub chunks: Vec<Chunk>,
    pub files: Vec<File>,
    /// Folders that exist without files in them. Folders holding files are
    /// implied by the file paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dirs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pack {
    /// Hash of the pack's bytes; also its storage name.
    pub id: Hash,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    /// Hash of the chunk's raw (decoded) bytes.
    #[serde(rename = "h")]
    pub id: Hash,
    /// Raw size.
    #[serde(rename = "s")]
    pub size: u32,
    /// Index into [`Manifest::packs`].
    #[serde(rename = "p")]
    pub pack: u32,
    /// Where the stored bytes start in the pack.
    #[serde(rename = "o")]
    pub offset: u64,
    /// Stored (encoded) size.
    #[serde(rename = "c")]
    pub stored: u32,
    #[serde(rename = "e", default)]
    pub encoding: Encoding,
}

impl Chunk {
    pub fn stored_end(&self) -> u64 {
        self.offset + u64::from(self.stored)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Encoding {
    /// One zstd frame.
    #[default]
    #[serde(rename = "z")]
    Zstd,
    /// The raw bytes, for data that does not compress.
    #[serde(rename = "r")]
    Raw,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct File {
    /// `/`-separated, relative to the package folder.
    pub path: String,
    pub size: u64,
    /// Indices into [`Manifest::chunks`], in file order. Empty for an
    /// empty file.
    pub chunks: Vec<u32>,
}

/// One chunk-sized piece of a file: which chunk belongs where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    /// Position in [`File::chunks`].
    pub index: u32,
    /// Index into [`Manifest::chunks`].
    pub chunk: u32,
    /// Byte offset in the file.
    pub offset: u64,
    pub size: u32,
}

impl Manifest {
    /// Decodes and validates published manifest bytes. With `expected`,
    /// the bytes must hash to it first.
    pub fn from_bytes(bytes: &[u8], expected: Option<Hash>) -> Result<Self> {
        if let Some(expected) = expected {
            let actual = Hash::of(bytes);
            if actual != expected {
                return Err(Error::ManifestHash {
                    expected: expected.to_hex(),
                    actual: actual.to_hex(),
                });
            }
        }
        if bytes.len() > MAX_WIRE_BYTES {
            return Err(Error::Invalid("the manifest is implausibly large".into()));
        }
        let decoder = zstd::stream::read::Decoder::new(bytes)
            .map_err(|e| Error::Invalid(format!("not zstd: {e}")))?;
        let limited = std::io::Read::take(decoder, MAX_JSON_BYTES);
        let manifest: Self = serde_json::from_reader(std::io::BufReader::new(limited))
            .map_err(|e| Error::Invalid(format!("unreadable: {e}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// The publishable bytes. Their hash goes in the catalog.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let json = serde_json::to_vec(self).map_err(|e| Error::Invalid(e.to_string()))?;
        zstd::bulk::compress(&json, 19).map_err(|e| Error::Invalid(format!("compress: {e}")))
    }

    /// Bytes of all files.
    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// The chunks of `file` with their offsets. Assumes a validated
    /// manifest.
    pub fn slots<'a>(&'a self, file: &'a File) -> impl Iterator<Item = Slot> + 'a {
        let mut offset = 0u64;
        file.chunks.iter().enumerate().map(move |(i, &c)| {
            let size = self.chunks[c as usize].size;
            let slot = Slot {
                index: i as u32,
                chunk: c,
                offset,
                size,
            };
            offset += u64::from(size);
            slot
        })
    }

    pub fn chunk(&self, index: u32) -> &Chunk {
        &self.chunks[index as usize]
    }

    pub fn pack_of(&self, chunk: &Chunk) -> &Pack {
        &self.packs[chunk.pack as usize]
    }

    /// Every check the installer relies on.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version > SCHEMA_VERSION {
            return Err(Error::Unsupported(format!(
                "the package manifest is format {}; update the manager",
                self.schema_version
            )));
        }
        if self.hash != HASH_ALGORITHM {
            return Err(Error::Unsupported(format!(
                "the package manifest uses the {} hash; update the manager",
                self.hash
            )));
        }
        let invalid = |m: String| Err(Error::Invalid(m));
        if self.packs.len() > u32::MAX as usize || self.chunks.len() > u32::MAX as usize {
            return invalid("too many entries".into());
        }
        for (i, chunk) in self.chunks.iter().enumerate() {
            let Some(pack) = self.packs.get(chunk.pack as usize) else {
                return invalid(format!("chunk {i} names a missing pack"));
            };
            if chunk.size == 0 || chunk.size as usize > MAX_CHUNK {
                return invalid(format!("chunk {i} has size {}", chunk.size));
            }
            if chunk.stored == 0
                || chunk
                    .offset
                    .checked_add(u64::from(chunk.stored))
                    .is_none_or(|end| end > pack.size)
            {
                return invalid(format!("chunk {i} lies outside its pack"));
            }
            if chunk.encoding == Encoding::Raw && chunk.stored != chunk.size {
                return invalid(format!("raw chunk {i} has a stored size unlike its size"));
            }
        }

        let mut seen = HashSet::new();
        let mut dirs_of_files = HashSet::new();
        for file in &self.files {
            check_path(&file.path)?;
            if RESERVED_FILES
                .iter()
                .any(|r| file.path.eq_ignore_ascii_case(r))
            {
                return invalid(format!("{} is reserved for the manager", file.path));
            }
            if !seen.insert(file.path.to_lowercase()) {
                return invalid(format!("{} is listed twice (ignoring case)", file.path));
            }
            let mut total = 0u64;
            for &c in &file.chunks {
                let Some(chunk) = self.chunks.get(c as usize) else {
                    return invalid(format!("{} names a missing chunk", file.path));
                };
                total += u64::from(chunk.size);
            }
            if total != file.size {
                return invalid(format!(
                    "{} is {} bytes but its chunks add up to {total}",
                    file.path, file.size
                ));
            }
            let mut parent = file.path.as_str();
            while let Some((dir, _)) = parent.rsplit_once('/') {
                dirs_of_files.insert(dir.to_lowercase());
                parent = dir;
            }
        }
        for dir in &self.dirs {
            check_path(dir)?;
            if seen.contains(&dir.to_lowercase()) {
                return invalid(format!("{dir} is both a file and a folder"));
            }
        }
        if let Some(file) = self
            .files
            .iter()
            .find(|f| dirs_of_files.contains(&f.path.to_lowercase()))
        {
            return invalid(format!("{} is both a file and a folder", file.path));
        }
        Ok(())
    }

    /// Where each chunk id is stored, first occurrence wins.
    pub fn chunk_index(&self) -> HashMap<Hash, u32> {
        let mut out = HashMap::with_capacity(self.chunks.len());
        for (i, c) in self.chunks.iter().enumerate() {
            out.entry(c.id).or_insert(i as u32);
        }
        out
    }

    pub fn file(&self, path: &str) -> Option<&File> {
        self.files.iter().find(|f| f.path == path)
    }
}

/// A relative, `/`-separated path that stays inside the package folder on
/// Windows and Unix alike.
pub fn check_path(path: &str) -> Result<()> {
    let bad = |why: &str| Err(Error::Invalid(format!("unsafe path {path:?}: {why}")));
    if path.is_empty() || path.len() > MAX_PATH_LEN {
        return bad("empty or too long");
    }
    if path.contains('\\') || path.contains('\0') || path.contains(':') {
        return bad("forbidden character");
    }
    for part in path.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return bad("empty, `.` or `..` component");
        }
        // Windows silently strips these, making two names one.
        if part.ends_with('.') || part.ends_with(' ') {
            return bad("component ends in a dot or space");
        }
        let stem = part.split('.').next().unwrap_or(part).to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit());
        if reserved {
            return bad("reserved device name");
        }
        if part
            .chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>' | '"' | '|' | '?' | '*'))
        {
            return bad("forbidden character");
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn sample() -> Manifest {
        let h = |b: &[u8]| Hash::of(b);
        Manifest {
            schema_version: SCHEMA_VERSION,
            hash: HASH_ALGORITHM.into(),
            product_id: "p".into(),
            version: "1.0.0".into(),
            packs: vec![Pack {
                id: h(b"pack"),
                size: 100,
            }],
            chunks: vec![
                Chunk {
                    id: h(b"a"),
                    size: 10,
                    pack: 0,
                    offset: 0,
                    stored: 8,
                    encoding: Encoding::Zstd,
                },
                Chunk {
                    id: h(b"b"),
                    size: 5,
                    pack: 0,
                    offset: 8,
                    stored: 5,
                    encoding: Encoding::Raw,
                },
            ],
            files: vec![
                File {
                    path: "a/one.bin".into(),
                    size: 15,
                    chunks: vec![0, 1],
                },
                File {
                    path: "two.bin".into(),
                    size: 10,
                    chunks: vec![0],
                },
                File {
                    path: "empty".into(),
                    size: 0,
                    chunks: vec![],
                },
            ],
            dirs: vec!["a/empty-dir".into()],
        }
    }

    #[test]
    fn round_trips_through_bytes_with_its_hash() {
        let m = sample();
        let bytes = m.to_bytes().unwrap();
        let got = Manifest::from_bytes(&bytes, Some(Hash::of(&bytes))).unwrap();
        assert_eq!(got, m);
        assert!(matches!(
            Manifest::from_bytes(&bytes, Some(Hash::of(b"other"))),
            Err(Error::ManifestHash { .. })
        ));
    }

    #[test]
    fn slots_carry_offsets() {
        let m = sample();
        let slots: Vec<_> = m.slots(&m.files[0]).collect();
        assert_eq!(
            slots[1],
            Slot {
                index: 1,
                chunk: 1,
                offset: 10,
                size: 5
            }
        );
    }

    #[test]
    fn rejects_impossible_manifests() {
        let mut m = sample();
        m.files[0].size = 14;
        assert!(m.validate().is_err());

        let mut m = sample();
        m.chunks[1].offset = 99;
        assert!(m.validate().is_err());

        let mut m = sample();
        m.files[1].path = "A/ONE.bin".into();
        assert!(m.validate().is_err());

        let mut m = sample();
        m.files[1].path = "a".into();
        assert!(m.validate().is_err(), "file shadowing a folder");

        let mut m = sample();
        m.files[1].path = "infinity.json".into();
        assert!(m.validate().is_err());

        let mut m = sample();
        m.schema_version = SCHEMA_VERSION + 1;
        assert!(matches!(m.validate(), Err(Error::Unsupported(_))));
    }

    #[test]
    fn paths_cannot_escape() {
        for bad in [
            "",
            "../x",
            "a/../b",
            "/abs",
            "C:/x",
            "a\\b",
            "a//b",
            "con",
            "x/LPT1.txt",
            "dot.",
            "q?",
        ] {
            assert!(check_path(bad).is_err(), "{bad}");
        }
        for good in [
            "a",
            "SimObjects/Airplanes/x/model/t.bin",
            "layout.json",
            "console.cfg",
        ] {
            assert!(check_path(good).is_ok(), "{good}");
        }
    }
}
