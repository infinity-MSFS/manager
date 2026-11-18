//! Building changed files beside the install, chunk by chunk.
//!
//! Every file that needs work is assembled at full size under the job's
//! `tree/` folder: chunks already on disk are copied in, the rest arrive
//! from the network. Each placed chunk is appended to a journal once its
//! bytes are flushed, so a pause, crash or quit loses at most a couple of
//! seconds of work. The journal is a hint, never trusted: before anything
//! is installed every staged file is read back and hashed chunk by chunk,
//! and whatever fails goes back to be fetched again.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use infinity_depot::{Hash, Manifest};

use super::io::{read_at, write_at};
use super::scan::{Place, Scan};
use crate::error::{Error, IoContext, Result, Stop};
use crate::extract::Progress;

const MAGIC: &[u8; 4] = b"IDJ1";
const HEADER: u64 = 4 + 32;
const RECORD: usize = 8;
const VERIFY_THREADS: usize = 4;

/// The job's working folder, `<stem>.depot` in the staging root.
#[derive(Debug, Clone)]
pub(crate) struct Dir {
    root: PathBuf,
}

impl Dir {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// What the job is for, so it can be listed after a restart.
    pub(crate) fn job(&self) -> PathBuf {
        self.root.join("job.json")
    }

    /// The verified manifest bytes.
    pub(crate) fn manifest(&self) -> PathBuf {
        self.root.join("manifest.zst")
    }

    pub(crate) fn tree(&self) -> PathBuf {
        self.root.join("tree")
    }

    pub(crate) fn journal(&self) -> PathBuf {
        self.root.join("journal")
    }

    pub(crate) fn commit(&self) -> PathBuf {
        self.root.join("commit.json")
    }

    /// Installed files being replaced, held until the new ones are in.
    pub(crate) fn aside(&self) -> PathBuf {
        self.root.join("aside")
    }
}

/// Where one chunk goes: a slot of a staged file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Target {
    pub(crate) file: u32,
    pub(crate) slot: u32,
    pub(crate) offset: u64,
}

/// Which chunks still have to be put where.
#[derive(Debug, Default)]
pub(crate) struct Work {
    /// Chunk index -> a copy on disk and the slots that want it.
    pub(crate) local: HashMap<u32, (Place, Vec<Target>)>,
    /// Chunk index -> slots that want it, to download.
    pub(crate) remote: HashMap<u32, Vec<Target>>,
}

impl Work {
    pub(crate) fn local_bytes(&self, manifest: &Manifest) -> u64 {
        self.local
            .keys()
            .map(|&c| u64::from(manifest.chunk(c).size))
            .sum()
    }
}

/// Sorts every slot of the files to stage into placed already, copy from
/// disk, or download.
pub(crate) fn plan(
    manifest: &Manifest,
    staged: &[u32],
    placed: &HashSet<(u32, u32)>,
    scan: &Scan,
) -> Work {
    let mut work = Work::default();
    for &fi in staged {
        let file = &manifest.files[fi as usize];
        for slot in manifest.slots(file) {
            if placed.contains(&(fi, slot.index)) {
                continue;
            }
            let target = Target {
                file: fi,
                slot: slot.index,
                offset: slot.offset,
            };
            let id = manifest.chunk(slot.chunk).id;
            match scan.source(&id) {
                Some(place) => work
                    .local
                    .entry(slot.chunk)
                    .or_insert_with(|| (place.clone(), Vec::new()))
                    .1
                    .push(target),
                None => work.remote.entry(slot.chunk).or_default().push(target),
            }
        }
    }
    work
}

/// An opened journal, the slots it records as placed, and whether it
/// started over.
pub(crate) type Opened = (Journal, HashSet<(u32, u32)>, bool);

/// Append-only record of placed chunks.
#[derive(Debug)]
pub(crate) struct Journal {
    file: File,
    path: PathBuf,
    pending: Vec<(u32, u32)>,
}

