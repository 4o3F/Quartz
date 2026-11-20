//! Simulation creation, benchmark copies, and synthetic classification workloads.

use super::super::{
    BackupSnafu, ConfigureSnafu, ConnectSnafu, InvalidPathSnafu, LoadMigrationsSnafu,
    OpenStoreError, ReadOnlyStore, Store,
    migrations::{ApplyMigrationError, Migrations},
    raw::ConnectionSql,
    schema::applications,
    versions::{BumpVersionError, TxEffects},
};
use diesel::{Connection, ExpressionMethods, QueryDsl, RunQueryDsl};

#[derive(Debug, snafu::Snafu)]
pub(crate) enum PrepareClassificationWorkloadError {
    #[snafu(display("could not update simulation applications: {source}"))]
    Database { source: diesel::result::Error },
    #[snafu(display("simulation application {exe_name} is missing"))]
    MissingApplication { exe_name: String },
    #[snafu(display("could not update simulation metadata version: {source}"))]
    Version { source: BumpVersionError },
}
impl From<diesel::result::Error> for PrepareClassificationWorkloadError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Database { source }
    }
}

impl Store {
    /// Closes the reader first so the final WAL writer can checkpoint the simulation file.
    pub(crate) fn finish_simulation(self) {
        let Self { writer, reader, .. } = self;
        drop(reader);
        drop(writer);
    }

    /// Replaces the synthetic executable names with representative builtin matches.
    pub(crate) fn use_classification_workload(
        &mut self,
    ) -> Result<(), PrepareClassificationWorkloadError> {
        self.write(|tx| {
            for mapping in ClassificationApplication::ALL {
                let path = format!(r"c:\simulation\{}.exe", mapping.original);
                let replacement = format!(r"c:\simulation\{}.exe", mapping.replacement);
                let changed = diesel::update(
                    applications::table
                        .filter(applications::identity_key.eq(format!("exe:{path}"))),
                )
                .set((
                    applications::identity_key.eq(format!("exe:{replacement}")),
                    applications::exe_name.eq(format!("{}.exe", mapping.replacement)),
                ))
                .execute(tx.connection)?;
                if changed != 1 {
                    return Err(PrepareClassificationWorkloadError::MissingApplication {
                        exe_name: format!("{}.exe", mapping.original),
                    });
                }
            }
            tx.effects
                .bump_metadata(tx.connection)
                .map_err(|source| PrepareClassificationWorkloadError::Version { source })?;
            Ok(())
        })
    }

    /// Migrates a caller-reserved, empty simulation file without production backups or locks.
    pub(crate) fn create_simulation(path: &std::path::Path) -> Result<Self, OpenStoreError> {
        use diesel_migrations::MigrationHarness;
        use snafu::{OptionExt, ResultExt};
        let mut writer =
            diesel::SqliteConnection::establish(path.to_str().context(InvalidPathSnafu)?)
                .context(ConnectSnafu)?;
        ConnectionSql::new(&mut writer)
            .configure_writer()
            .context(ConfigureSnafu)?;
        ApplyMigrationError::from_result(writer.run_pending_migrations(Migrations))
            .context(LoadMigrationsSnafu)?;
        Ok(Self {
            writer,
            reader: Some(ReadOnlyStore::connect(path)?),
            pending_effects: TxEffects::default(),
        })
    }
}

struct ClassificationApplication {
    original: &'static str,
    replacement: &'static str,
}
impl ClassificationApplication {
    const ALL: [Self; 4] = [
        Self {
            original: "editor",
            replacement: "code",
        },
        Self {
            original: "browser",
            replacement: "msedge",
        },
        Self {
            original: "terminal",
            replacement: "windowsterminal",
        },
        Self {
            original: "player",
            replacement: "vlc",
        },
    ];
}
impl ReadOnlyStore {
    /// Copies a snapshot for isolated benchmark preparation, including committed WAL data.
    pub(crate) fn copy_simulation(&mut self, path: &std::path::Path) -> Result<(), OpenStoreError> {
        use snafu::{OptionExt, ResultExt};
        let name = path.to_str().context(InvalidPathSnafu)?;
        ConnectionSql::new(&mut self.connection)
            .stage_backup(name)
            .context(BackupSnafu { path })
    }
}
