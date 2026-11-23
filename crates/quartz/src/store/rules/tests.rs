use super::*;
use crate::{
    data_directory::DataDirectory,
    rules::registry::{RegistryShell, RunRegistryCommandError},
    store::{ReadOnlyStore, Store},
};
use diesel::connection::SimpleConnection;
use quartz_domain::rules::registry::{
    DecideRegistryCommandError, RegistryCommand, RuleRegistry, RuleWarning, SiblingRevision,
    UpstreamState,
};
use quartz_domain::rules::{ParseContext, RuleBody};
use std::{fs, sync::Arc};

fn source(key: &str, label: &str) -> String {
    format!(
        "format='quartz-rules/1'\n[set]\nkey='{key}'\nkind='privacy'\nname='Test'\n[[rule]]\nid='one'\nlabel='{label}'\npreset='title_hidden'\nwhen=[{{field='app.exe_name',op='equals',value='quartz-test.exe'}}]\n"
    )
}
struct Fixture {
    directory: tempfile::TempDir,
    shell: RegistryShell,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("isolated data directory");
        let mut store = Store::open(&DataDirectory::new(directory.path())).expect("store");
        store.seed_categories().expect("builtin categories");
        let shell = RegistryShell::open(&mut store, test_now()).expect("shell");
        Self {
            directory,
            shell,
            store,
        }
    }
    fn apply(
        &mut self,
        key: &str,
        label: &str,
        base_version: u32,
    ) -> Result<crate::rules::registry::RegistryResult, RunRegistryCommandError> {
        self.shell.run(
            &mut self.store,
            test_now(),
            RegistryCommand::Apply {
                key: key.parse().expect("key"),
                file: "test.toml".into(),
                text: source(key, label),
                base_version,
            },
        )
    }
    fn connection(&self) -> SqliteConnection {
        SqliteConnection::establish(
            self.directory
                .path()
                .join("quartz.db")
                .to_str()
                .expect("Unicode path"),
        )
        .expect("fixture connection")
    }
    fn persisted(&self) -> RegistryState {
        ReadOnlyStore::open(&self.directory.path().join("quartz.db"))
            .expect("read-only store")
            .rule_sets()
            .load()
            .expect("domain state")
    }
    fn version_count(&self) -> i64 {
        rule_set_versions::table
            .count()
            .get_result(&mut self.connection())
            .expect("history count")
    }
}

#[test]
fn batch_failure_rolls_back_all_writes_and_never_acknowledges_the_core() {
    let mut fixture = Fixture::new();
    fixture.apply("user.a", "A", 0).expect("A");
    fixture.apply("user.b", "B", 0).expect("B");
    let before = fixture.shell.snapshots().load();
    let count = fixture.version_count();
    let saved = fixture.persisted();
    fixture.connection().batch_execute("CREATE TRIGGER fail_second_revision BEFORE INSERT ON rule_set_versions WHEN NEW.set_id=(SELECT id FROM rule_sets WHERE key='user.b') BEGIN SELECT RAISE(ABORT, 'fixture rejection'); END;").expect("failure injection");
    let siblings = ["builtin.privacy", "user.a", "user.b"]
        .into_iter()
        .map(|key| SiblingRevision {
            key: key.parse().expect("key"),
            base_version: 1,
        })
        .collect();
    let error = fixture
        .shell
        .run(
            &mut fixture.store,
            test_now(),
            RegistryCommand::Reorder { siblings },
        )
        .expect_err("last write fails");
    assert!(matches!(error, RunRegistryCommandError::Persist { .. }));
    assert_eq!(fixture.version_count(), count);
    let persisted = fixture.persisted();
    for key in saved.sets.keys() {
        let entry = &saved.sets[key];
        assert_eq!(persisted.sets[key].current.version, entry.current.version);
        assert_eq!(persisted.sets[key].versions, entry.versions);
        assert_eq!(
            fixture.shell.core().state().sets[key].current.version,
            entry.current.version
        );
    }
    assert!(Arc::ptr_eq(&before, &fixture.shell.snapshots().load()));
    assert_eq!(fixture.shell.snapshots().load().epoch().get(), 2);
}