impl Journal {
    /// Opens the journal for `manifest_hash`, returning what it says is
    /// placed. A journal for another manifest is started over, and so must
    /// the tree be: `fresh` says so.
    pub(crate) fn open(path: &Path, manifest_hash: &Hash) -> Result<Opened> {
        let mut placed = HashSet::new();
        let mut fresh = true;
        if let Ok(mut f) = File::open(path) {
            let mut bytes = Vec::new();
            if f.read_to_end(&mut bytes).is_ok()
                && bytes.len() as u64 >= HEADER
                && &bytes[..4] == MAGIC
                && bytes[4..36] == manifest_hash.0
            {
                fresh = false;
                for r in bytes[HEADER as usize..].chunks_exact(RECORD) {
                    let fi = u32::from_le_bytes([r[0], r[1], r[2], r[3]]);
                    let slot = u32::from_le_bytes([r[4], r[5], r[6], r[7]]);
                    placed.insert((fi, slot));
                }
            }
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .at(path)?;
        if fresh {
            file.set_len(0).at(path)?;
            file.write_all(MAGIC).at(path)?;
            file.write_all(&manifest_hash.0).at(path)?;
            file.sync_data().at(path)?;
        } else {
            // Drop a torn last record.
            let keep = HEADER + valid_len(path)?;
            file.set_len(keep).at(path)?;
        }
        file.seek(SeekFrom::End(0)).at(path)?;
        Ok((
            Self {
                file,
                path: path.to_path_buf(),
                pending: Vec::new(),
            },
            placed,
            fresh,
        ))
    }

    fn record(&mut self, fi: u32, slot: u32) {
        self.pending.push((fi, slot));
    }

    fn flush(&mut self) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let mut bytes = Vec::with_capacity(self.pending.len() * RECORD);
        for (fi, slot) in self.pending.drain(..) {
            bytes.extend_from_slice(&fi.to_le_bytes());
            bytes.extend_from_slice(&slot.to_le_bytes());
        }
        self.file.write_all(&bytes).at(&self.path)?;
        self.file.sync_data().at(&self.path)
    }
}

/// Bytes of whole records after the header.
fn valid_len(path: &Path) -> Result<u64> {
    let len = std::fs::metadata(path).at(path)?.len();
    Ok(len.saturating_sub(HEADER) / RECORD as u64 * RECORD as u64)
}

/// The staged files of a job, open for positional writes.
#[derive(Debug)]
pub(crate) struct Sink {
    files: HashMap<u32, (PathBuf, Arc<File>)>,
    journal: Mutex<Journal>,
    dirty: Mutex<HashSet<u32>>,
    closed: AtomicBool,
}

impl Sink {
    /// Opens (creating at full size) every staged file under `tree`.
    pub(crate) fn open(
        manifest: &Manifest,
        tree: &Path,
        staged: &[u32],
        journal: Journal,
    ) -> Result<Self> {
        let mut files = HashMap::new();
        for &fi in staged {
            let file = &manifest.files[fi as usize];
            let path = tree.join(&file.path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).at(parent)?;
            }
            let handle = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .at(&path)?;
            if handle.metadata().at(&path)?.len() != file.size {
                handle.set_len(file.size).at(&path)?;
            }
            files.insert(fi, (path, Arc::new(handle)));
        }
        Ok(Self {
            files,
            journal: Mutex::new(journal),
            dirty: Mutex::new(HashSet::new()),
            closed: AtomicBool::new(false),
        })
    }

    pub(crate) fn path(&self, fi: u32) -> Option<&Path> {
        self.files.get(&fi).map(|(p, _)| p.as_path())
    }

    /// Writes verified chunk bytes into every target.
    pub(crate) fn place(&self, targets: &[Target], raw: &[u8]) -> Result<()> {
        for t in targets {
            if self.closed.load(Ordering::Relaxed) {
                return Err(Error::Stopped(Stop::Pause));
            }
            let Some((path, file)) = self.files.get(&t.file) else {
                return Err(Error::Internal(format!("file {} is not staged", t.file)));
            };
            write_at(file, raw, t.offset).at(path)?;
            lock(&self.dirty).insert(t.file);
            lock(&self.journal).record(t.file, t.slot);
        }
        Ok(())
    }

    /// Flushes written data, then journals it. Blocking.
    pub(crate) fn checkpoint(&self) -> Result<()> {
        let dirty: Vec<u32> = lock(&self.dirty).drain().collect();
        for fi in dirty {
            if let Some((path, file)) = self.files.get(&fi) {
                file.sync_data().at(path)?;
            }
        }
        lock(&self.journal).flush()
    }

    /// Refuses further writes, so work still in flight after a stop cannot
    /// touch the files.
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Copies chunks found on disk into their targets, checking each against
/// its hash. Chunks whose copy turns out bad move to `work.remote`.
/// Blocking; `None` when stopped.
pub(crate) fn copy_local(
    manifest: &Manifest,
    sink: &Sink,
    work: &mut Work,
    progress: &Progress,
) -> Result<Option<()>> {
    let mut local: Vec<(u32, (Place, Vec<Target>))> = work.local.drain().collect();
    // Read each source file front to back.
    local.sort_by(|a, b| (&a.1.0.path, a.1.0.offset).cmp(&(&b.1.0.path, b.1.0.offset)));
    let mut open: Option<(PathBuf, File)> = None;
    let mut buf = Vec::new();
    for (chunk_index, (place, targets)) in local {
        if progress.stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let chunk = manifest.chunk(chunk_index);
        if open.as_ref().is_none_or(|(p, _)| *p != place.path) {
            open = File::open(&place.path)
                .ok()
                .map(|f| (place.path.clone(), f));
        }
        buf.resize(chunk.size as usize, 0);
        let good = open.as_ref().is_some_and(|(_, f)| {
            read_at(f, &mut buf, place.offset).is_ok_and(|n| n == buf.len())
                && Hash::of(&buf) == chunk.id
        });
        progress
            .done
            .fetch_add(u64::from(chunk.size), Ordering::Relaxed);
        if good {
            sink.place(&targets, &buf)?;
        } else {
            tracing::debug!(chunk = %chunk.id, path = %place.path.display(), "local copy is not usable; downloading it");
            work.remote.entry(chunk_index).or_default().extend(targets);
        }
    }
    sink.checkpoint()?;
    Ok(Some(()))
}

