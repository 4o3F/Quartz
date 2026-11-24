use super::*;
use crate::data_directory::DataDirectory;

#[test]
fn seeding_is_idempotent_preserves_edits_and_bumps_metadata_only_on_insert() {
    let directory = tempfile::tempdir().expect("isolated categories");
    let mut store = Store::open(&DataDirectory::new(directory.path())).expect("store");
    store.seed_categories().expect("seed categories");
    assert_eq!(store.take_committed_versions().metadata_version, Some(1));
    assert_eq!(
        categories::table
            .count()
            .get_result::<i64>(&mut store.writer)
            .expect("count"),
        5
    );
    diesel::update(categories::table.filter(categories::builtin_key.eq("development")))
        .set((
            categories::name.eq("My work"),
            categories::color.eq("cat.2"),
            categories::tendency.eq(enums::CategoryTendency::Neutral),
            categories::sort_order.eq(42_i64),
            categories::archived.eq(1),
        ))
        .execute(&mut store.writer)
        .expect("user edits");
    store.seed_categories().expect("repeat seed");
    assert_eq!(store.take_committed_versions().metadata_version, None);
    let edited = categories::table
        .filter(categories::builtin_key.eq("development"))
        .select(CategoriesRow::as_select())
        .first::<CategoriesRow>(&mut store.writer)
        .expect("edited row");
    assert_eq!(edited.name.as_deref(), Some("My work"));
    assert_eq!(edited.color, "cat.2");
    assert_eq!(edited.tendency, enums::CategoryTendency::Neutral);
    assert_eq!(edited.sort_order, 42);
    assert_eq!(edited.archived, 1);
    diesel::delete(categories::table.filter(categories::builtin_key.eq("system")))
        .execute(&mut store.writer)
        .expect("remove one default");
    store.seed_categories().expect("insert missing default");
    assert_eq!(store.take_committed_versions().metadata_version, Some(2));
}

struct Fixture {
    store: Store,
    _directory: tempfile::TempDir,
}
#[derive(Debug, PartialEq, Eq)]
struct StoredState {
    categories: Vec<String>,
    settings: Vec<StoredSetting>,
}
#[derive(Debug, PartialEq, Eq, diesel::Queryable, diesel::Selectable)]
#[diesel(table_name = super::super::schema::settings)]
struct StoredSetting {
    key: String,
    value: String,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("isolated category database");
        let mut store = Store::open(&DataDirectory::new(directory.path())).expect("store");
        store.seed_categories().expect("builtin categories");
        store.take_committed_versions();
        Self {
            store,
            _directory: directory,
        }
    }
    fn values(&self, parent: Option<CategoryUuid>) -> CategoryValues {
        CategoryValues {
            parent_uuid: parent,
            name: Some("  项目 🦀  ".parse().expect("valid name")),
            color: "cat.14".parse().expect("color"),
            tendency: CategoryTendency::default(),
        }
    }
    fn create(&mut self, number: u32, parent: Option<CategoryUuid>) -> CategoryEntity {
        let command = CreateCategory {
            uuid: Self::uuid(number),
            values: self.values(parent),
        };
        self.store
            .create_category(command)
            .expect("create category")
    }
    fn uuid(number: u32) -> CategoryUuid {
        CategoryUuid(format!("00000000-0000-4000-8000-{number:012x}"))
    }
    fn state(&mut self) -> StoredState {
        let rows = categories::table
            .order(categories::uuid)
            .select(CategoriesRow::as_select())
            .load::<CategoriesRow>(&mut self.store.writer)
            .expect("rows");
        let settings = super::super::schema::settings::table
            .order(super::super::schema::settings::key)
            .select(StoredSetting::as_select())
            .load::<StoredSetting>(&mut self.store.writer)
            .expect("settings");
        StoredState {
            categories: rows.iter().map(|row| format!("{row:?}")).collect(),
            settings,
        }
    }
    fn version(&mut self) -> i64 {
        super::super::settings::Settings::new(&mut self.store.writer)
            .metadata_version()
            .expect("version")
    }
    fn reject<T: std::fmt::Debug>(
        &mut self,
        expected: EditCategoryError,
        execute: impl FnOnce(&mut Store) -> Result<T, ExecuteCategoryCommandError>,
    ) {
        let before = self.state();
        self.store.take_committed_versions();
        let error = execute(&mut self.store).expect_err("invalid command");
        assert!(
            matches!(error, ExecuteCategoryCommandError::Validation { source } if source == expected),
            "{error:?}"
        );
        assert_eq!(self.state(), before, "rejected command changed database");
        self.no_effects();
    }
    fn no_effects(&mut self) {
        let committed = self.store.take_committed_versions();
        assert_eq!(committed.metadata_version, None);
        assert!(committed.facts.is_empty());
    }
    fn archived(&mut self, uuid: CategoryUuid, state: CategoryArchiveState) -> CategoryEntity {
        self.store
            .archive_category(ArchiveCategory { uuid, state })
            .expect("archive state")
    }
    fn all(&mut self) -> Vec<CategoryEntity> {
        self.store
            .list_categories(IncludeArchivedCategories::Include)
            .expect("category list")
    }
}

