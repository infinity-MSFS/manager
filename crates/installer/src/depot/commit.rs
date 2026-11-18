//! Putting staged files into the install folder.
//!
//! A first install is one folder rename. An update or repair replaces
//! files in place, in two journaled phases so no crash or locked file can
//! leave a mix of versions behind:
//!
//! 1. *Aside*: every installed file about to be replaced or removed is
//!    renamed into the job folder. A failure here (the sim holding a file
//!    open, typically) moves them all back and the package is untouched.
//! 2. *Place*: each staged file is renamed into the package. Renames of
//!    our own fresh files on the same volume do not fail in practice; if
//!    one does, or the app dies, the journal finishes the job on the next
//!    run.
//!
//! Only then are the set-aside files deleted.

use std::path::{Path, PathBuf};

use infinity_depot::{Hash, MARKER_FILE, Manifest};
use serde::{Deserialize, Serialize};

use super::io::write_atomic;
use super::stage::Dir;
use super::state::InstalledState;
use crate::error::{Error, IoContext, Result};
use crate::inspect;
use crate::staging::{Staging, remove_dir};
use crate::swap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CommitJournal {
    pub(crate) target: PathBuf,
    pub(crate) phase: Phase,
    /// Staged files moved into the package.
    pub(crate) replace: Vec<String>,
    /// Installed files the new version no longer has.
    pub(crate) remove: Vec<String>,
    pub(crate) version: String,
    pub(crate) package_dir_name: String,
    pub(crate) manifest_hash: Hash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    Aside,
    Place,
}

/// What a commit needs to know.
pub(crate) struct Commit<'a> {
    pub(crate) dir: &'a Dir,
    pub(crate) staging: &'a Staging,
    pub(crate) manifest: &'a Manifest,
    pub(crate) manifest_hash: Hash,
    pub(crate) target: &'a Path,
    pub(crate) package_dir_name: &'a str,
    /// Manifest file indices staged under `dir.tree()`.
    pub(crate) staged: &'a [u32],
    pub(crate) previous: Option<&'a InstalledState>,
}

impl Commit<'_> {
    /// Blocking.
    pub(crate) fn run(&self) -> Result<()> {
        if self.target.exists() {
            self.in_place()
        } else {
            self.fresh()
        }
    }

    fn fresh(&self) -> Result<()> {
        let tree = self.dir.tree();
        if self.staged.len() != self.manifest.files.len() {
            return Err(Error::Internal(
                "a first install must stage every file".into(),
            ));
        }
        std::fs::create_dir_all(&tree).at(&tree)?;
        for d in &self.manifest.dirs {
            let p = tree.join(d);
            std::fs::create_dir_all(&p).at(&p)?;
        }
        self.finish_files(&tree)?;
        swap::install(&tree, self.target, self.staging)
    }

    fn in_place(&self) -> Result<()> {
        let replace: Vec<String> = self
            .staged
            .iter()
            .map(|&fi| self.manifest.files[fi as usize].path.clone())
            .collect();
        let keep: std::collections::HashSet<String> = self
            .manifest
            .files
            .iter()
            .map(|f| f.path.to_lowercase())
            .collect();
        let remove: Vec<String> = self
            .previous
            .map(|p| {
                p.manifest
                    .files
                    .iter()
                    .filter(|f| !keep.contains(&f.path.to_lowercase()))
                    .map(|f| f.path.clone())
                    .collect()
            })
            .unwrap_or_default();
        let mut journal = CommitJournal {
            target: self.target.to_path_buf(),
            phase: Phase::Aside,
            replace,
            remove,
            version: self.manifest.version.clone(),
            package_dir_name: self.package_dir_name.to_owned(),
            manifest_hash: self.manifest_hash,
        };
        write_journal(self.dir, &journal)?;

        let aside = self.dir.aside();
        let mut moved: Vec<&str> = Vec::new();
        for rel in journal.replace.iter().chain(&journal.remove) {
            let from = self.target.join(rel);
            if !from.is_file() {
                continue;
            }
            let to = aside.join(rel);
            let step = to
                .parent()
                .map_or(Ok(()), |p| std::fs::create_dir_all(p).at(p))
                .and_then(|()| swap::rename(&from, &to));
            if let Err(e) = step {
                tracing::warn!(file = rel, error = %e, "an installed file is in use; leaving the package as it was");
                roll_back_aside(self.target, &aside, &moved);
                clear(self.dir);
                return Err(e);
            }
            moved.push(rel);
        }

        journal.phase = Phase::Place;
        write_journal(self.dir, &journal)?;
        place(self.dir, &journal)?;
        // Before the new version's empty folders exist, so none is pruned.
        prune(self.target, &journal.remove, self.previous, self.manifest);
        for d in &self.manifest.dirs {
            let p = self.target.join(d);
            std::fs::create_dir_all(&p).at(&p)?;
        }
        self.finish_files(self.target)?;
        clear(self.dir);
        Ok(())
    }

    /// The marker and install state, written last.
    fn finish_files(&self, package: &Path) -> Result<()> {
        let marker = package.join(MARKER_FILE);
        write_atomic(
            &marker,
            &inspect::marker_json(&self.manifest.version, self.package_dir_name),
        )
        .at(&marker)?;
        let state = InstalledState::new(self.manifest.clone(), self.manifest_hash, package);
        state.write(package).at(package)
    }
}

