use super::*;
use diesel::prelude::*;
use diesel::{
    connection::SimpleConnection,
    sql_types::{Integer, Text},
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

fn empty() -> SqliteConnection {
    let mut conn = SqliteConnection::establish(":memory:").expect("open memory database");
    Migrations.apply_all(&mut conn).expect("apply schema v1");
    conn
}

#[test]
fn migrate_empty_and_reopen_without_another_backup() {
    let directory = tempfile::tempdir().expect("temporary data directory");
    drop(Store::open(&DataDirectory::new(directory.path())).expect("first startup"));
    let before = fs::read_dir(directory.path().join("backups"))
        .expect("backup directory")
        .count();
    drop(Store::open(&DataDirectory::new(directory.path())).expect("repeat startup"));
    assert_eq!(
        fs::read_dir(directory.path().join("backups"))
            .expect("backup directory")
            .count(),
        before
    );
}

#[test]
fn enum_labels_match_every_rust_enum_exactly() {
    #[derive(Debug, Queryable, PartialEq, Eq, PartialOrd, Ord)]
    struct EnumLabel {
        enum_name: String,
        code: i32,
        label: String,
    }
    impl EnumLabel {
        fn all<T: enums::IntegerCode>() -> Vec<Self> {
            T::VARIANTS
                .iter()
                .map(|value| Self {
                    enum_name: T::COLUMN.to_string(),
                    code: value.code(),
                    label: value.label().to_owned(),
                })
                .collect()
        }
    }

    use schema::enum_labels::dsl::{code, enum_labels, enum_name, label};
    let mut conn = empty();
    let mut actual = enum_labels
        .select((enum_name, code, label))
        .load::<EnumLabel>(&mut conn)
        .expect("read enum labels");
    let mut expected = [
        EnumLabel::all::<enums::RunEndReason>(),
        EnumLabel::all::<enums::ClockEndReason>(),
        EnumLabel::all::<enums::ApplicationKind>(),
        EnumLabel::all::<enums::IntervalKind>(),
        EnumLabel::all::<enums::NoForegroundReason>(),
        EnumLabel::all::<enums::TitleStatus>(),
        EnumLabel::all::<enums::CaptureLevel>(),
        EnumLabel::all::<enums::IntervalEndReason>(),
        EnumLabel::all::<enums::AvailabilityReason>(),
        EnumLabel::all::<enums::AvailabilityEndReason>(),
        EnumLabel::all::<enums::IdleException>(),
        EnumLabel::all::<enums::CategoryTendency>(),
        EnumLabel::all::<enums::RuleSetKind>(),
        EnumLabel::all::<enums::RuleSetSource>(),
        EnumLabel::all::<enums::AnnotationKind>(),
        EnumLabel::all::<enums::DataSource>(),
        EnumLabel::all::<enums::GapReason>(),
        EnumLabel::all::<enums::TimedPolicyKind>(),
        EnumLabel::all::<enums::PrivacyMode>(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    actual.retain(|row| {
        expected
            .iter()
            .any(|known| known.enum_name == row.enum_name)
    });
    actual.sort();
    expected.sort();
    assert_eq!(
        actual, expected,
        "migration labels must match every retained Rust enum"
    );
}

#[test]
fn every_enum_code_round_trips() {
    fn check<T: enums::IntegerCode + std::fmt::Debug + PartialEq>() {
        for &value in T::VARIANTS {
            assert_eq!(
                T::try_from(value.code()).ok(),
                Some(value),
                "{} code {} must round-trip",
                T::COLUMN,
                value.code()
            );
        }
    }

    check::<enums::RunEndReason>();
    check::<enums::ClockEndReason>();
    check::<enums::ApplicationKind>();
    check::<enums::IntervalKind>();
    check::<enums::NoForegroundReason>();
    check::<enums::TitleStatus>();
    check::<enums::CaptureLevel>();
    check::<enums::IntervalEndReason>();
    check::<enums::AvailabilityReason>();
    check::<enums::AvailabilityEndReason>();
    check::<enums::IdleException>();
    check::<enums::CategoryTendency>();
    check::<enums::RuleSetKind>();
    check::<enums::RuleSetSource>();
    check::<enums::AnnotationKind>();
    check::<enums::DataSource>();
    check::<enums::GapReason>();
    check::<enums::TimedPolicyKind>();
    check::<enums::PrivacyMode>();
}

#[test]
fn unknown_sql_code_returns_decode_enum_error() {
    let mut conn = empty();
    #[derive(Debug, QueryableByName)]
    struct Unknown {
        #[diesel(sql_type = Integer)]
        _value: enums::TitleStatus,
    }
    for code in [i32::MIN, -1, 0, 999, i32::MAX] {
        let error = diesel::sql_query("SELECT ? AS _value")
            .bind::<Integer, _>(code)
            .get_result::<Unknown>(&mut conn)
            .expect_err("unknown enum code must fail through SQL decoding");
        let diesel::result::Error::DeserializationError(source) = error else {
            panic!("expected a deserialization error for {code}: {error}");
        };
        let cause = source
            .downcast_ref::<enums::DecodeEnumError>()
            .expect("SQL decoding must preserve DecodeEnumError");
        assert_eq!(
            cause.enum_name,
            <enums::TitleStatus as enums::EnumColumn>::COLUMN
        );
        assert_eq!(
            cause.to_string(),
            format!("unknown title_status code {code}")
        );
        assert_eq!(cause.code, code);
    }
}

#[test]
fn newer_database_is_refused_without_modifying_any_database_bytes() {
    let directory = tempfile::tempdir().expect("temporary data directory");
    drop(Store::open(&DataDirectory::new(directory.path())).expect("initial migration"));
    let path = directory.path().join("quartz.db");
    {
        let mut conn = SqliteConnection::establish(path.to_str().expect("database path"))
            .expect("open database");
        conn.batch_execute(
            "INSERT INTO __diesel_schema_migrations(version) VALUES ('99999999999999');",
        )
        .expect("simulate future migration");
    }
    let before = fs::read(&path).expect("database bytes");
    assert!(matches!(
        Store::open(&DataDirectory::new(directory.path())),
        Err(OpenStoreError::NewerDatabase { .. })
    ));
    assert_eq!(fs::read(&path).expect("unchanged database"), before);
}

#[derive(QueryableByName, Debug, PartialEq, Eq)]
struct SchemaEntry {
    #[diesel(sql_type = Text)]
    name: String,
    #[diesel(sql_type = Text)]
    sql: String,
}

#[test]
fn fixture_migrates_to_identical_schema_and_keeps_boundary_data() {
    let directory = tempfile::tempdir().expect("fixture directory");
    let path = directory.path().join("quartz.db");
    fs::write(&path, include_bytes!("../../../../tests/fixtures/db/v1.db")).expect("copy fixture");
    drop(Store::open(&DataDirectory::new(directory.path())).expect("migrate fixture"));
    let mut fixture = ReadOnlyStore::connect(&path)
        .expect("open fixture")
        .connection;
    assert_eq!(
        ConnectionSql::new(&mut fixture).schema_entries(),
        ConnectionSql::new(&mut empty()).schema_entries()
    );
    use schema::capture_runs::dsl::capture_runs;
    let runs = capture_runs
        .select(rows::CaptureRunsRow::as_select())
        .load::<rows::CaptureRunsRow>(&mut fixture)
        .expect("read fixture run");
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].started_utc_ms, 1_794_873_600_000);
    assert_eq!(runs[0].end_reason, Some(enums::RunEndReason::UserExit));
    assert!(runs[1].ended_utc_ms.is_none());
    assert!(
        ConnectionSql::new(&mut fixture)
            .is_healthy()
            .expect("fixture integrity")
    );
}

#[test]
fn migration_failure_rolls_back_its_transaction_and_backup_restores() {
    let directory = tempfile::tempdir().expect("data directory");
    drop(Store::open(&DataDirectory::new(directory.path())).expect("initial migration"));
    let path = directory.path().join("quartz.db");
    let mut conn = ReadOnlyStore::connect(&path)
        .expect("database before upgrade")
        .connection;
    let before = ConnectionSql::new(&mut conn).schema_entries();
    drop(conn);
    let mut rust = diesel_migrations::RustMigrationSource::new();
    rust.add_migration(
        "20261117000000",
        "successful",
        diesel_migrations::RustMigration::new(|conn: &mut SqliteConnection| {
            conn.batch_execute("CREATE TABLE successful_upgrade(id INTEGER);")
        }),
    )
    .expect("register successful migration");
    rust.add_migration("20261118000000", "broken", diesel_migrations::RustMigration::new(|conn: &mut SqliteConnection| conn.batch_execute("CREATE TABLE should_rollback(id INTEGER); INSERT INTO settings VALUES ('during-migration', '{}'); SELECT * FROM missing_table;"))).expect("register broken migration");
    struct WithRust(diesel_migrations::RustMigrationSource<SqliteConnection>);
    impl MigrationSource<diesel::sqlite::Sqlite> for WithRust {
        fn migrations(
            &self,
        ) -> diesel::migration::Result<
            Vec<Box<dyn diesel::migration::Migration<diesel::sqlite::Sqlite>>>,
        > {
            let mut migrations = Migrations.migrations()?;
            migrations.extend(self.0.migrations()?);
            Ok(migrations)
        }
    }
    let source = WithRust(rust);
    let error = Store::open_with_migrations(&DataDirectory::new(directory.path()), source)
        .expect_err("migration failure stops startup");
    let OpenStoreError::Migrate { backup, .. } = error else {
        panic!("expected migration error: {error}");
    };
    let mut failed = ReadOnlyStore::connect(&path)
        .expect("partially migrated database")
        .connection;
    let entries = ConnectionSql::new(&mut failed).schema_entries();
    assert!(entries.iter().any(|row| row.name == "successful_upgrade"));
    assert!(!entries.iter().any(|row| row.name == "should_rollback"));
    assert_eq!(
        ConnectionSql::new(&mut failed)
            .applied_versions()
            .expect("applied migrations")
            .len(),
        2
    );
    drop(failed);
    DataDirectory::new(directory.path())
        .restore_backup(&backup)
        .expect("restore pre-migration backup");
    let mut restored = ReadOnlyStore::connect(&path)
        .expect("restored database")
        .connection;
    assert_eq!(ConnectionSql::new(&mut restored).schema_entries(), before);
    assert!(
        ConnectionSql::new(&mut restored)
            .is_healthy()
            .expect("restored integrity")
    );
}

#[test]
fn readonly_connection_settings_and_uri_escaping() {
    let directory = tempfile::tempdir().expect("data directory");
    #[cfg(not(windows))]
    let nested = directory.path().join("data % # ? 中文");
    #[cfg(windows)]
    let nested = directory.path().join("data % # 中文");
    fs::create_dir(&nested).expect("special directory");
    let store = Store::open(&DataDirectory::new(&nested)).expect("special path startup");
    let mut reader = store.reader.expect("reader").connection;
    assert!(
        settings::Settings::new(&mut reader)
            .put_device_id("forbidden")
            .is_err()
    );
    #[derive(QueryableByName)]
    struct Setting {
        #[diesel(sql_type = Integer)]
        foreign_keys: i32,
    }
    assert_eq!(
        diesel::sql_query("PRAGMA foreign_keys")
            .get_result::<Setting>(&mut reader)
            .expect("foreign keys")
            .foreign_keys,
        1
    );
}

#[test]
fn checks_and_partial_indexes_enforce_open_intervals() {
    let mut conn = empty();
    conn.batch_execute("PRAGMA foreign_keys=ON; INSERT INTO capture_runs (id,device_id,win_session_id,app_version,started_utc_ms,confirmed_utc_ms) VALUES (1,'fixture',1,'0',0,0); INSERT INTO clock_segments (id,run_id,start_utc_ms,start_mono_ms,confirmed_utc_ms,confirmed_mono_ms) VALUES (1,1,0,0,0,0);").expect("parents");
    let insert = "INSERT INTO intervals(run_id,sequence,clock_segment_id,kind,title_status,capture_level,session_key,start_utc_ms,start_mono_ms,confirmed_utc_ms,confirmed_mono_ms) VALUES(1,1,1,2,2,1,1,0,0,0,0)";
    conn.batch_execute(insert).expect("first open interval");
    assert!(
        conn.batch_execute(insert).is_err(),
        "one open interval per run"
    );
    assert!(
        conn.batch_execute("UPDATE intervals SET end_utc_ms=1")
            .is_err(),
        "closed rows have no confirmation"
    );
    assert!(
        conn.batch_execute("UPDATE intervals SET end_utc_ms=-1, confirmed_utc_ms=NULL")
            .is_err(),
        "end cannot precede start"
    );
    for view in [
        "intervals_v",
        "availability_intervals_v",
        "web_intervals_v",
        "capture_gaps_v",
        "annotations_v",
    ] {
        diesel::sql_query(format!("SELECT * FROM {view} LIMIT 1"))
            .execute(&mut conn)
            .expect("diagnostic view resolves");
    }
}

#[test]
fn released_migration_files_match_lock_and_no_migration_is_unlisted() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let lock = fs::read_to_string(root.join("migrations.lock")).expect("migration lock");
    let expected: BTreeMap<_, _> = lock
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .map(|line| {
            let (hash, path) = line.split_once("  ").expect("sha256 and relative path");
            (path.to_owned(), hash.to_owned())
        })
        .collect();
    let mut actual = BTreeMap::new();
    for migration in fs::read_dir(root.join("migrations")).expect("migrations") {
        let migration = migration.expect("migration entry");
        for file in fs::read_dir(migration.path()).expect("migration files") {
            let file = file.expect("migration file");
            let path = file.path();
            assert!(path.is_file(), "migration entries must be files");
            let relative = path
                .strip_prefix(root)
                .expect("relative migration path")
                .to_str()
                .expect("Unicode migration name")
                .replace('\\', "/");
            actual.insert(
                relative,
                Sha256::digest(fs::read(path).expect("migration contents"))
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>(),
            );
        }
    }
    assert_eq!(
        actual, expected,
        "released migration content and inventory are immutable"
    );
}

