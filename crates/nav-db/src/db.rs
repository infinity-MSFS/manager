//! One read-only SQLite connection, shared behind a mutex.
//!
//! v3 opened a connection per query. Opening is cheap but not free (the
//! schema is parsed each time) and a single connection lets prepared
//! statements stay cached. Queries are milliseconds, so serialising them
//! costs nothing a user would notice.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use rusqlite::{Connection, OpenFlags};

use crate::error::{Error, Result};

#[derive(Debug)]
pub(crate) struct Db {
    path: PathBuf,
    stamp: Option<Stamp>,
    conn: Mutex<Connection>,
}

/// What the file looked like when opened, to notice it being replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl Stamp {
    fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Self {
            len: meta.len(),
            modified: meta.modified().ok(),
        })
    }
}

impl Db {
    pub(crate) fn open(path: PathBuf) -> Result<Self> {
        if !path.is_file() {
            return Err(Error::NotFound { path });
        }
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        Ok(Self {
            stamp: Stamp::of(&path),
            path,
            conn: Mutex::new(conn),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn is_stale(&self) -> bool {
        Stamp::of(&self.path) != self.stamp
    }

    /// Runs `f` with the connection. A query that panicked leaves the
    /// connection usable (reads only), so a poisoned lock is not fatal.
    pub(crate) fn with<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        f(&conn)
    }
}
