//! Windows user profiles the sim could be running under.
//!
//! Every layout this crate knows hangs off a profile's `AppData\Roaming` and
//! `AppData\Local`. Windows has exactly one profile. Linux has no native
//! sim, so each Proton/Wine prefix contributes one profile per synthetic
//! Windows user inside it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// One `AppData` pair, plus the Wine prefix it lives in, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Profile {
    pub(crate) roaming: PathBuf,
    pub(crate) local: PathBuf,
    pub(crate) prefix: Option<PathBuf>,
}

/// Profiles to search on this machine, most likely first.
pub(crate) fn discover(wine_prefix: Option<&Path>) -> Vec<Profile> {
    if cfg!(windows) {
        return native().into_iter().collect();
    }
    wine_prefixes(wine_prefix)
        .iter()
        .flat_map(|p| in_prefix(p))
        .collect()
}

fn native() -> Option<Profile> {
    Some(Profile {
        roaming: std::env::var_os("APPDATA")?.into(),
        local: std::env::var_os("LOCALAPPDATA")?.into(),
        prefix: None,
    })
}

/// The profiles inside one prefix (the directory containing `drive_c`).
pub(crate) fn in_prefix(prefix: &Path) -> Vec<Profile> {
    windows_users(prefix)
        .into_iter()
        .map(|user| Profile {
            roaming: user.join("AppData").join("Roaming"),
            local: user.join("AppData").join("Local"),
            prefix: Some(prefix.to_path_buf()),
        })
        .filter(|p| p.roaming.is_dir() || p.local.is_dir())
        .collect()
}

pub(crate) fn is_prefix(path: &Path) -> bool {
    path.join("drive_c").is_dir()
}

/// Proton names its user `steamuser`; plain Wine and old Proton use the host
/// account name, so enumerate. `Public` is a shared stub, never a user.
fn windows_users(prefix: &Path) -> Vec<PathBuf> {
    let mut users = subdirs(&prefix.join("drive_c").join("users"));
    users.retain(|p| {
        !p.file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("Public"))
    });
    users.sort_by_key(|p| {
        let steamuser = p
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("steamuser"));
        (!steamuser, p.clone())
    });
    users
}

/// Every prefix worth searching: the user's pick (or the
/// `INFINITY_MSFS_PREFIX` developer override), then each Proton prefix in
/// every Steam library. A stale pick only costs a few `stat`s, because
/// discovery still runs behind it.
pub(crate) fn wine_prefixes(preferred: Option<&Path>) -> Vec<PathBuf> {
    let env = std::env::var_os("INFINITY_MSFS_PREFIX").map(PathBuf::from);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for pick in [env.as_deref(), preferred].into_iter().flatten() {
        if is_prefix(pick) {
            if seen.insert(pick.to_path_buf()) {
                out.push(pick.to_path_buf());
            }
        } else {
            tracing::warn!(prefix = %pick.display(), "wine prefix has no drive_c, ignoring");
        }
    }
    for library in steam_libraries(&steam_roots()) {
        for prefix in prefixes_in_library(&library) {
            if seen.insert(prefix.clone()) {
                out.push(prefix);
            }
        }
    }
    out
}

/// Every Proton prefix in one Steam library. Deliberately not keyed on the
/// sim's Steam app id: matching on the sim's own directories is cheaper to
/// maintain and also covers non-Steam launchers that reuse compatdata.
fn prefixes_in_library(library: &Path) -> Vec<PathBuf> {
    subdirs(&library.join("steamapps").join("compatdata"))
        .into_iter()
        .map(|d| d.join("pfx"))
        .filter(|p| is_prefix(p))
        .collect()
}

fn steam_roots() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    [
        std::env::var_os("STEAM_ROOT").map(PathBuf::from),
        Some(home.join(".steam/steam")),
        Some(home.join(".steam/root")),
        Some(home.join(".local/share/Steam")),
        Some(home.join(".var/app/com.valvesoftware.Steam/data/Steam")),
    ]
    .into_iter()
    .flatten()
    .filter(|p| p.is_dir())
    .collect()
}

