//! Free-space checks, so a full drive is reported by name and size before
//! anything is written, rather than as `os error 112` halfway through.

use std::path::{Component, Path};

use crate::error::{Error, Result};

/// Left free so the sim still has room for its own caches afterwards.
const HEADROOM: u64 = 64 * 1024 * 1024;

/// Fails when the volume holding `path` has less than `needed` bytes free.
/// An unmeasurable volume passes: the real write error is reported instead.
pub(crate) fn ensure(path: &Path, needed: u64, what: &'static str) -> Result<()> {
    let Some(available) = available(path) else {
        return Ok(());
    };
    let needed = needed.saturating_add(HEADROOM);
    if available >= needed {
        return Ok(());
    }
    Err(Error::DiskFull {
        path: path.to_path_buf(),
        what,
        needed,
        available,
    })
}

/// Free bytes on the volume of `path`'s nearest existing ancestor.
fn available(path: &Path) -> Option<u64> {
    let existing = path.ancestors().find(|p| p.exists())?;
    fs4::available_space(existing).ok()
}

/// `D:\` on Windows; the path itself elsewhere. For messages only.
pub(crate) fn volume_label(path: &Path) -> String {
    match path.components().next() {
        Some(Component::Prefix(prefix)) => {
            format!("{}\\", prefix.as_os_str().to_string_lossy())
        }
        _ => path.display().to_string(),
    }
}

/// The volume filled up mid-write. `ErrorKind::StorageFull` is unstable, so
/// match the OS codes.
pub(crate) fn is_disk_full(e: &std::io::Error) -> bool {
    let codes: &[i32] = if cfg!(windows) { &[39, 112] } else { &[28] };
    e.raw_os_error().is_some_and(|c| codes.contains(&c))
}

#[allow(clippy::cast_precision_loss)]
pub(crate) fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(3 << 30), "3.0 GB");
    }

    #[test]
    fn checks_the_nearest_existing_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("a/b/c");
        assert!(ensure(&missing, 0, "test").is_ok());
        assert!(matches!(
            ensure(&missing, u64::MAX / 2, "test"),
            Err(Error::DiskFull { .. })
        ));
    }
}
