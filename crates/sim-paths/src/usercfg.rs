//! `UserCfg.opt`, the sim's own record of where it keeps packages.
//!
//! The Community folder is wherever the user told the sim installer to put
//! packages, recorded as `InstalledPackagesPath "D:\MSFS"`. Guessing the
//! default location instead finds the wrong (empty) folder for everyone who
//! moved their packages off `C:`, so this file is consulted first.

use std::path::{Path, PathBuf};

const PACKAGES_KEY: &str = "InstalledPackagesPath";

/// The `InstalledPackagesPath` value, as the Windows path the sim wrote.
pub(crate) fn installed_packages_path(text: &str) -> Option<&str> {
    text.lines().find_map(|line| {
        let line = line.trim();
        let (key, value) = line.split_once(char::is_whitespace)?;
        if !key.eq_ignore_ascii_case(PACKAGES_KEY) {
            return None;
        }
        let value = value.trim().trim_matches('"').trim();
        (!value.is_empty()).then_some(value)
    })
}

/// Where a Windows path written by the sim lives on this machine.
///
/// Natively that is the path itself. Under Wine it is reached through the
/// prefix's `dosdevices/<drive>:` links, which is how Wine itself maps drive
/// letters (`c:` to `drive_c`, `z:` to `/`, anything else the user set up).
pub(crate) fn host_path(windows_path: &str, prefix: Option<&Path>) -> Option<PathBuf> {
    let Some(prefix) = prefix else {
        return Some(PathBuf::from(windows_path));
    };
    let mut chars = windows_path.chars();
    let drive = chars.next().filter(char::is_ascii_alphabetic)?;
    if chars.next() != Some(':') {
        return None;
    }
    let mut path = prefix
        .join("dosdevices")
        .join(format!("{}:", drive.to_ascii_lowercase()));
    for part in chars.as_str().split(['\\', '/']).filter(|p| !p.is_empty()) {
        if part == ".." {
            return None;
        }
        path.push(part);
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Version 66\n{Video\n\tAdapter \"NVIDIA\"\n}\n\
        InstalledPackagesPath \"D:\\MSFS 2024\\Packages\"\n";

    #[test]
    fn reads_the_packages_path() {
        assert_eq!(
            installed_packages_path(SAMPLE),
            Some("D:\\MSFS 2024\\Packages")
        );
    }

    #[test]
    fn tolerates_case_and_missing_quotes() {
        assert_eq!(
            installed_packages_path("installedpackagespath   E:\\Sim\r\n"),
            Some("E:\\Sim")
        );
    }

    #[test]
    fn missing_or_empty_value_is_none() {
        assert_eq!(installed_packages_path("Version 66\n"), None);
        assert_eq!(
            installed_packages_path("InstalledPackagesPath \"\"\n"),
            None
        );
        assert_eq!(
            installed_packages_path("InstalledPackagesPathX \"C:\\a\"\n"),
            None
        );
    }

    #[test]
    fn native_paths_pass_through() {
        assert_eq!(host_path("D:\\MSFS", None), Some(PathBuf::from("D:\\MSFS")));
    }

    #[test]
    fn wine_paths_go_through_dosdevices() {
        let prefix = Path::new("/pfx");
        assert_eq!(
            host_path("D:\\MSFS 2024\\Packages", Some(prefix)),
            Some(
                prefix
                    .join("dosdevices")
                    .join("d:")
                    .join("MSFS 2024")
                    .join("Packages")
            )
        );
        assert_eq!(host_path("relative\\path", Some(prefix)), None);
        assert_eq!(host_path("C:\\a\\..\\..\\etc", Some(prefix)), None);
    }
}
