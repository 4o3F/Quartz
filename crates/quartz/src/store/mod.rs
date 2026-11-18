//! SQLite connections, schema migrations, and offline backup recovery.

pub(crate) mod capture;
#[expect(
    dead_code,
    reason = "schema v1 also includes browser and annotation models for later milestones"
)]
mod enums;
pub(crate) mod facts;
mod migrations;
mod raw;
#[expect(
    dead_code,
    reason = "schema v1 also includes browser and annotation models for later milestones"
)]
mod rows;
pub(crate) mod rules;
mod schema;

use crate::data_directory::DataDirectory;
use diesel::{Connection, SqliteConnection, migration::MigrationSource};
use diesel_migrations::MigrationHarness;
use migrations::{Migrations, typed};
use raw::ConnectionSql;
use snafu::{OptionExt, ResultExt, Snafu};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Snafu)]
pub(crate) enum OpenStoreError {
    #[snafu(display("database path is not valid Unicode"))]
    InvalidPath,
    #[snafu(display("could not open SQLite database"))]
    Connect { source: diesel::ConnectionError },
    #[snafu(display("could not inspect SQLite database"))]
    Inspect { source: diesel::result::Error },
    #[snafu(display("SQLite {version} is older than required 3.51.3"))]
    OldSqlite { version: String },
    #[snafu(display("database contains unknown migration {version}"))]
    NewerDatabase { version: String },
    #[snafu(display("could not load embedded migrations"))]
    LoadMigrations {
        source: migrations::ApplyMigrationError,
    },
    #[snafu(display("could not configure database connection: {source}"))]
    Configure { source: diesel::result::Error },
    #[snafu(display("could not create backup directory"))]
    CreateBackupDirectory { source: std::io::Error },
    #[snafu(display("could not create migration backup {}", path.display()))]
    Backup {
        path: PathBuf,
        source: diesel::result::Error,
    },
    #[snafu(display("migration failed; restore backup {}", backup.display()))]
    Migrate {
        backup: PathBuf,
        source: migrations::ApplyMigrationError,
    },
}

/// Owns both connections on the database thread.
pub(crate) struct Store {
    _writer: SqliteConnection,
    _reader: ReadOnlyStore,
}
impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").finish_non_exhaustive()
    }
}

impl Store {
    #[cfg(test)]
    pub(crate) fn capture<'connection>(
        &'connection mut self,
        keys: &'connection mut capture::CaptureKeys,
        metadata: &'connection capture::RunMetadata,
    ) -> capture::Capture<'connection> {
        capture::Capture::new(&mut self._writer, keys, metadata)
    }

    pub(crate) fn capture_transaction<T>(
        &mut self,
        keys: &mut capture::CaptureKeys,
        metadata: &capture::RunMetadata,
        execute: impl FnOnce(&mut capture::Capture<'_>) -> Result<T, capture::ExecuteCaptureBatchError>,
    ) -> Result<T, capture::ExecuteCaptureBatchError> {
        let mut staged = keys.clone();
        let result = self._writer.immediate_transaction(|connection| {
            execute(&mut capture::Capture::new(
                connection,
                &mut staged,
                metadata,
            ))
        })?;
        *keys = staged;
        Ok(result)
    }

    pub(crate) fn rule_sets(&mut self) -> rules::RuleSets<'_> {
        rules::RuleSets::new(&mut self._writer)
    }

    pub(crate) fn transaction<T>(
        &mut self,
        execute: impl FnOnce(
            &mut rules::RuleSetTransaction<'_>,
        ) -> Result<T, rules::ExecuteRuleWritesError>,
    ) -> Result<T, rules::ExecuteRuleWritesError> {
        self._writer.immediate_transaction(|connection| {
            execute(&mut rules::RuleSetTransaction::new(connection))
        })
    }

    /// The caller must hold the data-directory lock for this store's lifetime.
    pub(crate) fn open(directory: &DataDirectory) -> Result<Self, OpenStoreError> {
        Self::open_with_migrations(directory, Migrations)
    }

    fn open_with_migrations(
        directory: &DataDirectory,
        migrations: impl MigrationSource<diesel::sqlite::Sqlite>,
    ) -> Result<Self, OpenStoreError> {
        let path = directory.database();
        let mut source = typed(migrations.migrations()).context(LoadMigrationsSnafu)?;
        source.sort_by(|a, b| a.name().version().cmp(&b.name().version()));
        let applied = if path.exists() {
            // Inspect through mode=ro before any persistent PRAGMA or migration-table creation.
            let mut probe = ReadOnlyStore::connect(&path)?;
            ConnectionSql::new(&mut probe.connection).inspect(&source)?
        } else {
            Vec::new()
        };
        let mut writer = SqliteConnection::establish(path.to_str().context(InvalidPathSnafu)?)
            .context(ConnectSnafu)?;
        ConnectionSql::new(&mut writer).check_sqlite()?;
        ConnectionSql::new(&mut writer)
            .configure_writer()
            .context(ConfigureSnafu)?;
        let pending: Vec<_> = source
            .into_iter()
            .filter(|migration| !applied.contains(&migration.name().version().to_string()))
            .collect();
        if !pending.is_empty() {
            let backup_directory = directory.backups();
            fs::create_dir_all(&backup_directory).context(CreateBackupDirectorySnafu)?;
            let old = applied.last().map(String::as_str).unwrap_or("0");
            let timestamp = jiff::Timestamp::now().as_millisecond();
            let backup = backup_directory.join(format!("pre-migrate-{old}-{timestamp}.db"));
            ConnectionSql::new(&mut writer)
                .backup(backup.to_str().context(InvalidPathSnafu)?)
                .context(BackupSnafu { path: &backup })?;
            tracing::info!(backup = %backup.display(), "upgrading database");
            // Initialize Diesel's tracking table only after version checks and a usable backup.
            typed(writer.applied_migrations()).context(MigrateSnafu { backup: &backup })?;
            for migration in pending {
                typed(writer.run_migration(migration.as_ref()))
                    .context(MigrateSnafu { backup: &backup })?;
            }
            directory.prune_backups();
        }
        let reader = ReadOnlyStore::connect(&path)?;
        Ok(Self {
            _writer: writer,
            _reader: reader,
        })
    }
}

