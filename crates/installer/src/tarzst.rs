//! `.tar.zst` extraction into a staging folder.
//!
//! Same guarantees as the zip path: every entry's path is proven to stay
//! inside the package, links and device nodes are refused, and each file is
//! hashed as it is written. Three things differ because a tarball is one
//! compressed stream rather than an indexed archive:
//!
//! - It is decompressed and unpacked in a single pass on one thread.
//! - Whether one top-level folder wraps everything is only known at the
//!   end, so entries are written under a scratch folder and the package is
//!   renamed into `dest` afterwards (a rename on the same volume, so free).
//! - The unpacked size is not declared up front, so progress counts
//!   compressed bytes consumed against the archive's size.
//!
//! The tar crate's own `unpack` is deliberately not used: it would apply
//! permissions and timestamps from the archive and create links, none of
//! which a sim package needs.

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::sync::atomic::Ordering;

use tar::EntryType;

use crate::error::{Error, IoContext, Result};
use crate::extract::{BUFFER, Progress, Target, single_wrapper};
use crate::manifest::{FileDigest, Hashing, manifest_path};
use crate::{space, staging};

/// Frame magic, little-endian `0xFD2FB528`.
const MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// Accepts archives made with `zstd --long=31`. The default (27, 128 MiB)
/// would reject them; the window is only allocated as large as the frame
/// asks for, and a manifest-backed archive has passed its SHA-256 check
/// before it is decompressed.
const WINDOW_LOG_MAX: u32 = 31;

/// Whether `head` starts a zstd stream: a frame, or a skippable frame
/// (`0x184D2A50..=0x184D2A5F`) such as the one `pzstd` writes first.
pub(crate) fn is_zstd(head: &[u8]) -> bool {
    match head {
        [a, b, c, d] if [*a, *b, *c, *d] == MAGIC => true,
        [a, 0x2a, 0x4d, 0x18] => a & 0xf0 == 0x50,
        _ => false,
    }
}

/// Extracts a `.tar.zst` into `target.dest`. Returns what was written, or
/// `None` when stopped. Blocking.
pub(crate) fn extract(
    archive: &Path,
    target: Target<'_>,
    progress: &Progress,
) -> Result<Option<BTreeMap<String, FileDigest>>> {
    let compressed = std::fs::metadata(archive).at(archive)?.len();
    progress.total.store(compressed, Ordering::Relaxed);
    // Without a manifest the archive size is the best lower bound there is;
    // a drive that fills anyway is reported by the write that fails.
    space::ensure(
        target.dest,
        target.size_hint.unwrap_or(compressed),
        "extract this package",
    )?;

    let scratch = target.scratch;
    staging::remove_dir(scratch);
    std::fs::create_dir_all(scratch).at(scratch)?;
    let Some(Unpacked { listing, written }) = unpack(archive, scratch, progress)? else {
        return Ok(None);
    };
    if written.is_empty() {
        return Err(Error::Archive("the archive holds no files".into()));
    }

    let wrapper = single_wrapper(listing.iter().map(|(rel, is_dir)| (rel.as_str(), *is_dir)));
    let package = match &wrapper {
        Some(w) => scratch.join(w),
        None => scratch.to_path_buf(),
    };
    staging::remove_dir(target.dest);
    std::fs::rename(&package, target.dest).at(&package)?;
    let Some(wrapper) = wrapper else {
        return Ok(Some(written));
    };
    staging::remove_dir(scratch);
    let prefix = format!("{wrapper}/");
    written
        .into_iter()
        .map(|(rel, digest)| match rel.strip_prefix(&prefix) {
            Some(inner) => Ok((inner.to_owned(), digest)),
            None => Err(Error::Internal(format!("{rel} is outside {wrapper}"))),
        })
        .collect::<Result<_>>()
        .map(Some)
}

struct Unpacked {
    /// Every entry, folders included, for spotting a wrapper folder.
    listing: Vec<(String, bool)>,
    written: BTreeMap<String, FileDigest>,
}

