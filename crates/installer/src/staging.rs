//! Per-job working files.
//!
//! Everything a job writes lives in a `.infinity-staging` folder beside the
//! install folder, on the same volume. That keeps the final swap a rename
//! (atomic, instant) instead of a copy that can fail halfway, and puts the
//! large download on the drive the user chose rather than on `C:`.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{IoContext, Result};

const DIR_NAME: &str = ".infinity-staging";
const STATE_SUFFIX: &str = ".download.json";
const MAX_STEM: usize = 40;

#[derive(Debug, Clone)]
pub(crate) struct Staging {
    root: PathBuf,
    stem: String,
}

impl Staging {
    /// Creates the staging folder for a product, preferring the install
    /// folder's parent so the sim never sees it. Falls back to inside the
    /// install folder when the parent is not writable: the sim only loads
    /// folders with a `manifest.json` directly inside, which this lacks.
    pub(crate) fn create(install_dir: &Path, product_id: &str) -> Result<Self> {
        let inside = install_dir.join(DIR_NAME);
        let beside = install_dir.parent().map(|p| p.join(DIR_NAME));
        let root = match beside {
            Some(beside) if std::fs::create_dir_all(&beside).is_ok() => beside,
            _ => {
                std::fs::create_dir_all(&inside).at(&inside)?;
                inside
            }
        };
        Ok(Self {
            root,
            stem: stem(product_id),
        })
    }

    /// The staging folders [`Staging::create`] may have chosen for
    /// `install_dir`, beside it first.
    pub(crate) fn roots(install_dir: &Path) -> Vec<PathBuf> {
        let beside = install_dir.parent().map(|p| p.join(DIR_NAME));
        beside
            .into_iter()
            .chain([install_dir.join(DIR_NAME)])
            .collect()
    }

    /// A product's staging in an existing folder, without creating it.
    pub(crate) fn existing(root: &Path, product_id: &str) -> Self {
        Self {
            root: root.to_path_buf(),
            stem: stem(product_id),
        }
    }

    /// The stems of the download state files in `root`, with their paths.
    pub(crate) fn download_states(root: &Path) -> Vec<(String, PathBuf)> {
        let Ok(entries) = std::fs::read_dir(root) else {
            return Vec::new();
        };
        entries
            .filter_map(|e| {
                let path = e.ok()?.path();
                let name = path.file_name()?.to_str()?;
                let stem = name.strip_suffix(STATE_SUFFIX)?.to_owned();
                Some((stem, path))
            })
            .collect()
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// The archive, partial while downloading.
    pub(crate) fn download(&self) -> PathBuf {
        self.path("download")
    }

    /// What the partial download is of, so a resume never mixes versions.
    pub(crate) fn download_state(&self) -> PathBuf {
        self.root.join(format!("{}{STATE_SUFFIX}", self.stem))
    }

    /// The package as extracted, before it is swapped in.
    pub(crate) fn extract(&self) -> PathBuf {
        self.path("extract")
    }

    /// Working room for formats that unpack before they know the package's
    /// layout (`.tar.zst`).
    pub(crate) fn scratch(&self) -> PathBuf {
        self.path("unpack")
    }

    /// The package being replaced, held until the swap has succeeded.
    pub(crate) fn previous(&self) -> PathBuf {
        self.path("previous")
    }

    /// Where a package goes to be deleted, so it vanishes from the sim's
    /// view in one rename even if deleting it takes a while.
    pub(crate) fn trash(&self) -> PathBuf {
        self.path("trash")
    }

    fn path(&self, kind: &str) -> PathBuf {
        self.root.join(format!("{}.{kind}", self.stem))
    }

    /// Removes the extraction and swap leftovers, keeping the download.
    pub(crate) fn clear_work(&self) {
        for dir in [
            self.extract(),
            self.scratch(),
            self.previous(),
            self.trash(),
        ] {
            remove_dir(&dir);
        }
    }

    /// Removes everything this job wrote, and the staging folder if that
    /// leaves it empty.
    pub(crate) fn discard(&self) {
        self.clear_work();
        self.discard_download();
    }

    /// Removes the download and its state only. Used for leftovers found
    /// at startup, whose other files may be a previous package an
    /// interrupted swap set aside.
    pub(crate) fn discard_download(&self) {
        for file in [self.download(), self.download_state()] {
            if let Err(e) = std::fs::remove_file(&file)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(path = %file.display(), error = %e, "could not remove staging file");
            }
        }
        // Fails harmlessly while another job still has files here.
        let _ = std::fs::remove_dir(&self.root);
    }
}

pub(crate) fn remove_dir(dir: &Path) {
    if let Err(e) = std::fs::remove_dir_all(dir)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %dir.display(), error = %e, "could not remove staging folder");
    }
}

/// A filename-safe, bounded, deterministic stem for a product id. Ids like
/// `livery:aircraft:name` contain characters Windows rejects, and the
/// package tree below the stem needs most of the 260-character path budget.
fn stem(id: &str) -> String {
    let safe: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.len() <= MAX_STEM {
        return safe;
    }
    let hash = Sha256::digest(id.as_bytes());
    let suffix = format!(
        "{:02x}{:02x}{:02x}{:02x}",
        hash[0], hash[1], hash[2], hash[3]
    );
    format!("{}-{suffix}", &safe[..MAX_STEM - 1 - suffix.len()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems_are_safe_bounded_and_distinct() {
        assert_eq!(stem("aerodynamics-t38c"), "aerodynamics-t38c");
        assert_eq!(stem("livery:t38:beale"), "livery_t38_beale");
        let long_a = stem(&format!("livery:{}:a", "x".repeat(60)));
        let long_b = stem(&format!("livery:{}:b", "x".repeat(60)));
        assert_eq!(long_a.len(), MAX_STEM);
        assert_ne!(long_a, long_b);
    }

    #[test]
    fn stages_beside_the_install_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let community = tmp.path().join("Community");
        std::fs::create_dir_all(&community).unwrap();
        let staging = Staging::create(&community, "pkg").unwrap();
        assert_eq!(staging.root(), tmp.path().join(DIR_NAME));
        assert_eq!(
            staging.download(),
            tmp.path().join(DIR_NAME).join("pkg.download")
        );

        std::fs::write(staging.download(), b"x").unwrap();
        std::fs::create_dir_all(staging.extract().join("a")).unwrap();
        staging.discard();
        assert!(!staging.root().exists());
    }
}
