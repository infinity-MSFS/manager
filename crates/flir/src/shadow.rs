//! Private copies of the viewports DLLs for injection.
//!
//! A DLL loaded into the sim is locked on disk until the sim lets go of it.
//! Injecting the originals would lock the installed copies (a manager update
//! could not replace them) and a developer's build output (`cargo build`
//! in infinity-viewports would fail while flying). So the engine injects a
//! copy under `<app data>/flir/inject/<content hash>/`, one folder per
//! build: the same bytes reuse a folder, a new build gets a new one, and the
//! loaded module's path says exactly which build is in the sim.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sha2::{Digest, Sha256};

pub fn root(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("flir").join("inject")
}

/// Cheap fingerprint of a file on disk, to notice a rebuild without
/// reading it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

pub fn stamp(path: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(path).ok()?;
    Some(Stamp {
        len: m.len(),
        modified: m.modified().ok(),
    })
}

/// Copies `src` under `root` (if that build is not there already) and
/// returns the copy's path. The error is a sentence for the panel.
pub fn stage(src: &Path, root: &Path) -> Result<PathBuf, String> {
    let name = src
        .file_name()
        .ok_or_else(|| format!("{} is not a file", src.display()))?;
    let bytes = std::fs::read(src).map_err(|e| format!("read {}: {e}", src.display()))?;
    let digest = Sha256::digest(&bytes);
    let key: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    let dir = root.join(key);
    let dst = dir.join(name);
    let current = std::fs::metadata(&dst).is_ok_and(|m| m.len() == bytes.len() as u64);
    if !current {
        std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        // Written aside and renamed, so a half-written copy is never what
        // gets injected.
        let tmp = dir.join(format!("{}.part", name.to_string_lossy()));
        std::fs::write(&tmp, &bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &dst).map_err(|e| format!("stage {}: {e}", dst.display()))?;
    }
    Ok(dst)
}

/// Deletes the builds under `root` other than the folders of `keep`. One
/// still loaded in a sim is locked and stays until a later sweep.
pub fn sweep(root: &Path, keep: &[&Path]) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() || keep.iter().any(|k| k.parent() == Some(dir.as_path())) {
            continue;
        }
        if std::fs::remove_dir_all(&dir).is_ok() {
            tracing::debug!(dir = %dir.display(), "flir: removed an old injected build");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_is_staged_once_and_a_new_build_beside_it() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("infinity_viewports_core.dll");
        let root = dir.path().join("inject");
        std::fs::write(&src, b"MZ build one").unwrap();
        let one = stage(&src, &root).unwrap();
        assert_eq!(stage(&src, &root).unwrap(), one, "same bytes, same copy");
        assert_eq!(one.file_name(), src.file_name());
        assert_eq!(std::fs::read(&one).unwrap(), b"MZ build one");

        std::fs::write(&src, b"MZ build two!").unwrap();
        let two = stage(&src, &root).unwrap();
        assert_ne!(two, one);
        assert!(one.exists(), "the old build stays until swept");

        sweep(&root, &[&two]);
        assert!(!one.exists());
        assert!(two.exists());
    }

    #[test]
    fn a_rebuild_changes_the_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.dll");
        std::fs::write(&src, b"one").unwrap();
        let before = stamp(&src).unwrap();
        std::fs::write(&src, b"three").unwrap();
        assert_ne!(stamp(&src), Some(before));
        assert_eq!(stamp(&dir.path().join("missing.dll")), None);
    }
}