#[test]
fn successful_transaction_publishes_once_and_startup_rebuild_matches() {
    let mut fixture = Fixture::new();
    assert_eq!(fixture.shell.snapshots().load().epoch().get(), 0);
    let old = fixture.shell.snapshots().load();
    let first = fixture
        .apply("user.test", "First", 0)
        .expect("first command");
    assert_eq!(first.event.expect("publication").epoch.get(), 1);
    let second = fixture
        .apply("user.test", "Second", 1)
        .expect("second command");
    assert_eq!(second.event.expect("one publication").epoch.get(), 2);
    let published = fixture.shell.snapshots().load();
    assert_ne!(old.hashes(), published.hashes());
    assert_eq!(old.epoch().get(), 0);
    let state = fixture.persisted();
    assert_eq!(
        state.sets[&"user.test".parse().expect("key")]
            .current
            .version,
        2
    );
    assert_eq!(
        RuleRegistry::from_state(state)
            .expect("pure rebuild")
            .snapshots()
            .load()
            .hashes(),
        published.hashes()
    );
    drop(fixture.shell);
    let reopened = RegistryShell::open(
        &mut Store::open(&DataDirectory::new(fixture.directory.path())).expect("reopen store"),
        test_now(),
    )
    .expect("restart");
    assert_eq!(reopened.snapshots().load().hashes(), published.hashes());
    assert_eq!(reopened.snapshots().load().epoch().get(), 0);
}

#[test]
fn stale_and_invalid_commands_do_not_start_a_write_or_change_the_snapshot() {
    let mut fixture = Fixture::new();
    fixture.apply("user.test", "Original", 0).expect("original");
    let before = fixture.shell.snapshots().load();
    let count = fixture.version_count();
    let error = fixture
        .apply("user.test", "Old client", 0)
        .expect_err("stale");
    assert!(matches!(
        error,
        RunRegistryCommandError::Decide {
            source: DecideRegistryCommandError::Stale {
                current_version: Some(1),
                ..
            }
        }
    ));
    let bad = fixture
        .shell
        .run(
            &mut fixture.store,
            test_now(),
            RegistryCommand::Apply {
                key: "user.test".parse().expect("key"),
                file: "bad.toml".into(),
                text: "[set".into(),
                base_version: 1,
            },
        )
        .expect_err("invalid source");
    assert!(matches!(
        bad,
        RunRegistryCommandError::Decide {
            source: DecideRegistryCommandError::Parse { .. }
        }
    ));
    assert_eq!(fixture.version_count(), count);
    assert!(Arc::ptr_eq(&before, &fixture.shell.snapshots().load()));
}

#[test]
fn deletion_retains_relational_history_and_restoring_clears_the_null_marker() {
    let mut fixture = Fixture::new();
    fixture.apply("user.test", "Original", 0).expect("original");
    let original = fixture
        .shell
        .core()
        .state()
        .version(&"user.test".parse().expect("key"), 1)
        .expect("history")
        .content
        .clone();
    let reply = fixture
        .shell
        .run(
            &mut fixture.store,
            test_now(),
            RegistryCommand::Delete {
                key: "user.test".parse().expect("key"),
                base_version: 1,
            },
        )
        .expect("delete");
    assert_eq!(reply.event.expect("publication").epoch.get(), 2);
    let state = fixture.persisted();
    assert!(
        !state
            .installed_sets()
            .iter()
            .any(|entry| entry.set.header.key == "user.test")
    );
    assert_eq!(
        state
            .version(&"user.test".parse().expect("key"), 1)
            .expect("retained version")
            .content,
        original
    );
    let row = rule_sets::table
        .filter(rule_sets::key.eq("user.test"))
        .select(RuleSetsRow::as_select())
        .first::<RuleSetsRow>(&mut fixture.connection())
        .expect("retained row");
    assert!(row.deleted_utc_ms.is_some());
    assert_eq!(row.current_version, 2);
    let restored = fixture
        .apply("user.test", "Restored", 0)
        .expect("restore key");
    assert_eq!(
        restored.reply.versions[&"user.test".parse().expect("key")],
        3
    );
    assert!(
        fixture
            .persisted()
            .installed_sets()
            .iter()
            .any(|entry| entry.set.header.key == "user.test")
    );
    let marker: Option<i64> = rule_sets::table
        .filter(rule_sets::key.eq("user.test"))
        .select(rule_sets::deleted_utc_ms)
        .first(&mut fixture.connection())
        .expect("marker");
    assert!(marker.is_none());
    assert_eq!(
        crate::store::settings::Settings::new(&mut fixture.connection())
            .configuration_count()
            .expect("configuration count"),
        0
    );
}

