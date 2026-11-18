//! Comparing an installed package with a manifest.
//!
//! Each file is read in the manifest's chunk layout and every chunk-sized
//! slice hashed, so the scan says not just which files are damaged but
//! which of their chunks are still good. Good chunks anywhere in the
//! package are recorded as local sources: an update or repair copies them
//! instead of downloading them, which is what makes a patch to one texture
//! inside a 2 GB bank cost a few megabytes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use infinity_depot::{Hash, Manifest};

use super::io::read_at;
use super::state::{InstalledState, stat};
use crate::extract::Progress;

const MAX_THREADS: usize = 4;

/// A copy of a chunk on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Place {
    pub(crate) path: PathBuf,
    pub(crate) offset: u64,
}

#[derive(Debug, Default)]
pub(crate) struct Scan {
    /// Per manifest file: the installed copy is exactly right.
    pub(crate) intact: Vec<bool>,
    /// Chunks read and hashed just now.
    pub(crate) found: HashMap<Hash, Place>,
    /// Chunks that should be on disk according to the previous install,
    /// not read. Checked when copied.
    pub(crate) claimed: HashMap<Hash, Place>,
}

impl Scan {
    pub(crate) fn source(&self, id: &Hash) -> Option<&Place> {
        self.found.get(id).or_else(|| self.claimed.get(id))
    }

    pub(crate) fn intact_count(&self) -> usize {
        self.intact.iter().filter(|i| **i).count()
    }
}

/// Scans `package` against `manifest`. Without `full`, a file the
/// previous install recorded as verified, unchanged since and with the
/// same chunks is trusted unread. Blocking; `None` when stopped.
pub(crate) fn scan(
    manifest: &Manifest,
    package: &Path,
    previous: Option<&InstalledState>,
    full: bool,
    progress: &Progress,
) -> Option<Scan> {
    let mut out = Scan {
        intact: vec![false; manifest.files.len()],
        ..Scan::default()
    };
    let mut to_read = Vec::new();
    let mut total = 0u64;
    for (i, file) in manifest.files.iter().enumerate() {
        let path = package.join(&file.path);
        let on_disk = stat(&path);
        let trusted = !full && previous.is_some_and(|p| p.unchanged(manifest, file, on_disk));
        if trusted {
            out.intact[i] = true;
            for slot in manifest.slots(file) {
                out.claimed
                    .entry(manifest.chunk(slot.chunk).id)
                    .or_insert_with(|| Place {
                        path: path.clone(),
                        offset: slot.offset,
                    });
            }
        } else if let Some(s) = on_disk {
            total += s.size.min(file.size);
            to_read.push(i);
        }
    }
    progress.total.store(total.max(1), Ordering::Relaxed);
    // Biggest first, so one large file does not finish last on its own.
    to_read.sort_by_key(|&i| std::cmp::Reverse(manifest.files[i].size));

    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism()
        .map_or(2, std::num::NonZero::get)
        .clamp(1, MAX_THREADS)
        .min(to_read.len().max(1));
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                let mut buf = Vec::new();
                let mut mine = Vec::new();
                loop {
                    let n = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&fi) = to_read.get(n) else { break };
                    if progress.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    mine.push((fi, check_file(manifest, package, fi, &mut buf, progress)));
                }
                if let Ok(mut r) = results.lock() {
                    r.extend(mine);
                }
            });
        }
    });
    if progress.stop.load(Ordering::Relaxed) {
        return None;
    }
    for (fi, (intact, good)) in results
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
    {
        out.intact[fi] = intact;
        for (id, place) in good {
            out.found.entry(id).or_insert(place);
        }
    }

    // What the previous install had where; a moved or shifted chunk is
    // found here even though the new layout does not line up with it.
    if let Some(prev) = previous {
        for file in &prev.manifest.files {
            let path = package.join(&file.path);
            for slot in prev.manifest.slots(file) {
                let id = prev.manifest.chunk(slot.chunk).id;
                if !out.found.contains_key(&id) {
                    out.claimed.entry(id).or_insert_with(|| Place {
                        path: path.clone(),
                        offset: slot.offset,
                    });
                }
            }
        }
    }
    Some(out)
}

