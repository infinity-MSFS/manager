//! Archive extraction into a staging folder.
//!
//! Every file is hashed while it is written, so checking the result against
//! the package manifest costs no second pass over the data. The zip
//! format's own CRC is checked as each entry is read to its end. Zip
//! entries are extracted in parallel; `.tar.zst` is a single stream and
//! lives in [`crate::tarzst`].

use std::collections::{BTreeMap, HashSet};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use infinity_ipc::catalog::ArchiveFormat;
use zip::ZipArchive;

use crate::error::{Error, IoContext, Result};
use crate::manifest::{FileDigest, Hashing, manifest_path};
use crate::space;

const MAX_WORKERS: usize = 8;
pub(crate) const BUFFER: usize = 1 << 20;

/// Shared between the extracting threads and the job reporting on them.
#[derive(Debug, Default)]
pub(crate) struct Progress {
    pub(crate) done: AtomicU64,
    pub(crate) total: AtomicU64,
    pub(crate) stop: AtomicBool,
}

struct Entry {
    index: usize,
    rel: String,
    size: u64,
}

/// Where an extraction writes.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Target<'a> {
    /// Receives the package contents.
    pub(crate) dest: &'a Path,
    /// Working room beside `dest`, for formats that need it.
    pub(crate) scratch: &'a Path,
    /// The unpacked size, when known up front (from the manifest).
    pub(crate) size_hint: Option<u64>,
}

/// Extracts `archive`, which the catalog says is `format`, so its package
/// contents land directly in `target.dest`. A single top-level folder
/// wrapping everything is unwrapped. Returns what was written, or `None`
/// when stopped. Blocking.
///
/// The declared format is checked against the file's leading bytes rather
/// than trusted or guessed: a mismatch is reported, never worked around.
pub(crate) fn extract(
    format: ArchiveFormat,
    archive: &Path,
    target: Target<'_>,
    progress: &Progress,
) -> Result<Option<BTreeMap<String, FileDigest>>> {
    if format == ArchiveFormat::Unknown {
        return Err(Error::Unsupported(
            "the package uses an archive format this version cannot install; update the manager"
                .into(),
        ));
    }
    let found = sniff(archive)?;
    if found != Some(format) {
        return Err(Error::Format(format!(
            "the catalog lists it as {} but the download is {}",
            format_name(Some(format)),
            format_name(found),
        )));
    }
    match format {
        ArchiveFormat::Zip => extract_zip(archive, target.dest, progress),
        ArchiveFormat::TarZst => crate::tarzst::extract(archive, target, progress),
        ArchiveFormat::Unknown => Err(Error::Internal("unreachable archive format".into())),
    }
}

/// The format a file's magic bytes announce.
fn sniff(archive: &Path) -> Result<Option<ArchiveFormat>> {
    let mut head = [0u8; 4];
    let mut file = std::fs::File::open(archive).at(archive)?;
    let mut filled = 0;
    while filled < head.len() {
        match file.read(&mut head[filled..]).at(archive)? {
            0 => break,
            n => filled += n,
        }
    }
    let head = &head[..filled];
    Ok(if matches!(head, [b'P', b'K', 3, 4] | [b'P', b'K', 5, 6]) {
        Some(ArchiveFormat::Zip)
    } else if crate::tarzst::is_zstd(head) {
        Some(ArchiveFormat::TarZst)
    } else {
        None
    })
}

fn format_name(format: Option<ArchiveFormat>) -> &'static str {
    match format {
        Some(ArchiveFormat::Zip) => "a zip",
        Some(ArchiveFormat::TarZst) => "a tar.zst",
        Some(ArchiveFormat::Unknown) | None => "neither zip nor tar.zst",
    }
}

fn extract_zip(
    archive: &Path,
    dest: &Path,
    progress: &Progress,
) -> Result<Option<BTreeMap<String, FileDigest>>> {
    let (dirs, files) = plan(archive)?;
    let total = files.iter().map(|f| f.size).sum();
    progress.total.store(total, Ordering::Relaxed);
    space::ensure(dest, total, "extract this package")?;

    std::fs::create_dir_all(dest).at(dest)?;
    let parents = files
        .iter()
        .filter_map(|f| f.rel.rsplit_once('/').map(|(d, _)| d));
    for dir in dirs.iter().map(String::as_str).chain(parents) {
        let path = dest.join(dir);
        std::fs::create_dir_all(&path).at(&path)?;
    }

    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let written = Mutex::new(BTreeMap::new());
    let first_error = Mutex::new(None);
    let workers = std::thread::available_parallelism()
        .map_or(2, usize::from)
        .clamp(1, MAX_WORKERS)
        .min(files.len().max(1));

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                let result = worker(archive, dest, &files, &next, progress, &failed, &written);
                if let Err(e) = result {
                    failed.store(true, Ordering::Relaxed);
                    if let Ok(mut slot) = first_error.lock() {
                        slot.get_or_insert(e);
                    }
                }
            });
        }
    });

    if let Some(e) = first_error.into_inner().ok().flatten() {
        return Err(e);
    }
    if progress.stop.load(Ordering::Relaxed) {
        return Ok(None);
    }
    written
        .into_inner()
        .map(Some)
        .map_err(|_| Error::Internal("extraction results lost".into()))
}

