//! Preset visibility: which of a package's sim presets the account may fly.
//!
//! The sim lists a preset when its `aircraft.cfg` says `isUserSelectable=1`.
//! The manager writes that key into each preset's own `[FLTSIM.N]`
//! sections, 1 for the presets the account covers and 0 for the rest, and
//! keeps the package's `layout.json` in step. This only decides what the
//! aircraft list shows; it is not a lock.
//!
//! Everything here is blocking file work, free of the app shell.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use infinity_ipc::catalog::PresetGating;
use infinity_ipc::dto::UserProfile;
use serde::{Deserialize, Serialize};

use crate::inspect::MARKER_FILE;

/// The key the sim reads, as written when inserting it.
const KEY: &str = "isUserSelectable";
const LAYOUT_FILE: &str = "layout.json";
/// Each preset's config, relative to its folder.
const CFG: &str = "config/aircraft.cfg";

/// The presets an account sees: every free group, each group whose own
/// permission the profile holds, or every listed preset for a `see_all`
/// permission. `None` is signed out.
pub fn visible(gating: &PresetGating, profile: Option<&UserProfile>) -> BTreeSet<String> {
    let has = |permission: &str| profile.is_some_and(|p| p.has_permission(permission));
    let see_all = gating.see_all.iter().any(|p| has(p));
    gating
        .groups
        .iter()
        .filter(|g| see_all || g.requires.as_deref().is_none_or(has))
        .flat_map(|g| g.presets.iter().cloned())
        .collect()
}

/// Applies a product's gating for an account. `None` when the product
/// lists no presets: then nothing is read or written, so missing data can
/// never hide every preset. `profile` `None` is signed out. Blocking.
pub fn gate(
    package: &Path,
    gating: Option<&PresetGating>,
    profile: Option<&UserProfile>,
) -> Option<Outcome> {
    let gating = gating?;
    Some(apply(package, gating, &visible(gating, profile)))
}

/// What [`apply`] did.
#[derive(Debug)]
pub enum Outcome {
    /// The folder has no manager marker (a dev build linked in, a manual
    /// install): nothing was touched.
    Unmanaged,
    /// The marker is there but the preset folder is not.
    NoPresets,
    Applied(Report),
}

#[derive(Debug, Default)]
pub struct Report {
    /// Package-relative paths (`/`-separated, as on disk) of the files
    /// written, `layout.json` included.
    pub changed: Vec<String>,
    /// Preset folders on disk that no group lists; set to 0.
    pub unlisted: Vec<String>,
    /// Listed presets with no folder on disk; skipped.
    pub missing: Vec<String>,
    /// Files that could not be read or written.
    pub failed: Vec<Failure>,
}

impl Report {
    /// Whether any write failed because another process holds the file.
    pub fn locked(&self) -> bool {
        self.failed.iter().any(Failure::is_locked)
    }
}

#[derive(Debug)]
pub struct Failure {
    pub path: PathBuf,
    pub error: io::Error,
}

impl Failure {
    /// A sharing violation or a denied replace: the file is open elsewhere.
    pub fn is_locked(&self) -> bool {
        // ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION.
        matches!(self.error.raw_os_error(), Some(32 | 33))
            || self.error.kind() == io::ErrorKind::PermissionDenied
    }
}