#[test]
fn builtin_upgrade_uses_the_same_shell_and_keeps_user_overlays() {
    let mut fixture = Fixture::new();
    let mut set = RuleSet::parse(
        "builtin.toml",
        &source("builtin.test", "Original"),
        ParseContext {
            origin: RuleOrigin::Builtin,
            references: &ReferenceCatalog::default(),
        },
    )
    .expect("builtin");
    fixture
        .shell
        .run(
            &mut fixture.store,
            test_now(),
            RegistryCommand::InstallBuiltin {
                set: set.clone(),
                revision: 1,
            },
        )
        .expect("install");
    let overlay = "format='quartz-rules/1'\n[set]\nkey='user.overlay'\nkind='privacy'\nname='Overlay'\nparent='builtin.test'\n[[override]]\ntarget='builtin.test/one'\nlabel='User replacement'\npreset='excluded'\nwhen=[{field='app.exe_name',op='equals',value='quartz-test.exe'}]\n";
    fixture
        .shell
        .run(
            &mut fixture.store,
            test_now(),
            RegistryCommand::Apply {
                key: "user.overlay".parse().expect("key"),
                file: "overlay.toml".into(),
                text: overlay.into(),
                base_version: 0,
            },
        )
        .expect("overlay");
    fixture
        .shell
        .run(
            &mut fixture.store,
            test_now(),
            RegistryCommand::Toggle {
                key: "builtin.test".parse().expect("key"),
                base_version: 1,
                enabled: Enablement::Disabled,
            },
        )
        .expect("metadata");
    set.content.rule[0].label = Some("Changed".into());
    let result = fixture
        .shell
        .run(
            &mut fixture.store,
            test_now(),
            RegistryCommand::InstallBuiltin {
                set: set.clone(),
                revision: 2,
            },
        )
        .expect("upstream upgrade");
    assert_eq!(
        result.reply.versions[&"builtin.test".parse().expect("key")],
        3
    );
    assert_eq!(result.reply.upstream_changes.len(), 1);
    assert_eq!(
        result.reply.upstream_changes[0].state(),
        UpstreamState::UpstreamChanged
    );
    assert_eq!(
        fixture.shell.snapshots().load().upstream_changes(),
        result.reply.upstream_changes
    );
    let state = fixture.persisted();
    assert_eq!(
        state.sets[&"user.overlay".parse().expect("key")]
            .current
            .version,
        1
    );
    assert_eq!(
        state
            .version(&"user.overlay".parse().expect("key"), 1)
            .expect("overlay baseline")
            .parent_version,
        Some(1)
    );
    #[derive(Debug, diesel::Queryable)]
    struct RuleRevisions {
        current: i64,
        builtin: Option<i64>,
    }
    let stored: RuleRevisions = rule_sets::table
        .filter(rule_sets::key.eq("builtin.test"))
        .select((rule_sets::current_version, rule_sets::builtin_revision))
        .first(&mut fixture.connection())
        .expect("separate local and upstream revisions");
    assert_eq!(stored.current, 3);
    assert_eq!(stored.builtin, Some(2));
    let same = fixture
        .shell
        .run(
            &mut fixture.store,
            test_now(),
            RegistryCommand::InstallBuiltin { set, revision: 2 },
        )
        .expect("idempotent install");
    assert!(same.event.is_none());
    assert_eq!(same.reply.epoch, result.reply.epoch);
    assert_eq!(
        crate::store::settings::Settings::new(&mut fixture.connection())
            .configuration_count()
            .expect("configuration count"),
        0
    );
}

#[test]
fn builtin_revision_check_rejects_user_values_and_missing_builtin_values() {
    let mut fixture = Fixture::new();
    fixture.apply("user.test", "User", 0).expect("user");
    let error = diesel::update(rule_sets::table.filter(rule_sets::key.eq("user.test")))
        .set(rule_sets::builtin_revision.eq(Some(1i64)))
        .execute(&mut fixture.connection())
        .expect_err("user cannot carry builtin revision");
    assert!(matches!(
        error,
        diesel::result::Error::DatabaseError(diesel::result::DatabaseErrorKind::CheckViolation, _)
    ));
    for revision in [None, Some(0i64)] {
        assert!(
            diesel::update(rule_sets::table.filter(rule_sets::key.eq("builtin.privacy")))
                .set(rule_sets::builtin_revision.eq(revision))
                .execute(&mut fixture.connection())
                .is_err()
        );
    }
}

