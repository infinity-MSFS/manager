//! Where Microsoft Flight Simulator keeps things on this machine.
//!
//! [`Locator::discover`] takes one snapshot of every sim install it can find
//! (Microsoft Store and Steam, 2020 and 2024, natively or inside Proton) and
//! answers path questions from it: the Community folder packages install
//! into, the WASM root that holds per-package `work` folders, and a given
//! package's `work` folder.
//!
//! Discovery is plain filesystem probing and blocks; call it off the async
//! runtime. The snapshot is cheap to keep but goes stale when the user
//! installs or moves a sim, so take a fresh one per user action.

mod probe;
mod profiles;
mod usercfg;

use std::path::{Path, PathBuf};

pub use infinity_ipc::dto::Sim;
pub use probe::{PrefixProbe, WorkRootProbe, detected_prefixes, probe_prefix, probe_work_root};

use crate::profiles::Profile;

/// Every sim, newest first: the order to search in when nothing says which
/// one the user flies.
pub const ALL_SIMS: [Sim; 2] = [Sim::Msfs2024, Sim::Msfs2020];

/// How a sim was installed, which decides its directory layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storefront {
    MicrosoftStore,
    Steam,
}

/// Where a resolved path came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Discovered(Storefront),
    /// A folder the user picked in settings.
    UserOverride,
}

/// User-chosen paths for setups discovery cannot reach. Both are tried
/// before discovery, and discovery still runs behind them, so a stale pick
/// degrades to auto-detection instead of breaking the app.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    /// Proton/Wine prefix, the directory containing `drive_c`. For launchers
    /// that keep prefixes outside Steam (Heroic, Lutris, Bottles).
    pub wine_prefix: Option<PathBuf>,
    /// A `WASM/MSFS20xx` folder, or its `WASM` parent.
    pub work_root: Option<PathBuf>,
}

/// One installed sim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimInstall {
    pub sim: Sim,
    pub storefront: Storefront,
    /// The sim's profile folder, the one holding `UserCfg.opt`.
    pub root: PathBuf,
    /// The Community folder, if it exists.
    pub community: Option<PathBuf>,
    /// `WASM/MSFS20xx`, if the sim has created it yet.
    pub wasm_root: Option<PathBuf>,
    /// The Wine prefix the sim runs in; `None` on Windows.
    pub wine_prefix: Option<PathBuf>,
}

/// A folder holding per-package `work` directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmRoot {
    /// `None` when a user-picked folder is not named after a sim.
    pub sim: Option<Sim>,
    pub origin: Origin,
    pub path: PathBuf,
}

/// A package's `work` folder, where the sim lets its gauges persist files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageWork {
    pub root: WasmRoot,
    /// `<root>/<package>`.
    pub package: PathBuf,
    /// `<root>/<package>/work`.
    pub work: PathBuf,
}

/// A snapshot of the sims installed on this machine.
#[derive(Debug, Clone, Default)]
pub struct Locator {
    installs: Vec<SimInstall>,
    work_roots: Vec<PathBuf>,
}

impl Locator {
    /// Probes the filesystem. Blocking.
    pub fn discover(overrides: &Overrides) -> Self {
        let profiles = profiles::discover(overrides.wine_prefix.as_deref());
        let locator = Self {
            installs: installs_in(&profiles),
            work_roots: overrides
                .work_root
                .as_deref()
                .map(probe::work_root_candidates)
                .unwrap_or_default(),
        };
        if overrides.work_root.is_some() && locator.work_roots.is_empty() {
            tracing::warn!(?overrides.work_root, "work root override is not a directory");
        }
        tracing::debug!(
            installs = locator.installs.len(),
            profiles = profiles.len(),
            "sim discovery finished"
        );
        locator
    }

    /// Installs found, newest sim first, Store before Steam within a sim.
    pub fn installs(&self) -> &[SimInstall] {
        &self.installs
    }

    /// The first install, in `sims` order, that has a Community folder.
    pub fn community(&self, sims: &[Sim]) -> Option<&SimInstall> {
        self.in_order(sims).find(|i| i.community.is_some())
    }

    /// The folder holding package `work` directories: a user override if
    /// set, else the first sim in `sims` order that has one.
    pub fn wasm_root(&self, sims: &[Sim]) -> Option<WasmRoot> {
        self.wasm_roots(sims).next()
    }

    /// The `work` folder of the first package whose directory name starts
    /// with `package_prefix` (ASCII case-insensitively), searching overrides
    /// first and then each sim in `sims` order.
    pub fn package_work(&self, package_prefix: &str, sims: &[Sim]) -> Option<PackageWork> {
        let found = self
            .wasm_roots(sims)
            .find_map(|root| probe::package_in(&root.path, package_prefix).map(|p| (root, p)));
        if found.is_none() {
            tracing::debug!(package_prefix, ?sims, "no package work folder found");
        }
        found.map(|(root, package)| PackageWork {
            work: package.join("work"),
            package,
            root,
        })
    }

