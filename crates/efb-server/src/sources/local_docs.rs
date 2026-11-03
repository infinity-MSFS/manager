//! PDFs the user drops into a folder, for a [`crate::DocProvider`] to list
//! alongside its own. Rendering is the provider's business.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Local documents get `user-<slug>` ids, so they never collide with
/// remote ones and the provider can tell where to read them from.
pub const LOCAL_DOC_PREFIX: &str = "user-";
pub const LOCAL_DOC_CATEGORY: &str = "My Documents";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalDoc {
    pub id: String,
    /// The file stem.
    pub title: String,
    pub path: PathBuf,
}

/// Every `*.pdf` directly in `dir`, by file name, with ids stable for a
/// given folder content (`user-foo`, `user-foo-2`...). Blocking; a missing
/// folder is just empty.
pub fn scan_local_docs(dir: &Path) -> Vec<LocalDoc> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
        })
        .collect();
    paths.sort();

    let mut seen = HashSet::new();
    paths
        .into_iter()
        .map(|path| {
            let title = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("document")
                .to_owned();
            let slug = slugify(&title);
            let mut id = format!("{LOCAL_DOC_PREFIX}{slug}");
            let mut n = 2;
            while !seen.insert(id.clone()) {
                id = format!("{LOCAL_DOC_PREFIX}{slug}-{n}");
                n += 1;
            }
            LocalDoc { id, title, path }
        })
        .collect()
}

/// Lowercase `[a-z0-9._-]`, anything else to `-`; never empty.
fn slugify(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "document".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// `id` if it is safe as a single path component under a cache dir. Ids come
/// from clients, so this is what keeps `../../` out of the filesystem.
pub fn sanitize_doc_id(id: &str) -> Option<&str> {
    let ok = !id.is_empty()
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    ok.then_some(id)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_slugged_and_deduplicated() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["Checklist A.pdf", "checklist-a.PDF", "notes.txt", "§§.pdf"] {
            std::fs::write(dir.path().join(name), b"%PDF").unwrap();
        }
        let docs = scan_local_docs(dir.path());
        let ids: Vec<&str> = docs.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(
            ids,
            ["user-checklist-a", "user-checklist-a-2", "user-document"]
        );
        assert_eq!(docs[0].title, "Checklist A");
        assert!(scan_local_docs(&dir.path().join("missing")).is_empty());
    }

    #[test]
    fn ids_cannot_escape_the_cache() {
        assert_eq!(sanitize_doc_id("t38-dash-1.v2"), Some("t38-dash-1.v2"));
        for bad in ["", "..", "a/b", "a\\b", "../x", "c:"] {
            assert_eq!(sanitize_doc_id(bad), None, "{bad}");
        }
    }
}