#[test]
fn prune_only_generated_backups_and_keep_two_newest() {
    let directory = tempfile::tempdir().expect("backup directory");
    let root = DataDirectory::new(directory.path());
    fs::create_dir_all(root.backups()).expect("backups directory");
    for name in [
        "pre-migrate-0--42.db",
        "pre-migrate-0-1.db",
        "pre-migrate-20261116000000-2.db",
        "pre-migrate-0-3.db",
        "manual.db",
        "pre-migrate-not-generated.db",
    ] {
        fs::write(root.backups().join(name), []).expect("backup file");
    }
    root.prune_backups();
    assert!(!root.backups().join("pre-migrate-0--42.db").exists());
    assert!(!root.backups().join("pre-migrate-0-1.db").exists());
    assert!(
        root.backups()
            .join("pre-migrate-20261116000000-2.db")
            .exists()
    );
    assert!(root.backups().join("pre-migrate-0-3.db").exists());
    assert_eq!(fs::read_dir(root.backups()).expect("backups").count(), 4);
}

#[test]
fn availability_and_web_open_indexes_and_range_checks() {
    let mut conn = empty();
    conn.batch_execute("PRAGMA foreign_keys=ON; INSERT INTO capture_runs (id,device_id,win_session_id,app_version,started_utc_ms,confirmed_utc_ms) VALUES (1,'fixture',1,'0',0,0); INSERT INTO clock_segments (id,run_id,start_utc_ms,start_mono_ms,confirmed_utc_ms,confirmed_mono_ms) VALUES (1,1,0,0,0,0);").expect("parents");
    let availability = "INSERT INTO availability_intervals(run_id,clock_segment_id,reason,start_utc_ms,start_mono_ms,confirmed_utc_ms,confirmed_mono_ms,policy_version) VALUES(1,1,1,0,0,0,0,1)";
    conn.batch_execute(availability)
        .expect("first idle interval");
    assert!(
        conn.batch_execute(availability).is_err(),
        "one open interval for each availability reason"
    );
    conn.batch_execute(&availability.replace("VALUES(1,1,1,", "VALUES(1,1,2,"))
        .expect("simultaneous locked reason");
    conn.batch_execute("INSERT INTO intervals(id,run_id,sequence,clock_segment_id,kind,title_status,capture_level,session_key,start_utc_ms,start_mono_ms,confirmed_utc_ms,confirmed_mono_ms) VALUES(1,1,1,1,2,2,1,1,0,0,0,0); INSERT INTO browser_streams VALUES(1,'fixture','profile','extension',NULL,0,0);").expect("web parents");
    let web = "INSERT INTO web_intervals(stream_id,interval_id,start_utc_ms,confirmed_utc_ms,tab_title_status,link_status,web_level) VALUES(1,1,0,0,5,1,2)";
    conn.batch_execute(web).expect("first web interval");
    assert!(
        conn.batch_execute(web).is_err(),
        "one open web interval per stream"
    );
    assert!(
        conn.batch_execute("UPDATE web_intervals SET interval_id=999")
            .is_err(),
        "foreign keys enforced"
    );
    assert!(conn.batch_execute("INSERT INTO annotations(uuid,kind,start_utc_ms,end_utc_ms,created_utc_ms,updated_utc_ms) VALUES('invalid',5,1,1,0,0)").is_err(), "annotations require positive duration");
    assert!(conn.batch_execute("INSERT INTO deletion_barriers(start_utc_ms,end_utc_ms,scope,created_utc_ms) VALUES(1,0,'{}',0)").is_err(), "barriers require ordered bounds");
}