impl ConnectionSql<'_> {
    fn check_sqlite(&mut self) -> Result<(), OpenStoreError> {
        let version = self.sqlite_version().context(InspectSnafu)?;
        let parts: Option<Vec<u32>> = version.split('.').map(|part| part.parse().ok()).collect();
        snafu::ensure!(
            parts
                .as_deref()
                .is_some_and(|p| p.len() == 3 && p >= [3, 51, 3].as_slice()),
            OldSqliteSnafu { version }
        );
        Ok(())
    }
}

impl ConnectionSql<'_> {
    fn inspect(
        &mut self,
        source: &[Box<dyn diesel::migration::Migration<diesel::sqlite::Sqlite>>],
    ) -> Result<Vec<String>, OpenStoreError> {
        self.check_sqlite()?;
        let applied = self.applied_versions().context(InspectSnafu)?;
        for version in &applied {
            snafu::ensure!(
                source
                    .iter()
                    .any(|migration| migration.name().version().to_string() == *version),
                NewerDatabaseSnafu { version }
            );
        }
        Ok(applied)
    }
}

/// Owns a mode=ro connection; opening never migrates or installs rule revisions.
pub(crate) struct ReadOnlyStore {
    connection: SqliteConnection,
}
impl std::fmt::Debug for ReadOnlyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadOnlyStore").finish_non_exhaustive()
    }
}
impl ReadOnlyStore {
    pub(crate) fn open(path: &Path) -> Result<Self, ReadRulesError> {
        Self::open_facts(path).context(read_rules_error::OpenSnafu)
    }
    /// Opens capture facts without migrations or an instance lock.
    pub(crate) fn open_facts(path: &Path) -> Result<Self, OpenStoreError> {
        let mut store = Self::connect(path)?;
        let source = typed(Migrations.migrations()).context(LoadMigrationsSnafu)?;
        ConnectionSql::new(&mut store.connection).inspect(&source)?;
        Ok(store)
    }
    pub(crate) fn rule_sets(&mut self) -> rules::RuleSets<'_> {
        rules::RuleSets::new(&mut self.connection)
    }
    fn connect(path: &Path) -> Result<Self, OpenStoreError> {
        let path = path.to_str().context(InvalidPathSnafu)?.replace('\\', "/");
        // Escape URI delimiters without changing the Windows drive prefix or directory separators.
        let path = path
            .replace('%', "%25")
            .replace('?', "%3F")
            .replace('#', "%23");
        // An empty URI authority preserves Windows UNC paths without treating the server as an authority.
        let path = if path.starts_with("//") {
            format!("//{path}")
        } else {
            path
        };
        let mut conn =
            SqliteConnection::establish(&format!("file:{path}?mode=ro")).context(ConnectSnafu)?;
        ConnectionSql::new(&mut conn)
            .configure_reader()
            .context(ConfigureSnafu)?;
        Ok(Self { connection: conn })
    }
}

impl DataDirectory {
    fn prune_backups(&self) {
        let result = (|| -> std::io::Result<()> {
            let mut files = fs::read_dir(self.backups())?
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(is_migration_backup)
                })
                .collect::<Vec<_>>();
            files.sort_by_key(|path| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.rsplit('-').next())
                    .and_then(|s| s.parse::<i64>().ok())
            });
            let remove_count = files.len().saturating_sub(2);
            for path in files.into_iter().take(remove_count) {
                fs::remove_file(path)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            tracing::warn!(%error, "could not prune migration backups");
        }
    }
}

fn is_migration_backup(name: &str) -> bool {
    name.strip_prefix("pre-migrate-")
        .and_then(|s| s.strip_suffix(".db"))
        .and_then(|s| s.split_once('-'))
        .is_some_and(|(version, time)| {
            !version.is_empty()
                && version.bytes().all(|b| b.is_ascii_digit())
                && time.parse::<i64>().is_ok()
        })
}

