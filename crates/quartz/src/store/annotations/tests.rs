use super::*;
use crate::{
    data_directory::DataDirectory,
    store::versions::{UtcDay, VersionSource},
};
use quartz_domain::capture::limits::UTC_DAY_MS;
use quartz_protocol::{BuiltinCategory, CreateAnnotation, UpdateAnnotation};

struct Fixture {
    store: Store,
    _directory: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("isolated annotation database");
        let mut store = Store::open(&DataDirectory::new(directory.path())).expect("store");
        store.seed_categories().expect("categories");
        store.take_committed_versions();
        Self {
            store,
            _directory: directory,
        }
    }
    fn create(
        &mut self,
        entities: Vec<CreateAnnotation>,
    ) -> Result<AnnotationsResponse, ExecuteAnnotationWritesError> {
        self.store.create_annotations(
            AnnotationsCreateRequest { entities },
            UtcInstant::try_from(100).expect("UTC"),
        )
    }
    fn update(
        &mut self,
        entities: Vec<UpdateAnnotation>,
    ) -> Result<AnnotationsResponse, ExecuteAnnotationWritesError> {
        self.store.update_annotations(
            AnnotationsUpdateRequest { entities },
            UtcInstant::try_from(200).expect("UTC"),
        )
    }
    fn rows(&mut self) -> Vec<AnnotationsRow> {
        annotations::table
            .order((annotations::start_utc_ms, annotations::uuid))
            .select(AnnotationsRow::as_select())
            .load(&mut self.store.writer)
            .expect("annotations")
    }
}
fn id(value: u32) -> AnnotationId {
    AnnotationId(
        format!("00000000-0000-4000-8000-{value:012x}")
            .parse()
            .expect("UUID"),
    )
}
fn entity(value: u32, kind: AnnotationKind, start: i64, end: i64) -> CreateAnnotation {
    CreateAnnotation {
        uuid: id(value),
        kind,
        values: AnnotationValues {
            start_utc_ms: start,
            end_utc_ms: end,
            category: (kind == AnnotationKind::CategoryOverride).then_some(
                AnnotationCategory::Builtin {
                    key: BuiltinCategory::Development,
                },
            ),
            label_preset: None,
            label: (kind == AnnotationKind::Note).then(|| "note".into()),
            note: None,
        },
    }
}
fn update(entity: CreateAnnotation) -> UpdateAnnotation {
    UpdateAnnotation {
        uuid: entity.uuid,
        kind: Some(entity.kind),
        values: entity.values,
    }
}

#[test]
fn create_retry_returns_stored_values_without_clipping_or_version_bumps() {
    let mut fixture = Fixture::new();
    let first = fixture
        .create(vec![entity(1, AnnotationKind::ActiveOverride, 0, 100)])
        .expect("create");
    assert_eq!(first.entities[0].created_utc_ms, 100);
    assert_eq!(first.entities[0].updated_utc_ms, 100);
    fixture.store.take_committed_versions();
    fixture
        .create(vec![entity(2, AnnotationKind::Note, 0, 200)])
        .expect("overlapping note");
    fixture.store.take_committed_versions();
    let retry = fixture
        .create(vec![entity(1, AnnotationKind::Note, -100, 300)])
        .expect("retry existing UUID");
    assert_eq!(first, retry);
    assert_eq!(fixture.rows().len(), 2);
    assert!(fixture.store.take_committed_versions().facts.is_empty());
}

#[test]
fn trims_left_and_right_and_deletes_fully_covered_annotations() {
    let mut fixture = Fixture::new();
    fixture
        .create(vec![
            entity(1, AnnotationKind::ActiveOverride, 0, 20),
            entity(2, AnnotationKind::ActiveOverride, 30, 40),
            entity(3, AnnotationKind::ActiveOverride, 50, 70),
        ])
        .expect("old disjoint ranges");
    fixture
        .create(vec![entity(4, AnnotationKind::ActiveOverride, 10, 60)])
        .expect("clip three rows");
    let rows = fixture.rows();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].identity().expect("identity"), id(1));
    assert_eq!(rows[0].range(), QueryRange { start: 0, end: 10 });
    assert_eq!(rows[1].identity().expect("identity"), id(4));
    assert_eq!(rows[2].identity().expect("identity"), id(3));
    assert_eq!(rows[2].range(), QueryRange { start: 60, end: 70 });
}

