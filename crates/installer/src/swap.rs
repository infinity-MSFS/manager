//! Putting a staged package in place, and taking one away.
//!
//! The installed package is never deleted before its replacement is in
//! place: it is renamed aside, the new one renamed in, and only then is
//! the old one removed. A failure at any point leaves either the old
//! package or the new one installed, never half of each.

use std::path::Path;
use std::time::Duration;

use crate::error::{IoContext, Result};
use crate::staging::{Staging, remove_dir};

/// Antivirus scanners briefly hold freshly written files open, which makes
/// a rename fail with "access denied" for a moment.
const RENAME_ATTEMPTS: u32 = 5;
const RENAME_BACKOFF: Duration = Duration::from_millis(200);

/// Replaces `target` with `staged`. Blocking.
pub(crate) fn install(staged: &Path, target: &Path, staging: &Staging) -> Result<()> {
    let previous = staging.previous();
    remove_dir(&previous);
    let replacing = target.exists();
    if replacing {
        rename(target, &previous)?;
    }
    if let Err(e) = rename(staged, target) {
        if replacing && let Err(back) = rename(&previous, target) {
            tracing::error!(
                target = %target.display(),
                kept_at = %previous.display(),
                error = %back,
                "could not restore the previous package"
            );
        }
        return Err(e);
    }
    if replacing {
        remove_dir(&previous);
    }
    Ok(())
}

/// Removes an installed package: renamed out of the sim's sight first, so
/// a delete interrupted by a locked file leaves nothing half-removed in
/// the install folder. Blocking.
pub(crate) fn remove(package: &Path, staging: &Staging) -> Result<()> {
    let trash = staging.trash();
    remove_dir(&trash);
    match rename(package, &trash) {
        Ok(()) => {}
        Err(crate::error::Error::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            return Ok(());
        }
        Err(e) => return Err(e),
    }
    remove_dir(&trash);
    Ok(())
}

fn rename(from: &Path, to: &Path) -> Result<()> {
    let mut attempt = 1;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e)
                if e.kind() == std::io::ErrorKind::PermissionDenied
                    && attempt < RENAME_ATTEMPTS =>
            {
                std::thread::sleep(RENAME_BACKOFF * attempt);
                attempt += 1;
            }
            Err(e) => return Err(e).at(from),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn setup() -> (tempfile::TempDir, Staging, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let community = tmp.path().join("Community");
        fs::create_dir_all(&community).unwrap();
        let staging = Staging::create(&community, "pkg").unwrap();
        (tmp, staging, community.join("pkg"))
    }

    #[test]
    fn replaces_an_existing_package() {
        let (_tmp, staging, target) = setup();
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("old.txt"), "old").unwrap();
        let staged = staging.extract();
        fs::create_dir_all(&staged).unwrap();
        fs::write(staged.join("new.txt"), "new").unwrap();

        install(&staged, &target, &staging).unwrap();
        assert!(target.join("new.txt").is_file());
        assert!(!target.join("old.txt").exists());
        assert!(!staging.previous().exists());
        assert!(!staged.exists());
    }

    #[test]
    fn failed_swap_keeps_the_old_package() {
        let (_tmp, staging, target) = setup();
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("old.txt"), "old").unwrap();
        // Nothing staged: the second rename fails.
        assert!(install(&staging.extract(), &target, &staging).is_err());
        assert_eq!(fs::read_to_string(target.join("old.txt")).unwrap(), "old");
    }

    #[test]
    fn remove_is_idempotent() {
        let (_tmp, staging, target) = setup();
        fs::create_dir_all(target.join("a")).unwrap();
        remove(&target, &staging).unwrap();
        assert!(!target.exists());
        remove(&target, &staging).unwrap();
    }
}
