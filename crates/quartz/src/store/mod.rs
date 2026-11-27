//! SQLite connections, schema migrations, and offline backup recovery.

pub(crate) mod annotations;
pub(crate) mod browser;
pub(crate) mod capture;
pub(crate) mod categories;
pub(crate) mod data;
mod enums;
pub(crate) mod facts;
mod identifiers;
mod migrations;
pub(crate) mod query;
pub(crate) mod query_error;
mod raw;
mod restore;
mod rows;
pub(crate) mod rules;
mod schema;
pub(crate) mod settings;
pub(crate) mod versions;

use crate::data_directory::DataDirectory;
use diesel::{Connection, SqliteConnection, migration::MigrationSource};
use diesel_migrations::MigrationHarness;
use migrations::{ApplyMigrationError, Migrations};
use raw::ConnectionSql;
use snafu::{OptionExt, ResultExt, Snafu};
use std::{
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

#[derive(Debug, Snafu)]
pub(crate) enum OpenStoreError {
    #[snafu(display("database path is not valid Unicode"))]
    InvalidPath,
    #[snafu(display("could not open SQLite database"))]
    Connect { source: diesel::ConnectionError },
    #[snafu(display("could not inspect SQLite database"))]
    Inspect { source: diesel::result::Error },
    #[snafu(display("could not parse SQLite version {version}"))]
    SqliteVersion {
        version: String,
        source: ParseSqliteVersionError,
    },
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

/// Recovery information needed when opening the core database fails.
pub(crate) enum StartupStoreFailure<'error> {
    NewerDatabase,
    MigrationFailed { backup: &'error Path },
    Other,
}
impl OpenStoreError {
    pub(crate) fn startup_failure(&self) -> StartupStoreFailure<'_> {
        match self {
            Self::NewerDatabase { .. } => StartupStoreFailure::NewerDatabase,
            Self::Migrate { backup, .. } => StartupStoreFailure::MigrationFailed { backup },
            _ => StartupStoreFailure::Other,
        }
    }
}

/// Owns both connections on the database thread.
pub(crate) struct Store {
    /// Database file used to reopen the query reader.
    path: PathBuf,
    /// Data directory for backups and size reporting.
    directory: DataDirectory,
    writer: SqliteConnection,
    reader: Option<ReadOnlyStore>,
    pending_effects: versions::TxEffects,
}
impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").finish_non_exhaustive()
    }
}

/// Connection and invalidations owned by one immediate write transaction.
struct WriteTx<'connection> {
    connection: &'connection mut SqliteConnection,
    effects: &'connection mut versions::TxEffects,
}

