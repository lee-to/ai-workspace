mod crud;
mod schema;

use rusqlite::{Connection, Error, ErrorCode, Transaction, TransactionBehavior};
use std::time::Duration;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Optional maintenance must not queue behind a full rebuild. Reserve the
/// writer before doing any work, then restore the normal timeout for callers.
fn try_write_transaction(conn: &Connection) -> rusqlite::Result<Option<Transaction<'_>>> {
    conn.busy_timeout(Duration::ZERO)?;
    let result = Transaction::new_unchecked(conn, TransactionBehavior::Immediate);
    conn.busy_timeout(BUSY_TIMEOUT)?;
    match result {
        Ok(tx) => Ok(Some(tx)),
        Err(Error::SqliteFailure(error, _))
            if matches!(
                error.code,
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

pub use crud::{
    AmbiguousItemLabel, CodeGraphEdgeDirection, Db, ScopeChange, SharedItemUpdate,
    ValidatedProjectPath, validate_project_rel_path,
};