fn unpack(archive: &Path, root: &Path, progress: &Progress) -> Result<Option<Unpacked>> {
    let file = File::open(archive).at(archive)?;
    let mut decoder = zstd::stream::read::Decoder::new(Counted {
        inner: file,
        progress,
    })
    .at(archive)?;
    decoder.window_log_max(WINDOW_LOG_MAX).at(archive)?;
    let mut tar = tar::Archive::new(decoder);

    let mut listing = Vec::new();
    let mut written = BTreeMap::new();
    let mut seen = HashSet::new();
    let mut buf = vec![0; BUFFER];
    for entry in tar.entries().map_err(unreadable)? {
        if progress.stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let mut entry = entry.map_err(unreadable)?;
        let name = std::str::from_utf8(&entry.path_bytes())
            .map_err(|_| Error::Archive("an entry's name is not UTF-8".into()))?
            .to_owned();
        let is_dir = match entry.header().entry_type() {
            EntryType::Regular | EntryType::Continuous => false,
            EntryType::Directory => true,
            // Archive-wide pax notes (the tar crate folds per-entry pax
            // and GNU long-name headers into the entry they describe).
            EntryType::XGlobalHeader => continue,
            other => {
                return Err(Error::Archive(format!(
                    "{name:?} is {}, which a package may not contain",
                    describe(other)
                )));
            }
        };
        let rel = safe_path(&name)
            .ok_or_else(|| Error::Archive(format!("{name:?} would land outside the package")))?;
        let path = root.join(&rel);
        if is_dir {
            std::fs::create_dir_all(&path).at(&path)?;
            listing.push((rel, true));
            continue;
        }
        // Windows, and the sim under Wine, cannot hold both. This also
        // refuses tar's "a later copy replaces the earlier one".
        if !seen.insert(rel.to_lowercase()) {
            return Err(Error::Archive(format!(
                "{rel} appears twice, ignoring case"
            )));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).at(parent)?;
        }
        let expected = entry.size();
        // `create_new`: nothing under the fresh scratch folder may already
        // be there, so never open (or follow) an existing file.
        let out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .at(&path)?;
        let mut out = Hashing::new(BufWriter::with_capacity(BUFFER, out));
        loop {
            if progress.stop.load(Ordering::Relaxed) {
                return Ok(None);
            }
            let n = entry
                .read(&mut buf)
                .map_err(|e| Error::Archive(format!("{rel} could not be read: {e}")))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).at(&path)?;
        }
        let (out, digest) = out.into_parts();
        out.into_inner()
            .map_err(|e| e.into_error())
            .and_then(|f| f.sync_all())
            .at(&path)?;
        if digest.size != expected {
            return Err(Error::Archive(format!(
                "{rel} is {} bytes, the archive says {expected}",
                digest.size
            )));
        }
        written.insert(rel.clone(), digest);
        listing.push((rel, false));
    }
    Ok(Some(Unpacked { listing, written }))
}

/// A tar entry name as a `/`-separated path inside the package, or `None`
/// if it is absolute, climbs out, or could mean something else on Windows
/// (a backslash, a drive or stream colon, a trailing dot or space, which
/// Windows silently drops so two names would land on one file).
fn safe_path(name: &str) -> Option<String> {
    if name.starts_with('/') || name.contains(['\\', ':', '\0']) {
        return None;
    }
    let parts: Vec<&str> = name
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    if parts
        .iter()
        .any(|p| *p == ".." || p.ends_with('.') || p.ends_with(' ') || is_device_name(p))
    {
        return None;
    }
    // The platform's own parser gets the last word.
    manifest_path(Path::new(&parts.join("/")))
}

/// `NUL`, `COM1.txt` and the like open a device on Windows instead of
/// creating a file, whatever folder they are in.
fn is_device_name(part: &str) -> bool {
    let base = part.split('.').next().unwrap_or(part).trim_end();
    let upper = base.to_ascii_uppercase();
    let numbered = |prefix: &str| {
        upper
            .strip_prefix(prefix)
            .is_some_and(|n| matches!(n.as_bytes(), [b'1'..=b'9']))
    };
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL") || numbered("COM") || numbered("LPT")
}