impl ConnectionSql<'_> {
    fn schema_entries(&mut self) -> Vec<SchemaEntry> {
        diesel::sql_query("SELECT name, sql FROM sqlite_schema WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' ORDER BY name").load(self.connection).expect("read schema")
    }
}

#[test]
fn timed_policy_constraints_enforce_scope_mode_order_and_foreign_keys() {
    use diesel::result::{DatabaseErrorKind, Error};
    let mut conn = empty();
    conn.batch_execute("PRAGMA foreign_keys = ON; INSERT INTO applications (id, identity_key, kind, exe_path, display_name, first_seen_utc_ms) VALUES (1, 'exe:test', 1, 'test', 'test', 0);").expect("application scope");
    for values in [
        "(1, NULL, NULL, 0, 100)",
        "(2, 3, NULL, 0, 100)",
        "(3, NULL, NULL, 0, 100)",
        "(4, NULL, 1, 0, 100)",
        "(5, NULL, 1, 0, 100)",
    ] {
        conn.batch_execute(&format!("INSERT INTO timed_policies VALUES {values}"))
            .expect("valid policy");
    }
    conn.batch_execute("DELETE FROM timed_policies")
        .expect("clear policies");
    for values in [
        "(2, NULL, NULL, 0, 100)",
        "(1, 1, NULL, 0, 100)",
        "(3, NULL, 1, 0, 100)",
        "(4, NULL, NULL, 0, 100)",
        "(5, NULL, NULL, 0, 100)",
        "(2, 4, NULL, 0, 100)",
        "(6, NULL, NULL, 0, 100)",
        "(1, NULL, NULL, 100, 0)",
        "(1, NULL, NULL, NULL, 100)",
        "(1, NULL, NULL, 0, NULL)",
    ] {
        let error = conn
            .batch_execute(&format!("INSERT INTO timed_policies VALUES {values}"))
            .expect_err("invalid policy");
        assert!(
            matches!(
                error,
                Error::DatabaseError(
                    DatabaseErrorKind::CheckViolation | DatabaseErrorKind::NotNullViolation,
                    _
                )
            ),
            "{values}: {error}"
        );
    }
    let error = conn
        .batch_execute("INSERT INTO timed_policies VALUES (4, NULL, 99, 0, 100)")
        .expect_err("unknown application");
    assert!(matches!(
        error,
        Error::DatabaseError(DatabaseErrorKind::ForeignKeyViolation, _)
    ));
}