#[test]
fn create_retry_returns_original_before_checking_parent_or_missing_name() {
    let mut fixture = Fixture::new();
    let original = fixture.create(1, None);
    assert_eq!(
        original.values.name.as_ref().expect("name").as_str(),
        "项目 🦀"
    );
    assert_eq!(original.sort_order, 5);
    assert_eq!(fixture.version(), 2);
    assert_eq!(
        fixture.store.take_committed_versions().metadata_version,
        Some(2)
    );
    let before = fixture.state();
    let retry = fixture
        .store
        .create_category(CreateCategory {
            uuid: original.uuid.clone(),
            values: CategoryValues {
                parent_uuid: Some(Fixture::uuid(999)),
                name: None,
                color: "neutral".parse().expect("color"),
                tendency: CategoryTendency::Focus,
            },
        })
        .expect("existing UUID");
    assert_eq!(retry, original);
    assert_eq!(fixture.state(), before);
    fixture.no_effects();
    let builtin = fixture
        .all()
        .into_iter()
        .find(|row| row.uuid == BuiltinCategory::Development.uuid())
        .expect("builtin");
    let retry = fixture
        .store
        .create_category(CreateCategory {
            uuid: builtin.uuid.clone(),
            values: fixture.values(None),
        })
        .expect("builtin UUID retry");
    assert_eq!(retry, builtin);
    fixture.no_effects();
}

#[test]
fn missing_categories_parents_names_and_reserved_builtin_identity_are_atomic() {
    let mut fixture = Fixture::new();
    let values = fixture.values(None);
    fixture.reject(EditCategoryError::NotFound, |store| {
        store.update_category(UpdateCategory {
            uuid: Fixture::uuid(999),
            values,
        })
    });
    fixture.reject(EditCategoryError::NotFound, |store| {
        store.archive_category(ArchiveCategory {
            uuid: Fixture::uuid(999),
            state: CategoryArchiveState::Archived,
        })
    });
    let mut values = fixture.values(None);
    values.name = None;
    fixture.reject(EditCategoryError::MissingName, |store| {
        store.create_category(CreateCategory {
            uuid: Fixture::uuid(1),
            values,
        })
    });
    let original = fixture.create(1, None);
    let mut values = original.values.clone();
    values.name = None;
    fixture.reject(EditCategoryError::MissingName, |store| {
        store.update_category(UpdateCategory {
            uuid: original.uuid.clone(),
            values,
        })
    });
    let values = fixture.values(Some(Fixture::uuid(999)));
    fixture.reject(EditCategoryError::InvalidParent, |store| {
        store.create_category(CreateCategory {
            uuid: Fixture::uuid(2),
            values,
        })
    });
    let values = fixture.values(Some(Fixture::uuid(999)));
    fixture.reject(EditCategoryError::InvalidParent, |store| {
        store.update_category(UpdateCategory {
            uuid: original.uuid,
            values,
        })
    });
    fixture.reject(EditCategoryError::InvalidParent, |store| {
        store.reorder_categories(ReorderCategories {
            parent_uuid: Some(Fixture::uuid(999)),
            uuids: vec![],
        })
    });
    // A fresh store has no builtin rows until startup seeding runs.
    let directory = tempfile::tempdir().expect("unseeded database");
    fixture.store = Store::open(&DataDirectory::new(directory.path())).expect("unseeded store");
    let values = fixture.values(None);
    fixture.reject(EditCategoryError::BuiltinIdentityViolation, |store| {
        store.create_category(CreateCategory {
            uuid: BuiltinCategory::Development.uuid(),
            values,
        })
    });
}

