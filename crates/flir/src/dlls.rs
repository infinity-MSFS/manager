//! Finding the two infinity-viewports DLLs.
//!
//! The nose view needs both in the sim: `infinity_viewports_bridge.dll` (GPU
//! hooks, command channel, video stream; injected by `Host::attach`) and
//! `infinity_viewports_core.dll` (creates the extra scene view and places its
//! camera; nothing in the library injects it, so the engine does).

use std::path::{Path, PathBuf};

pub const BRIDGE_DLL: &str = "infinity_viewports_bridge.dll";
pub const CORE_DLL: &str = "infinity_viewports_core.dll";

/// Where to look, in order, when the settings name no path.
///
/// A release build looks where the installer puts them: `<exe dir>/
/// viewports/`, `<exe dir>`, then `<appdata>/flir/viewports/`. A dev build
/// first uses a sibling `infinity-viewports` checkout's `target/release`
/// (`INFINITY_VIEWPORTS_DIR` overrides the checkout, as for `xtask dist`),
/// so a fresh `cargo build --release` there is picked up over anything
/// staged; its `target/debug` is the last resort.
pub fn candidates(app_data_dir: &Path) -> Vec<PathBuf> {
    let checkouts = if cfg!(debug_assertions) {
        sibling_checkouts()
    } else {
        Vec::new()
    };
    let mut dirs: Vec<PathBuf> = checkouts
        .iter()
        .map(|repo| repo.join("target").join("release"))
        .collect();
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        dirs.push(exe_dir.join("viewports"));
        dirs.push(exe_dir);
    }
    dirs.push(app_data_dir.join("flir").join("viewports"));
    dirs.extend(
        checkouts
            .iter()
            .map(|repo| repo.join("target").join("debug")),
    );
    dirs
}

/// `infinity-viewports` checkouts next to this source tree (found from where
/// it was compiled, so the working directory does not matter) or named by
/// `INFINITY_VIEWPORTS_DIR`.
fn sibling_checkouts() -> Vec<PathBuf> {
    let mut repos: Vec<PathBuf> = std::env::var_os("INFINITY_VIEWPORTS_DIR")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    let roots = [
        Some(PathBuf::from(env!("CARGO_MANIFEST_DIR"))),
        std::env::current_dir().ok(),
    ];
    for root in roots.into_iter().flatten() {
        for base in root.ancestors().take(5) {
            let repo = base.join("infinity-viewports");
            if repo.is_dir() && !repos.contains(&repo) {
                repos.push(repo);
            }
        }
    }
    repos
}

/// `configured` if given, else the first candidate holding `name`. The
/// error is a sentence for the panel.
pub fn resolve(
    configured: Option<&Path>,
    name: &str,
    app_data_dir: &Path,
) -> Result<PathBuf, String> {
    if let Some(p) = configured {
        return if p.is_file() {
            Ok(p.to_path_buf())
        } else {
            Err(format!(
                "{name}: configured path {} does not exist",
                p.display()
            ))
        };
    }
    let found = candidates(app_data_dir)
        .into_iter()
        .map(|d| d.join(name))
        .find(|p| p.is_file());
    if let Some(p) = &found {
        tracing::info!(dll = %p.display(), "flir: using viewports DLL");
    }
    found.ok_or_else(|| {
        format!(
            "{name} not found: build infinity-viewports (with MSFS closed) or set its path in the FLIR settings"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configured_path_must_exist() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join(BRIDGE_DLL);
        assert!(resolve(Some(&missing), BRIDGE_DLL, dir.path()).is_err());
        std::fs::write(&missing, b"").unwrap();
        assert_eq!(
            resolve(Some(&missing), BRIDGE_DLL, dir.path()).unwrap(),
            missing
        );
    }

    #[test]
    fn dev_builds_prefer_the_sibling_release_build() {
        let dirs = candidates(Path::new("appdata"));
        let release = dirs.iter().position(|d| d.ends_with("target/release"));
        let staged = dirs
            .iter()
            .position(|d| d.ends_with("viewports") && !d.ends_with("infinity-viewports"));
        let debug = dirs.iter().position(|d| d.ends_with("target/debug"));
        // Only meaningful where the sibling checkout exists (a dev machine).
        if let (Some(release), Some(staged), Some(debug)) = (release, staged, debug) {
            assert!(release < staged && staged < debug, "{dirs:?}");
        }
    }

    #[test]
    fn finds_the_dll_in_app_data() {
        let dir = tempfile::tempdir().unwrap();
        let viewports = dir.path().join("flir").join("viewports");
        std::fs::create_dir_all(&viewports).unwrap();
        // A name nothing else on the machine has.
        let name = "infinity_viewports_test_probe.dll";
        std::fs::write(viewports.join(name), b"").unwrap();
        assert_eq!(
            resolve(None, name, dir.path()).unwrap(),
            viewports.join(name)
        );
    }
}