#[test]
fn backup_names_round_trip_and_preserve_negative_timestamps() {
    let parsed = BackupName::parse("pre-migrate-20261116000000--42.db").expect("generated backup");
    assert_eq!(parsed.version, "20261116000000");
    assert_eq!(parsed.timestamp, -42);
    assert_eq!(parsed.to_string(), "pre-migrate-20261116000000--42.db");
    for name in [
        "manual.db",
        "pre-migrate--1.db",
        "pre-migrate-v1-1.db",
        "pre-migrate-0-overflow.db",
        "pre-migrate-0-9223372036854775808.db",
    ] {
        assert!(
            BackupName::parse(name).is_none(),
            "unrecognized backup {name}"
        );
    }
}

#[test]
fn sqlite_versions_compare_numerically_and_reject_malformed_components() {
    assert!("3.9.0".parse::<SqliteVersion>().expect("valid version") < SqliteVersion::MINIMUM);
    assert!("3.100.0".parse::<SqliteVersion>().expect("valid version") > SqliteVersion::MINIMUM);
    assert_eq!(
        "3.51.3".parse::<SqliteVersion>().expect("minimum version"),
        SqliteVersion::MINIMUM
    );
    for version in ["3.51", "3.51.3.1"] {
        assert!(
            matches!(
                version.parse::<SqliteVersion>(),
                Err(ParseSqliteVersionError::Components)
            ),
            "{version}"
        );
    }
    for version in ["3.no.3", "3.51.-1", "3.51.4294967296"] {
        assert!(
            matches!(
                version.parse::<SqliteVersion>(),
                Err(ParseSqliteVersionError::Integer { .. })
            ),
            "{version}"
        );
    }
}