#[test]
fn invalid_value_construction_cannot_change_database_or_metadata() {
    let mut fixture = Fixture::new();
    let before = fixture.state();
    for name in [
        "".to_owned(),
        " \n\t".to_owned(),
        "🦀".repeat(quartz_domain::categories::MAX_CATEGORY_NAME_SCALARS + 1),
    ] {
        assert!(
            name.parse::<quartz_domain::categories::CategoryName>()
                .is_err()
        );
        assert_eq!(fixture.state(), before);
        fixture.no_effects();
    }
    for color in ["", "cat.0", "cat.15", "cat.01", "cat.+1", "red"] {
        assert!(
            color
                .parse::<quartz_domain::categories::CategoryColor>()
                .is_err()
        );
        assert_eq!(fixture.state(), before);
        fixture.no_effects();
    }
    for tendency in ["", "Focus", "neutral ", "other"] {
        assert!(tendency.parse::<CategoryTendency>().is_err());
        assert_eq!(fixture.state(), before);
        fixture.no_effects();
    }
}

#[test]
fn updates_replace_values_clear_nulls_preserve_builtin_identity_and_retry_without_bump() {
    let mut fixture = Fixture::new();
    let parent = fixture.create(1, None);
    let first_child = fixture.create(2, Some(parent.uuid.clone()));
    let builtin_uuid = BuiltinCategory::Development.uuid();
    let mut values = fixture.values(Some(parent.uuid));
    values.tendency = CategoryTendency::Distraction;
    let before_version = fixture.version();
    let moved = fixture
        .store
        .update_category(UpdateCategory {
            uuid: builtin_uuid.clone(),
            values: values.clone(),
        })
        .expect("move builtin");
    assert_eq!(moved.builtin_key, Some(BuiltinCategory::Development));
    assert_eq!(moved.sort_order, first_child.sort_order + 1);
    assert_eq!(moved.values, values);
    assert_eq!(fixture.version(), before_version + 1);
    fixture.store.take_committed_versions();
    assert_eq!(
        fixture
            .store
            .update_category(UpdateCategory {
                uuid: builtin_uuid.clone(),
                values
            })
            .expect("update retry"),
        moved
    );
    fixture.no_effects();
    let values = CategoryValues {
        parent_uuid: None,
        name: None,
        color: "neutral".parse().expect("color"),
        tendency: CategoryTendency::Neutral,
    };
    let restored = fixture
        .store
        .update_category(UpdateCategory {
            uuid: builtin_uuid.clone(),
            values: values.clone(),
        })
        .expect("restore default name and root");
    assert_eq!(restored.id, moved.id);
    assert_eq!(restored.uuid, moved.uuid);
    assert_eq!(restored.builtin_key, moved.builtin_key);
    assert_eq!(restored.values, values);
    assert_eq!(fixture.version(), before_version + 2);
    let reloaded = fixture
        .all()
        .into_iter()
        .find(|row| row.uuid == builtin_uuid)
        .expect("reloaded builtin");
    assert_eq!(reloaded, restored);
    let changed = CategoryValues {
        color: "cat.2".parse().expect("color"),
        ..reloaded.values
    };
    let before_order = reloaded.sort_order;
    let updated = fixture
        .store
        .update_category(UpdateCategory {
            uuid: reloaded.uuid,
            values: changed,
        })
        .expect("same parent edit");
    assert_eq!(updated.sort_order, before_order);
    assert_eq!(fixture.version(), before_version + 3);
}