/// Writes each preset's visibility into the package at `package`. Files
/// already right are left alone, so a second run writes nothing. Blocking.
pub fn apply(package: &Path, gating: &PresetGating, visible: &BTreeSet<String>) -> Outcome {
    if !package.join(MARKER_FILE).is_file() {
        return Outcome::Unmanaged;
    }
    let root_rel = gating.root.trim_matches('/');
    let root = package.join(root_rel);
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(_) => return Outcome::NoPresets,
    };
    let mut on_disk: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    on_disk.sort();

    let listed: BTreeSet<&str> = gating
        .groups
        .iter()
        .flat_map(|g| g.presets.iter().map(String::as_str))
        .collect();
    let mut report = Report {
        missing: listed
            .iter()
            .filter(|p| !on_disk.iter().any(|d| d == *p))
            .map(|p| (*p).to_owned())
            .collect(),
        ..Report::default()
    };
    for preset in &report.missing {
        tracing::debug!(preset, "listed preset is not in this build; skipped");
    }

    // (package-relative path, size, written this run) of each cfg, for
    // layout.json.
    let mut cfgs: Vec<(String, u64, bool)> = Vec::new();
    for preset in &on_disk {
        if !listed.contains(preset.as_str()) {
            tracing::warn!(preset, "preset is in no visibility group; hidden");
            report.unlisted.push(preset.clone());
        }
        let shown = visible.contains(preset);
        let rel = format!("{root_rel}/{preset}/{CFG}");
        let path = package.join(&rel);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                tracing::warn!(preset, "preset has no aircraft.cfg; skipped");
                continue;
            }
            Err(error) => {
                report.failed.push(Failure { path, error });
                continue;
            }
        };
        let edited = match set_user_selectable(&bytes, shown) {
            Edit::Unchanged => {
                cfgs.push((rel, bytes.len() as u64, false));
                continue;
            }
            Edit::NoFltsim => {
                tracing::warn!(preset, "aircraft.cfg has no [FLTSIM.N] section; skipped");
                continue;
            }
            Edit::Changed(edited) => edited,
        };
        match write_atomic(&path, &edited) {
            Ok(()) => {
                cfgs.push((rel.clone(), edited.len() as u64, true));
                report.changed.push(rel);
            }
            Err(error) => report.failed.push(Failure { path, error }),
        }
    }

    match update_layout(package, &cfgs) {
        Ok(true) => report.changed.push(LAYOUT_FILE.to_owned()),
        Ok(false) => {}
        Err(error) => report.failed.push(Failure {
            path: package.join(LAYOUT_FILE),
            error,
        }),
    }
    Outcome::Applied(report)
}

/// The result of [`set_user_selectable`].
#[derive(Debug, PartialEq, Eq)]
pub enum Edit {
    /// Every `[FLTSIM.N]` section already says so.
    Unchanged,
    NoFltsim,
    Changed(Vec<u8>),
}

/// Sets `isUserSelectable` in every `[FLTSIM.N]` section of a cfg:
/// replacing the value where the key is present, otherwise inserting it on
/// the line after the section header. Every other byte, line endings
/// included, is kept.
pub fn set_user_selectable(cfg: &[u8], shown: bool) -> Edit {
    let value: &[u8] = if shown { b"1" } else { b"0" };
    let lines = split_lines(cfg);
    let mut out = Vec::with_capacity(cfg.len() + KEY.len() + 4);
    let mut changed = false;
    let mut any_fltsim = false;
    let mut i = 0;
    while i < lines.len() {
        let (body, ending) = lines[i];
        out.extend_from_slice(body);
        out.extend_from_slice(ending);
        i += 1;
        if !section_name(body).is_some_and(is_fltsim) {
            continue;
        }
        any_fltsim = true;
        // The section runs to the next header.
        let end = (i..lines.len())
            .find(|&j| section_name(lines[j].0).is_some())
            .unwrap_or(lines.len());
        let section = &lines[i..end];
        if !section.iter().any(|(line, _)| value_span(line).is_some()) {
            changed = true;
            if ending.is_empty() {
                // The header is the last line and has no ending: end it
                // the way the file's other lines end, and leave the new
                // last line unterminated like the header was.
                let eol: &[u8] = if cfg.windows(2).any(|w| w == b"\r\n") {
                    b"\r\n"
                } else {
                    b"\n"
                };
                out.extend_from_slice(eol);
            }
            out.extend_from_slice(KEY.as_bytes());
            out.push(b'=');
            out.extend_from_slice(value);
            out.extend_from_slice(ending);
        }
        for &(line, line_ending) in section {
            match value_span(line) {
                Some((start, stop)) => {
                    if &line[start..stop] != value {
                        changed = true;
                    }
                    out.extend_from_slice(&line[..start]);
                    out.extend_from_slice(value);
                    out.extend_from_slice(&line[stop..]);
                }
                None => out.extend_from_slice(line),
            }
            out.extend_from_slice(line_ending);
        }
        i = end;
    }
    match (any_fltsim, changed) {
        (false, _) => Edit::NoFltsim,
        (true, false) => Edit::Unchanged,
        (true, true) => Edit::Changed(out),
    }
}