fn describe(kind: EntryType) -> &'static str {
    match kind {
        EntryType::Symlink => "a symbolic link",
        EntryType::Link => "a hard link",
        EntryType::Char | EntryType::Block => "a device",
        EntryType::Fifo => "a pipe",
        EntryType::GNUSparse => "a sparse file",
        _ => "an unknown kind of entry",
    }
}

/// Read failures inside the stream mean a damaged archive.
fn unreadable(e: std::io::Error) -> Error {
    Error::Archive(format!("it could not be read: {e}"))
}

/// Counts compressed bytes as the decoder pulls them from the file.
struct Counted<'a> {
    inner: File,
    progress: &'a Progress,
}

impl Read for Counted<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.progress.done.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// One tar entry for a fixture: its raw name, kind and contents.
    pub(crate) struct Raw<'a> {
        pub(crate) name: &'a [u8],
        pub(crate) kind: EntryType,
        pub(crate) data: &'a [u8],
    }

    pub(crate) fn file<'a>(name: &'a str, data: &'a [u8]) -> Raw<'a> {
        Raw {
            name: name.as_bytes(),
            kind: if name.ends_with('/') {
                EntryType::Directory
            } else {
                EntryType::Regular
            },
            data,
        }
    }

    /// Writes a `.tar.zst` of `entries`. Names go into the header
    /// untouched, so fixtures can hold what no honest tool would write.
    pub(crate) fn tar_zst_of(path: &Path, entries: &[Raw<'_>]) {
        let out = File::create(path).unwrap();
        let encoder = zstd::stream::write::Encoder::new(out, 3).unwrap();
        let mut tar = tar::Builder::new(encoder);
        for entry in entries {
            let mut header = tar::Header::new_ustar();
            let name = &mut header.as_old_mut().name;
            name[..entry.name.len()].copy_from_slice(entry.name);
            header.set_entry_type(entry.kind);
            header.set_size(entry.data.len() as u64);
            header.set_mode(0o644);
            if matches!(entry.kind, EntryType::Symlink | EntryType::Link) {
                header.set_link_name_literal(b"../../etc/passwd").unwrap();
            }
            header.set_cksum();
            tar.append(&header, entry.data).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
    }

    fn run(entries: &[Raw<'_>]) -> (tempfile::TempDir, Result<BTreeMap<String, FileDigest>>) {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.tar.zst");
        tar_zst_of(&archive, entries);
        let target = Target {
            dest: &tmp.path().join("out"),
            scratch: &tmp.path().join("scratch"),
            size_hint: None,
        };
        let out = crate::extract::extract(
            infinity_ipc::catalog::ArchiveFormat::TarZst,
            &archive,
            target,
            &Progress::default(),
        )
        .map(|o| o.unwrap());
        (tmp, out)
    }

    fn rejected(entries: &[Raw<'_>]) -> String {
        let (tmp, out) = run(entries);
        assert!(!tmp.path().join("out").exists(), "nothing reaches dest");
        assert!(!tmp.path().join("etc").exists());
        match out {
            Err(Error::Archive(m)) => m,
            other => panic!("expected an archive error, got {other:?}"),
        }
    }

    #[test]
    fn unwraps_a_single_top_folder_and_hashes_files() {
        let big = vec![7u8; 3 << 20];
        let (tmp, out) = run(&[
            file("pkg/", b""),
            file("pkg/manifest.json", b"hello"),
            file("pkg/SimObjects/a.bin", &big),
            file("pkg/empty/", b""),
        ]);
        let out = out.unwrap();
        assert_eq!(
            out.keys().collect::<Vec<_>>(),
            vec!["SimObjects/a.bin", "manifest.json"]
        );
        assert_eq!(out["SimObjects/a.bin"].size, big.len() as u64);
        assert_eq!(
            out["manifest.json"].sha256,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        let root = tmp.path().join("out");
        assert_eq!(std::fs::read(root.join("manifest.json")).unwrap(), b"hello");
        assert!(root.join("empty").is_dir());
        assert!(!tmp.path().join("scratch").exists());
    }

    #[test]
    fn keeps_flat_archives_flat() {
        let (tmp, out) = run(&[
            file("./manifest.json", b"m"),
            file("SimObjects/a.cfg", b"a"),
        ]);
        assert_eq!(out.unwrap().len(), 2);
        assert!(tmp.path().join("out/SimObjects/a.cfg").is_file());
    }

    #[test]
    fn rejects_paths_that_escape() {
        for name in [
            "../evil.txt",
            "pkg/../../evil.txt",
            "/etc/evil.txt",
            "C:/evil.txt",
            "pkg\\..\\evil.txt",
            "pkg/trailing.",
        ] {
            let m = rejected(&[file("pkg/ok.txt", b"ok"), file(name, b"x")]);
            assert!(m.contains("outside the package"), "{name}: {m}");
        }
    }

    #[test]
    fn rejects_links_and_devices() {
        for kind in [
            EntryType::Symlink,
            EntryType::Link,
            EntryType::Char,
            EntryType::Block,
            EntryType::Fifo,
        ] {
            let m = rejected(&[
                file("pkg/ok.txt", b"ok"),
                Raw {
                    name: b"pkg/link",
                    kind,
                    data: b"",
                },
            ]);
            assert!(m.contains("may not contain"), "{kind:?}: {m}");
        }
    }

    #[test]
    fn rejects_duplicates_and_empty_archives() {
        let m = rejected(&[file("a/File.txt", b"x"), file("a/file.TXT", b"y")]);
        assert!(m.contains("twice"), "{m}");
        let m = rejected(&[file("a/File.txt", b"x"), file("a/File.txt", b"y")]);
        assert!(m.contains("twice"), "{m}");
        let m = rejected(&[file("pkg/", b"")]);
        assert!(m.contains("no files"), "{m}");
    }

    #[test]
    fn detects_a_truncated_stream() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.tar.zst");
        let mut x: u32 = 1;
        let noise: Vec<u8> = (0..256 << 10)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                (x >> 16) as u8
            })
            .collect();
        tar_zst_of(&archive, &[file("pkg/data.bin", &noise)]);
        let bytes = std::fs::read(&archive).unwrap();
        std::fs::write(&archive, &bytes[..bytes.len() / 2]).unwrap();
        let target = Target {
            dest: &tmp.path().join("out"),
            scratch: &tmp.path().join("scratch"),
            size_hint: None,
        };
        let out = extract(&archive, target, &Progress::default());
        assert!(matches!(out, Err(Error::Archive(_))), "{out:?}");
        assert!(!tmp.path().join("out").exists());
    }

    #[test]
    fn stops_when_asked() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.tar.zst");
        tar_zst_of(&archive, &[file("pkg/a.bin", &[1; 4096])]);
        let progress = Progress::default();
        progress.stop.store(true, Ordering::Relaxed);
        let target = Target {
            dest: &tmp.path().join("out"),
            scratch: &tmp.path().join("scratch"),
            size_hint: None,
        };
        assert!(extract(&archive, target, &progress).unwrap().is_none());
        assert!(!tmp.path().join("out").exists());
    }

    #[test]
    fn recognises_zstd_magic() {
        assert!(is_zstd(&MAGIC));
        assert!(is_zstd(&[0x50, 0x2a, 0x4d, 0x18]));
        assert!(!is_zstd(b"PK\x03\x04"));
        assert!(!is_zstd(&MAGIC[..2]));
    }

    #[test]
    fn safe_paths() {
        assert_eq!(safe_path("./a//b/").as_deref(), Some("a/b"));
        assert_eq!(safe_path("a/./b").as_deref(), Some("a/b"));
        assert_eq!(safe_path("COM10/console").as_deref(), Some("COM10/console"));
        for bad in [
            "",
            ".",
            "..",
            "a/..",
            "/a",
            "a:b",
            "a\\b",
            "a/b ",
            "pkg/NUL",
            "pkg/com1.txt",
            "aux/a",
        ] {
            assert_eq!(safe_path(bad), None, "{bad}");
        }
    }
}