#[test]
fn direct_and_deep_cycles_are_rejected_atomically() {
    let mut fixture = Fixture::new();
    let root = fixture.create(1, None);
    let child = fixture.create(2, Some(root.uuid.clone()));
    let descendant = fixture.create(3, Some(child.uuid));
    for parent in [root.uuid.clone(), descendant.uuid] {
        let values = CategoryValues {
            parent_uuid: Some(parent),
            ..root.values.clone()
        };
        fixture.reject(EditCategoryError::Cycle, |store| {
            store.update_category(UpdateCategory {
                uuid: root.uuid.clone(),
                values,
            })
        });
    }
}

#[test]
fn archive_and_restore_do_not_cascade_and_archived_parents_accept_children() {
    let mut fixture = Fixture::new();
    let root = fixture.create(1, None);
    let child = fixture.create(2, Some(root.uuid.clone()));
    let archived_child = fixture.create(3, Some(root.uuid.clone()));
    fixture.archived(archived_child.uuid.clone(), CategoryArchiveState::Archived);
    let before_version = fixture.version();
    let archived = fixture.archived(root.uuid.clone(), CategoryArchiveState::Archived);
    assert_eq!(fixture.version(), before_version + 1);
    fixture.store.take_committed_versions();
    assert_eq!(
        fixture.archived(root.uuid.clone(), CategoryArchiveState::Archived),
        archived
    );
    fixture.no_effects();
    let created_under_archive = fixture.create(4, Some(root.uuid.clone()));
    assert_eq!(
        created_under_archive.sort_order,
        archived_child.sort_order + 1
    );
    let moved = fixture.create(5, None);
    let moved = fixture
        .store
        .update_category(UpdateCategory {
            uuid: moved.uuid,
            values: CategoryValues {
                parent_uuid: Some(root.uuid.clone()),
                ..moved.values
            },
        })
        .expect("move under archived parent");
    assert_eq!(moved.sort_order, created_under_archive.sort_order + 1);
    let all = fixture.all();
    assert_eq!(
        all.iter()
            .find(|row| row.uuid == child.uuid)
            .expect("child"),
        &child
    );
    assert_eq!(
        all.iter()
            .find(|row| row.uuid == archived_child.uuid)
            .expect("archived child")
            .archive_state,
        CategoryArchiveState::Archived
    );
    let active = fixture
        .store
        .list_categories(IncludeArchivedCategories::Exclude)
        .expect("active list");
    assert!(
        active
            .iter()
            .all(|row| row.uuid != root.uuid && row.uuid != archived_child.uuid)
    );
    assert!(active.iter().any(|row| row.uuid == child.uuid));
    fixture.archived(root.uuid.clone(), CategoryArchiveState::Active);
    fixture.store.take_committed_versions();
    assert_eq!(
        fixture
            .archived(root.uuid, CategoryArchiveState::Active)
            .archive_state,
        CategoryArchiveState::Active
    );
    fixture.no_effects();
    assert_eq!(
        fixture
            .all()
            .iter()
            .find(|row| row.uuid == archived_child.uuid)
            .expect("child unchanged")
            .archive_state,
        CategoryArchiveState::Archived
    );
}