#[test]
fn containment_splits_with_deterministic_identity_and_injected_timestamps() {
    let mut fixture = Fixture::new();
    fixture
        .create(vec![entity(1, AnnotationKind::OfflineActivity, 0, 100)])
        .expect("old range");
    let response = fixture
        .update(vec![update(entity(
            1,
            AnnotationKind::OfflineActivity,
            0,
            120,
        ))])
        .expect("update old");
    assert_eq!(response.entities[0].created_utc_ms, 100);
    assert_eq!(response.entities[0].updated_utc_ms, 200);
    let writer = entity(2, AnnotationKind::OfflineActivity, 20, 80);
    let first = fixture.create(vec![writer.clone()]).expect("split");
    let rows = fixture.rows();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].identity().expect("left identity"), id(1));
    assert_eq!(rows[0].range(), QueryRange { start: 0, end: 20 });
    let expected = SplitIdentity {
        old: &id(1),
        writing: &id(2),
        old_range: QueryRange { start: 0, end: 120 },
        writing_range: QueryRange { start: 20, end: 80 },
    }
    .derive();
    assert_eq!(rows[2].identity().expect("right identity"), expected);
    assert_eq!(
        rows[2].range(),
        QueryRange {
            start: 80,
            end: 120
        }
    );
    assert_eq!(rows[2].created_utc_ms, 100);
    assert_ne!(
        expected,
        SplitIdentity {
            old: &id(1),
            writing: &id(3),
            old_range: QueryRange { start: 0, end: 120 },
            writing_range: QueryRange { start: 20, end: 80 },
        }
        .derive()
    );
    fixture.store.take_committed_versions();
    assert_eq!(fixture.create(vec![writer]).expect("retry"), first);
    assert_eq!(
        fixture.rows()[2].identity().expect("stable remainder"),
        expected
    );
    assert!(fixture.store.take_committed_versions().facts.is_empty());
}

#[test]
fn notes_and_different_kinds_keep_overlapping_ranges() {
    let mut fixture = Fixture::new();
    let mut entities = vec![
        entity(1, AnnotationKind::Note, 0, 100),
        entity(2, AnnotationKind::Note, 0, 100),
    ];
    for (index, kind) in [
        AnnotationKind::CategoryOverride,
        AnnotationKind::OfflineActivity,
        AnnotationKind::ActiveOverride,
        AnnotationKind::DismissedAway,
    ]
    .into_iter()
    .enumerate()
    {
        entities.push(entity(
            u32::try_from(index).expect("small index") + 3,
            kind,
            0,
            100,
        ));
    }
    fixture
        .create(entities)
        .expect("cross-kind and note overlaps");
    assert_eq!(fixture.rows().len(), 6);
    assert!(
        fixture
            .rows()
            .iter()
            .all(|row| row.range() == QueryRange { start: 0, end: 100 })
    );
}

#[test]
fn every_clipping_kind_rejects_overlapping_batch_atomically_and_allows_adjacent_ranges() {
    for kind in [
        AnnotationKind::CategoryOverride,
        AnnotationKind::OfflineActivity,
        AnnotationKind::ActiveOverride,
        AnnotationKind::DismissedAway,
    ] {
        let mut fixture = Fixture::new();
        assert!(matches!(
            fixture.create(vec![entity(1, kind, 0, 20), entity(2, kind, 10, 30)]),
            Err(ExecuteAnnotationWritesError::InvalidRange)
        ));
        assert!(fixture.rows().is_empty());
        assert!(fixture.store.take_committed_versions().facts.is_empty());
        fixture
            .create(vec![entity(1, kind, 0, 10), entity(2, kind, 10, 20)])
            .expect("adjacent batch");
        fixture
            .create(vec![entity(3, kind, 5, 15)])
            .expect("all clipping kinds trim");
        assert_eq!(fixture.rows().len(), 3);
    }
}

#[test]
fn updates_clip_and_retry_preserves_timestamps_and_versions() {
    let mut fixture = Fixture::new();
    fixture
        .create(vec![
            entity(1, AnnotationKind::ActiveOverride, 0, 10),
            entity(2, AnnotationKind::ActiveOverride, 20, 100),
        ])
        .expect("create");
    let changed = update(entity(1, AnnotationKind::ActiveOverride, 30, 60));
    let first = fixture
        .update(vec![changed.clone()])
        .expect("update clips old containing range");
    assert_eq!(fixture.rows().len(), 3);
    fixture.store.take_committed_versions();
    let retry = fixture
        .store
        .update_annotations(
            AnnotationsUpdateRequest {
                entities: vec![changed],
            },
            UtcInstant::try_from(300).expect("later clock"),
        )
        .expect("retry update");
    assert_eq!(first, retry);
    assert!(fixture.store.take_committed_versions().facts.is_empty());
    assert_eq!(
        fixture.rows()[2].identity().expect("split identity"),
        SplitIdentity {
            old: &id(2),
            writing: &id(1),
            old_range: QueryRange {
                start: 20,
                end: 100
            },
            writing_range: QueryRange { start: 30, end: 60 },
        }
        .derive()
    );
}

