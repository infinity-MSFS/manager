//! Finding the two infinity-viewports DLLs.
//!
//! The nose view needs both in the sim: `infinity_viewports_bridge.dll` (GPU
//! hooks, command channel, video stream; injected by `Host::attach`) and
//! `infinity_viewports_core.dll` (creates the extra scene view and places its
//! camera; nothing in the library injects it, so the engine does).

use std::path::{Path, PathBuf};

pub const BRIDGE_DLL: &str = "infinity_viewports_bridge.dll";
pub const CORE_DLL: &str = "infinity_viewports_core.dll";

/// Where to look, in order, when the settings name no path:
/// `<exe dir>/viewports/`, `<exe dir>`, `<appdata>/flir/viewports/`, then a
/// sibling `infinity-viewports` checkout's `target/release` and
/// `target/debug` (the development layout).
pub fn candidates(app_data_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        dirs.push(exe_dir.join("viewports"));
        dirs.push(exe_dir);
    }
    dirs.push(app_data_dir.join("flir").join("viewports"));
    if let Ok(cwd) = std::env::current_dir() {
        for base in cwd.ancestors().take(4) {
            let repo = base.join("infinity-viewports").join("target");
            dirs.push(repo.join("release"));
            dirs.push(repo.join("debug"));
        }
    }
    dirs
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
    candidates(app_data_dir)
        .into_iter()
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .ok_or_else(|| {
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
