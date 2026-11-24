//! One ordered migration source for embedded SQL and future Rust migrations.

#[cfg(test)]
use diesel::sqlite::SqliteConnection;
use diesel::{
    migration::{Migration, MigrationMetadata, MigrationName, MigrationSource},
    sqlite::Sqlite,
};
#[cfg(test)]
use diesel_migrations::MigrationHarness;
use diesel_migrations::{EmbeddedMigrations, embed_migrations};
use snafu::Snafu;

const SQL: EmbeddedMigrations = embed_migrations!("migrations");

#[derive(Debug, Clone, Copy)]
pub(super) struct Migrations;
impl MigrationSource<Sqlite> for Migrations {
    fn migrations(&self) -> diesel::migration::Result<Vec<Box<dyn Migration<Sqlite>>>> {
        let migrations: Vec<Box<dyn Migration<Sqlite>>> = SQL.migrations()?;
        migrations
            .into_iter()
            .map(|inner| {
                let script = match inner.name().version().to_string().as_str() {
                    "20261116000000" => {
                        include_str!("../../migrations/20261116000000_schema_v1/up.sql")
                    }
                    "20261123000000" => {
                        include_str!(
                            "../../migrations/20261123000000_annotation_label_preset/up.sql"
                        )
                    }
                    _ => return Err(ApplyMigrationError::UnregisteredSql.into()),
                };
                let migration: Box<dyn Migration<Sqlite>> =
                    Box::new(SqlMigration { inner, script });
                Ok(migration)
            })
            .collect()
        // Append Rust Migration implementations to this same ordered source when needed.
    }
}

#[derive(Debug, Snafu)]
pub(crate) enum ApplyMigrationError {
    #[snafu(display("database operation failed during migration"))]
    Database { source: diesel::result::Error },
    #[snafu(display("embedded SQL migration has no registered script"))]
    UnregisteredSql,
    #[snafu(display("could not load migrations"))]
    Load {
        source: diesel_migrations::MigrationError,
    },
    #[snafu(display("migration returned an unsupported error type"))]
    UnexpectedCause,
}

// Diesel's trait boundary erases errors. Recover concrete causes immediately.
impl ApplyMigrationError {
    pub(super) fn from_result<T>(
        result: diesel::migration::Result<T>,
    ) -> Result<T, ApplyMigrationError> {
        result.map_err(|source| {
            let source = match source.downcast::<ApplyMigrationError>() {
                Ok(source) => return *source,
                Err(source) => source,
            };
            let source = match source.downcast::<diesel::result::Error>() {
                Ok(source) => return ApplyMigrationError::Database { source: *source },
                Err(source) => source,
            };
            match source.downcast::<diesel_migrations::MigrationError>() {
                Ok(source) => ApplyMigrationError::Load { source: *source },
                Err(_) => ApplyMigrationError::UnexpectedCause,
            }
        })
    }
}

#[cfg(test)]
impl Migrations {
    pub(super) fn apply_all(&self, conn: &mut SqliteConnection) -> Result<(), ApplyMigrationError> {
        ApplyMigrationError::from_result(conn.run_pending_migrations(*self)).map(|_| ())
    }
}

// Keep SQL failures concrete at Diesel's erased migration boundary.
struct SqlMigration {
    inner: Box<dyn Migration<Sqlite>>,
    script: &'static str,
}
impl Migration<Sqlite> for SqlMigration {
    fn run(
        &self,
        conn: &mut dyn diesel::connection::BoxableConnection<Sqlite>,
    ) -> diesel::migration::Result<()> {
        Ok(conn.batch_execute(self.script)?)
    }
    fn revert(
        &self,
        _conn: &mut dyn diesel::connection::BoxableConnection<Sqlite>,
    ) -> diesel::migration::Result<()> {
        Err(diesel_migrations::MigrationError::NoMigrationRevertFile.into())
    }
    fn metadata(&self) -> &dyn MigrationMetadata {
        self.inner.metadata()
    }
    fn name(&self) -> &dyn MigrationName {
        self.inner.name()
    }
}