#[test]
fn prune_backups_propagates_directory_read_failure() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let directory = DataDirectory::new(temporary.path());
    let error = directory
        .try_prune_backups()
        .expect_err("missing backup directory");
    assert!(
        matches!(error, PruneMigrationBackupsError::ReadDirectory { source } if source.kind() == std::io::ErrorKind::NotFound)
    );
}

#[test]
fn startup_failure_exposes_only_recovery_classification_and_backup() {
    assert!(matches!(
        OpenStoreError::NewerDatabase {
            version: "future".into()
        }
        .startup_failure(),
        StartupStoreFailure::NewerDatabase
    ));
    let backup = PathBuf::from("migration-backup.db");
    let error = OpenStoreError::Migrate {
        backup: backup.clone(),
        source: migrations::ApplyMigrationError::UnregisteredSql,
    };
    let StartupStoreFailure::MigrationFailed { backup: actual } = error.startup_failure() else {
        panic!("expected migration recovery information");
    };
    assert_eq!(actual, backup);
    assert!(matches!(
        OpenStoreError::InvalidPath.startup_failure(),
        StartupStoreFailure::Other
    ));
}

struct RestoreFixture {
    directory: DataDirectory,
    backup: PathBuf,
    _temporary: tempfile::TempDir,
}
impl RestoreFixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().expect("isolated restore");
        let directory = DataDirectory::new(temporary.path());
        let backup = temporary.path().join("backup.db");
        let mut store = Store::open(&directory).expect("live store");
        store
            .test_metadata_write(versions::TestCommit::Commit)
            .expect("backup marker");
        ConnectionSql::new(&mut store.writer)
            .backup(backup.to_str().expect("backup path"))
            .expect("independent backup");
        drop(store.reader.take());
        assert!(
            ConnectionSql::new(&mut store.writer)
                .checkpoint()
                .expect("checkpoint fixture")
        );
        drop(store);
        Self {
            directory,
            backup,
            _temporary: temporary,
        }
    }
    fn assert_restored(&self) {
        let mut restored = Store::open(&self.directory).expect("restored store");
        assert_eq!(restored.rule_read_metadata().expect("backup marker"), 1);
        assert!(
            ConnectionSql::new(&mut restored.writer)
                .is_healthy()
                .expect("integrity")
        );
    }
    fn sidecar(&self, suffix: &str) -> PathBuf {
        let mut path = self.directory.database().into_os_string();
        path.push(suffix);
        PathBuf::from(path)
    }
}