fn worker(
    archive: &Path,
    dest: &Path,
    files: &[Entry],
    next: &AtomicUsize,
    progress: &Progress,
    failed: &AtomicBool,
    written: &Mutex<BTreeMap<String, FileDigest>>,
) -> Result<()> {
    let file = std::fs::File::open(archive).at(archive)?;
    let mut zip = ZipArchive::new(BufReader::new(file))?;
    let mut buf = vec![0; BUFFER];
    loop {
        let i = next.fetch_add(1, Ordering::Relaxed);
        let Some(entry) = files.get(i) else {
            return Ok(());
        };
        if failed.load(Ordering::Relaxed) || progress.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let path = dest.join(&entry.rel);
        let mut reader = zip.by_index(entry.index)?;
        let out = std::fs::File::create(&path).at(&path)?;
        let mut out = Hashing::new(BufWriter::with_capacity(BUFFER, out));
        loop {
            if progress.stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let n = reader
                .read(&mut buf)
                .map_err(|e| Error::Archive(format!("{} could not be read: {e}", entry.rel)))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).at(&path)?;
            progress.done.fetch_add(n as u64, Ordering::Relaxed);
        }
        let (out, digest) = out.into_parts();
        out.into_inner()
            .map_err(|e| e.into_error())
            .and_then(|f| f.sync_all())
            .at(&path)?;
        if digest.size != entry.size {
            return Err(Error::Archive(format!(
                "{} is {} bytes, the archive says {}",
                entry.rel, digest.size, entry.size
            )));
        }
        written
            .lock()
            .map_err(|_| Error::Internal("extraction results lost".into()))?
            .insert(entry.rel.clone(), digest);
    }
}

/// Reads the central directory: the folders and files to create, with
/// every path proven to stay inside the destination. Largest files come
/// first so the slowest entry starts early.
fn plan(archive: &Path) -> Result<(Vec<String>, Vec<Entry>)> {
    let file = std::fs::File::open(archive).at(archive)?;
    let mut zip = ZipArchive::new(BufReader::new(file))?;
    let mut entries = Vec::with_capacity(zip.len());
    for index in 0..zip.len() {
        let entry = zip.by_index_raw(index)?;
        let unsafe_path =
            || Error::Archive(format!("{:?} would land outside the package", entry.name()));
        if entry.is_symlink() {
            return Err(Error::Archive(format!("{:?} is a link", entry.name())));
        }
        let rel = entry
            .enclosed_name()
            .as_deref()
            .and_then(manifest_path)
            .ok_or_else(unsafe_path)?;
        entries.push((index, rel, entry.is_dir(), entry.size()));
    }

    let wrapper = single_wrapper(
        entries
            .iter()
            .map(|(_, rel, is_dir, _)| (rel.as_str(), *is_dir)),
    );
    let strip = |rel: &str| -> Option<String> {
        match &wrapper {
            Some(w) => rel
                .strip_prefix(w.as_str())?
                .strip_prefix('/')
                .map(str::to_owned),
            None => Some(rel.to_owned()),
        }
    };

    let mut seen = HashSet::new();
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for (index, rel, is_dir, size) in entries {
        let Some(rel) = strip(&rel) else { continue };
        if is_dir {
            dirs.push(rel);
            continue;
        }
        // Windows, and the sim under Wine, cannot hold both.
        if !seen.insert(rel.to_lowercase()) {
            return Err(Error::Archive(format!(
                "{rel} appears twice, ignoring case"
            )));
        }
        files.push(Entry { index, rel, size });
    }
    if files.is_empty() {
        return Err(Error::Archive("the archive holds no files".into()));
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.size));
    Ok((dirs, files))
}