#[derive(Debug, Snafu)]
pub(crate) enum RestoreBackupError {
    #[snafu(display("could not open backup"))]
    Open { source: OpenStoreError },
    #[snafu(display("could not validate or stage backup"))]
    Validate { source: diesel::result::Error },
    #[snafu(display("backup failed integrity or foreign key checks"))]
    Corrupt,
    #[snafu(display("database is busy; close other database connections before restoring"))]
    Busy,
    #[snafu(display("could not replace database"))]
    Replace { source: std::io::Error },
}

impl DataDirectory {
    /// Restores a consistent staged copy; the caller holds the data-directory lock.
    pub(crate) fn restore_backup(&self, file: &Path) -> Result<(), RestoreBackupError> {
        let mut backup = ReadOnlyStore::connect(file).context(OpenSnafu)?;
        let source = typed(Migrations.migrations())
            .context(LoadMigrationsSnafu)
            .context(OpenSnafu)?;
        ConnectionSql::new(&mut backup.connection)
            .inspect(&source)
            .context(OpenSnafu)?;
        snafu::ensure!(
            ConnectionSql::new(&mut backup.connection)
                .is_healthy()
                .context(ValidateSnafu)?,
            CorruptSnafu
        );
        let staged = self.staged_restore();
        // A previous interrupted attempt can only leave this staged file, never a partial live DB.
        if staged.exists() {
            fs::remove_file(&staged).context(ReplaceSnafu)?;
        }
        // VACUUM INTO writes an independent snapshot even when the input has a WAL.
        ConnectionSql::new(&mut backup.connection)
            .stage_backup(
                staged
                    .to_str()
                    .context(InvalidPathSnafu)
                    .context(OpenSnafu)?,
            )
            .context(ValidateSnafu)?;
        drop(backup);
        fs::OpenOptions::new()
            .write(true)
            .open(&staged)
            .context(ReplaceSnafu)?
            .sync_all()
            .context(ReplaceSnafu)?;
        let database = self.database();
        if database.exists() {
            let mut current = SqliteConnection::establish(
                database
                    .to_str()
                    .context(InvalidPathSnafu)
                    .context(OpenSnafu)?,
            )
            .context(ConnectSnafu)
            .context(OpenSnafu)?;
            snafu::ensure!(
                ConnectionSql::new(&mut current)
                    .checkpoint()
                    .context(ValidateSnafu)?,
                BusySnafu
            );
            drop(current);
        }
        // rename replaces the destination atomically on both Windows and Unix.
        fs::rename(&staged, &database).context(ReplaceSnafu)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;

#[derive(Debug, Snafu)]
#[snafu(module)]
pub(crate) enum ReadRulesError {
    #[snafu(display("could not open rule database read-only"))]
    Open { source: OpenStoreError },
    #[snafu(display("could not load rule database"))]
    Load { source: rules::LoadRulesError },
}

impl From<diesel::result::Error> for ReadRulesError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Load {
            source: rules::LoadRulesError::Query { source },
        }
    }
}

impl From<rules::LoadRulesError> for ReadRulesError {
    fn from(source: rules::LoadRulesError) -> Self {
        Self::Load { source }
    }
}

#[cfg(any(windows, test))]
#[derive(Debug, Snafu)]
pub(crate) enum LoadDeviceIdError {
    #[snafu(display("could not persist installation identity"))]
    Database { source: diesel::result::Error },
    #[snafu(display("could not decode installation identity"))]
    Decode { source: serde_json::Error },
}
#[cfg(any(windows, test))]
impl From<diesel::result::Error> for LoadDeviceIdError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Database { source }
    }
}
#[cfg(any(windows, test))]
impl Store {
    /// Bound SQLite lock waits by the remaining worker shutdown budget.
    pub(crate) fn limit_write_wait(&mut self, remaining: std::time::Duration) {
        const NORMAL_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
        self._writer
            .set_busy_timeout(remaining.min(NORMAL_LOCK_WAIT));
    }
    pub(crate) fn installation_id(&mut self, proposed: &str) -> Result<String, LoadDeviceIdError> {
        use diesel::{ExpressionMethods, OptionalExtension, QueryDsl, RunQueryDsl};
        use schema::settings;
        self._writer.immediate_transaction(|connection| {
            if let Some(value) = settings::table
                .filter(settings::key.eq("device_id"))
                .select(settings::value)
                .first::<String>(connection)
                .optional()
                .map_err(|source| LoadDeviceIdError::Database { source })?
            {
                return serde_json::from_str(&value)
                    .map_err(|source| LoadDeviceIdError::Decode { source });
            }
            let value = serde_json::to_string(proposed)
                .map_err(|source| LoadDeviceIdError::Decode { source })?;
            diesel::insert_into(settings::table)
                .values((settings::key.eq("device_id"), settings::value.eq(value)))
                .execute(connection)
                .map_err(|source| LoadDeviceIdError::Database { source })?;
            Ok(proposed.to_owned())
        })
    }
}