/// Reads every slot of the staged files back and returns those that do
/// not hash right, as chunk index and target. Blocking; `None` when
/// stopped.
pub(crate) fn verify(
    manifest: &Manifest,
    sink: &Sink,
    files: &[u32],
    progress: &Progress,
) -> Option<Vec<(u32, Target)>> {
    let total: u64 = files.iter().map(|&f| manifest.files[f as usize].size).sum();
    progress.total.store(total.max(1), Ordering::Relaxed);
    let next = AtomicUsize::new(0);
    let bad = Mutex::new(Vec::new());
    let threads = VERIFY_THREADS.min(files.len().max(1));
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                let mut buf = Vec::new();
                while let Some(&fi) = files.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let file = &manifest.files[fi as usize];
                    let Some(path) = sink.path(fi) else { continue };
                    let handle = File::open(path).ok();
                    for slot in manifest.slots(file) {
                        if progress.stop.load(Ordering::Relaxed) {
                            return;
                        }
                        buf.resize(slot.size as usize, 0);
                        let ok = handle.as_ref().is_some_and(|h| {
                            read_at(h, &mut buf, slot.offset).is_ok_and(|n| n == buf.len())
                                && Hash::of(&buf) == manifest.chunk(slot.chunk).id
                        });
                        progress
                            .done
                            .fetch_add(u64::from(slot.size), Ordering::Relaxed);
                        if !ok {
                            lock(&bad).push((
                                slot.chunk,
                                Target {
                                    file: fi,
                                    slot: slot.index,
                                    offset: slot.offset,
                                },
                            ));
                        }
                    }
                }
            });
        }
    });
    if progress.stop.load(Ordering::Relaxed) {
        return None;
    }
    Some(
        bad.into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_survives_reopen_and_resets_for_another_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("journal");
        let a = Hash::of(b"a");
        let (mut j, placed, fresh) = Journal::open(&path, &a).unwrap();
        assert!(placed.is_empty() && fresh);
        j.record(1, 2);
        j.record(3, 4);
        j.flush().unwrap();
        drop(j);
        // A torn record at the end is ignored.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&[9, 9, 9])
            .unwrap();
        let (_, placed, fresh) = Journal::open(&path, &a).unwrap();
        assert!(!fresh);
        assert_eq!(placed, [(1, 2), (3, 4)].into());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), HEADER + 16);
        let (_, placed, fresh) = Journal::open(&path, &Hash::of(b"b")).unwrap();
        assert!(placed.is_empty() && fresh);
    }
}