#[test]
fn update_batches_validate_before_writes_and_can_swap_ranges() {
    let mut fixture = Fixture::new();
    fixture
        .create(vec![
            entity(1, AnnotationKind::ActiveOverride, 0, 10),
            entity(2, AnnotationKind::ActiveOverride, 20, 30),
        ])
        .expect("create");
    fixture.store.take_committed_versions();
    assert!(matches!(
        fixture.update(vec![
            update(entity(1, AnnotationKind::ActiveOverride, 5, 25)),
            update(entity(2, AnnotationKind::ActiveOverride, 20, 30))
        ]),
        Err(ExecuteAnnotationWritesError::InvalidRange)
    ));
    assert_eq!(fixture.rows()[0].range(), QueryRange { start: 0, end: 10 });
    assert!(fixture.store.take_committed_versions().facts.is_empty());
    fixture
        .update(vec![
            update(entity(1, AnnotationKind::ActiveOverride, 20, 30)),
            update(entity(2, AnnotationKind::ActiveOverride, 0, 10)),
        ])
        .expect("swap ranges");
    let rows = fixture.rows();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].identity().expect("swapped"), id(2));
}

#[test]
fn delete_missing_succeeds_and_deletion_never_restores_clipped_ranges() {
    let mut fixture = Fixture::new();
    fixture
        .create(vec![entity(1, AnnotationKind::DismissedAway, 0, 100)])
        .expect("old");
    fixture
        .create(vec![entity(2, AnnotationKind::DismissedAway, 20, 80)])
        .expect("split");
    fixture
        .store
        .delete_annotations(AnnotationsDeleteRequest {
            uuids: vec![id(2), id(9)],
        })
        .expect("delete");
    fixture.store.take_committed_versions();
    fixture
        .store
        .delete_annotations(AnnotationsDeleteRequest { uuids: vec![id(2)] })
        .expect("retry delete");
    assert!(fixture.store.take_committed_versions().facts.is_empty());
    let rows = fixture.rows();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].range(), QueryRange { start: 0, end: 20 });
    assert_eq!(
        rows[1].range(),
        QueryRange {
            start: 80,
            end: 100
        }
    );
}

#[test]
fn rejects_kind_changes_missing_updates_duplicate_ids_invalid_ranges_and_fields() {
    let mut fixture = Fixture::new();
    fixture
        .create(vec![entity(1, AnnotationKind::ActiveOverride, 0, 10)])
        .expect("create");
    fixture.store.take_committed_versions();
    assert!(matches!(
        fixture.update(vec![update(entity(
            1,
            AnnotationKind::OfflineActivity,
            0,
            10
        ))]),
        Err(ExecuteAnnotationWritesError::ChangeKind)
    ));
    assert!(matches!(
        fixture.update(vec![update(entity(
            99,
            AnnotationKind::ActiveOverride,
            0,
            10
        ))]),
        Err(ExecuteAnnotationWritesError::MissingAnnotation)
    ));
    assert!(matches!(
        fixture.create(vec![
            entity(2, AnnotationKind::Note, 0, 10),
            entity(2, AnnotationKind::Note, 20, 30)
        ]),
        Err(ExecuteAnnotationWritesError::DuplicateIdentity)
    ));
    for end in [0, -1] {
        assert!(matches!(
            fixture.create(vec![entity(2, AnnotationKind::ActiveOverride, 0, end)]),
            Err(ExecuteAnnotationWritesError::InvalidRange)
        ));
    }
    let mut missing_category = entity(2, AnnotationKind::CategoryOverride, 0, 10);
    missing_category.values.category = None;
    let mut blank_note = entity(2, AnnotationKind::Note, 0, 10);
    blank_note.values.label = Some(" \t".into());
    let mut empty_note = blank_note.clone();
    empty_note.values.label = None;
    let mut dismissed_label = entity(2, AnnotationKind::DismissedAway, 0, 10);
    dismissed_label.values.label = Some(String::new());
    let mut dismissed_note = entity(2, AnnotationKind::DismissedAway, 0, 10);
    dismissed_note.values.note = Some("private".into());
    let mut dismissed_category = entity(2, AnnotationKind::DismissedAway, 0, 10);
    dismissed_category.values.category = Some(AnnotationCategory::Builtin {
        key: BuiltinCategory::Learning,
    });
    for invalid in [
        missing_category,
        blank_note,
        empty_note,
        dismissed_label,
        dismissed_note,
        dismissed_category,
    ] {
        assert!(matches!(
            fixture.create(vec![invalid]),
            Err(ExecuteAnnotationWritesError::InvalidFields)
        ));
    }
    assert_eq!(fixture.rows().len(), 1);
    assert!(fixture.store.take_committed_versions().facts.is_empty());
}