    fn in_order<'a>(&'a self, sims: &[Sim]) -> impl Iterator<Item = &'a SimInstall> + use<'a> {
        let sims = sims.to_vec();
        sims.into_iter()
            .flat_map(move |sim| self.installs.iter().filter(move |i| i.sim == sim))
    }

    fn wasm_roots<'a>(&'a self, sims: &[Sim]) -> impl Iterator<Item = WasmRoot> + use<'a> {
        let overrides = self.work_roots.iter().map(|path| WasmRoot {
            sim: sim_from_wasm_dir(path),
            origin: Origin::UserOverride,
            path: path.clone(),
        });
        let discovered = self.in_order(sims).filter_map(|i| {
            Some(WasmRoot {
                sim: Some(i.sim),
                origin: Origin::Discovered(i.storefront),
                path: i.wasm_root.clone()?,
            })
        });
        overrides.chain(discovered)
    }
}

/// Per-sim directory names. A trait because [`Sim`] lives in the IPC crate.
trait Layout {
    fn wasm_dir(self) -> &'static str;
    fn steam_dir(self) -> &'static str;
    fn store_package_prefix(self) -> &'static str;
}

impl Layout for Sim {
    fn wasm_dir(self) -> &'static str {
        match self {
            Self::Msfs2024 => "MSFS2024",
            Self::Msfs2020 => "MSFS2020",
        }
    }

    /// Steam profile folder under `AppData\Roaming`.
    fn steam_dir(self) -> &'static str {
        match self {
            Self::Msfs2024 => "Microsoft Flight Simulator 2024",
            Self::Msfs2020 => "Microsoft Flight Simulator",
        }
    }

    /// Store package folder prefix under `AppData\Local\Packages`.
    fn store_package_prefix(self) -> &'static str {
        match self {
            Self::Msfs2024 => "Microsoft.Limitless_",
            Self::Msfs2020 => "Microsoft.FlightSimulator_",
        }
    }
}

/// The sim a `WASM/MSFS20xx` folder belongs to, from its name. Wine hands
/// back whatever case the creating process used.
fn sim_from_wasm_dir(path: &Path) -> Option<Sim> {
    let name = path.file_name()?.to_str()?;
    ALL_SIMS
        .into_iter()
        .find(|s| s.wasm_dir().eq_ignore_ascii_case(name))
}

fn installs_in(profiles: &[Profile]) -> Vec<SimInstall> {
    let mut out = Vec::new();
    for sim in ALL_SIMS {
        for profile in profiles {
            out.extend(store_installs(profile, sim));
            out.extend(steam_install(profile, sim));
        }
    }
    out
}

/// `AppData\Local\Packages\<prefix><publisher hash>\{LocalCache,LocalState}`.
fn store_installs(profile: &Profile, sim: Sim) -> Vec<SimInstall> {
    profiles::subdirs(&profile.local.join("Packages"))
        .into_iter()
        .filter(|dir| {
            dir.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(sim.store_package_prefix()))
        })
        .filter_map(|package| {
            let cache = package.join("LocalCache");
            let state = package.join("LocalState");
            install(
                sim,
                Storefront::MicrosoftStore,
                profile,
                &cache,
                cache.join("Packages"),
                state.join("WASM").join(sim.wasm_dir()),
            )
        })
        .collect()
}

/// `AppData\Roaming\Microsoft Flight Simulator[ 2024]`.
fn steam_install(profile: &Profile, sim: Sim) -> Option<SimInstall> {
    let root = profile.roaming.join(sim.steam_dir());
    install(
        sim,
        Storefront::Steam,
        profile,
        &root,
        root.join("Packages"),
        root.join("WASM").join(sim.wasm_dir()),
    )
}