#[test]
fn reorder_requires_the_complete_set_including_archives_and_bumps_once() {
    let mut fixture = Fixture::new();
    let parent = fixture.create(1, None);
    let first = fixture.create(2, Some(parent.uuid.clone()));
    let second = fixture.create(3, Some(parent.uuid.clone()));
    let third = fixture.create(4, Some(parent.uuid.clone()));
    fixture.archived(second.uuid.clone(), CategoryArchiveState::Archived);
    for uuids in [
        vec![first.uuid.clone(), third.uuid.clone()],
        vec![first.uuid.clone(), second.uuid.clone(), second.uuid.clone()],
        vec![first.uuid.clone(), second.uuid.clone(), parent.uuid.clone()],
        vec![
            first.uuid.clone(),
            second.uuid.clone(),
            third.uuid.clone(),
            Fixture::uuid(999),
        ],
    ] {
        let parent_uuid = Some(parent.uuid.clone());
        fixture.reject(EditCategoryError::ReorderSetMismatch, |store| {
            store.reorder_categories(ReorderCategories { parent_uuid, uuids })
        });
    }
    let command = ReorderCategories {
        parent_uuid: Some(parent.uuid.clone()),
        uuids: vec![third.uuid, second.uuid, first.uuid],
    };
    let before_version = fixture.version();
    fixture
        .store
        .reorder_categories(command.clone())
        .expect("reorder");
    assert_eq!(fixture.version(), before_version + 1);
    assert_eq!(
        fixture.store.take_committed_versions().metadata_version,
        Some(before_version + 1)
    );
    let children: Vec<_> = fixture
        .all()
        .into_iter()
        .filter(|row| row.values.parent_uuid.as_ref() == Some(&parent.uuid))
        .collect();
    assert_eq!(
        children
            .iter()
            .map(|row| row.uuid.clone())
            .collect::<Vec<_>>(),
        command.uuids
    );
    assert_eq!(
        children
            .iter()
            .map(|row| row.sort_order)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    fixture
        .store
        .reorder_categories(command)
        .expect("reorder retry");
    fixture
        .store
        .reorder_categories(ReorderCategories {
            parent_uuid: Some(children[0].uuid.clone()),
            uuids: vec![],
        })
        .expect("empty sibling set");
    fixture.no_effects();
}

#[test]
fn list_is_preorder_with_uuid_ties_and_gapped_order_retry_is_a_noop() {
    let mut fixture = Fixture::new();
    let root = fixture.create(30, None);
    let child_high = fixture.create(20, Some(root.uuid.clone()));
    let child_low = fixture.create(10, Some(root.uuid.clone()));
    let grandchild = fixture.create(40, Some(child_low.uuid.clone()));
    diesel::update(categories::table.filter(categories::parent_id.eq(root.id.0)))
        .set(categories::sort_order.eq(42_i64))
        .execute(&mut fixture.store.writer)
        .expect("order ties");
    let all = fixture.all();
    let user_order: Vec<_> = all
        .into_iter()
        .filter(|row| row.builtin_key.is_none())
        .map(|row| row.uuid)
        .collect();
    assert_eq!(
        user_order,
        vec![
            root.uuid.clone(),
            child_low.uuid.clone(),
            grandchild.uuid,
            child_high.uuid.clone()
        ]
    );
    fixture.store.take_committed_versions();
    let before = fixture.state();
    fixture
        .store
        .reorder_categories(ReorderCategories {
            parent_uuid: Some(root.uuid),
            uuids: vec![child_low.uuid, child_high.uuid],
        })
        .expect("same order");
    assert_eq!(fixture.state(), before);
    fixture.no_effects();
}

#[test]
fn order_overflow_rejects_create_and_move_without_effects() {
    let mut fixture = Fixture::new();
    let parent = fixture.create(1, None);
    let last_child = fixture.create(2, Some(parent.uuid.clone()));
    let outsider = fixture.create(3, None);
    diesel::update(categories::table.filter(categories::uuid.eq(&last_child.uuid.0)))
        .set(categories::sort_order.eq(i64::MAX))
        .execute(&mut fixture.store.writer)
        .expect("exhaust order");
    let values = fixture.values(Some(parent.uuid.clone()));
    fixture.reject(EditCategoryError::OrderOverflow, |store| {
        store.create_category(CreateCategory {
            uuid: Fixture::uuid(4),
            values,
        })
    });
    let values = CategoryValues {
        parent_uuid: Some(parent.uuid),
        ..outsider.values
    };
    fixture.reject(EditCategoryError::OrderOverflow, |store| {
        store.update_category(UpdateCategory {
            uuid: outsider.uuid,
            values,
        })
    });
}

#[test]
fn metadata_overflow_rolls_back_every_command_after_its_row_writes() {
    let mut fixture = Fixture::new();
    let parent = fixture.create(1, None);
    let first = fixture.create(2, Some(parent.uuid.clone()));
    let second = fixture.create(3, Some(parent.uuid.clone()));
    super::super::settings::Settings::new(&mut fixture.store.writer)
        .put_metadata_version(i64::MAX)
        .expect("exhaust metadata");
    fixture.store.take_committed_versions();
    let before = fixture.state();
    let create = fixture.store.create_category(CreateCategory {
        uuid: Fixture::uuid(4),
        values: fixture.values(None),
    });
    assert!(matches!(
        create,
        Err(ExecuteCategoryCommandError::Version { .. })
    ));
    assert_eq!(fixture.state(), before);
    fixture.no_effects();
    let update = fixture.store.update_category(UpdateCategory {
        uuid: first.uuid.clone(),
        values: fixture.values(None),
    });
    assert!(matches!(
        update,
        Err(ExecuteCategoryCommandError::Version { .. })
    ));
    assert_eq!(fixture.state(), before);
    fixture.no_effects();
    let reorder = fixture.store.reorder_categories(ReorderCategories {
        parent_uuid: Some(parent.uuid),
        uuids: vec![second.uuid, first.uuid.clone()],
    });
    assert!(matches!(
        reorder,
        Err(ExecuteCategoryCommandError::Version { .. })
    ));
    assert_eq!(fixture.state(), before);
    fixture.no_effects();
    let archive = fixture.store.archive_category(ArchiveCategory {
        uuid: first.uuid.clone(),
        state: CategoryArchiveState::Archived,
    });
    assert!(matches!(
        archive,
        Err(ExecuteCategoryCommandError::Version { .. })
    ));
    assert_eq!(fixture.state(), before);
    fixture.no_effects();
    // No-op commands do not attempt a version increment, even at the limit.
    fixture
        .store
        .update_category(UpdateCategory {
            uuid: first.uuid.clone(),
            values: first.values.clone(),
        })
        .expect("no-op at version limit");
    fixture
        .store
        .create_category(CreateCategory {
            uuid: first.uuid.clone(),
            values: first.values,
        })
        .expect("retry at version limit");
    fixture
        .store
        .archive_category(ArchiveCategory {
            uuid: first.uuid,
            state: CategoryArchiveState::Active,
        })
        .expect("archive no-op at version limit");
    fixture.no_effects();
}

#[test]
fn failed_second_reorder_write_rolls_back_earlier_sibling_writes() {
    use diesel::connection::SimpleConnection;
    let mut fixture = Fixture::new();
    let parent = fixture.create(1, None);
    let first = fixture.create(2, Some(parent.uuid.clone()));
    let second = fixture.create(3, Some(parent.uuid.clone()));
    fixture.store.writer.batch_execute(&format!(
        "CREATE TRIGGER reject_reorder BEFORE UPDATE OF sort_order ON categories WHEN OLD.id={} BEGIN SELECT RAISE(ABORT, 'test rejection'); END;", first.id.0
    )).expect("reject second update");
    fixture.store.take_committed_versions();
    let before = fixture.state();
    let result = fixture.store.reorder_categories(ReorderCategories {
        parent_uuid: Some(parent.uuid),
        uuids: vec![second.uuid, first.uuid],
    });
    assert!(matches!(
        result,
        Err(ExecuteCategoryCommandError::Database { .. })
    ));
    assert_eq!(fixture.state(), before);
    fixture.no_effects();
}

#[test]
fn archive_preserves_annotation_and_persisted_rule_references_on_reload() {
    use crate::{rules::registry::RegistryShell, store::query::InputRow};
    use quartz_domain::{
        capture::UtcInstant,
        facts::QueryRange,
        rules::{RuleBody, registry::RegistryCommand},
    };
    use quartz_protocol::{
        AnnotationCategory, AnnotationId, AnnotationKind, AnnotationValues,
        AnnotationsCreateRequest, CreateAnnotation,
    };
    let mut fixture = Fixture::new();
    let category = fixture.create(1, None);
    let now = UtcInstant::try_from(100).expect("UTC");
    let annotation_id = AnnotationId(
        "00000000-0000-4000-8000-000000000099"
            .parse()
            .expect("annotation UUID"),
    );
    let annotation = fixture
        .store
        .create_annotations(
            AnnotationsCreateRequest {
                entities: vec![CreateAnnotation {
                    uuid: annotation_id.clone(),
                    kind: AnnotationKind::CategoryOverride,
                    values: AnnotationValues {
                        start_utc_ms: 0,
                        end_utc_ms: 100,
                        category: Some(AnnotationCategory::Uuid {
                            uuid: category.uuid.0.parse().expect("category UUID"),
                        }),
                        label: None,
                        note: None,
                    },
                }],
            },
            now,
        )
        .expect("annotation");
    let mut shell = RegistryShell::open(&mut fixture.store, now).expect("registry");
    let key = "user.category-reference".parse().expect("rule set key");
    shell.run(&mut fixture.store, now, RegistryCommand::Apply {
        key,
        file: "category-test.toml".into(),
        text: format!("format='quartz-rules/1'\n[set]\nkey='user.category-reference'\nkind='classification'\nname='Test'\n[[rule]]\nid='one'\nlabel='Test rule'\ncategory='{}'\nwhen=[{{field='app.exe_name',op='equals',value='test.exe'}}]\n", category.uuid.0),
        base_version: 0,
    }).expect("persist category rule");
    let before_rules = fixture.store.rule_sets().load().expect("rule state");
    fixture.store.take_committed_versions();
    let before_version = fixture.version();
    fixture.archived(category.uuid.clone(), CategoryArchiveState::Archived);
    assert_eq!(fixture.version(), before_version + 1);
    let effects = fixture.store.take_committed_versions();
    assert_eq!(effects.metadata_version, Some(before_version + 1));
    assert!(
        effects.facts.is_empty(),
        "category archive must not rewrite annotation facts"
    );
    let state = fixture.store.rule_sets().load().expect("reload rules");
    let key = "user.category-reference".parse().expect("key");
    assert_eq!(
        state.sets[&key].current.set.content,
        before_rules.sets[&key].current.set.content
    );
    let RuleBody::Classification {
        category: reference,
        ..
    } = &state.sets[&key].current.set.content.rule[0].body
    else {
        panic!("classification rule expected")
    };
    assert_eq!(reference.uuid, category.uuid);
    let mut snapshot = fixture
        .store
        .take_query_reader()
        .expect("reader")
        .snapshot();
    snapshot.begin().expect("begin snapshot");
    let mut loaded = Vec::new();
    while let Some(batch) = snapshot
        .read_batch(QueryRange { start: 0, end: 100 })
        .expect("read facts")
    {
        for row in batch {
            if let InputRow::Annotation(row) = row {
                loaded.push(row);
            }
        }
    }
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].id, annotation_id);
    assert_eq!(loaded[0].category, Some(category.id));
    assert!(
        snapshot
            .categories()
            .expect("active metadata")
            .iter()
            .all(|row| row.id != category.id)
    );
    fixture
        .store
        .return_query_reader(snapshot.finish().expect("finish snapshot"));
    // The annotation write retry still returns the original retained category reference.
    let retry = fixture
        .store
        .create_annotations(
            AnnotationsCreateRequest {
                entities: vec![CreateAnnotation {
                    uuid: annotation_id,
                    kind: AnnotationKind::Note,
                    values: AnnotationValues {
                        start_utc_ms: 0,
                        end_utc_ms: 1,
                        category: None,
                        label: None,
                        note: None,
                    },
                }],
            },
            now,
        )
        .expect("retry annotation after archive");
    assert_eq!(retry, annotation);
    fixture.no_effects();
}

