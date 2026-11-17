//! SQL that cannot be expressed through Diesel's typed query builder.

use diesel::{
    connection::SimpleConnection,
    prelude::*,
    sql_types::{BigInt, Text},
};

const WRITE_SETTINGS: &str = "PRAGMA busy_timeout = 2000; PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON; PRAGMA secure_delete = ON; PRAGMA cache_size = -2048; PRAGMA temp_store = MEMORY;";
const READ_SETTINGS: &str = "PRAGMA busy_timeout = 2000; PRAGMA query_only = ON; PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON; PRAGMA secure_delete = ON; PRAGMA cache_size = -2048; PRAGMA temp_store = MEMORY;";
const SQLITE_VERSION: &str = "SELECT sqlite_version() AS value";
const HAS_MIGRATIONS: &str = "SELECT count(*) AS value FROM sqlite_schema WHERE type = 'table' AND name = '__diesel_schema_migrations'";
const APPLIED: &str = "SELECT version AS value FROM __diesel_schema_migrations ORDER BY version";
const ALLOW_BACKUP: &str = "PRAGMA query_only = OFF";
const RESTORE_QUERY_ONLY: &str = "PRAGMA query_only = ON";
const BACKUP: &str = "VACUUM INTO ?";
const INTEGRITY: &str = "PRAGMA integrity_check";
const CHECKPOINT: &str = "PRAGMA wal_checkpoint(TRUNCATE)";
const FOREIGN_KEYS: &str = "PRAGMA foreign_key_check";

#[derive(QueryableByName)]
struct TextValue {
    #[diesel(sql_type = Text)]
    value: String,
}
#[derive(QueryableByName)]
struct IntegerValue {
    #[diesel(sql_type = BigInt)]
    value: i64,
}
#[derive(QueryableByName)]
struct Integrity {
    #[diesel(sql_type = Text)]
    integrity_check: String,
}
#[derive(QueryableByName)]
struct ForeignKeyViolation {
    #[diesel(sql_type = Text, column_name = table)]
    _table: String,
}

/// SQL operations borrowing one live SQLite connection.
pub(super) struct ConnectionSql<'connection> {
    pub(super) connection: &'connection mut SqliteConnection,
}
impl<'connection> ConnectionSql<'connection> {
    pub(super) fn new(connection: &'connection mut SqliteConnection) -> Self {
        Self { connection }
    }
    pub(super) fn configure_writer(&mut self) -> QueryResult<()> {
        self.connection.batch_execute(WRITE_SETTINGS)
    }
    pub(super) fn configure_reader(&mut self) -> QueryResult<()> {
        self.connection.batch_execute(READ_SETTINGS)
    }
    pub(super) fn sqlite_version(&mut self) -> QueryResult<String> {
        diesel::sql_query(SQLITE_VERSION)
            .get_result::<TextValue>(self.connection)
            .map(|row| row.value)
    }
    pub(super) fn applied_versions(&mut self) -> QueryResult<Vec<String>> {
        if diesel::sql_query(HAS_MIGRATIONS)
            .get_result::<IntegerValue>(self.connection)?
            .value
            == 0
        {
            return Ok(Vec::new());
        }
        diesel::sql_query(APPLIED)
            .load::<TextValue>(self.connection)
            .map(|rows| rows.into_iter().map(|row| row.value).collect())
    }
    pub(super) fn backup(&mut self, path: &str) -> QueryResult<()> {
        diesel::sql_query(BACKUP)
            .bind::<Text, _>(path)
            .execute(self.connection)
            .map(|_| ())
    }
    pub(super) fn is_healthy(&mut self) -> QueryResult<bool> {
        let checks = diesel::sql_query(INTEGRITY).load::<Integrity>(self.connection)?;
        let violations =
            diesel::sql_query(FOREIGN_KEYS).load::<ForeignKeyViolation>(self.connection)?;
        Ok(checks.len() == 1 && checks[0].integrity_check == "ok" && violations.is_empty())
    }

    /// The source remains protected by mode=ro while VACUUM writes the destination.
    pub(super) fn stage_backup(&mut self, path: &str) -> QueryResult<()> {
        self.connection.batch_execute(ALLOW_BACKUP)?;
        let result = self.backup(path);
        self.connection.batch_execute(RESTORE_QUERY_ONLY)?;
        result
    }

    pub(super) fn checkpoint(&mut self) -> QueryResult<bool> {
        #[derive(QueryableByName)]
        struct Checkpoint {
            #[diesel(sql_type = diesel::sql_types::Integer)]
            busy: i32,
        }
        diesel::sql_query(CHECKPOINT)
            .get_result::<Checkpoint>(self.connection)
            .map(|row| row.busy == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_statement_prepares_on_migrated_database() {
        let mut conn = SqliteConnection::establish(":memory:").expect("open empty database");
        super::super::migrations::Migrations
            .apply_all(&mut conn)
            .expect("migrate empty database");
        for sql in WRITE_SETTINGS
            .split(';')
            .chain(READ_SETTINGS.split(';'))
            .filter(|sql| !sql.trim().is_empty())
            .chain([
                SQLITE_VERSION,
                HAS_MIGRATIONS,
                APPLIED,
                BACKUP,
                INTEGRITY,
                FOREIGN_KEYS,
                CHECKPOINT,
                ALLOW_BACKUP,
                RESTORE_QUERY_ONLY,
            ])
        {
            let explain = format!("EXPLAIN {sql}");
            let query = diesel::sql_query(explain);
            if sql == BACKUP {
                query
                    .bind::<Text, _>("unused.db")
                    .execute(&mut conn)
                    .expect("prepare bound backup");
            } else {
                query
                    .execute(&mut conn)
                    .unwrap_or_else(|error| panic!("prepare {sql}: {error}"));
            }
        }
    }
}
