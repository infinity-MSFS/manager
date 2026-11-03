//! "What is in the folder the user just picked?"
//!
//! Settings shows these answers before saving an override, so a wrong pick
//! is caught at the folder dialog instead of at the next failed export.

use std::path::{Path, PathBuf};

use crate::{ALL_SIMS, Layout, Sim, SimInstall, installs_in, profiles, sim_from_wasm_dir};

/// What a candidate work root holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkRootProbe {
    /// The folder that would be searched: the pick itself, or its
    /// `MSFS20xx` child when the user picked the `WASM` parent. `None` when
    /// the pick is not a directory.
    pub resolved: Option<PathBuf>,
    pub sim: Option<Sim>,
    /// Package folders (those with a `work` subfolder), sorted.
    pub packages: Vec<String>,
}

/// The layout is `WASM/MSFS20xx/<package>/work`. A folder dialog shows the
/// whole tree, so accept either level and report which one was used.
pub fn probe_work_root(path: &Path) -> WorkRootProbe {
    let candidates = work_root_candidates(path);
    let with_packages = candidates
        .iter()
        .map(|c| (c, packages_in(c)))
        .find(|(_, packages)| !packages.is_empty());
    let (resolved, packages) = match with_packages {
        Some((c, packages)) => (Some(c.clone()), packages),
        None => (candidates.into_iter().next(), Vec::new()),
    };
    WorkRootProbe {
        sim: resolved.as_deref().and_then(sim_from_wasm_dir),
        resolved,
        packages,
    }
}

/// What a Wine/Proton prefix holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixProbe {
    pub prefix: PathBuf,
    /// `drive_c` exists, so this is a prefix at all.
    pub is_prefix: bool,
    pub installs: Vec<SimInstall>,
}

/// Inspects one prefix without adopting it.
pub fn probe_prefix(prefix: &Path) -> PrefixProbe {
    PrefixProbe {
        prefix: prefix.to_path_buf(),
        is_prefix: profiles::is_prefix(prefix),
        installs: installs_in(&profiles::in_prefix(prefix)),
    }
}

/// Discovered prefixes that hold a sim. A Steam library holds dozens of
/// prefixes, so the rest are dropped rather than listed. Blocking.
pub fn detected_prefixes() -> Vec<PrefixProbe> {
    profiles::wine_prefixes(None)
        .iter()
        .map(|p| probe_prefix(p))
        .filter(|p| !p.installs.is_empty())
        .collect()
}

/// The pick itself, then its per-sim children, keeping only directories.
pub(crate) fn work_root_candidates(path: &Path) -> Vec<PathBuf> {
    std::iter::once(path.to_path_buf())
        .chain(ALL_SIMS.map(|s| path.join(s.wasm_dir())))
        .filter(|p| p.is_dir())
        .collect()
}

fn packages_in(root: &Path) -> Vec<String> {
    profiles::subdirs(root)
        .into_iter()
        .filter(|p| p.join("work").is_dir())
        .filter_map(|p| Some(p.file_name()?.to_string_lossy().into_owned()))
        .collect()
}

pub(crate) fn package_in(root: &Path, prefix: &str) -> Option<PathBuf> {
    profiles::subdirs(root).into_iter().find(|p| {
        p.join("work").is_dir()
            && p.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.get(..prefix.len()))
                .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn accepts_the_sim_folder_or_its_wasm_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let wasm = tmp.path().join("WASM");
        let root = wasm.join("MSFS2020");
        fs::create_dir_all(root.join("aerodynamics-dc10/work")).unwrap();
        fs::create_dir_all(root.join("scratch")).unwrap();

        for pick in [&wasm, &root] {
            let probe = probe_work_root(pick);
            assert_eq!(probe.resolved.as_deref(), Some(root.as_path()));
            assert_eq!(probe.sim, Some(Sim::Msfs2020));
            assert_eq!(probe.packages, vec!["aerodynamics-dc10"]);
        }
    }

    #[test]
    fn distinguishes_empty_from_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let empty = probe_work_root(tmp.path());
        assert_eq!(empty.resolved.as_deref(), Some(tmp.path()));
        assert!(empty.packages.is_empty());

        let missing = probe_work_root(&tmp.path().join("nope"));
        assert_eq!(missing.resolved, None);
        assert_eq!(missing.sim, None);
    }

    #[test]
    fn prefix_probe_finds_a_steam_sim_and_rejects_non_prefixes() {
        let tmp = tempfile::tempdir().unwrap();
        let sim = tmp
            .path()
            .join("drive_c/users/steamuser/AppData/Roaming/Microsoft Flight Simulator 2024");
        fs::create_dir_all(sim.join("Packages/Community")).unwrap();
        fs::create_dir_all(sim.join("WASM/MSFS2024")).unwrap();

        let probe = probe_prefix(tmp.path());
        assert!(probe.is_prefix);
        assert_eq!(probe.installs.len(), 1);
        let install = &probe.installs[0];
        assert_eq!(install.sim, Sim::Msfs2024);
        assert_eq!(install.wine_prefix.as_deref(), Some(tmp.path()));
        assert_eq!(install.wasm_root, Some(sim.join("WASM/MSFS2024")));

        let other = tempfile::tempdir().unwrap();
        let probe = probe_prefix(other.path());
        assert!(!probe.is_prefix);
        assert!(probe.installs.is_empty());
    }

    #[test]
    fn package_prefix_match_is_case_insensitive_and_needs_work() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("aerodynamics-t38c")).unwrap();
        fs::create_dir_all(tmp.path().join("AERODYNAMICS-T38N/work")).unwrap();
        assert_eq!(
            package_in(tmp.path(), "aerodynamics-t38"),
            Some(tmp.path().join("AERODYNAMICS-T38N"))
        );
        assert_eq!(package_in(tmp.path(), "aerodynamics-t38c"), None);
    }
}