impl Store {
    #[cfg(test)]
    pub(crate) fn capture<'connection>(
        &'connection mut self,
        keys: &'connection mut capture::CaptureKeys,
        metadata: &'connection capture::RunMetadata,
    ) -> capture::Capture<'connection> {
        capture::Capture::new(&mut self.writer, keys, metadata)
    }

    /// Publishes transaction effects only after SQLite has committed the writes.
    fn write<T, E>(
        &mut self,
        execute: impl FnOnce(&mut WriteTx<'_>) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<diesel::result::Error>,
    {
        let mut effects = versions::TxEffects::default();
        let value = self.writer.immediate_transaction(|connection| {
            execute(&mut WriteTx {
                connection,
                effects: &mut effects,
            })
        })?;
        self.pending_effects.merge(effects);
        Ok(value)
    }

    pub(crate) fn capture_transaction<T, E>(
        &mut self,
        keys: &mut capture::CaptureKeys,
        metadata: &capture::RunMetadata,
        execute: impl FnOnce(&mut capture::Capture<'_>) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<diesel::result::Error>,
    {
        keys.begin();
        let result = self.write(|tx| {
            let mut capture = capture::Capture::new(tx.connection, keys, metadata);
            let result = execute(&mut capture);
            tx.effects.merge(capture.effects);
            result
        });
        keys.finish(if result.is_ok() {
            capture::KeyTransaction::Commit
        } else {
            capture::KeyTransaction::Rollback
        });
        result
    }

    pub(crate) fn rule_sets(&mut self) -> rules::RuleSets<'_> {
        rules::RuleSets::new(&mut self.writer)
    }

    pub(crate) fn rule_transaction<T, E>(
        &mut self,
        execute: impl FnOnce(&mut rules::RuleSetTransaction<'_>) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<diesel::result::Error>,
    {
        self.write(|tx| execute(&mut rules::RuleSetTransaction::new(tx.connection)))
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
        let mut source = ApplyMigrationError::from_result(migrations.migrations())
            .context(LoadMigrationsSnafu)?;
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
            // An empty migration history uses the initial version marker in its backup name.
            let old = applied
                .last()
                .map(String::as_str)
                .unwrap_or(INITIAL_DATABASE_VERSION);
            let name = BackupName {
                version: old.to_owned(),
                timestamp: jiff::Timestamp::now().as_millisecond(),
            };
            let backup = backup_directory.join(name.to_string());
            ConnectionSql::new(&mut writer)
                .backup(backup.to_str().context(InvalidPathSnafu)?)
                .context(BackupSnafu { path: &backup })?;
            tracing::info!(backup = %backup.display(), "upgrading database");
            // Initialize Diesel's tracking table only after version checks and a usable backup.
            ApplyMigrationError::from_result(writer.applied_migrations())
                .context(MigrateSnafu { backup: &backup })?;
            for migration in pending {
                ApplyMigrationError::from_result(writer.run_migration(migration.as_ref()))
                    .context(MigrateSnafu { backup: &backup })?;
            }
            directory.prune_backups();
        }
        let reader = ReadOnlyStore::connect(&path)?;
        Ok(Self {
            path,
            directory: directory.clone(),
            writer,
            reader: Some(reader),
            pending_effects: versions::TxEffects::default(),
        })
    }
}

impl ConnectionSql<'_> {
    fn check_sqlite(&mut self) -> Result<(), OpenStoreError> {
        let version = self.sqlite_version().context(InspectSnafu)?;
        let parsed = version
            .parse::<SqliteVersion>()
            .context(SqliteVersionSnafu { version: &version })?;
        snafu::ensure!(parsed >= SqliteVersion::MINIMUM, OldSqliteSnafu { version });
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
        let source = ApplyMigrationError::from_result(Migrations.migrations())
            .context(LoadMigrationsSnafu)?;
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

/// Version marker for a database before its first migration.
const INITIAL_DATABASE_VERSION: &str = "0";
/// Keep the two newest automatic snapshots for migration recovery.
const RETAINED_MIGRATION_BACKUPS: usize = 2;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct BackupName {
    version: String,
    pub(super) timestamp: i64,
}
impl BackupName {
    /// Recognizes automatic backup names; unrelated files remain untouched.
    pub(super) fn parse(name: &str) -> Option<Self> {
        let body = name.strip_prefix("pre-migrate-")?.strip_suffix(".db")?;
        let (version, timestamp) = body.split_once('-')?;
        if version.is_empty() || !version.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let Ok(timestamp) = timestamp.parse() else {
            return None;
        };
        Some(Self {
            version: version.to_owned(),
            timestamp,
        })
    }
}
impl std::fmt::Display for BackupName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "pre-migrate-{}-{}.db",
            self.version, self.timestamp
        )
    }
}
#[derive(Debug, Snafu)]
#[snafu(module(prune_backups_error))]
enum PruneMigrationBackupsError {
    #[snafu(display("could not list migration backups: {source}"))]
    ReadDirectory { source: std::io::Error },
    #[snafu(display("could not read migration backup entry: {source}"))]
    ReadEntry { source: std::io::Error },
    #[snafu(display("could not inspect migration backup {}: {source}", path.display()))]
    InspectEntry {
        path: PathBuf,
        source: std::io::Error,
    },
    #[snafu(display("could not remove migration backup {}: {source}", path.display()))]
    RemoveFile {
        path: PathBuf,
        source: std::io::Error,
    },
}

struct MigrationBackup {
    path: PathBuf,
    name: BackupName,
}
impl DataDirectory {
    fn prune_backups(&self) {
        if let Err(error) = self.try_prune_backups() {
            tracing::warn!(%error, "could not prune migration backups");
        }
    }
    fn try_prune_backups(&self) -> Result<(), PruneMigrationBackupsError> {
        let mut files = Vec::new();
        for entry in
            fs::read_dir(self.backups()).context(prune_backups_error::ReadDirectorySnafu)?
        {
            let entry = entry.context(prune_backups_error::ReadEntrySnafu)?;
            let path = entry.path();
            if !entry
                .file_type()
                .context(prune_backups_error::InspectEntrySnafu { path: &path })?
                .is_file()
            {
                continue;
            }
            if let Some(name) = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(BackupName::parse)
            {
                files.push(MigrationBackup { path, name });
            }
        }
        files.sort_by_key(|backup| backup.name.timestamp);
        let remove_count = files.len().saturating_sub(RETAINED_MIGRATION_BACKUPS);
        for backup in files.into_iter().take(remove_count) {
            fs::remove_file(&backup.path)
                .context(prune_backups_error::RemoveFileSnafu { path: &backup.path })?;
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SqliteVersion {
    major: u32,
    minor: u32,
    patch: u32,
}
impl SqliteVersion {
    const MINIMUM: Self = Self {
        major: 3,
        minor: 51,
        patch: 3,
    };
}
#[derive(Debug, Snafu)]
#[snafu(module(sqlite_version_error))]
pub(crate) enum ParseSqliteVersionError {
    #[snafu(display("SQLite version must have major, minor, and patch components"))]
    Components,
    #[snafu(display("SQLite version component is not an unsigned integer"))]
    Integer { source: std::num::ParseIntError },
}
impl FromStr for SqliteVersion {
    type Err = ParseSqliteVersionError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut parts = value.split('.');
        let major = parts
            .next()
            .context(sqlite_version_error::ComponentsSnafu)?
            .parse()
            .context(sqlite_version_error::IntegerSnafu)?;
        let minor = parts
            .next()
            .context(sqlite_version_error::ComponentsSnafu)?
            .parse()
            .context(sqlite_version_error::IntegerSnafu)?;
        let patch = parts
            .next()
            .context(sqlite_version_error::ComponentsSnafu)?
            .parse()
            .context(sqlite_version_error::IntegerSnafu)?;
        snafu::ensure!(
            parts.next().is_none(),
            sqlite_version_error::ComponentsSnafu
        );
        Ok(Self {
            major,
            minor,
            patch,
        })
    }
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
    #[snafu(display("could not prepare live database for restore: {source}"))]
    Prepare {
        source: restore::PrepareRestoreError,
    },
    #[snafu(display("could not replace database"))]
    Replace { source: std::io::Error },
}

impl DataDirectory {
    /// Restores a consistent staged copy; the caller holds the data-directory lock.
    pub(crate) fn restore_backup(&self, file: &Path) -> Result<(), RestoreBackupError> {
        let mut backup = ReadOnlyStore::connect(file).context(OpenSnafu)?;
        let source = ApplyMigrationError::from_result(Migrations.migrations())
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
            let current = restore::RestoreDatabase::prepare(&database).map_err(|error| {
                if error.is_busy() {
                    RestoreBackupError::Busy
                } else {
                    RestoreBackupError::Prepare { source: error }
                }
            })?;
            // SQLite's Windows handles must close before deleting sidecars or replacing the file.
            // The caller retains the data-directory lock throughout this operation.
            drop(current);
        }
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut sidecar = database.as_os_str().to_os_string();
            sidecar.push(suffix);
            match fs::remove_file(PathBuf::from(sidecar)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => return Err(RestoreBackupError::Replace { source }),
            }
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
    #[snafu(display("could not access installation identity"))]
    Setting {
        source: settings::AccessSettingError,
    },
}
#[cfg(any(windows, test))]
impl From<diesel::result::Error> for LoadDeviceIdError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Database { source }
    }
}
#[cfg(any(windows, test))]
impl Store {
    /// Bound SQLite lock waits by the current operation or shutdown budget.
    pub(crate) fn limit_write_wait(&mut self, remaining: std::time::Duration) {
        const NORMAL_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
        self.writer
            .set_busy_timeout(remaining.min(NORMAL_LOCK_WAIT));
    }
    pub(crate) fn installation_id(&mut self, proposed: &str) -> Result<String, LoadDeviceIdError> {
        self.write(|tx| {
            let mut settings = settings::Settings::new(tx.connection);
            if let Some(value) = settings
                .device_id()
                .map_err(|source| LoadDeviceIdError::Setting { source })?
            {
                return Ok(value);
            }
            settings
                .put_device_id(proposed)
                .map_err(|source| LoadDeviceIdError::Setting { source })?;
            Ok(proposed.to_owned())
        })
    }
}