#[test]
fn resolves_builtin_and_uuid_categories_and_rejects_missing_or_archived_ones_atomically() {
    let mut fixture = Fixture::new();
    let first = fixture
        .create(vec![entity(1, AnnotationKind::CategoryOverride, 0, 10)])
        .expect("builtin category");
    let category = first.entities[0]
        .values
        .category
        .clone()
        .expect("canonical UUID category");
    assert!(matches!(category, AnnotationCategory::Uuid { .. }));
    let mut by_uuid = entity(2, AnnotationKind::OfflineActivity, 20, 30);
    by_uuid.values.category = Some(category.clone());
    fixture
        .create(vec![by_uuid.clone()])
        .expect("category UUID");
    let mut missing = by_uuid.clone();
    missing.uuid = id(3);
    missing.values.category = Some(AnnotationCategory::Uuid { uuid: id(99).0 });
    fixture.store.take_committed_versions();
    assert!(matches!(
        fixture.create(vec![entity(4, AnnotationKind::Note, 0, 10), missing]),
        Err(ExecuteAnnotationWritesError::InvalidCategory)
    ));
    diesel::update(categories::table.filter(categories::builtin_key.eq("development")))
        .set(categories::archived.eq(1))
        .execute(&mut fixture.store.writer)
        .expect("archive category");
    by_uuid.uuid = id(3);
    assert!(matches!(
        fixture.create(vec![by_uuid]),
        Err(ExecuteAnnotationWritesError::InvalidCategory)
    ));
    assert!(matches!(
        fixture.create(vec![entity(3, AnnotationKind::CategoryOverride, 30, 40)]),
        Err(ExecuteAnnotationWritesError::InvalidCategory)
    ));
    assert_eq!(
        fixture
            .create(vec![entity(1, AnnotationKind::CategoryOverride, 0, 10)])
            .expect("retry retains archived reference"),
        first
    );
    assert_eq!(fixture.rows().len(), 2);
    assert!(fixture.store.take_committed_versions().facts.is_empty());
}

#[test]
fn all_old_new_split_trimmed_and_deleted_utc_days_are_invalidated() {
    let mut fixture = Fixture::new();
    fixture
        .create(vec![entity(
            1,
            AnnotationKind::ActiveOverride,
            -UTC_DAY_MS,
            3 * UTC_DAY_MS,
        )])
        .expect("multi-day old");
    let created = fixture.store.take_committed_versions();
    assert_eq!(
        created
            .facts
            .iter()
            .map(|fact| fact.key.day)
            .collect::<Vec<_>>(),
        vec![UtcDay(-1), UtcDay(0), UtcDay(1), UtcDay(2)]
    );
    fixture
        .create(vec![entity(
            2,
            AnnotationKind::ActiveOverride,
            UTC_DAY_MS,
            2 * UTC_DAY_MS,
        )])
        .expect("split middle day");
    let split = fixture.store.take_committed_versions();
    assert_eq!(split.facts.len(), 4);
    assert!(
        split
            .facts
            .iter()
            .all(|fact| fact.key.source == VersionSource(4) && fact.version > 1)
    );
    fixture
        .update(vec![update(entity(
            2,
            AnnotationKind::ActiveOverride,
            4 * UTC_DAY_MS,
            5 * UTC_DAY_MS,
        ))])
        .expect("move");
    let moved = fixture.store.take_committed_versions();
    assert_eq!(
        moved
            .facts
            .iter()
            .map(|fact| fact.key.day)
            .collect::<Vec<_>>(),
        vec![UtcDay(1), UtcDay(4)]
    );
    fixture
        .store
        .delete_annotations(AnnotationsDeleteRequest { uuids: vec![id(1)] })
        .expect("delete multi-day left");
    let deleted = fixture.store.take_committed_versions();
    assert_eq!(
        deleted
            .facts
            .iter()
            .map(|fact| fact.key.day)
            .collect::<Vec<_>>(),
        vec![UtcDay(-1), UtcDay(0)]
    );
}

