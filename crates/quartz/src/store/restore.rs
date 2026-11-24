//! Checkpoints a live database and checks SQLite's file lock before replacement.

use super::{OpenStoreError, raw::ConnectionSql};
use diesel::{Connection, SqliteConnection, connection::SimpleConnection};
use libsqlite3_sys::{self as sqlite, ErrorCode};
use std::{path::Path, ptr};

#[derive(Debug, snafu::Snafu)]
#[snafu(module)]
pub(crate) enum PrepareRestoreError {
    #[snafu(display("could not open live database for restore"))]
    Open { source: OpenStoreError },
    #[snafu(display("could not checkpoint or lock live database: {source}"))]
    Sqlite { source: sqlite::Error },
}
impl PrepareRestoreError {
    pub(super) fn is_busy(&self) -> bool {
        matches!(self, Self::Sqlite { source } if matches!(source.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked))
    }
}

pub(super) struct RestoreDatabase {
    _connection: SqliteConnection,
}
impl RestoreDatabase {
    pub(super) fn prepare(path: &Path) -> Result<Self, PrepareRestoreError> {
        let path = path.to_str().ok_or(PrepareRestoreError::Open {
            source: OpenStoreError::InvalidPath,
        })?;
        let mut connection =
            SqliteConnection::establish(path).map_err(|source| PrepareRestoreError::Open {
                source: OpenStoreError::Connect { source },
            })?;
        let mut sql = ConnectionSql::new(&mut connection);
        sql.checkpoint_for_restore()?;
        sql.lock_for_restore()?;
        // No SQL may run after the VFS lock: only closing this dedicated connection releases it.
        Ok(Self {
            _connection: connection,
        })
    }
}

impl ConnectionSql<'_> {
    fn sqlite_error(&mut self) -> sqlite::Error {
        // SAFETY: Reads the error code without retaining the handle or changing Diesel state.
        let code = unsafe {
            self.connection
                .with_raw_connection(|connection| sqlite::sqlite3_extended_errcode(connection))
        };
        sqlite::Error::new(code)
    }

    fn checkpoint_for_restore(&mut self) -> Result<(), PrepareRestoreError> {
        // Load the schema so both corrupt headers and malformed b-trees are checked before WAL access.
        let result = self
            .connection
            .batch_execute("SELECT name FROM sqlite_schema LIMIT 1")
            .and_then(|()| self.checkpoint());
        match result {
            Ok(true) => Ok(()),
            Ok(false) => Err(PrepareRestoreError::Sqlite {
                source: sqlite::Error::new(sqlite::SQLITE_BUSY),
            }),
            Err(_) => {
                let source = self.sqlite_error();
                match source.code {
                    ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase => Ok(()),
                    _ => Err(PrepareRestoreError::Sqlite { source }),
                }
            }
        }
    }

    fn lock_for_restore(&mut self) -> Result<(), PrepareRestoreError> {
        // SAFETY: SQLite owns the file and method table for this callback. We only upgrade its
        // OS lock, never change transactions or statements, and run no SQL before connection close.
        let status = unsafe {
            self.connection.with_raw_connection(|connection| {
                let mut file: *mut sqlite::sqlite3_file = ptr::null_mut();
                let status = sqlite::sqlite3_file_control(
                    connection,
                    c"main".as_ptr(),
                    sqlite::SQLITE_FCNTL_FILE_POINTER,
                    ptr::from_mut(&mut file).cast(),
                );
                if status != sqlite::SQLITE_OK {
                    return status;
                }
                let Some(file_pointer) = file.as_ref() else {
                    return sqlite::SQLITE_NOTFOUND;
                };
                let Some(methods) = file_pointer.pMethods.as_ref() else {
                    return sqlite::SQLITE_NOTFOUND;
                };
                let Some(lock) = methods.xLock else {
                    return sqlite::SQLITE_NOTFOUND;
                };
                RestoreLock::acquire_exclusive(|level| lock(file, level.code()))
            })
        };
        if status == sqlite::SQLITE_OK {
            Ok(())
        } else {
            Err(PrepareRestoreError::Sqlite {
                source: sqlite::Error::new(status),
            })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestoreLock {
    Shared,
    Reserved,
    Exclusive,
}
impl RestoreLock {
    fn code(self) -> i32 {
        match self {
            Self::Shared => sqlite::SQLITE_LOCK_SHARED,
            Self::Reserved => sqlite::SQLITE_LOCK_RESERVED,
            Self::Exclusive => sqlite::SQLITE_LOCK_EXCLUSIVE,
        }
    }
    fn acquire_exclusive(mut lock: impl FnMut(Self) -> i32) -> i32 {
        for level in [Self::Shared, Self::Reserved, Self::Exclusive] {
            let status = lock(level);
            if status != sqlite::SQLITE_OK {
                return status;
            }
        }
        sqlite::SQLITE_OK
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_lock_requires_reserved_before_exclusive() {
        let mut previous = None;
        let status = RestoreLock::acquire_exclusive(|level| {
            match level {
                RestoreLock::Shared => assert_eq!(previous, None),
                RestoreLock::Reserved => assert_eq!(previous, Some(RestoreLock::Shared)),
                RestoreLock::Exclusive => assert_eq!(
                    previous,
                    Some(RestoreLock::Reserved),
                    "VFS requires a reserved lock before upgrading"
                ),
            }
            previous = Some(level);
            sqlite::SQLITE_OK
        });
        assert_eq!(status, sqlite::SQLITE_OK);
        assert_eq!(previous, Some(RestoreLock::Exclusive));
    }

    #[test]
    fn restore_lock_stops_on_each_failed_upgrade() {
        const ORDERED_LEVELS: [RestoreLock; 3] = [
            RestoreLock::Shared,
            RestoreLock::Reserved,
            RestoreLock::Exclusive,
        ];
        for blocked in ORDERED_LEVELS {
            let mut calls = Vec::new();
            let status = RestoreLock::acquire_exclusive(|level| {
                calls.push(level);
                if level == blocked {
                    sqlite::SQLITE_BUSY
                } else {
                    sqlite::SQLITE_OK
                }
            });
            let error = PrepareRestoreError::Sqlite {
                source: sqlite::Error::new(status),
            };
            assert!(
                error.is_busy(),
                "failure at {blocked:?} must propagate busy"
            );
            assert_eq!(
                calls,
                ORDERED_LEVELS
                    .into_iter()
                    .take_while(|level| *level != blocked)
                    .chain([blocked])
                    .collect::<Vec<_>>(),
                "no upgrade may follow {blocked:?} failure"
            );
        }
    }
}