#[test]
fn restore_replaces_not_a_database() {
    let fixture = RestoreFixture::new();
    fs::write(fixture.directory.database(), b"not a database").expect("corrupt file");
    fixture
        .directory
        .restore_backup(&fixture.backup)
        .expect("restore invalid header");
    fixture.assert_restored();
}

#[test]
fn restore_replaces_corrupt_btree() {
    let fixture = RestoreFixture::new();
    let mut bytes = fs::read(fixture.directory.database()).expect("database bytes");
    // The first b-tree page starts after SQLite's 100-byte database header.
    const FIRST_BTREE_HEADER: usize = 100;
    bytes[FIRST_BTREE_HEADER] = 0;
    fs::write(fixture.directory.database(), bytes).expect("corrupt b-tree");
    fixture
        .directory
        .restore_backup(&fixture.backup)
        .expect("restore corrupt b-tree");
    fixture.assert_restored();
}

#[test]
fn restore_removes_surviving_committed_wal_when_main_is_missing() {
    let fixture = RestoreFixture::new();
    let mut live = Store::open(&fixture.directory).expect("live store");
    live.test_metadata_write(versions::TestCommit::Commit)
        .expect("newer marker in WAL");
    let wal = fs::read(fixture.sidecar("-wal")).expect("committed WAL");
    let shm = fs::read(fixture.sidecar("-shm")).expect("shared memory");
    drop(live);
    fs::remove_file(fixture.directory.database()).expect("missing main");
    fs::write(fixture.sidecar("-wal"), wal).expect("surviving WAL");
    fs::write(fixture.sidecar("-shm"), shm).expect("surviving SHM");
    fixture
        .directory
        .restore_backup(&fixture.backup)
        .expect("restore without main");
    assert!(
        !fixture.sidecar("-wal").exists(),
        "old WAL must be gone before opening the replacement"
    );
    assert!(
        !fixture.sidecar("-shm").exists(),
        "old SHM must be gone before opening the replacement"
    );
    fixture.assert_restored();
}