/// The folder wrapping every entry, when the archive has exactly one at the
/// top and no loose top-level files.
pub(crate) fn single_wrapper<'a>(entries: impl Iterator<Item = (&'a str, bool)>) -> Option<String> {
    let mut top: Option<&str> = None;
    for (rel, is_dir) in entries {
        let (head, rest) = rel.split_once('/').unwrap_or((rel, ""));
        if rest.is_empty() && !is_dir {
            return None;
        }
        match top {
            None => top = Some(head),
            Some(t) if t == head => {}
            Some(_) => return None,
        }
    }
    top.map(str::to_owned)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;

    use zip::write::SimpleFileOptions;

    use super::*;

    /// Writes a zip of `(name, contents)`; a name ending in `/` is a folder.
    pub(crate) fn zip_of(path: &Path, entries: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let opts =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in entries {
            if name.ends_with('/') {
                zip.add_directory(*name, opts).unwrap();
            } else {
                zip.start_file(*name, opts).unwrap();
                zip.write_all(data).unwrap();
            }
        }
        zip.finish().unwrap();
    }

    fn run(entries: &[(&str, &[u8])]) -> (tempfile::TempDir, Result<BTreeMap<String, FileDigest>>) {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.zip");
        zip_of(&archive, entries);
        let out = extract_zip(&archive, &tmp.path().join("out"), &Progress::default())
            .map(|o| o.unwrap());
        (tmp, out)
    }

    #[test]
    fn unwraps_a_single_top_folder_and_hashes_files() {
        let big = vec![7u8; 3 << 20];
        let (tmp, out) = run(&[
            ("pkg/", b""),
            ("pkg/manifest.json", b"hello"),
            ("pkg/SimObjects/a.bin", &big),
            ("pkg/empty/", b""),
        ]);
        let out = out.unwrap();
        assert_eq!(
            out.keys().collect::<Vec<_>>(),
            vec!["SimObjects/a.bin", "manifest.json"]
        );
        assert_eq!(out["SimObjects/a.bin"].size, big.len() as u64);
        let root = tmp.path().join("out");
        assert_eq!(std::fs::read(root.join("manifest.json")).unwrap(), b"hello");
        assert!(root.join("empty").is_dir());
    }

    #[test]
    fn keeps_flat_archives_flat() {
        let (tmp, out) = run(&[("manifest.json", b"m"), ("SimObjects/a.cfg", b"a")]);
        assert_eq!(out.unwrap().len(), 2);
        assert!(tmp.path().join("out/SimObjects/a.cfg").is_file());
    }

    #[test]
    fn rejects_traversal_and_case_collisions() {
        let (_tmp, out) = run(&[("../evil.txt", b"x")]);
        assert!(matches!(out, Err(Error::Archive(_))));
        let (_tmp, out) = run(&[("a/File.txt", b"x"), ("a/file.TXT", b"y")]);
        assert!(matches!(out, Err(Error::Archive(_))));
    }

    #[test]
    fn detects_corrupt_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.zip");
        let mut x: u32 = 1;
        let noise: Vec<u8> = (0..64 << 10)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                (x >> 16) as u8
            })
            .collect();
        zip_of(&archive, &[("data.bin", &noise)]);
        let mut bytes = std::fs::read(&archive).unwrap();
        // Flip a byte inside the stored data, past the local header.
        bytes[200] ^= 0xff;
        std::fs::write(&archive, bytes).unwrap();
        let out = extract_zip(&archive, &tmp.path().join("out"), &Progress::default());
        assert!(matches!(out, Err(Error::Archive(_))), "{out:?}");
    }

    #[test]
    fn declared_format_must_match_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.zip");
        zip_of(&archive, &[("manifest.json", b"m")]);
        let target = Target {
            dest: &tmp.path().join("out"),
            scratch: &tmp.path().join("scratch"),
            size_hint: None,
        };
        let out = extract(
            ArchiveFormat::TarZst,
            &archive,
            target,
            &Progress::default(),
        );
        assert!(matches!(out, Err(Error::Format(_))), "{out:?}");
        let out = extract(
            ArchiveFormat::Unknown,
            &archive,
            target,
            &Progress::default(),
        );
        assert!(matches!(out, Err(Error::Unsupported(_))), "{out:?}");
        std::fs::write(&archive, b"Rar!").unwrap();
        let out = extract(ArchiveFormat::Zip, &archive, target, &Progress::default());
        assert!(matches!(out, Err(Error::Format(_))), "{out:?}");
        assert!(!tmp.path().join("out").exists());
    }

    #[test]
    fn wrapper_detection() {
        let w = |e: &[(&str, bool)]| single_wrapper(e.iter().copied());
        assert_eq!(w(&[("p/", true), ("p/a", false)]), Some("p".into()));
        assert_eq!(w(&[("p/a", false), ("q/a", false)]), None);
        assert_eq!(w(&[("p/a", false), ("loose", false)]), None);
    }
}