/// Base installs plus every extra library listed in `libraryfolders.vdf`.
/// The base roots are often symlinks to one another, so dedupe on the
/// canonical path.
fn steam_libraries(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut push = |p: PathBuf| {
        let key = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
        if p.is_dir() && seen.insert(key) {
            out.push(p);
        }
    };
    for root in roots {
        push(root.clone());
        let vdf = root.join("steamapps").join("libraryfolders.vdf");
        if let Ok(text) = std::fs::read_to_string(vdf) {
            for path in library_paths(&text) {
                push(PathBuf::from(path));
            }
        }
    }
    out
}

/// `"path"` values from Valve's `libraryfolders.vdf`. Only one key matters,
/// so this reads quoted tokens per line rather than modelling VDF; anything
/// it does not recognise yields nothing instead of an error.
fn library_paths(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let mut tokens = quoted_tokens(line);
            let key = tokens.next()?;
            key.eq_ignore_ascii_case("path").then(|| tokens.next())?
        })
        .collect()
}

/// Double-quoted tokens on a line, with VDF backslash escapes resolved.
/// An unterminated token (a truncated file) is dropped.
fn quoted_tokens(line: &str) -> impl Iterator<Item = String> + '_ {
    let mut chars = line.chars();
    std::iter::from_fn(move || {
        chars.by_ref().find(|&c| c == '"')?;
        let mut token = String::new();
        while let Some(c) = chars.next() {
            match c {
                '\\' => token.extend(chars.next()),
                '"' => return Some(token),
                c => token.push(c),
            }
        }
        None
    })
}

/// Sorted subdirectories, so repeated scans resolve the same install.
pub(crate) fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    dirs
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn parses_library_folders() {
        let vdf = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"/home/p/.local/share/Steam\"\n\
            \t\t\"label\"\t\t\"\"\n\t}\n\t\"1\"\n\t{\n\t\t\"Path\"\t\"D:\\\\SteamLibrary\"\n\t}\n}\n";
        assert_eq!(
            library_paths(vdf),
            vec!["/home/p/.local/share/Steam", "D:\\SteamLibrary"]
        );
    }

    #[test]
    fn ignores_malformed_lines() {
        assert!(library_paths("\"label\"\t\"path\"\n").is_empty());
        assert!(library_paths("\t\"path\"\n").is_empty());
        assert!(library_paths("\"path\"  \"/mnt/trunc").is_empty());
        assert!(library_paths("").is_empty());
    }

    #[test]
    fn finds_prefixes_in_extra_libraries() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("steam");
        let extra = tmp.path().join("library");
        fs::create_dir_all(root.join("steamapps")).unwrap();
        fs::create_dir_all(extra.join("steamapps/compatdata/2537590/pfx/drive_c")).unwrap();
        fs::create_dir_all(extra.join("steamapps/compatdata/999/not-a-prefix")).unwrap();
        fs::write(
            root.join("steamapps/libraryfolders.vdf"),
            format!(
                "\"path\" \"{}\"\n\"path\" \"/does/not/exist\"\n",
                extra.display().to_string().replace('\\', "\\\\")
            ),
        )
        .unwrap();

        let libraries = steam_libraries(std::slice::from_ref(&root));
        assert_eq!(libraries, vec![root, extra.clone()]);
        assert_eq!(
            prefixes_in_library(&extra),
            vec![extra.join("steamapps/compatdata/2537590/pfx")]
        );
    }

    #[test]
    fn prefix_users_put_steamuser_first_and_skip_public() {
        let tmp = tempfile::tempdir().unwrap();
        for user in ["alice", "Public", "steamuser"] {
            fs::create_dir_all(
                tmp.path()
                    .join("drive_c/users")
                    .join(user)
                    .join("AppData/Roaming"),
            )
            .unwrap();
        }
        let users: Vec<_> = in_prefix(tmp.path())
            .into_iter()
            .map(|p| p.roaming)
            .collect();
        assert_eq!(
            users,
            vec![
                tmp.path().join("drive_c/users/steamuser/AppData/Roaming"),
                tmp.path().join("drive_c/users/alice/AppData/Roaming"),
            ]
        );
    }
}