#[test]
fn restore_refuses_busy_live_database_and_keeps_committed_data() {
    let fixture = RestoreFixture::new();
    let mut live = Store::open(&fixture.directory).expect("live store");
    let mut snapshot = live.take_query_reader().expect("reader").snapshot();
    snapshot.begin().expect("hold read lock");
    live.test_metadata_write(versions::TestCommit::Commit)
        .expect("new live data");
    assert!(matches!(
        fixture.directory.restore_backup(&fixture.backup),
        Err(RestoreBackupError::Busy)
    ));
    assert!(fixture.directory.database().exists());
    assert_eq!(live.rule_read_metadata().expect("live data retained"), 2);
    snapshot.finish().expect("release reader");
}

#[test]
fn restore_refuses_idle_wal_connection_and_keeps_live_database() {
    let fixture = RestoreFixture::new();
    let mut live = Store::open(&fixture.directory).expect("live connection");
    live.test_metadata_write(versions::TestCommit::Commit)
        .expect("newer live data");
    assert!(matches!(
        fixture.directory.restore_backup(&fixture.backup),
        Err(RestoreBackupError::Busy)
    ));
    assert_eq!(live.rule_read_metadata().expect("live data"), 2);
}

#[test]
fn restore_refuses_locked_corrupt_database() {
    let fixture = RestoreFixture::new();
    let corrupt = b"not a SQLite database";
    fs::write(fixture.directory.database(), corrupt).expect("corrupt live database");
    let locked = restore::RestoreDatabase::prepare(&fixture.directory.database())
        .expect("hold corrupt file lock");
    assert!(matches!(
        fixture.directory.restore_backup(&fixture.backup),
        Err(RestoreBackupError::Busy)
    ));
    assert!(
        fs::read(fixture.directory.database()).expect("original file retained") == corrupt,
        "busy restore must retain corrupt file"
    );
    drop(locked);
    fixture
        .directory
        .restore_backup(&fixture.backup)
        .expect("restore after lock released");
    fixture.assert_restored();
}