/// Assembles an install from its profile folder, or `None` when the folder
/// shows no sign of a sim ever having run there.
fn install(
    sim: Sim,
    storefront: Storefront,
    profile: &Profile,
    root: &Path,
    default_packages: PathBuf,
    wasm_root: PathBuf,
) -> Option<SimInstall> {
    let usercfg = std::fs::read_to_string(root.join("UserCfg.opt")).ok();
    let configured = usercfg
        .as_deref()
        .and_then(usercfg::installed_packages_path)
        .and_then(|p| usercfg::host_path(p, profile.prefix.as_deref()))
        .map(|p| p.join("Community"));
    let community = configured
        .into_iter()
        .chain(std::iter::once(default_packages.join("Community")))
        .find(|p| p.is_dir());
    let wasm_root = Some(wasm_root).filter(|p| p.is_dir());

    if usercfg.is_none() && community.is_none() && wasm_root.is_none() {
        return None;
    }
    Some(SimInstall {
        sim,
        storefront,
        root: root.to_path_buf(),
        community,
        wasm_root,
        wine_prefix: profile.prefix.clone(),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn profile(dir: &Path) -> Profile {
        Profile {
            roaming: dir.join("Roaming"),
            local: dir.join("Local"),
            prefix: None,
        }
    }

    fn locator(dir: &Path, work_root: Option<PathBuf>) -> Locator {
        Locator {
            installs: installs_in(&[profile(dir)]),
            work_roots: work_root
                .as_deref()
                .map(probe::work_root_candidates)
                .unwrap_or_default(),
        }
    }

    #[test]
    fn steam_install_reads_community_from_usercfg() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Roaming/Microsoft Flight Simulator 2024");
        let moved = tmp.path().join("D/MSFS");
        fs::create_dir_all(moved.join("Community")).unwrap();
        fs::create_dir_all(root.join("Packages/Community")).unwrap();
        fs::write(
            root.join("UserCfg.opt"),
            format!(
                "Version 66\nInstalledPackagesPath \"{}\"\n",
                moved.display()
            ),
        )
        .unwrap();

        let locator = locator(tmp.path(), None);
        let install = locator.community(&ALL_SIMS).unwrap();
        assert_eq!(install.sim, Sim::Msfs2024);
        assert_eq!(install.storefront, Storefront::Steam);
        assert_eq!(
            install.community.as_deref(),
            Some(moved.join("Community").as_path())
        );
    }

    #[test]
    fn falls_back_to_default_community_when_usercfg_points_nowhere() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Roaming/Microsoft Flight Simulator");
        fs::create_dir_all(root.join("Packages/Community")).unwrap();
        fs::write(
            root.join("UserCfg.opt"),
            "InstalledPackagesPath \"Q:\\gone\"\n",
        )
        .unwrap();

        let locator = locator(tmp.path(), None);
        let install = locator.community(&ALL_SIMS).unwrap();
        assert_eq!(install.sim, Sim::Msfs2020);
        assert_eq!(
            install.community.as_deref(),
            Some(root.join("Packages/Community").as_path())
        );
    }

    #[test]
    fn store_layout_is_found_and_ordered_by_sim_preference() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp
            .path()
            .join("Local/Packages/Microsoft.FlightSimulator_8wekyb3d8bbwe");
        fs::create_dir_all(store.join("LocalCache/Packages/Community")).unwrap();
        fs::create_dir_all(store.join("LocalState/WASM/MSFS2020/pkg-a/work")).unwrap();
        let steam = tmp.path().join("Roaming/Microsoft Flight Simulator 2024");
        fs::create_dir_all(steam.join("Packages/Community")).unwrap();

        let locator = locator(tmp.path(), None);
        assert_eq!(locator.installs().len(), 2);
        assert_eq!(locator.community(&ALL_SIMS).unwrap().sim, Sim::Msfs2024);
        let older = locator.community(&[Sim::Msfs2020]).unwrap();
        assert_eq!(older.storefront, Storefront::MicrosoftStore);
        assert_eq!(
            locator.wasm_root(&ALL_SIMS).unwrap().sim,
            Some(Sim::Msfs2020)
        );
    }

    #[test]
    fn empty_profile_has_no_installs() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("Roaming/Microsoft Flight Simulator 2024")).unwrap();
        fs::create_dir_all(tmp.path().join("Local/Packages/Spotify_123")).unwrap();
        assert!(locator(tmp.path(), None).installs().is_empty());
    }

    #[test]
    fn package_work_prefers_the_override() {
        let tmp = tempfile::tempdir().unwrap();
        let steam = tmp.path().join("Roaming/Microsoft Flight Simulator 2024");
        fs::create_dir_all(steam.join("WASM/MSFS2024/aerodynamics-t38c/work")).unwrap();
        let custom = tmp.path().join("custom/WASM");
        fs::create_dir_all(custom.join("MSFS2024/Aerodynamics-T38C-extra/work")).unwrap();

        let found = locator(tmp.path(), Some(custom.clone()))
            .package_work("aerodynamics-t38c", &ALL_SIMS)
            .unwrap();
        assert_eq!(found.root.origin, Origin::UserOverride);
        assert_eq!(found.root.sim, Some(Sim::Msfs2024));
        assert_eq!(
            found.work,
            custom.join("MSFS2024/Aerodynamics-T38C-extra/work")
        );

        let found = locator(tmp.path(), None)
            .package_work("aerodynamics-t38c", &ALL_SIMS)
            .unwrap();
        assert_eq!(found.root.origin, Origin::Discovered(Storefront::Steam));
        assert!(
            locator(tmp.path(), None)
                .package_work("aerodynamics-dc10", &ALL_SIMS)
                .is_none()
        );
    }

    #[test]
    fn sim_from_wasm_dir_ignores_case() {
        assert_eq!(
            sim_from_wasm_dir(Path::new("x/msfs2020")),
            Some(Sim::Msfs2020)
        );
        assert_eq!(sim_from_wasm_dir(Path::new("x/WASM")), None);
    }
}