#[test]
fn split_identity_collision_rolls_back_rows_and_versions() {
    let mut fixture = Fixture::new();
    let mut collision = entity(3, AnnotationKind::Note, 200, 300);
    collision.uuid = SplitIdentity {
        old: &id(1),
        writing: &id(2),
        old_range: QueryRange { start: 0, end: 100 },
        writing_range: QueryRange { start: 20, end: 80 },
    }
    .derive();
    fixture
        .create(vec![
            entity(1, AnnotationKind::ActiveOverride, 0, 100),
            collision,
        ])
        .expect("old and colliding UUID");
    fixture.store.take_committed_versions();
    assert!(matches!(
        fixture.create(vec![entity(2, AnnotationKind::ActiveOverride, 20, 80)]),
        Err(ExecuteAnnotationWritesError::Database { .. })
    ));
    assert_eq!(fixture.rows().len(), 2);
    assert_eq!(fixture.rows()[0].range(), QueryRange { start: 0, end: 100 });
    assert!(fixture.store.take_committed_versions().facts.is_empty());
}

#[test]
fn splitting_during_update_reserves_the_writers_existing_sqlite_identity() {
    let mut fixture = Fixture::new();
    fixture
        .create(vec![
            entity(1, AnnotationKind::ActiveOverride, 20, 100),
            entity(2, AnnotationKind::ActiveOverride, 0, 10),
        ])
        .expect("writer has largest SQLite rowid");
    fixture
        .update(vec![update(entity(
            2,
            AnnotationKind::ActiveOverride,
            30,
            60,
        ))])
        .expect("split does not reuse writer's rowid");
    let rows = fixture.rows();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].range(), QueryRange { start: 20, end: 30 });
    assert_eq!(rows[1].identity().expect("writer identity"), id(2));
    assert_eq!(
        rows[2].range(),
        QueryRange {
            start: 60,
            end: 100
        }
    );
}

#[test]
fn moving_the_same_writer_splits_the_original_annotation_again() {
    let mut fixture = Fixture::new();
    fixture
        .create(vec![entity(1, AnnotationKind::ActiveOverride, 0, 100)])
        .expect("original annotation");
    fixture
        .create(vec![entity(2, AnnotationKind::ActiveOverride, 40, 60)])
        .expect("first split");
    let first_remainder = fixture.rows()[2].identity().expect("first remainder UUID");
    fixture
        .update(vec![update(entity(
            2,
            AnnotationKind::ActiveOverride,
            10,
            20,
        ))])
        .expect("same writer splits original again");
    let rows = fixture.rows();
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].identity().expect("original UUID"), id(1));
    assert_eq!(rows[0].range(), QueryRange { start: 0, end: 10 });
    assert_eq!(rows[1].identity().expect("writer UUID"), id(2));
    assert_eq!(rows[1].range(), QueryRange { start: 10, end: 20 });
    assert_eq!(rows[2].range(), QueryRange { start: 20, end: 40 });
    assert_ne!(
        rows[2].identity().expect("second remainder UUID"),
        first_remainder
    );
    assert_eq!(
        rows[3].identity().expect("first remainder survives"),
        first_remainder
    );
    assert_eq!(
        rows[3].range(),
        QueryRange {
            start: 60,
            end: 100
        }
    );
}

#[test]
fn retrying_a_writer_move_after_a_second_split_is_idempotent() {
    let mut fixture = Fixture::new();
    fixture
        .create(vec![entity(1, AnnotationKind::ActiveOverride, 0, 100)])
        .expect("original annotation");
    fixture
        .create(vec![entity(2, AnnotationKind::ActiveOverride, 40, 60)])
        .expect("first split");
    let moved = update(entity(2, AnnotationKind::ActiveOverride, 10, 20));
    let response = fixture.update(vec![moved.clone()]).expect("second split");
    let before = fixture.rows();
    fixture.store.take_committed_versions();
    let retry = fixture
        .store
        .update_annotations(
            AnnotationsUpdateRequest {
                entities: vec![moved],
            },
            UtcInstant::try_from(300).expect("later clock"),
        )
        .expect("retry second split");
    assert_eq!(retry, response);
    let after = fixture.rows();
    assert_eq!(after.len(), before.len());
    for (old, current) in before.iter().zip(&after) {
        assert_eq!(
            old.identity().expect("old UUID"),
            current.identity().expect("current UUID")
        );
        assert_eq!(old.range(), current.range());
        assert_eq!(old.created_utc_ms, current.created_utc_ms);
        assert_eq!(old.updated_utc_ms, current.updated_utc_ms);
    }
    assert!(fixture.store.take_committed_versions().facts.is_empty());
}