#[test]
fn update_normalizes_a_stored_untrimmed_name_once() {
    let mut fixture = Fixture::new();
    let original = fixture.create(1, None);
    diesel::update(categories::table.filter(categories::uuid.eq(&original.uuid.0)))
        .set(categories::name.eq("  项目 🦀  "))
        .execute(&mut fixture.store.writer)
        .expect("legacy name");
    fixture.store.take_committed_versions();
    let version = fixture.version();
    let command = UpdateCategory {
        uuid: original.uuid,
        values: original.values,
    };
    fixture
        .store
        .update_category(command.clone())
        .expect("normalize name");
    assert_eq!(fixture.version(), version + 1);
    let name = categories::table
        .filter(categories::uuid.eq(&command.uuid.0))
        .select(categories::name)
        .first::<Option<String>>(&mut fixture.store.writer)
        .expect("stored name");
    assert_eq!(name.as_deref(), Some("项目 🦀"));
    assert_eq!(
        fixture.store.take_committed_versions().metadata_version,
        Some(version + 1)
    );
    fixture
        .store
        .update_category(command)
        .expect("normalized retry");
    fixture.no_effects();
}

#[test]
fn create_uses_one_tree_load_and_retry_uses_no_tree_load() {
    use crate::persistence::dispatch::WriteRequest;
    use diesel::connection::InstrumentationEvent;
    use std::sync::{Arc, Mutex};
    let mut fixture = Fixture::new();
    let queries = Arc::new(Mutex::new(Vec::new()));
    let observed = queries.clone();
    fixture
        .store
        .writer
        .set_instrumentation(move |event: InstrumentationEvent<'_>| {
            if let InstrumentationEvent::StartQuery { query, .. } = event {
                observed.lock().expect("queries").push(query.to_string());
            }
        });
    let entity = quartz_protocol::NewCategory {
        uuid: "abcdef00-0000-4000-8000-000000000001"
            .parse()
            .expect("UUID"),
        parent_uuid: None,
        name: Some("New category".into()),
        color: "neutral".into(),
        tendency: quartz_protocol::CategoryTendency::Neutral,
    };
    WriteRequest::decode(
        quartz_protocol::Op::CategoriesCreate,
        serde_json::to_value(quartz_protocol::CategoriesCreateRequest {
            entity: entity.clone(),
        })
        .expect("request"),
    )
    .expect("decode")
    .execute(
        &mut fixture.store,
        quartz_domain::capture::UtcInstant::try_from(0).expect("UTC"),
        None,
    )
    .expect("create");
    let created = std::mem::take(&mut *queries.lock().expect("queries"));
    let tree_loads = |queries: &[String]| {
        queries
            .iter()
            .filter(|sql| sql.contains("FROM `categories`") && !sql.contains("WHERE"))
            .count()
    };
    assert_eq!(tree_loads(&created), 1);
    let mut invalid_retry = entity;
    invalid_retry.name = Some("builtin:reserved".into());
    invalid_retry.color = "invalid".into();
    WriteRequest::decode(
        quartz_protocol::Op::CategoriesCreate,
        serde_json::to_value(quartz_protocol::CategoriesCreateRequest {
            entity: invalid_retry,
        })
        .expect("request"),
    )
    .expect("decode")
    .execute(
        &mut fixture.store,
        quartz_domain::capture::UtcInstant::try_from(0).expect("UTC"),
        None,
    )
    .expect("UUID retry precedes value validation");
    assert_eq!(tree_loads(&queries.lock().expect("queries")), 0);
}
