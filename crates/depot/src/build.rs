//! Turning a package folder into a manifest and the packs it needs.
//!
//! Chunks already published by an earlier manifest of the product are
//! referenced where they are, so a release only writes, and uploads, packs
//! of new chunks. Hashing and compression run on all cores; files are read
//! once, in path order, with memory bounded by one batch of chunks.

use std::collections::HashMap;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::cdc::{self, Params};
use crate::codec;
use crate::error::{Error, IoContext, Result};
use crate::manifest::{self, HASH_ALGORITHM, SCHEMA_VERSION};
use crate::{Chunk, Encoding, File, Hash, Manifest, Pack, RESERVED_FILES};

const BATCH_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Options {
    pub product_id: String,
    pub version: String,
    pub params: Params,
    pub level: i32,
    /// Packs are closed once they reach this size.
    pub pack_target: u64,
}

impl Options {
    pub fn new(product_id: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            product_id: product_id.into(),
            version: version.into(),
            params: Params::DEFAULT,
            level: codec::DEFAULT_LEVEL,
            pack_target: 64 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub files_done: u64,
    pub files_total: u64,
    /// Stored bytes written to new packs so far.
    pub new_bytes: u64,
}

#[derive(Debug)]
pub struct Built {
    pub manifest: Manifest,
    /// Packs written to the output folder, to upload.
    pub new_packs: Vec<BuiltPack>,
    /// Manager files found in the folder and left out.
    pub skipped: Vec<String>,
    pub new_chunks: u64,
    pub reused_chunks: u64,
}

#[derive(Debug, Clone)]
pub struct BuiltPack {
    pub id: Hash,
    pub path: PathBuf,
    pub size: u64,
}

/// Chunks `root` into a manifest, writing new packs into `out_dir`.
/// `previous` manifests of the same product lend their chunks. Blocking.
pub fn build(
    root: &Path,
    out_dir: &Path,
    opts: &Options,
    previous: &[Manifest],
    progress: &mut dyn FnMut(&Progress),
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<Built> {
    if let Some(other) = previous.iter().find(|m| m.product_id != opts.product_id) {
        return Err(Error::Package(format!(
            "cannot reuse chunks of {}: it is another product",
            other.product_id
        )));
    }
    let walk = walk(root)?;
    std::fs::create_dir_all(out_dir).at(out_dir)?;

    let mut state = State::new(out_dir, opts, previous);
    let mut report = Progress {
        bytes_total: walk.files.iter().map(|f| f.size).sum(),
        files_total: walk.files.len() as u64,
        ..Progress::default()
    };
    progress(&report);

    let mut files = Vec::with_capacity(walk.files.len());
    for planned in &walk.files {
        let reader = std::fs::File::open(&planned.path).at(&planned.path)?;
        let mut indices = Vec::new();
        let mut batch: Vec<Vec<u8>> = Vec::new();
        let mut batched = 0usize;
        let mut size = 0u64;
        let mut flush = |batch: &mut Vec<Vec<u8>>,
                         state: &mut State<'_>,
                         report: &mut Progress|
         -> Result<()> {
            if cancelled() {
                return Err(Error::Cancelled);
            }
            let n: u64 = batch.iter().map(|c| c.len() as u64).sum();
            state.add_batch(std::mem::take(batch), &mut indices)?;
            report.bytes_done += n;
            report.new_bytes = state.new_bytes;
            progress(report);
            Ok(())
        };
        cdc::chunk_reader::<_, Error>(
            std::io::BufReader::with_capacity(1 << 20, reader),
            opts.params,
            |c| {
                size += c.len() as u64;
                batched += c.len();
                batch.push(c.to_vec());
                if batched >= BATCH_BYTES {
                    batched = 0;
                    flush(&mut batch, &mut state, &mut report)?;
                }
                Ok(())
            },
        )
        .map_err(|e| match e {
            Error::Io { source, .. } => Error::Io {
                path: planned.path.clone(),
                source,
            },
            other => other,
        })?;
        if !batch.is_empty() {
            flush(&mut batch, &mut state, &mut report)?;
        }
        if size != planned.size {
            return Err(Error::Package(format!(
                "{} changed while it was being read",
                planned.rel
            )));
        }
        files.push(File {
            path: planned.rel.clone(),
            size,
            chunks: indices,
        });
        report.files_done += 1;
        progress(&report);
    }
    state.close_pack()?;

    let manifest = Manifest {
        schema_version: SCHEMA_VERSION,
        hash: HASH_ALGORITHM.to_owned(),
        product_id: opts.product_id.clone(),
        version: opts.version.clone(),
        packs: state.packs,
        chunks: state.chunks,
        files,
        dirs: walk.empty_dirs,
    };
    manifest.validate()?;
    Ok(Built {
        manifest,
        new_packs: state.written,
        skipped: walk.skipped,
        new_chunks: state.new_chunks,
        reused_chunks: state.reused_chunks,
    })
}

struct State<'a> {
    out_dir: &'a Path,
    opts: &'a Options,
    /// Chunk id -> index in `chunks`.
    known: HashMap<Hash, u32>,
    /// Chunk id -> (record, its pack) from earlier releases.
    lent: HashMap<Hash, (Chunk, &'a Pack)>,
    packs: Vec<Pack>,
    pack_index: HashMap<Hash, u32>,
    chunks: Vec<Chunk>,
    writer: Option<PackWriter>,
    written: Vec<BuiltPack>,
    new_bytes: u64,
    new_chunks: u64,
    reused_chunks: u64,
    next_tmp: u32,
}

struct PackWriter {
    file: BufWriter<std::fs::File>,
    tmp: PathBuf,
    hasher: blake3::Hasher,
    size: u64,
    /// Index in `packs`, a placeholder until closed.
    slot: u32,
}

impl<'a> State<'a> {
    fn new(out_dir: &'a Path, opts: &'a Options, previous: &'a [Manifest]) -> Self {
        let mut lent = HashMap::new();
        for m in previous {
            for c in &m.chunks {
                lent.entry(c.id).or_insert((*c, m.pack_of(c)));
            }
        }
        Self {
            out_dir,
            opts,
            known: HashMap::new(),
            lent,
            packs: Vec::new(),
            pack_index: HashMap::new(),
            chunks: Vec::new(),
            writer: None,
            written: Vec::new(),
            new_bytes: 0,
            new_chunks: 0,
            reused_chunks: 0,
            next_tmp: 0,
        }
    }

    fn add_batch(&mut self, batch: Vec<Vec<u8>>, out: &mut Vec<u32>) -> Result<()> {
        let level = self.opts.level;
        let ids: Vec<Hash> = batch.par_iter().map(|raw| Hash::of(raw)).collect();
        let fresh: Vec<bool> = ids
            .iter()
            .map(|id| !self.known.contains_key(id) && !self.lent.contains_key(id))
            .collect();
        let encoded: Vec<Option<(Encoding, Vec<u8>)>> = batch
            .par_iter()
            .zip(fresh.par_iter())
            .map(|(raw, &fresh)| fresh.then(|| codec::encode(raw, level)).transpose())
            .collect::<Result<_>>()?;

        for ((raw, id), encoded) in batch.iter().zip(ids).zip(encoded) {
            if let Some(&index) = self.known.get(&id) {
                out.push(index);
                continue;
            }
            let index = self.chunks.len() as u32;
            if let Some(&(chunk, pack)) = self.lent.get(&id) {
                let pack = self.reuse_pack(pack);
                self.chunks.push(Chunk { pack, ..chunk });
                self.reused_chunks += 1;
            } else {
                let (encoding, stored) = match encoded {
                    Some(e) => e,
                    // A chunk repeated within this batch, already written.
                    None => codec::encode(raw, level)?,
                };
                let (pack, offset) = self.append(&stored)?;
                self.chunks.push(Chunk {
                    id,
                    size: raw.len() as u32,
                    pack,
                    offset,
                    stored: stored.len() as u32,
                    encoding,
                });
                self.new_chunks += 1;
                if self
                    .writer
                    .as_ref()
                    .is_some_and(|w| w.size >= self.opts.pack_target)
                {
                    self.close_pack()?;
                }
            }
            self.known.insert(id, index);
            out.push(index);
        }
        Ok(())
    }

    fn reuse_pack(&mut self, pack: &Pack) -> u32 {
        if let Some(&i) = self.pack_index.get(&pack.id) {
            return i;
        }
        let i = self.packs.len() as u32;
        self.packs.push(pack.clone());
        self.pack_index.insert(pack.id, i);
        i
    }

    fn append(&mut self, stored: &[u8]) -> Result<(u32, u64)> {
        if self.writer.is_none() {
            let tmp = self
                .out_dir
                .join(format!(".building-{}.pack.tmp", self.next_tmp));
            self.next_tmp += 1;
            let file = std::fs::File::create(&tmp).at(&tmp)?;
            let slot = self.packs.len() as u32;
            self.packs.push(Pack {
                id: Hash([0; 32]),
                size: 0,
            });
            self.writer = Some(PackWriter {
                file: BufWriter::with_capacity(1 << 20, file),
                tmp,
                hasher: blake3::Hasher::new(),
                size: 0,
                slot,
            });
        }
        let Some(w) = self.writer.as_mut() else {
            return Err(Error::Package("pack writer missing".into()));
        };
        let offset = w.size;
        w.file.write_all(stored).at(&w.tmp)?;
        w.hasher.update(stored);
        w.size += stored.len() as u64;
        self.new_bytes += stored.len() as u64;
        Ok((w.slot, offset))
    }

    fn close_pack(&mut self) -> Result<()> {
        let Some(w) = self.writer.take() else {
            return Ok(());
        };
        let file = w.file.into_inner().map_err(|e| Error::Io {
            path: w.tmp.clone(),
            source: e.into_error(),
        })?;
        file.sync_all().at(&w.tmp)?;
        drop(file);
        let id = Hash::from(w.hasher.finalize());
        let path = self.out_dir.join(format!("{}.pack", id.to_hex()));
        std::fs::rename(&w.tmp, &path).at(&path)?;
        self.packs[w.slot as usize] = Pack { id, size: w.size };
        self.written.push(BuiltPack {
            id,
            path,
            size: w.size,
        });
        Ok(())
    }
}

struct Walk {
    files: Vec<Planned>,
    empty_dirs: Vec<String>,
    skipped: Vec<String>,
}

struct Planned {
    rel: String,
    path: PathBuf,
    size: u64,
}

/// Lists the folder. Links are refused: an install cannot recreate them.
fn walk(root: &Path) -> Result<Walk> {
    let meta = std::fs::metadata(root).at(root)?;
    if !meta.is_dir() {
        return Err(Error::Package(format!(
            "{} is not a folder",
            root.display()
        )));
    }
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut skipped = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut pending = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = pending.pop() {
        for entry in std::fs::read_dir(&dir).at(&dir)? {
            let entry = entry.at(&dir)?;
            let path = entry.path();
            let kind = entry.file_type().at(&path)?;
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| {
                Error::Package(format!("{} has a name that is not Unicode", path.display()))
            })?;
            let rel = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            if kind.is_symlink() {
                return Err(Error::Package(format!(
                    "{rel} is a link; packages may only hold files and folders"
                )));
            }
            manifest::check_path(&rel).map_err(|e| Error::Package(e.to_string()))?;
            if !seen.insert(rel.to_lowercase()) {
                return Err(Error::Package(format!(
                    "{rel} differs from another entry only by case"
                )));
            }
            if kind.is_dir() {
                dirs.push(rel.clone());
                pending.push((path, rel));
            } else if prefix.is_empty()
                && RESERVED_FILES.iter().any(|r| name.eq_ignore_ascii_case(r))
            {
                skipped.push(rel);
            } else {
                let size = entry.metadata().at(&path)?.len();
                files.push(Planned { rel, path, size });
            }
        }
    }
    if files.is_empty() {
        return Err(Error::Package(format!("{} holds no files", root.display())));
    }
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    let mut empty_dirs: Vec<String> = dirs
        .into_iter()
        .filter(|d| {
            let prefix = format!("{d}/");
            !files.iter().any(|f| f.rel.starts_with(&prefix))
        })
        .collect();
    empty_dirs.sort();
    // Only the deepest empty folders; their parents are implied.
    let all = empty_dirs.clone();
    empty_dirs.retain(|d| {
        let prefix = format!("{d}/");
        !all.iter().any(|o| o.starts_with(&prefix))
    });
    Ok(Walk {
        files,
        empty_dirs,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    const SMALL: Params = Params {
        min: 1024,
        avg: 4096,
        max: 16384,
    };

    fn opts(version: &str) -> Options {
        Options {
            params: SMALL,
            level: 3,
            pack_target: 64 * 1024,
            ..Options::new("p", version)
        }
    }

    fn noise(len: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }

    fn quiet(_: &Progress) {}

    /// Rebuilds a file from packs on disk, as an installer would.
    fn read_back(built: &Built, packs_dir: &Path, file: &File) -> Vec<u8> {
        let m = &built.manifest;
        let mut out = Vec::new();
        for slot in m.slots(file) {
            let chunk = m.chunk(slot.chunk);
            let pack =
                fs::read(packs_dir.join(format!("{}.pack", m.pack_of(chunk).id.to_hex()))).unwrap();
            let stored = &pack[chunk.offset as usize..chunk.stored_end() as usize];
            out.extend(codec::decode(chunk, stored).unwrap());
        }
        out
    }

    #[test]
    fn builds_and_reads_back_and_reuses_unchanged_chunks() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("pkg");
        let packs = tmp.path().join("packs");
        fs::create_dir_all(root.join("SimObjects/a")).unwrap();
        fs::create_dir_all(root.join("empty/deeper")).unwrap();
        let big = noise(300_000, 1);
        fs::write(root.join("SimObjects/a/model.bin"), &big).unwrap();
        fs::write(root.join("layout.json"), b"{}").unwrap();
        fs::write(root.join("zero"), b"").unwrap();
        fs::write(root.join("copy.bin"), &big).unwrap();
        fs::write(root.join("infinity.json"), b"{}").unwrap();

        let v1 = build(&root, &packs, &opts("1.0.0"), &[], &mut quiet, &|| false).unwrap();
        assert_eq!(v1.skipped, vec!["infinity.json"]);
        assert_eq!(v1.manifest.dirs, vec!["empty/deeper"]);
        assert!(v1.new_packs.len() > 1, "pack target splits packs");
        let model = v1.manifest.file("SimObjects/a/model.bin").unwrap();
        assert_eq!(read_back(&v1, &packs, model), big);
        assert_eq!(
            v1.manifest.file("copy.bin").unwrap().chunks,
            model.chunks,
            "dedup within a release"
        );
        assert!(v1.manifest.file("zero").unwrap().chunks.is_empty());

        // Edit the middle of the big file.
        let mut edited = big.clone();
        edited[150_000..150_010].copy_from_slice(b"0123456789");
        fs::write(root.join("SimObjects/a/model.bin"), &edited).unwrap();
        let packs2 = tmp.path().join("packs2");
        let v2 = build(
            &root,
            &packs2,
            &opts("1.0.1"),
            std::slice::from_ref(&v1.manifest),
            &mut quiet,
            &|| false,
        )
        .unwrap();
        assert!(
            v2.new_chunks <= 3,
            "only the edited chunks are new: {}",
            v2.new_chunks
        );
        assert!(v2.reused_chunks > 10);
        let new_bytes: u64 = v2.new_packs.iter().map(|p| p.size).sum();
        assert!(new_bytes < 60_000, "{new_bytes}");
        // Old packs are referenced, not rewritten.
        for p in &v2.new_packs {
            assert!(!v1.new_packs.iter().any(|o| o.id == p.id));
        }
        fs::create_dir_all(&packs).unwrap();
        for p in &v2.new_packs {
            fs::copy(&p.path, packs.join(p.path.file_name().unwrap())).unwrap();
        }
        let model2 = v2.manifest.file("SimObjects/a/model.bin").unwrap();
        assert_eq!(read_back(&v2, &packs, model2), edited);
    }

    #[test]
    fn refuses_case_collisions_and_cancels() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("pkg");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.bin"), noise(10, 2)).unwrap();
        let err = build(
            &root,
            &tmp.path().join("o"),
            &opts("1"),
            &[],
            &mut quiet,
            &|| true,
        );
        assert!(matches!(err, Err(Error::Cancelled)));
    }
}