#[test]
fn another_writer_can_split_a_generated_remainder() {
    let mut fixture = Fixture::new();
    fixture
        .create(vec![entity(1, AnnotationKind::ActiveOverride, 0, 100)])
        .expect("original annotation");
    fixture
        .create(vec![entity(2, AnnotationKind::ActiveOverride, 40, 60)])
        .expect("first split");
    let remainder = fixture.rows()[2]
        .identity()
        .expect("generated remainder UUID");
    fixture
        .create(vec![entity(3, AnnotationKind::ActiveOverride, 70, 80)])
        .expect("split generated remainder");
    let rows = fixture.rows();
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[2].identity().expect("remainder left UUID"), remainder);
    assert_eq!(rows[2].range(), QueryRange { start: 60, end: 70 });
    assert_eq!(rows[3].identity().expect("new writer UUID"), id(3));
    assert_eq!(rows[3].range(), QueryRange { start: 70, end: 80 });
    let expected = SplitIdentity {
        old: &remainder,
        writing: &id(3),
        old_range: QueryRange {
            start: 60,
            end: 100,
        },
        writing_range: QueryRange { start: 70, end: 80 },
    }
    .derive();
    assert_ne!(expected, remainder);
    assert_eq!(rows[4].identity().expect("remainder right UUID"), expected);
    assert_eq!(
        rows[4].range(),
        QueryRange {
            start: 80,
            end: 100
        }
    );
}

#[test]
fn annotation_ranges_and_aggregate_work_are_rejected_before_database_access() {
    use diesel::connection::SimpleConnection;
    let mut fixture = Fixture::new();
    fixture
        .create(vec![entity(1, AnnotationKind::Note, 0, 1)])
        .expect("existing annotation");
    fixture.store.take_committed_versions();
    fixture
        .store
        .writer
        .batch_execute("PRAGMA query_only = ON")
        .expect("deny transactions that write");
    let max = MAX_QUERY_RANGE_MS;
    for range in [
        QueryRange {
            start: 0,
            end: i64::MAX,
        },
        QueryRange {
            start: i64::MIN,
            end: i64::MAX,
        },
        QueryRange {
            start: 0,
            end: max + 1,
        },
        QueryRange {
            start: i64::MAX - 1,
            end: i64::MAX,
        },
        QueryRange {
            start: i64::MIN,
            end: i64::MIN + 1,
        },
        QueryRange { start: 1, end: 1 },
        QueryRange { start: 1, end: 0 },
    ] {
        assert!(
            matches!(
                fixture.create(vec![entity(
                    2,
                    AnnotationKind::Note,
                    range.start,
                    range.end
                )]),
                Err(ExecuteAnnotationWritesError::InvalidRange)
            ),
            "create range {range:?}"
        );
        assert!(
            matches!(
                fixture.update(vec![update(entity(
                    1,
                    AnnotationKind::Note,
                    range.start,
                    range.end
                ))]),
                Err(ExecuteAnnotationWritesError::InvalidRange)
            ),
            "update range {range:?}"
        );
    }
    let entities = (2..=369)
        .map(|value| entity(value, AnnotationKind::Note, 0, 1))
        .collect::<Vec<_>>();
    assert!(matches!(
        fixture.create(entities.clone()),
        Err(ExecuteAnnotationWritesError::InvalidRange)
    ));
    assert!(matches!(
        fixture.update(entities.into_iter().map(update).collect()),
        Err(ExecuteAnnotationWritesError::InvalidRange)
    ));
    assert_eq!(fixture.rows().len(), 1);
    assert!(fixture.store.take_committed_versions().facts.is_empty());
}

#[test]
fn annotation_range_accepts_exact_limit_across_utc_midnight() {
    let mut fixture = Fixture::new();
    fixture
        .create(vec![entity(
            1,
            AnnotationKind::Note,
            -1,
            MAX_QUERY_RANGE_MS - 1,
        )])
        .expect("maximum positive duration");
    assert_eq!(fixture.store.take_committed_versions().facts.len(), 367);
}