/// Lines as (body, ending), where the ending is `\r\n`, `\n` or empty.
fn split_lines(bytes: &[u8]) -> Vec<(&[u8], &[u8])> {
    let mut lines = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let (line, after) = match rest.iter().position(|&b| b == b'\n') {
            Some(n) => (&rest[..=n], &rest[n + 1..]),
            None => (rest, &[][..]),
        };
        let body_len = if line.ends_with(b"\r\n") {
            line.len() - 2
        } else if line.ends_with(b"\n") {
            line.len() - 1
        } else {
            line.len()
        };
        lines.push(line.split_at(body_len));
        rest = after;
    }
    lines
}

/// The name of a `[section]` header line.
fn section_name(line: &[u8]) -> Option<&[u8]> {
    let line = line.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(line);
    let line = line.trim_ascii_start().strip_prefix(b"[")?;
    let close = line.iter().position(|&b| b == b']')?;
    Some(line[..close].trim_ascii())
}

fn is_fltsim(name: &[u8]) -> bool {
    name.len() > 7
        && name[..7].eq_ignore_ascii_case(b"FLTSIM.")
        && name[7..].iter().all(u8::is_ascii_digit)
}

/// Where the value of an `isUserSelectable = value ; comment` line sits.
fn value_span(line: &[u8]) -> Option<(usize, usize)> {
    let eq = line.iter().position(|&b| b == b'=')?;
    if !line[..eq].trim_ascii().eq_ignore_ascii_case(KEY.as_bytes()) {
        return None;
    }
    let start = eq
        + 1
        + line[eq + 1..]
            .iter()
            .take_while(|b| b.is_ascii_whitespace())
            .count();
    let stop = start
        + line[start..]
            .iter()
            .take_while(|&&b| !b.is_ascii_whitespace() && b != b';')
            .count();
    Some((start, stop))
}

/// Temp file and rename in the same folder.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[derive(Debug, Serialize, Deserialize)]
struct Layout {
    content: Vec<LayoutEntry>,
    #[serde(flatten)]
    rest: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
struct LayoutEntry {
    path: String,
    size: u64,
    date: u64,
    #[serde(flatten)]
    rest: serde_json::Map<String, serde_json::Value>,
}

/// Brings the `layout.json` entries of the cfgs in step: the size and date
/// of each file written this run, and the size of any whose entry is off
/// (an earlier run cut short between a cfg and the layout). Returns
/// whether the file was written.
fn update_layout(package: &Path, cfgs: &[(String, u64, bool)]) -> io::Result<bool> {
    let path = package.join(LAYOUT_FILE);
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if cfgs.iter().any(|c| c.2) {
                tracing::warn!("package has no layout.json to update");
            }
            return Ok(false);
        }
        Err(e) => return Err(e),
    };
    let mut layout: Layout = serde_json::from_slice(&raw).map_err(io::Error::other)?;
    let mut dirty = false;
    for (rel, size, written) in cfgs {
        let key = rel.to_ascii_lowercase();
        let Some(entry) = layout.content.iter_mut().find(|e| e.path == key) else {
            if *written {
                tracing::warn!(path = key, "layout.json has no entry for an edited cfg");
            }
            continue;
        };
        if !*written && entry.size == *size {
            continue;
        }
        entry.size = *size;
        if let Some(date) = filetime(&package.join(rel)) {
            entry.date = date;
        }
        dirty = true;
    }
    if !dirty {
        return Ok(false);
    }
    let json = serde_json::to_vec_pretty(&layout).map_err(io::Error::other)?;
    write_atomic(&path, &json)?;
    Ok(true)
}

/// A file's modified time as a Windows FILETIME: 100 ns ticks since 1601.
fn filetime(path: &Path) -> Option<u64> {
    const UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    filetime_of(modified).map(|t| t + UNIX_EPOCH_TICKS)
}

fn filetime_of(time: SystemTime) -> Option<u64> {
    let since = time.duration_since(UNIX_EPOCH).ok()?;
    u64::try_from(since.as_nanos() / 100).ok()
}