/// Hashes one installed file slot by slot. Returns whether it is intact
/// and the chunks that are good.
fn check_file(
    manifest: &Manifest,
    package: &Path,
    fi: usize,
    buf: &mut Vec<u8>,
    progress: &Progress,
) -> (bool, Vec<(Hash, Place)>) {
    let file = &manifest.files[fi];
    let path = package.join(&file.path);
    let mut good = Vec::new();
    let Ok(handle) = std::fs::File::open(&path) else {
        return (false, good);
    };
    let len = handle.metadata().map_or(0, |m| m.len());
    let mut intact = len == file.size;
    for slot in manifest.slots(file) {
        if progress.stop.load(Ordering::Relaxed) {
            return (false, good);
        }
        let size = slot.size as usize;
        buf.resize(size, 0);
        let read = match read_at(&handle, buf, slot.offset) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "reading an installed file");
                return (false, good);
            }
        };
        progress.done.fetch_add(read as u64, Ordering::Relaxed);
        if read < size {
            intact = false;
            break;
        }
        let id = manifest.chunk(slot.chunk).id;
        if Hash::of(buf) == id {
            good.push((
                id,
                Place {
                    path: path.clone(),
                    offset: slot.offset,
                },
            ));
        } else {
            intact = false;
        }
    }
    (intact, good)
}

#[cfg(test)]
mod tests {
    use infinity_depot::{Chunk, Encoding, File, Pack};

    use super::*;

    pub(crate) fn manifest_of(files: &[(&str, &[&[u8]])]) -> Manifest {
        let mut chunks: Vec<Chunk> = Vec::new();
        let mut out = Vec::new();
        for (path, parts) in files {
            let mut idx = Vec::new();
            let mut size = 0;
            for part in *parts {
                idx.push(chunks.len() as u32);
                size += part.len() as u64;
                chunks.push(Chunk {
                    id: Hash::of(part),
                    size: part.len() as u32,
                    pack: 0,
                    offset: 0,
                    stored: part.len() as u32,
                    encoding: Encoding::Raw,
                });
            }
            out.push(File {
                path: (*path).into(),
                size,
                chunks: idx,
            });
        }
        Manifest {
            schema_version: 1,
            hash: "blake3".into(),
            product_id: "p".into(),
            version: "1".into(),
            packs: vec![Pack {
                id: Hash::of(b"p"),
                size: 1 << 30,
            }],
            chunks,
            files: out,
            dirs: vec![],
        }
    }

    #[test]
    fn finds_damage_down_to_the_chunk() {
        let tmp = tempfile::tempdir().unwrap();
        let m = manifest_of(&[
            ("a", &[b"aaaa", b"bbbb"]),
            ("b", &[b"cccc"]),
            ("missing", &[b"dddd"]),
        ]);
        std::fs::write(tmp.path().join("a"), b"aaaaXbbb").unwrap();
        std::fs::write(tmp.path().join("b"), b"cccc").unwrap();
        let progress = Progress::default();
        let scan = scan(&m, tmp.path(), None, true, &progress).unwrap();
        assert_eq!(scan.intact, vec![false, true, false]);
        assert!(scan.found.contains_key(&Hash::of(b"aaaa")));
        assert!(!scan.found.contains_key(&Hash::of(b"bbbb")));
        assert_eq!(
            scan.found[&Hash::of(b"cccc")],
            Place {
                path: tmp.path().join("b"),
                offset: 0
            }
        );
        assert_eq!(progress.done.load(Ordering::Relaxed), 12);
    }

    #[test]
    fn trusts_verified_unchanged_files_unless_full() {
        let tmp = tempfile::tempdir().unwrap();
        let m = manifest_of(&[("a", &[b"aaaa"])]);
        std::fs::write(tmp.path().join("a"), b"aaaa").unwrap();
        let state = InstalledState::new(m.clone(), Hash::of(b"m"), tmp.path());
        // Damage it without changing size or time: trusted, then caught.
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(tmp.path().join("a"))
            .unwrap();
        let mtime = f.metadata().unwrap().modified().unwrap();
        super::super::io::write_at(&f, b"X", 0).unwrap();
        f.set_modified(mtime).unwrap();
        drop(f);
        let p = Progress::default();
        assert_eq!(
            scan(&m, tmp.path(), Some(&state), false, &p)
                .unwrap()
                .intact,
            vec![true]
        );
        assert_eq!(
            scan(&m, tmp.path(), Some(&state), true, &p).unwrap().intact,
            vec![false]
        );
    }
}