fn write_journal(dir: &Dir, journal: &CommitJournal) -> Result<()> {
    let path = dir.commit();
    let bytes = serde_json::to_vec(journal).map_err(|e| Error::Internal(e.to_string()))?;
    write_atomic(&path, &bytes).at(&path)
}

/// Moves the staged files in. Idempotent: a file already moved is skipped.
fn place(dir: &Dir, journal: &CommitJournal) -> Result<()> {
    let tree = dir.tree();
    for rel in &journal.replace {
        let from = tree.join(rel);
        let to = journal.target.join(rel);
        if !from.exists() {
            continue;
        }
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).at(parent)?;
        }
        if to.exists() {
            // Recreated since the aside phase; ours wins, theirs is kept aside.
            let spare = dir.aside().join(format!("{rel}.replaced"));
            if let Some(p) = spare.parent() {
                std::fs::create_dir_all(p).at(p)?;
            }
            swap::rename(&to, &spare)?;
        }
        swap::rename(&from, &to)?;
    }
    Ok(())
}

fn roll_back_aside(target: &Path, aside: &Path, moved: &[&str]) {
    for rel in moved.iter().rev() {
        let back = target.join(rel);
        let moved = back
            .parent()
            .map_or(Ok(()), |p| std::fs::create_dir_all(p).at(p))
            .and_then(|()| swap::rename(&aside.join(rel), &back));
        if let Err(e) = moved {
            tracing::error!(file = rel, error = %e, kept_at = %aside.display(), "could not put an installed file back");
        }
    }
}

/// Removes folders the update emptied.
fn prune(
    target: &Path,
    removed: &[String],
    previous: Option<&InstalledState>,
    manifest: &Manifest,
) {
    let mut dirs: Vec<PathBuf> = removed
        .iter()
        .filter_map(|r| Path::new(r).parent().map(Path::to_path_buf))
        .collect();
    if let Some(prev) = previous {
        let now: std::collections::HashSet<&str> =
            manifest.dirs.iter().map(String::as_str).collect();
        dirs.extend(
            prev.manifest
                .dirs
                .iter()
                .filter(|d| !now.contains(d.as_str()))
                .map(PathBuf::from),
        );
    }
    for dir in dirs {
        let mut at = Some(dir.as_path());
        while let Some(d) = at.filter(|d| !d.as_os_str().is_empty()) {
            if std::fs::remove_dir(target.join(d)).is_err() {
                break;
            }
            at = d.parent();
        }
    }
}

fn clear(dir: &Dir) {
    remove_dir(&dir.aside());
    let _ = std::fs::remove_file(dir.commit());
}

/// Finishes or undoes a commit an earlier run left half done. Returns
/// whether there was one. Blocking.
pub(crate) fn recover(dir: &Dir) -> Result<bool> {
    let path = dir.commit();
    let Ok(bytes) = std::fs::read(&path) else {
        return Ok(false);
    };
    let Ok(journal) = serde_json::from_slice::<CommitJournal>(&bytes) else {
        tracing::warn!(path = %path.display(), "unreadable commit journal; leaving the job folder for inspection");
        return Ok(false);
    };
    match journal.phase {
        Phase::Aside => {
            tracing::info!(target = %journal.target.display(), "undoing an interrupted update");
            let aside = dir.aside();
            let moved: Vec<&str> = journal
                .replace
                .iter()
                .chain(&journal.remove)
                .map(String::as_str)
                .filter(|rel| aside.join(rel).is_file() && !journal.target.join(rel).exists())
                .collect();
            roll_back_aside(&journal.target, &aside, &moved);
        }
        Phase::Place => {
            tracing::info!(target = %journal.target.display(), "finishing an interrupted update");
            place(dir, &journal)?;
            let marker = journal.target.join(MARKER_FILE);
            write_atomic(
                &marker,
                &inspect::marker_json(&journal.version, &journal.package_dir_name),
            )
            .at(&marker)?;
            match std::fs::read(dir.manifest())
                .ok()
                .and_then(|b| Manifest::from_bytes(&b, Some(journal.manifest_hash)).ok())
            {
                Some(manifest) => {
                    InstalledState::new(manifest, journal.manifest_hash, &journal.target)
                        .write(&journal.target)
                        .at(&journal.target)?;
                }
                // The next update checks every file instead.
                None => {
                    let _ = std::fs::remove_file(journal.target.join(infinity_depot::STATE_FILE));
                }
            }
        }
    }
    clear(dir);
    Ok(true)
}