#[test]
fn clipping_work_limit_is_checked_before_any_row_mutation() {
    use diesel::{Connection, connection::InstrumentationEvent};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let mut fixture = Fixture::new();
    // Seed disjoint old rows without exercising request admission.
    let rows = (0..MAX_ANNOTATION_DAY_WRITES)
        .map(|offset| {
            let value = u32::try_from(offset + 1).expect("fixture identity");
            AnnotationsRow {
                id: None,
                uuid: id(value).0.to_string(),
                kind: enums::AnnotationKind::ActiveOverride,
                start_utc_ms: offset * 2,
                end_utc_ms: offset * 2 + 1,
                category_id: None,
                label_preset: None,
                label: None,
                note: None,
                created_utc_ms: 0,
                updated_utc_ms: 0,
            }
        })
        .collect::<Vec<_>>();
    for chunk in rows.chunks(super::super::query::READ_BATCH_ROWS) {
        diesel::insert_into(annotations::table)
            .values(chunk)
            .execute(&mut fixture.store.writer)
            .expect("old annotations");
    }
    let mutations = Arc::new(AtomicUsize::new(0));
    let observed = mutations.clone();
    fixture
        .store
        .writer
        .set_instrumentation(move |event: InstrumentationEvent<'_>| {
            if let InstrumentationEvent::StartQuery { query, .. } = event {
                let sql = query.to_string();
                if sql.starts_with("INSERT")
                    || sql.starts_with("UPDATE")
                    || sql.starts_with("DELETE")
                {
                    observed.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
    let value = u32::try_from(MAX_ANNOTATION_DAY_WRITES + 1).expect("new identity");
    assert!(matches!(
        fixture.create(vec![entity(
            value,
            AnnotationKind::ActiveOverride,
            0,
            MAX_ANNOTATION_DAY_WRITES * 2
        )]),
        Err(ExecuteAnnotationWritesError::InvalidRange)
    ));
    assert_eq!(
        mutations.load(Ordering::Relaxed),
        0,
        "work must be checked before changing rows"
    );
    assert_eq!(fixture.rows().len(), rows.len());
    assert!(fixture.store.take_committed_versions().facts.is_empty());
}

#[test]
fn multi_entity_ranges_share_the_request_day_budget() {
    let mut fixture = Fixture::new();
    let half = MAX_QUERY_RANGE_MS / 2;
    let entities = vec![
        entity(1, AnnotationKind::Note, 0, half),
        entity(2, AnnotationKind::Note, half, MAX_QUERY_RANGE_MS),
    ];
    let created = fixture
        .create(entities)
        .expect("one year of aggregate work");
    fixture.store.take_committed_versions();
    let mut updates = created
        .entities
        .into_iter()
        .map(|row| UpdateAnnotation {
            uuid: row.uuid,
            kind: Some(row.kind),
            values: row.values,
        })
        .collect::<Vec<_>>();
    updates[1].values.end_utc_ms += 2 * UTC_DAY_MS;
    assert!(matches!(
        fixture.update(updates),
        Err(ExecuteAnnotationWritesError::InvalidRange)
    ));
    assert_eq!(fixture.rows()[1].end_utc_ms, MAX_QUERY_RANGE_MS);
    assert!(fixture.store.take_committed_versions().facts.is_empty());
}

#[test]
fn annotation_delete_rejects_uuid_limit_before_database_access() {
    use diesel::{Connection, connection::InstrumentationEvent};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let mut fixture = Fixture::new();
    fixture
        .create(vec![entity(1, AnnotationKind::Note, 0, 1)])
        .expect("existing annotation");
    fixture.store.take_committed_versions();
    let queries = Arc::new(AtomicUsize::new(0));
    let observed = queries.clone();
    fixture
        .store
        .writer
        .set_instrumentation(move |event: InstrumentationEvent<'_>| {
            if matches!(event, InstrumentationEvent::StartQuery { .. }) {
                observed.fetch_add(1, Ordering::Relaxed);
            }
        });
    let uuids = (1..=MAX_ANNOTATION_REQUEST_DAYS + 1)
        .map(|value| id(u32::try_from(value).expect("UUID index")))
        .collect();
    assert!(matches!(
        fixture
            .store
            .delete_annotations(AnnotationsDeleteRequest { uuids }),
        Err(ExecuteAnnotationWritesError::InvalidRange)
    ));
    assert_eq!(
        queries.load(Ordering::Relaxed),
        0,
        "oversized request must not reach SQLite"
    );
    assert_eq!(fixture.rows().len(), 1);
    assert!(fixture.store.take_committed_versions().facts.is_empty());
}

#[test]
fn annotation_delete_rejects_day_work_before_deleting_rows() {
    use diesel::{Connection, connection::InstrumentationEvent};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let mut fixture = Fixture::new();
    let days_per_annotation = MAX_QUERY_RANGE_MS / UTC_DAY_MS;
    let count = MAX_ANNOTATION_DAY_WRITES / days_per_annotation + 1;
    let mut uuids = Vec::new();
    for value in 1..=count {
        let value = u32::try_from(value).expect("UUID index");
        fixture
            .create(vec![entity(
                value,
                AnnotationKind::Note,
                0,
                MAX_QUERY_RANGE_MS,
            )])
            .expect("one maximum-range note per request");
        uuids.push(id(value));
    }
    fixture.store.take_committed_versions();
    let before = fixture
        .store
        .version_state()
        .expect("original versions")
        .days;
    let deletes = Arc::new(AtomicUsize::new(0));
    let observed = deletes.clone();
    fixture
        .store
        .writer
        .set_instrumentation(move |event: InstrumentationEvent<'_>| {
            if let InstrumentationEvent::StartQuery { query, .. } = event
                && query.to_string().starts_with("DELETE")
            {
                observed.fetch_add(1, Ordering::Relaxed);
            }
        });
    assert!(matches!(
        fixture
            .store
            .delete_annotations(AnnotationsDeleteRequest { uuids }),
        Err(ExecuteAnnotationWritesError::InvalidRange)
    ));
    assert_eq!(
        deletes.load(Ordering::Relaxed),
        0,
        "day budget must be checked before deleting any row"
    );
    assert_eq!(
        i64::try_from(fixture.rows().len()).expect("row count"),
        count
    );
    assert_eq!(
        fixture
            .store
            .version_state()
            .expect("rolled back versions")
            .days,
        before
    );
    assert!(fixture.store.take_committed_versions().facts.is_empty());
}

#[test]
fn label_preset_satisfies_note_content_and_is_preserved_by_updates() {
    let mut fixture = Fixture::new();
    let mut rest = entity(1, AnnotationKind::Note, 0, 100);
    rest.values.label = None;
    rest.values.label_preset = Some(AnnotationLabelPreset::Rest);
    let created = fixture
        .create(vec![rest.clone()])
        .expect("preset alone is valid note content");
    assert_eq!(created.entities[0].values, rest.values);
    assert_eq!(
        fixture.rows()[0].label_preset,
        Some(enums::AnnotationLabelPreset::Rest)
    );
    fixture.store.take_committed_versions();
    let unchanged = fixture
        .update(vec![update(rest.clone())])
        .expect("unchanged preset");
    assert_eq!(created, unchanged);
    assert!(fixture.store.take_committed_versions().facts.is_empty());
    rest.values.label = Some("custom label".into());
    rest.values.note = Some("custom note".into());
    let with_text = fixture
        .update(vec![update(rest.clone())])
        .expect("preset with user text");
    assert_eq!(with_text.entities[0].values, rest.values);
    rest.values.label_preset = None;
    let cleared = fixture
        .update(vec![update(rest.clone())])
        .expect("clear preset, keep text");
    assert_eq!(cleared.entities[0].values, rest.values);
    assert_eq!(fixture.rows()[0].label_preset, None);
}

#[test]
fn label_preset_is_rejected_on_every_non_note_kind() {
    let mut fixture = Fixture::new();
    for kind in [
        AnnotationKind::CategoryOverride,
        AnnotationKind::OfflineActivity,
        AnnotationKind::ActiveOverride,
        AnnotationKind::DismissedAway,
    ] {
        let mut invalid = entity(1, kind, 0, 100);
        invalid.values.label_preset = Some(AnnotationLabelPreset::Rest);
        assert!(
            matches!(
                fixture.create(vec![invalid.clone()]),
                Err(ExecuteAnnotationWritesError::InvalidFields)
            ),
            "{kind:?}"
        );
        invalid.values.label_preset = None;
        fixture
            .create(vec![invalid.clone()])
            .expect("valid non-note");
        invalid.values.label_preset = Some(AnnotationLabelPreset::Rest);
        assert!(
            matches!(
                fixture.update(vec![update(invalid)]),
                Err(ExecuteAnnotationWritesError::InvalidFields)
            ),
            "{kind:?}"
        );
        fixture
            .store
            .delete_annotations(AnnotationsDeleteRequest { uuids: vec![id(1)] })
            .expect("remove non-note");
    }
    assert!(fixture.rows().is_empty());
}

#[test]
fn note_without_label_preset_or_non_whitespace_text_is_rejected() {
    let mut fixture = Fixture::new();
    for text in [None, Some(""), Some(" \t\n\u{3000}")] {
        let mut empty = entity(1, AnnotationKind::Note, 0, 100);
        empty.values.label = text.map(String::from);
        empty.values.note = text.map(String::from);
        assert!(matches!(
            fixture.create(vec![empty]),
            Err(ExecuteAnnotationWritesError::InvalidFields)
        ));
    }
    let mut rest = entity(1, AnnotationKind::Note, 0, 100);
    rest.values.label = None;
    rest.values.label_preset = Some(AnnotationLabelPreset::Rest);
    fixture.create(vec![rest.clone()]).expect("preset note");
    rest.values.label_preset = None;
    assert!(matches!(
        fixture.update(vec![update(rest)]),
        Err(ExecuteAnnotationWritesError::InvalidFields)
    ));
    assert_eq!(
        fixture.rows()[0].label_preset,
        Some(enums::AnnotationLabelPreset::Rest)
    );
}