#[test]
fn restore_sidecar_cleanup_failure_keeps_old_and_staged_database() {
    let fixture = RestoreFixture::new();
    let original = fs::read(fixture.directory.database()).expect("original database");
    // A directory at the sidecar path makes removal fail on both Windows and Unix.
    fs::create_dir(fixture.sidecar("-wal")).expect("block sidecar deletion");
    assert!(fixture.directory.restore_backup(&fixture.backup).is_err());
    assert!(
        fs::read(fixture.directory.database()).expect("original database retained") == original,
        "failed restore must retain original file"
    );
    assert!(
        fixture.directory.staged_restore().is_file(),
        "synced replacement remains available"
    );
}

#[test]
fn restore_removes_surviving_hot_journal_when_main_is_missing() {
    const FIXTURE_ROWS: usize = 16;
    const PAYLOAD_BYTES: usize = 4096;
    const CACHE_PAGES: usize = 1;
    const JOURNAL_MAGIC: [u8; 8] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];
    let fixture = RestoreFixture::new();
    let mut live =
        SqliteConnection::establish(fixture.directory.database().to_str().expect("live path"))
            .expect("rollback-mode database");
    live.batch_execute(&format!("PRAGMA journal_mode = DELETE; PRAGMA synchronous = FULL; PRAGMA cache_size = {CACHE_PAGES}; PRAGMA cache_spill = ON; CREATE TABLE journal_fixture (payload BLOB NOT NULL)")).expect("rollback journal setup");
    for _ in 0..FIXTURE_ROWS {
        live.batch_execute(&format!(
            "INSERT INTO journal_fixture VALUES (zeroblob({PAYLOAD_BYTES}))"
        ))
        .expect("pages to spill");
    }
    live.batch_execute(&format!("BEGIN IMMEDIATE; UPDATE settings SET value = '2' WHERE key = 'metadata_version'; UPDATE journal_fixture SET payload = randomblob({PAYLOAD_BYTES})")).expect("spill uncommitted pages and sync journal");
    let journal = fs::read(fixture.sidecar("-journal")).expect("hot rollback journal");
    assert!(
        journal.starts_with(&JOURNAL_MAGIC),
        "fixture must have a synced hot journal header"
    );
    live.batch_execute("ROLLBACK")
        .expect("release fixture connection");
    drop(live);
    fs::remove_file(fixture.directory.database()).expect("missing main database");
    fs::write(fixture.sidecar("-journal"), journal).expect("surviving hot journal");
    fixture
        .directory
        .restore_backup(&fixture.backup)
        .expect("restore without main");
    assert!(
        !fixture.sidecar("-journal").exists(),
        "stale rollback journal must be removed before opening the replacement"
    );
    fixture.assert_restored();
}
