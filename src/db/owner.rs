//! A separate SQLite exclusive transaction is the cross-platform process lock.
//! The OS releases it on exit/crash. Never delete/recreate this file: doing so
//! would let contenders lock different inodes. The application DB stays in WAL.
use anyhow::{Context, Result};
use rusqlite::{Connection, Error, ErrorCode};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(crate) struct OwnerGuard {
    _connection: Connection,
}

pub(crate) fn canonical_database_path() -> Result<PathBuf> {
    let path = super::crud::default_db_path()?;
    if path.exists() {
        return path
            .canonicalize()
            .context("Cannot resolve workspace database");
    }
    let absolute = std::path::absolute(path)?;
    let parent = absolute
        .parent()
        .context("Database requires a parent directory")?;
    std::fs::create_dir_all(parent)?;
    Ok(parent.canonicalize()?.join(
        absolute
            .file_name()
            .context("Database requires a file name")?,
    ))
}

pub(crate) fn companion(db: &Path, suffix: &str) -> PathBuf {
    let mut path = db.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

pub(crate) fn try_acquire(db: &Path) -> Result<Option<OwnerGuard>> {
    let connection = Connection::open(companion(db, ".owner-lock"))?;
    connection.busy_timeout(Duration::ZERO)?;
    match connection.execute_batch("BEGIN EXCLUSIVE") {
        Ok(()) => Ok(Some(OwnerGuard {
            _connection: connection,
        })),
        Err(Error::SqliteFailure(error, _))
            if matches!(
                error.code,
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error).context("Cannot acquire workspace ownership"),
    }
}