#[test]
fn offline_state_loading_changes_no_database_bytes_and_contains_deleted_history() {
    let mut fixture = Fixture::new();
    fixture.apply("user.test", "Original", 0).expect("original");
    fixture
        .shell
        .run(
            &mut fixture.store,
            test_now(),
            RegistryCommand::Delete {
                key: "user.test".parse().expect("key"),
                base_version: 1,
            },
        )
        .expect("delete");
    drop(fixture.shell);
    let database = fixture.directory.path().join("quartz.db");
    let before = fs::read(&database).expect("database bytes");
    let state = ReadOnlyStore::open(&database)
        .expect("read-only store")
        .rule_sets()
        .load()
        .expect("domain state");
    assert_eq!(
        state.sets[&"user.test".parse().expect("key")].deletion,
        RuleSetDeletion::Deleted
    );
    assert!(
        state
            .version(&"user.test".parse().expect("key"), 1)
            .is_some()
    );
    assert_eq!(before, fs::read(&database).expect("unchanged database"));
}

#[test]
fn startup_rebuild_degrades_uncompilable_user_or_builtin_sets_without_sql_decisions() {
    for key in ["user.bad", "builtin.privacy"] {
        let mut fixture = Fixture::new();
        fixture.apply("user.bad", "Bad", 0).expect("bad fixture");
        fixture
            .apply("user.healthy", "Healthy", 0)
            .expect("healthy fixture");
        let directory = DataDirectory::new(fixture.directory.path());
        drop(fixture.shell);
        let mut connection =
            SqliteConnection::establish(directory.database().to_str().expect("path"))
                .expect("fixture connection");
        let id: Option<i64> = rule_sets::table
            .filter(rule_sets::key.eq(key))
            .select(rule_sets::id)
            .first(&mut connection)
            .expect("set id");
        let row = rule_set_versions::table
            .find((id.expect("id"), 1i64))
            .select(RuleSetVersionsRow::as_select())
            .first::<RuleSetVersionsRow>(&mut connection)
            .expect("revision");
        let mut content = row.decode_content().expect("content");
        if let RuleBody::Privacy { when, .. } = &mut content.rule[0].body {
            when[0].op = rules::Operator::Regex;
            when[0].value = "(".into();
        }
        diesel::update(rule_set_versions::table.find((id.expect("id"), 1i64)))
            .set((
                rule_set_versions::content.eq(content.canonical_json()),
                rule_set_versions::content_hash.eq(content.content_hash().to_string()),
            ))
            .execute(&mut connection)
            .expect("older accepted pattern");
        drop(connection);
        let mut store = Store::open(&directory).expect("store");
        let mut shell = RegistryShell::open(&mut store, test_now()).expect("degraded startup");
        assert!(
            matches!(shell.snapshots().load().warnings(), [RuleWarning::Compilation { key: warning_key, .. }] if warning_key.as_str() == key)
        );
        shell
            .run(
                &mut store,
                test_now(),
                RegistryCommand::Apply {
                    key: "user.healthy".parse().expect("key"),
                    file: "healthy.toml".into(),
                    text: source("user.healthy", "Edited"),
                    base_version: 1,
                },
            )
            .expect("healthy edits remain possible");
        assert_eq!(shell.snapshots().load().epoch().get(), 1);
    }
}

#[test]
fn loading_rules_rejects_missing_version_history_with_the_set_key() {
    let mut fixture = Fixture::new();
    fixture
        .apply("user.missing_history", "History", 0)
        .expect("persist rule set");
    let mut connection = fixture.connection();
    let id = rule_sets::table
        .filter(rule_sets::key.eq("user.missing_history"))
        .select(rule_sets::id)
        .first::<Option<i64>>(&mut connection)
        .expect("stored set")
        .expect("persisted set has a primary key");
    diesel::delete(rule_set_versions::table.filter(rule_set_versions::set_id.eq(id)))
        .execute(&mut connection)
        .expect("remove history to simulate corruption");
    let error = RuleSets::new(&mut connection)
        .load()
        .expect_err("missing version history is corrupt");
    assert!(
        matches!(error, LoadRulesError::MissingVersionHistory { set } if set.as_str() == "user.missing_history")
    );
}

fn test_now() -> quartz_domain::capture::UtcInstant {
    quartz_domain::capture::UtcInstant::try_from(0).expect("UTC")
}
