use super::*;
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
    fn labels<T: enums::IntegerCode>() -> Vec<(String, i32, String)> {
        T::ALL
            .iter()
            .map(|value| (T::COLUMN.to_owned(), value.code(), value.label().to_owned()))
            .collect()
    }

    use schema::enum_labels::dsl::{code, enum_labels, enum_name, label};
    let mut conn = empty();
    let mut actual = enum_labels
        .select((enum_name, code, label))
        .load::<(String, i32, String)>(&mut conn)
        .expect("read enum labels");
    let mut expected = [
        labels::<enums::RunEndReason>(),
        labels::<enums::ClockEndReason>(),
        labels::<enums::ApplicationKind>(),
        labels::<enums::IntervalKind>(),
        labels::<enums::NoForegroundReason>(),
        labels::<enums::TitleStatus>(),
        labels::<enums::CaptureLevel>(),
        labels::<enums::IntervalEndReason>(),
        labels::<enums::AvailabilityReason>(),
        labels::<enums::AvailabilityEndReason>(),
        labels::<enums::IdleException>(),
        labels::<enums::WebEndReason>(),
        labels::<enums::LinkStatus>(),
        labels::<enums::WebLevel>(),
        labels::<enums::IngestOutcome>(),
        labels::<enums::CategoryTendency>(),
        labels::<enums::RuleSetKind>(),
        labels::<enums::RuleSetSource>(),
        labels::<enums::AnnotationKind>(),
        labels::<enums::DataSource>(),
        labels::<enums::GapReason>(),
    ]
    .concat();
    actual.sort();
    expected.sort();
    assert_eq!(
        actual, expected,
        "migration labels must match in both directions"
    );
}

#[test]
fn every_enum_code_round_trips() {
    fn check<T: enums::IntegerCode + std::fmt::Debug + PartialEq>() {
        for &value in T::ALL {
            assert_eq!(
                T::from_code(value.code()),
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
    check::<enums::WebEndReason>();
    check::<enums::LinkStatus>();
    check::<enums::WebLevel>();
    check::<enums::IngestOutcome>();
    check::<enums::CategoryTendency>();
    check::<enums::RuleSetKind>();
    check::<enums::RuleSetSource>();
    check::<enums::AnnotationKind>();
    check::<enums::DataSource>();
    check::<enums::GapReason>();
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
        assert_eq!(cause.enum_name, "title_status");
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
    let mut reader = store._reader.connection;
    assert!(
        diesel::insert_into(schema::settings::table)
            .values((
                schema::settings::key.eq("forbidden"),
                schema::settings::value.eq("{}")
            ))
            .execute(&mut reader)
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
        "pre-migrate-0-1.db",
        "pre-migrate-20261116000000-2.db",
        "pre-migrate-0-3.db",
        "manual.db",
        "pre-migrate-not-generated.db",
    ] {
        fs::write(root.backups().join(name), []).expect("backup file");
    }
    root.prune_backups();
    assert!(!root.backups().join("pre-migrate-0-1.db").exists());
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
